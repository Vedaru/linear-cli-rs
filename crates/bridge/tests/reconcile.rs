//! The mirror, end to end: real sockets, two stateful fakes, the real store.
//!
//! The unit tests check the decision; this checks the thing the decision is for -
//! that one platform's change appears on the other, once, and that the webhook the
//! far platform then sends back does *nothing*. The fakes apply the writes they are
//! sent, so the echo path is genuine: the second delivery is produced by the state
//! the first one created, not by a script.
//!
//! ```text
//! Linear (fake GraphQL)  <->  Forgejo (fake REST)
//!          \                    /
//!           ReconcileHandler + SqliteStore
//! ```

mod support;

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use linear_bridge::connector::Source;
use linear_bridge::domain::{ConnectorId, EntityKind, EntityRef, Secret, UserMap};
use linear_bridge::queue::Handler;
use linear_bridge::reconcile::handler::{default_policy, Endpoint, Mapping, ReconcileHandler};
use linear_bridge::reconcile::{Direction, Sides, StateNames};
use linear_bridge::sink::Sink;
use linear_bridge::sources::declarative::DeclarativeSource;
use linear_bridge::sources::presets;
use linear_bridge::store::sqlite::SqliteStore;
use linear_bridge::store::{Delivery, Store};

use support::Fake;

const WEBHOOK_SECRET: &str = "0123456789abcdef";

/// The two platforms' state, as the fakes hold it. Writes are applied here, which
/// is what makes the mirror's echo real.
#[derive(Default)]
struct World {
    linear: Option<Value>,
    forgejo: Option<Value>,
    forgejo_next: i64,
    /// Comments posted on the forge, and every edit or deletion of one.
    forgejo_comments: Vec<Value>,
    forgejo_comment_calls: Vec<(String, Value)>,
    /// Every write the forge was asked to make, so a test can tell "nothing to do"
    /// from "did the same thing again".
    forgejo_writes: usize,
    linear_comments: Vec<Value>,
    /// Edits and deletions of mirrored comments, as Linear received them.
    linear_comment_edits: Vec<Value>,
    linear_comment_deletions: Vec<Value>,
}

fn state() -> Arc<Mutex<World>> {
    Arc::new(Mutex::new(World {
        forgejo_next: 12,
        ..World::default()
    }))
}

/// The Linear side: one issue, addressed by GraphQL query text.
fn linear_routes(
    world: Arc<Mutex<World>>,
) -> impl Fn(&str, &str, &Value) -> (u16, Value) + Send + Sync {
    move |_method, path, body| {
        assert_eq!(path, "/graphql");
        let query = body["query"].as_str().unwrap_or_default();
        let mut world = world.lock().unwrap();
        match () {
            _ if query.contains("query Teams") => (
                200,
                json!({ "data": { "teams": { "nodes": [{ "id": "uuid-team", "key": "VED" }] } } }),
            ),
            _ if query.contains("TeamStates") => (
                200,
                json!({ "data": { "team": { "states": {
                "nodes": [
                    { "id": "state-todo", "name": "Todo" },
                    { "id": "state-progress", "name": "In Progress" },
                    { "id": "state-done", "name": "Done" }
                ]
            } } } }),
            ),
            _ if query.contains("TeamLabels") => (
                200,
                json!({ "data": { "team": { "labels": {
                "nodes": [{ "id": "label-bug", "name": "Bug" }]
            } } } }),
            ),
            _ if query.contains("query Users") => {
                (200, json!({ "data": { "users": { "nodes": [] } } }))
            }
            _ if query.contains("query Issue") => match &world.linear {
                Some(issue) => (200, json!({ "data": { "issue": issue } })),
                None => (
                    200,
                    json!({ "errors": [{ "message": "Entity not found" }] }),
                ),
            },
            _ if query.contains("IssueUpdate") => {
                let input = &body["variables"]["input"];
                if let Some(issue) = world.linear.as_mut() {
                    if let Some(title) = input.get("title") {
                        issue["title"] = title.clone();
                    }
                    if let Some(description) = input.get("description") {
                        issue["description"] = description.clone();
                    }
                    if let Some(state_id) = input.get("stateId").and_then(Value::as_str) {
                        let name = match state_id {
                            "state-done" => "Done",
                            "state-progress" => "In Progress",
                            _ => "Todo",
                        };
                        issue["state"] = json!({ "name": name });
                    }
                }
                (
                    200,
                    json!({ "data": { "issueUpdate": { "success": true } } }),
                )
            }
            _ if query.contains("CommentUpdate") => {
                world.linear_comment_edits.push(body["variables"].clone());
                (
                    200,
                    json!({ "data": { "commentUpdate": { "success": true } } }),
                )
            }
            _ if query.contains("CommentDelete") => {
                world
                    .linear_comment_deletions
                    .push(body["variables"]["id"].clone());
                (
                    200,
                    json!({ "data": { "commentDelete": { "success": true } } }),
                )
            }
            _ if query.contains("CommentCreate") => {
                let input = &body["variables"]["input"];
                world.linear_comments.push(input.clone());
                (
                    200,
                    json!({ "data": { "commentCreate": { "success": true, "comment": {
                    "id": "linear-comment-1", "url": "https://linear.app/comment/1"
                } } } }),
                )
            }
            _ => (
                200,
                json!({ "errors": [{ "message": format!("unrouted: {query}") }] }),
            ),
        }
    }
}

