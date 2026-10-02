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
use linear_bridge::domain::{ConnectorId, EntityKind, EntityRef, Secret};
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
    forgejo_comments: Vec<Value>,
    linear_comments: Vec<Value>,
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
                world.forgejo_next += 1;
                let number = world.forgejo_next;
                let issue = json!({
                    "number": number,
                    "html_url": format!("http://forge/Vedaru/linear-cli-rs/issues/{number}"),
                    "title": body["title"].clone(),
                    "body": body["body"].as_str().unwrap_or_default(),
                    "state": "open",
                    "labels": [{ "id": 3, "name": "Bug" }],
                });
                world.forgejo = Some(issue.clone());
                (201, issue)
            }
            ("PATCH", true, false, false) => {
                let issue = world.forgejo.as_mut().expect("a patch needs an issue");
                for key in ["title", "body", "state"] {
                    if let Some(value) = body.get(key) {
                        issue[key] = value.clone();
                    }
                }
                (200, issue.clone())
            }
            ("PUT", true, false, true) => {
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
    let path = std::env::temp_dir().join(format!(
        "bridge-reconcile-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("a sane clock")
            .as_nanos()
    ));
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
            source: Endpoint::parse("linear:VED").unwrap(),
            sink: Endpoint::parse("forgejo:Vedaru/linear-cli-rs").unwrap(),
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
    json!({
        "action": "create",
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
    json!({
        "action": "created",
        "repository": { "full_name": "Vedaru/linear-cli-rs" },
        "sender": { "login": "vedaru" },
        "issue": { "number": 13 },
        "comment": { "id": 1, "body": body }
    })
    .to_string()
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
    assert_eq!(created["body"], "why it matters");
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