/// The Forgejo side: a real issue store, addressed by REST paths.
fn forgejo_routes(
    world: Arc<Mutex<World>>,
) -> impl Fn(&str, &str, &Value) -> (u16, Value) + Send + Sync {
    move |method, path, body| {
        let mut world = world.lock().unwrap();
        // A comment is addressed by its own id, which is a different path from the
        // issue it belongs to - and the difference is the whole point of this route.
        if path.contains("/issues/comments/") {
            world
                .forgejo_comment_calls
                .push((method.to_string(), body.clone()));
            return match method {
                "PATCH" => (200, json!({ "id": 1, "body": body["body"] })),
                "DELETE" => (200, json!({})),
                _ => (405, json!({ "message": "no such comment method" })),
            };
        }
        let issue_at = path.starts_with("/api/v1/repos/Vedaru/linear-cli-rs/issues/");
        let suffix = path.rsplit('/').next().unwrap_or_default().to_string();
        match (
            method,
            issue_at,
            path.ends_with("/comments"),
            path.ends_with("/labels"),
        ) {
            ("GET", false, _, _) if path.ends_with("/labels") => (
                200,
                json!([
                    { "id": 3, "name": "Bug" },
                    { "id": 11, "name": "priority:high" }
                ]),
            ),
            ("GET", true, false, false) => match &world.forgejo {
                Some(issue) => (200, issue.clone()),
                None => (404, json!({ "message": "issue does not exist" })),
            },
            ("POST", false, false, false) => {
                world.forgejo_writes += 1;
                world.forgejo_next += 1;
                let number = world.forgejo_next;
                let issue = json!({
                    "number": number,
                    "html_url": format!("http://forge/Vedaru/linear-cli-rs/issues/{number}"),
                    "title": body["title"].clone(),
                    "body": body["body"].as_str().unwrap_or_default(),
                    "state": "open",
                    "labels": [{ "id": 3, "name": "Bug" }],
                    // Applied, not ignored: what the forge *holds* is what the next
                    // delivery is compared against.
                    "assignees": body.get("assignees").cloned().unwrap_or(json!([])),
                    "due_date": body.get("due_date").cloned().unwrap_or(Value::Null),
                });
                world.forgejo = Some(issue.clone());
                (201, issue)
            }
            ("PATCH", true, false, false) => {
                world.forgejo_writes += 1;
                let issue = world.forgejo.as_mut().expect("a patch needs an issue");
                // A real patch: the keys that arrived are applied, and the ones that
                // did not are left as they were - which is exactly what a partial
                // update promises.
                for key in ["title", "body", "state", "assignees", "due_date"] {
                    if let Some(value) = body.get(key) {
                        issue[key] = value.clone();
                    }
                }
                (200, issue.clone())
            }
            ("PUT", true, false, true) => {
                world.forgejo_writes += 1;
                let issue = world
                    .forgejo
                    .as_mut()
                    .expect("a label write needs an issue");
                if let Some(labels) = body.get("labels").and_then(Value::as_array) {
                    issue["labels"] = Value::Array(
                        labels
                            .iter()
                            .map(|id| json!({ "id": id, "name": if id == 3 { "Bug" } else { "priority:high" } }))
                            .collect(),
                    );
                }
                (200, json!([]))
            }
            ("POST", true, true, false) => {
                world.forgejo_comments.push(body.clone());
                let id = world.forgejo_comments.len();
                (
                    201,
                    json!({ "id": id, "html_url": format!("http://forge/comment/{id}") }),
                )
            }
            _ => (
                404,
                json!({ "message": format!("no route for {method} {path} ({suffix})") }),
            ),
        }
    }
}

/// The handler, wired to both fakes and a real database.
///
/// The database is a file rather than `:memory:` because the harness reads the
/// links the handler wrote: two connections to an in-memory database are two
/// different databases, and the assertion would pass against an empty one.
struct Harness {
    handler: ReconcileHandler,
    store: Box<dyn Store>,
    world: Arc<Mutex<World>>,
    sources: Vec<Arc<dyn Source>>,
    linear: Fake,
    forgejo: Fake,
}

fn database() -> String {
    let path = support::test_database("bridge-reconcile");
    path.to_string_lossy().to_string()
}

impl Harness {
    fn start() -> Self {
        Self::with_direction(Direction::Both)
    }

    fn with_direction(direction: Direction) -> Self {
        let world = state();
        let linear = Fake::start(linear_routes(Arc::clone(&world)));
        let forgejo = Fake::start(forgejo_routes(Arc::clone(&world)));

        let sources: Vec<Arc<dyn Source>> = vec![
            Arc::new(DeclarativeSource::new(
                "linear",
                Secret::new(WEBHOOK_SECRET),
                presets::preset("linear").expect("the linear preset loads"),
            )),
            Arc::new(DeclarativeSource::new(
                "forgejo",
                Secret::new(WEBHOOK_SECRET),
                presets::preset("forgejo").expect("the forgejo preset loads"),
            )),
        ];
        let sinks: Vec<Arc<dyn Sink>> = vec![
            Arc::new(linear.sink("linear")),
            Arc::new(forgejo.sink("forgejo")),
        ];

        let mut policy = default_policy(Sides::new(
            StateNames {
                closed: vec!["Done".into(), "Canceled".into()],
                initial: Some("Todo".into()),
                open: Some("In Progress".into()),
            },
            StateNames {
                closed: vec!["closed".into()],
                initial: None,
                open: Some("open".into()),
            },
        ));
        policy.direction = direction;
        let mapping = Mapping {
            name: "linear-cli-rs".into(),
            users: UserMap::default(),
            source: Endpoint::parse("linear:VED").unwrap(),
            sink: Endpoint::parse("forgejo:Vedaru/linear-cli-rs").unwrap(),
            routes: Default::default(),
            sink_location: Default::default(),
            policy,
        };

        let path = database();
        let mut store = SqliteStore::open(&path).expect("a store");
        store.migrate().expect("migrated");
        let handler = ReconcileHandler::new(
            sources.clone(),
            sinks,
            vec![mapping],
            Box::new(SqliteStore::open(&path).expect("the handler's own connection")),
        )
        .expect("the mapping is carryable");

        Self {
            handler,
            store: Box::new(store),
            world,
            sources,
            linear,
            forgejo,
        }
    }

    /// Deliver a stored body, exactly as the queue does: the row is built from
    /// what intake parsed, and the handler re-parses the body it kept.
    fn deliver(&mut self, connector: &str, event: &str, body: &str) {
        let source = self
            .sources
            .iter()
            .find(|source| source.id().as_str() == connector)
            .expect("a configured source");
        let parsed = source
            .parse(&source.replay_headers(event), body.as_bytes())
            .expect("the body parses");
        let first = parsed.first().expect("at least one event");
        let delivery = Delivery {
            id: 1,
            connector: first.connector.clone(),
            delivery_id: first.delivery.as_str().to_string(),
            event: event.to_string(),
            kind: first.kind.clone(),
            action: first.action.clone(),
            scope: first.subject.scope.clone(),
            native_id: first.subject.native_id.clone(),
            body: body.to_string(),
            attempts: 1,
            last_error: None,
        };
        self.handler
            .handle(&delivery)
            .expect("the delivery is handled");
    }

    /// How many writes the forge was asked to make.
    fn forgejo_writes(&self) -> usize {
        self.world.lock().unwrap().forgejo_writes
    }

    /// The forge issue as the fake holds it now.
    fn forgejo(&self) -> Value {
        self.world
            .lock()
            .unwrap()
            .forgejo
            .clone()
            .expect("the forge has an issue")
    }

    /// Every link that has this entity at either end.
    fn links_for(&mut self, side: EntityRef) -> Vec<linear_bridge::store::Link> {
        self.store.find_links(&side).expect("a readable store")
    }

    fn links(&mut self) -> Vec<linear_bridge::store::Link> {
        let mut side = issue_ref();
        side.connector = ConnectorId::new("linear");
        self.store.find_links(&side).expect("a readable store")
    }

    /// The body of the one create the forge received.
    fn the_create(&self) -> Value {
        let created: Vec<_> = self
            .forgejo
            .seen()
            .into_iter()
            .filter(|record| record.method == "POST" && !record.path.ends_with("/comments"))
            .collect();
        assert_eq!(created.len(), 1, "exactly one create");
        created[0].body.clone()
    }

    /// Writes the forge received: what a loop would multiply.
    fn writes_to_the_forge(&self) -> usize {
        self.forgejo
            .seen()
            .iter()
            .filter(|record| matches!(record.method.as_str(), "POST" | "PATCH" | "PUT" | "DELETE"))
            .count()
    }

    fn patches(&self) -> Vec<Value> {
        self.forgejo
            .seen()
            .into_iter()
            .filter(|record| record.method == "PATCH")
            .map(|record| record.body)
            .collect()
    }
}

/// A Linear issue body, as the webhook carries it.
///
/// Built with `serde_json` rather than a template: a mirrored comment body
/// contains newlines and quotes, and a hand-written JSON string that interpolates
/// one is a test that fails on escaping instead of on behaviour.
fn linear_issue(action: &str) -> String {
    json!({
        "action": action,
        "type": "Issue",
        "webhookTimestamp": linear_bridge::clock::now_millis(),
        "url": "https://linear.app/vedaru/issue/VED-99",
        "actor": { "id": "u-1", "name": "vedaru" },
        "data": { "id": "issue-1", "identifier": "VED-99", "team": { "key": "VED" } }
    })
    .to_string()
}

/// A Linear comment body: the issue is the subject, the comment is the content.
fn linear_comment(body: &str) -> String {
    linear_comment_action("create", body)
}

fn linear_comment_action(action: &str, body: &str) -> String {
    json!({
        "action": action,
        "type": "Comment",
        "webhookTimestamp": linear_bridge::clock::now_millis(),
        "actor": { "id": "u-1", "name": "vedaru" },
        "data": { "id": "comment-9", "body": body, "issue": { "id": "issue-1" } }
    })
    .to_string()
}

/// A forge issue body (the event name arrives in a header, which `deliver` puts
/// back from the stored row).
fn forgejo_issue(action: &str) -> String {
    json!({
        "action": action,
        "repository": { "full_name": "Vedaru/linear-cli-rs" },
        "sender": { "login": "vedaru" },
        "issue": { "number": 13, "html_url": "http://forge/13" }
    })
    .to_string()
}

fn forgejo_comment(body: &str) -> String {
    forgejo_comment_action("created", body)
}

fn forgejo_comment_action(action: &str, body: &str) -> String {
    json!({
        "action": action,
        "repository": { "full_name": "Vedaru/linear-cli-rs" },
        "sender": { "login": "vedaru" },
        "issue": { "number": 13 },
        "comment": { "id": 1, "body": body }
    })
    .to_string()
}

/// The forge's comment in the reverse-direction tests.
fn forgejo_comment_ref() -> EntityRef {
    EntityRef {
        connector: ConnectorId::new("forgejo"),
        kind: EntityKind::Comment,
        scope: Some("Vedaru/linear-cli-rs".into()),
        native_id: "1".into(),
        url: None,
    }
}

/// The comment the comment deliveries in these tests are about.
fn comment_ref() -> EntityRef {
    EntityRef {
        connector: ConnectorId::new("linear"),
        kind: EntityKind::Comment,
        scope: Some("VED".into()),
        native_id: "comment-9".into(),
        url: None,
    }
}

fn issue_ref() -> EntityRef {
    EntityRef {
        connector: ConnectorId::new("linear"),
        kind: EntityKind::Issue,
        scope: Some("VED".into()),
        native_id: "issue-1".into(),
        url: Some("https://linear.app/vedaru/issue/VED-99".into()),
    }
}

/// The world's Linear issue, in the shape the API returns it.
/// The Linear issue, with somebody assigned to it.
fn linear_issue_assigned(title: &str, state: &str, email: &str) -> Value {
    let mut issue = linear_issue_state(title, state);
    issue["assignee"] = json!({ "email": email });
    issue
}

fn linear_issue_state(title: &str, state: &str) -> Value {
    json!({
        "id": "issue-1",
        "identifier": "VED-99",
        "url": "https://linear.app/vedaru/issue/VED-99",
        "title": title,
        "description": "why it matters",
        "dueDate": null,
        "priority": 0,
        "state": { "name": state },
        "labels": { "nodes": [{ "name": "Bug" }] },
        "assignee": null,
    })
}

#[test]
fn a_new_issue_appears_once_on_the_other_platform() {
    let mut harness = Harness::start();
    harness.world.lock().unwrap().linear = Some(linear_issue_state("Mirror the thing", "Todo"));

    harness.deliver("linear", "Issue", &linear_issue("create"));

    let created = harness.the_create();
    assert_eq!(created["title"], "Mirror the thing");
    // The copy carries our marker: that is how it is recognised later *without* the
    // store, and why comparing content strips markers first.
    let copied = created["body"].as_str().expect("a body");
    assert!(copied.starts_with("why it matters"), "{copied}");
    assert!(copied.contains("linear-bridge:linear:issue-1"), "{copied}");
    assert_eq!(created["labels"], json!([3]));

    // The pairing is recorded, with the content key of what was written - which is
    // what makes the far side's own webhook recognisable a second later.
    let links = harness.links();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].right.native_id, "13");
    assert!(links[0].last_synced_hash.is_some());
}

#[test]
fn the_webhooks_our_own_write_provokes_do_nothing() {
    let mut harness = Harness::start();
    harness.world.lock().unwrap().linear = Some(linear_issue_state("Mirror the thing", "Todo"));
    harness.deliver("linear", "Issue", &linear_issue("create"));
    let before = harness.writes_to_the_forge();

    // The forge now announces the issue we just created on it. This is the loop:
    // without the link's recorded key, this event mirrors straight back and the two
    // platforms ping-pong forever.
    harness.deliver("forgejo", "issues", &forgejo_issue("opened"));
    // Reads are expected - the reconciler reads both sides before deciding - so a
    // *write* is the thing a loop multiplies.
    assert_eq!(
        harness.writes_to_the_forge(),
        before,
        "the echo caused no write"
    );
    assert!(
        harness.linear.graphql("IssueUpdate").is_empty(),
        "and nothing was pushed back to Linear"
    );

    // Same for the `edited` shape a forge sends after a create.
    harness.deliver("forgejo", "issues", &forgejo_issue("edited"));
    assert!(harness.linear.graphql("IssueUpdate").is_empty());
}

#[test]
fn an_edit_on_one_side_reaches_the_other_once() {
    let mut harness = Harness::start();
    harness.world.lock().unwrap().linear = Some(linear_issue_state("Mirror the thing", "Todo"));
    harness.deliver("linear", "Issue", &linear_issue("create"));

    // Linear edits the title.
    harness.world.lock().unwrap().linear =
        Some(linear_issue_state("Mirror the thing, properly", "Todo"));
    harness.deliver("linear", "Issue", &linear_issue("update"));

    let patches = harness.patches();
    assert_eq!(patches.len(), 1, "one patch, not one per field");
    assert_eq!(patches[0]["title"], "Mirror the thing, properly");

    // Re-delivering the same change is a no-op: the link now records it.
    harness.deliver("linear", "Issue", &linear_issue("update"));
    assert_eq!(
        harness.patches().len(),
        1,
        "the second delivery changed nothing"
    );
}

#[test]
fn closing_on_one_side_closes_the_other() {
    let mut harness = Harness::start();
    harness.world.lock().unwrap().linear = Some(linear_issue_state("Mirror the thing", "Todo"));
    harness.deliver("linear", "Issue", &linear_issue("create"));

    harness.world.lock().unwrap().linear = Some(linear_issue_state("Mirror the thing", "Done"));
    harness.deliver("linear", "Issue", &linear_issue("update"));

    let state = harness.world.lock().unwrap().forgejo.clone().unwrap()["state"].clone();
    assert_eq!(
        state,
        json!("closed"),
        "Linear's Done is the forge's closed"
    );
}

#[test]
fn a_comment_is_mirrored_with_attribution_and_then_recognised() {
    let mut harness = Harness::start();
    harness.world.lock().unwrap().linear = Some(linear_issue_state("Mirror the thing", "Todo"));
    harness.deliver("linear", "Issue", &linear_issue("create"));

    harness.deliver("linear", "Comment", &linear_comment("looks good to me"));

    let posted = harness.world.lock().unwrap().forgejo_comments.clone();
    assert_eq!(posted.len(), 1);
    let body = posted[0]["body"].as_str().unwrap().to_string();
    assert!(body.contains("**vedaru** wrote on linear"), "{body}");
    assert!(body.contains("looks good to me"), "{body}");
    assert!(body.contains("linear-bridge:linear:comment-9"), "{body}");

    // The forge announces that comment. It carries our marker, so it stops here.
    harness.deliver("forgejo", "issue_comment", &forgejo_comment(&body));
    assert!(harness.linear.graphql("CommentCreate").is_empty());
}

#[test]
fn an_edited_comment_edits_the_mirrored_copy_and_does_not_post_a_second_one() {
    let mut harness = Harness::start();
    harness.world.lock().unwrap().linear = Some(linear_issue_state("Mirror the thing", "Todo"));
    harness.deliver("linear", "Issue", &linear_issue("create"));
    harness.deliver("linear", "Comment", &linear_comment("looks good to me"));
    assert_eq!(harness.world.lock().unwrap().forgejo_comments.len(), 1);

    // The comment is edited on Linear. Without a pairing this would post the edited
    // text as a second comment - the duplicate a reader would notice.
    harness.deliver(
        "linear",
        "Comment",
        &linear_comment_action("update", "looks good to me (edited)"),
    );

    let world = harness.world.lock().unwrap();
    assert_eq!(
        world.forgejo_comments.len(),
        1,
        "an edit must not become a second comment"
    );
    assert_eq!(
        world.forgejo_comment_calls.len(),
        1,
        "{:?}",
        world.forgejo_comment_calls
    );
    let (method, body) = &world.forgejo_comment_calls[0];
    assert_eq!(method, "PATCH");
    let text = body["body"].as_str().expect("a body");
    assert!(text.contains("looks good to me (edited)"), "{text}");
    assert!(text.contains("linear-bridge:linear:comment-9"), "{text}");
    drop(world);

    // The forge announces that edit of its own copy: the marker stops it.
    let echoed = harness.world.lock().unwrap().forgejo_comment_calls[0].1["body"]
        .as_str()
        .expect("a body")
        .to_string();
    harness.deliver("forgejo", "issue_comment", &forgejo_comment(&echoed));
    assert!(harness.linear.graphql("CommentUpdate").is_empty());
    assert!(harness.linear.graphql("CommentCreate").is_empty());
}

#[test]
fn a_deleted_comment_is_deleted_on_the_other_side_and_unpaired() {
    let mut harness = Harness::start();
    harness.world.lock().unwrap().linear = Some(linear_issue_state("Mirror the thing", "Todo"));
    harness.deliver("linear", "Issue", &linear_issue("create"));
    harness.deliver("linear", "Comment", &linear_comment("looks good to me"));
    // The comment has its own pairing, alongside the issue's.
    assert_eq!(harness.links_for(comment_ref()).len(), 1);

    harness.deliver(
        "linear",
        "Comment",
        &linear_comment_action("remove", "looks good to me"),
    );

    let world = harness.world.lock().unwrap();
    let calls = world.forgejo_comment_calls.clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "DELETE");
    drop(world);
    // And the comment's pairing is gone, so a later comment with the same id starts
    // clean rather than editing something that is no longer there.
    assert_eq!(
        harness.links_for(comment_ref()).len(),
        0,
        "the comment pairing was dropped"
    );
    assert_eq!(harness.links().len(), 1, "the issue pairing stays");
}

#[test]
fn a_comment_written_edited_and_deleted_on_the_forge_does_the_same_on_linear() {
    let mut harness = Harness::start();
    harness.world.lock().unwrap().linear = Some(linear_issue_state("Mirror the thing", "Todo"));
    harness.deliver("linear", "Issue", &linear_issue("create"));

    harness.deliver("forgejo", "issue_comment", &forgejo_comment("written here"));
    assert_eq!(harness.world.lock().unwrap().linear_comments.len(), 1);
    assert_eq!(harness.links_for(forgejo_comment_ref()).len(), 1);

    // An edit has to reach the copy Linear holds - addressed by *Linear's* id for the
    // comment, not by anything in the forge's payload.
    harness.deliver(
        "forgejo",
        "issue_comment",
        &forgejo_comment_action("edited", "written here (edited)"),
    );
    let edits = harness.world.lock().unwrap().linear_comment_edits.clone();
    assert_eq!(edits.len(), 1, "one edit, not a second comment");
    assert_eq!(edits[0]["id"], "linear-comment-1");
    let body = edits[0]["input"]["body"].as_str().expect("a body");
    assert!(body.contains("written here (edited)"), "{body}");

    // And a deletion removes it there, rather than leaving a comment behind.
    harness.deliver(
        "forgejo",
        "issue_comment",
        &forgejo_comment_action("deleted", "written here (edited)"),
    );
    assert_eq!(
        harness.world.lock().unwrap().linear_comment_deletions,
        vec![json!("linear-comment-1")]
    );
    assert_eq!(harness.links_for(forgejo_comment_ref()).len(), 0);
}

#[test]
fn an_assignee_with_no_identity_map_stays_behind_and_stops_being_a_difference() {
    let mut harness = Harness::start();
    // The person exists on Linear and nowhere else: the mapping has no identity map,
    // so the mirror has not been told what this login is called on the forge.
    harness.world.lock().unwrap().linear = Some(linear_issue_assigned(
        "Mirror the thing",
        "Todo",
        "loner@example.com",
    ));

    harness.deliver("linear", "Issue", &linear_issue("create"));

    assert_eq!(harness.forgejo_writes(), 1, "the issue is mirrored once");
    assert_eq!(
        harness.forgejo()["assignees"],
        json!([]),
        "an unconfigured identity is left behind, not sent as a Linear login"
    );

    // An edit that has nothing to do with the assignee. The mirror must write the
    // title and nothing else - an assignee write here would be the same unwritable
    // value going across on every single delivery.
    harness.world.lock().unwrap().linear = Some(linear_issue_assigned(
        "Mirror the thing (edited)",
        "Todo",
        "loner@example.com",
    ));
    harness.deliver("linear", "Issue", &linear_issue("update"));

    assert_eq!(harness.forgejo_writes(), 2, "one create, one update");
    assert_eq!(harness.forgejo()["title"], "Mirror the thing (edited)");
    assert_eq!(
        harness.forgejo()["assignees"],
        json!([]),
        "the edit was about the title, and the assignee is not writable here"
    );

    // And it must not keep coming back. A provider re-send of the same change has
    // nothing left to do, because what the link records is what the *forge* holds -
    // which never included the assignee.
    harness.deliver("linear", "Issue", &linear_issue("update"));
    assert_eq!(
        harness.forgejo_writes(),
        2,
        "the same edit arrived twice and was written twice: the hash never converged"
    );
}

#[test]
fn a_one_way_mapping_never_writes_backwards() {
    // The mapping mirrors one way, so a forge-side edit has nowhere to go even
    // though the far side is reachable.
    let mut harness = Harness::with_direction(Direction::SourceToSink);
    harness.world.lock().unwrap().linear = Some(linear_issue_state("Mirror the thing", "Todo"));
    harness.deliver("linear", "Issue", &linear_issue("create"));

    harness.deliver("forgejo", "issues", &forgejo_issue("edited"));
    assert!(harness.linear.graphql("IssueUpdate").is_empty());
}

#[test]
fn a_change_the_mapping_does_not_claim_is_left_alone() {
    let mut harness = Harness::start();
    harness.world.lock().unwrap().linear = Some(linear_issue_state("Mirror the thing", "Todo"));
    harness.deliver("linear", "Issue", &linear_issue("create"));

    // An event from a repository nobody mapped: acknowledged, and nothing written.
    let body = r#"{"action":"opened","repository":{"full_name":"Vedaru/somewhere-else"},
        "sender":{"login":"vedaru"},"issue":{"number":99}}"#;
    let before = harness.writes_to_the_forge();
    harness.deliver("forgejo", "issues", body);
    assert_eq!(harness.writes_to_the_forge(), before);
}
