//! A sweep and a delivery, over the same store.
//!
//! The two ways into the engine are a webhook and `linear sync`. They share the decision
//! code and the records a link holds, so the property that matters is that neither
//! surprises the other: a sweep after a delivery has nothing to do, and a delivery after
//! a sweep does not re-create what the sweep wrote.

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use linear_bridge::connector::Source;
use linear_bridge::domain::{Secret, UserMap};
use linear_bridge::queue::Handler;
use linear_bridge::reconcile::handler::{default_policy, Endpoint, Mapping, ReconcileHandler};
use linear_bridge::reconcile::survey::Action;
use linear_bridge::reconcile::{Side, Sides, StateNames};
use linear_bridge::sink::Sink;
use linear_bridge::sources::declarative::DeclarativeSource;
use linear_bridge::sources::presets;
use linear_bridge::store::sqlite::SqliteStore;
use linear_bridge::store::{Delivery, Store};

mod support;
use support::Fake;

const SECRET: &str = "0123456789abcdef";
const SCOPE: &str = "Vedaru/linear-cli-rs";

/// The forge's issues, as it holds them: a fake that ignores what it was sent cannot
/// answer the next read, and a sweep is mostly reads.
#[derive(Default)]
struct Forge {
    issues: Vec<Value>,
    next: i64,
}

fn forge_issue(number: i64, title: &str, body: &str) -> Value {
    json!({
        "number": number,
        "html_url": format!("http://forge/{SCOPE}/issues/{number}"),
        "title": title,
        "body": body,
        "state": "open",
        "labels": [],
        "assignees": [],
        "due_date": "0001-01-01T00:00:00Z",
    })
}

fn forgejo_routes(world: Arc<Mutex<Forge>>) -> impl Fn(&str, &str, &Value) -> (u16, Value) {
    move |method: &str, path: &str, body: &Value| {
        let mut forge = world.lock().expect("the fake is not poisoned");
        let path = path.split('?').next().unwrap_or(path);

        if path.ends_with("/labels") && method == "GET" {
            return (200, json!([]));
        }
        if path.ends_with("/labels") {
            return (201, json!({ "id": 3, "name": "Bug" }));
        }
        if path.ends_with("/comments") {
            return (
                201,
                json!({ "id": 1, "html_url": "http://forge/comment/1" }),
            );
        }
        if path.ends_with("/issues") && method == "GET" {
            return (200, Value::Array(forge.issues.clone()));
        }
        if path.ends_with("/issues") && method == "POST" {
            forge.next = forge.issues.len() as i64 + 1;
            let number = forge.issues.len() as i64 + 10;
            let mut issue = forge_issue(
                number,
                body["title"].as_str().unwrap_or(""),
                body["body"].as_str().unwrap_or(""),
            );
            issue["labels"] = body
                .get("labels")
                .and_then(Value::as_array)
                .map(|ids| {
                    ids.iter()
                        .map(|id| json!({ "id": id, "name": "Bug" }))
                        .collect()
                })
                .unwrap_or_else(|| json!([]));
            forge.issues.push(issue.clone());
            return (
                201,
                json!({ "number": number, "html_url": issue["html_url"] }),
            );
        }
        if let Some(number) = path
            .rsplit('/')
            .next()
            .and_then(|last| last.parse::<i64>().ok())
        {
            let found = forge
                .issues
                .iter()
                .position(|issue| issue["number"] == json!(number));
            match (method, found) {
                ("GET", Some(index)) => return (200, forge.issues[index].clone()),
                ("GET", None) => return (404, json!({ "message": "no such issue" })),
                (_, Some(index)) => {
                    let issue = &mut forge.issues[index];
                    for key in ["title", "body", "state", "assignees", "due_date"] {
                        if let Some(value) = body.get(key) {
                            issue[key] = value.clone();
                        }
                    }
                    return (200, issue.clone());
                }
                _ => {}
            }
        }
        (
            404,
            json!({ "message": format!("no route for {method} {path}") }),
        )
    }
}

/// The Linear side: two issues to sweep, and a stateful list so a created issue appears
/// on the next read.
#[derive(Default)]
struct Workspace {
    issues: Vec<Value>,
}

fn linear_issue(id: &str, title: &str) -> Value {
    json!({
        "id": id,
        "identifier": id.to_uppercase(),
        "url": format!("https://linear.app/vedaru/issue/{id}"),
        "title": title,
        "description": "body",
        "dueDate": Value::Null,
        "priority": 0,
        "state": { "name": "Todo" },
        "labels": { "nodes": [] },
        "assignee": Value::Null,
    })
}

fn linear_routes(world: Arc<Mutex<Workspace>>) -> impl Fn(&str, &str, &Value) -> (u16, Value) {
    move |_method: &str, path: &str, body: &Value| {
        assert_eq!(path, "/graphql");
        let query = body["query"].as_str().unwrap_or_default();
        if query.contains("query Teams") {
            return (
                200,
                json!({ "data": { "teams": { "nodes": [{ "id": "team-uuid", "key": "VED" }] } } }),
            );
        }
        if query.contains("TeamStates") {
            return (
                200,
                json!({ "data": { "team": { "states": { "nodes": [
                    { "id": "state-todo", "name": "Todo" }] } } } }),
            );
        }
        if query.contains("TeamLabels") {
            return (
                200,
                json!({ "data": { "team": { "labels": { "nodes": [] } } } }),
            );
        }
        if query.contains("query Users") {
            return (200, json!({ "data": { "users": { "nodes": [] } } }));
        }
        let mut workspace = world.lock().expect("the fake is not poisoned");
        if query.contains("query Issues") {
            return (
                200,
                json!({ "data": { "issues": {
                    "nodes": workspace.issues.clone(),
                    "pageInfo": { "hasNextPage": false, "endCursor": Value::Null },
                } } }),
            );
        }
        if query.contains("query Issue(") {
            let wanted = body["variables"]["id"].clone();
            let found = workspace
                .issues
                .iter()
                .find(|issue| issue["id"] == wanted)
                .cloned()
                .unwrap_or(Value::Null);
            return (200, json!({ "data": { "issue": found } }));
        }
        if query.contains("mutation IssueCreate") {
            let incoming = &body["variables"]["input"];
            let id = format!("issue-{}", workspace.issues.len() + 1);
            let issue = linear_issue(id.as_str(), incoming["title"].as_str().unwrap_or(""));
            workspace.issues.push(issue.clone());
            return (
                200,
                json!({ "data": { "issueCreate": { "success": true, "issue": {
                    "id": issue["id"], "identifier": issue["identifier"], "url": issue["url"] } } } }),
            );
        }
        if query.contains("mutation IssueUpdate") {
            return (
                200,
                json!({ "data": { "issueUpdate": { "success": true } } }),
            );
        }
        (
            200,
            json!({ "errors": [{ "message": format!("unrouted: {query}") }] }),
        )
    }
}

fn policy() -> linear_bridge::reconcile::Policy {
    default_policy(Sides::new(
        StateNames {
            closed: vec!["Done".into(), "Canceled".into()],
            open: Some("In Progress".into()),
            initial: Some("Todo".into()),
        },
        StateNames {
            closed: vec!["closed".into()],
            open: Some("open".into()),
            initial: None,
        },
    ))
}

struct Harness {
    handler: ReconcileHandler,
    /// Both fakes are kept: a `Fake` shuts its server down when it is dropped, and a
    /// local one dies the moment the harness is built.
    forge: Fake,
    /// Held, not used: a dropped `Fake` shuts its server down.
    _linear: Fake,
    sources: Vec<Arc<dyn Source>>,
    forge_world: Arc<Mutex<Forge>>,
    linear_world: Arc<Mutex<Workspace>>,
}

impl Harness {
    fn start() -> Self {
        let forge_world = Arc::new(Mutex::new(Forge::default()));
        let linear_world = Arc::new(Mutex::new(Workspace::default()));
        let forge = Fake::start(forgejo_routes(Arc::clone(&forge_world)));
        let linear = Fake::start(linear_routes(Arc::clone(&linear_world)));

        let sources: Vec<Arc<dyn Source>> = ["linear", "forgejo"]
            .into_iter()
            .map(|name| {
                Arc::new(DeclarativeSource::new(
                    name,
                    Secret::new(SECRET),
                    presets::preset(name).expect("the preset loads"),
                )) as Arc<dyn Source>
            })
            .collect();
        let sinks: Vec<Arc<dyn Sink>> = vec![
            Arc::new(forge.sink("forgejo")) as Arc<dyn Sink>,
            Arc::new(linear.sink("linear")) as Arc<dyn Sink>,
        ];
        let mapping = Mapping {
            name: "sync".into(),
            source: Endpoint::parse("linear:VED").expect("an endpoint"),
            sink: Endpoint::parse(&format!("forgejo:{SCOPE}")).expect("an endpoint"),
            users: UserMap::default(),
            project_scopes: Default::default(),
            policy: policy(),
        };
        let path = support::test_database("linear-bridge-sync");
        let mut store = SqliteStore::open(&path).expect("a store");
        store.migrate().expect("migrated");

        let handler = ReconcileHandler::new(sources.clone(), sinks, vec![mapping], Box::new(store))
            .expect("the handler builds");

        Self {
            handler,
            forge,
            _linear: linear,
            sources,
            forge_world,
            linear_world,
        }
    }

    /// A Linear issue that already exists, as the platform would report it.
    fn existing(&self, id: &str, title: &str) {
        self.linear_world
            .lock()
            .expect("not poisoned")
            .issues
            .push(linear_issue(id, title));
    }

    /// Retitle an issue the way a user would: on the platform, so the bridge's re-read
    /// sees the change the delivery announces.
    fn retitle(&self, id: &str, title: &str) {
        let mut workspace = self.linear_world.lock().expect("not poisoned");
        for issue in workspace.issues.iter_mut() {
            if issue["id"] == json!(id) {
                issue["title"] = json!(title);
            }
        }
    }

    /// What the forge holds now.
    fn forge_issues(&self) -> Vec<Value> {
        self.forge_world
            .lock()
            .expect("not poisoned")
            .issues
            .clone()
    }

    /// One webhook delivery, through the same path the service uses.
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
        self.handler.handle(&delivery).expect("handled");
    }

    fn forge_writes(&self) -> usize {
        self.forge
            .seen()
            .iter()
            .filter(|record| matches!(record.method.as_str(), "POST" | "PATCH" | "PUT"))
            .count()
    }
}

fn linear_issue_event(id: &str, title: &str, action: &str) -> String {
    json!({
        "action": action,
        "type": "Issue",
        "webhookTimestamp": linear_bridge::clock::now_millis(),
        "actor": { "id": "u-1", "name": "vedaru" },
        "url": format!("https://linear.app/vedaru/issue/{id}"),
        "data": { "id": id, "team": { "key": "VED" }, "title": title }
    })
    .to_string()
}

#[test]
fn a_sweep_reports_the_plan_and_writes_nothing_on_its_own() {
    let mut harness = Harness::start();
    harness.existing("issue-A", "Deploy the widget");

    let survey = harness.handler.survey(0).expect("a survey");

    assert_eq!(survey.writes(), 1, "{:?}", survey.entries);
    let creating = survey
        .entries
        .iter()
        .find(|entry| entry.action == Action::Create)
        .expect("one creation");
    assert_eq!(creating.subject.native_id, "issue-A");
    assert_eq!(
        creating.side,
        Side::Source,
        "the source is what gets mirrored"
    );
    assert_eq!(harness.forge_writes(), 0, "a survey must not write");
}

#[test]
fn a_delivery_then_a_sweep_has_nothing_left_to_do() {
    // The acceptance clause: both paths share the records, so a sweep after real webhook
    // activity finds the two ends agreeing - it does not write the same thing again.
    let mut harness = Harness::start();
    harness.existing("issue-A", "Deploy the widget");
    harness.deliver(
        "linear",
        "Issue",
        &linear_issue_event("issue-A", "Deploy the widget", "create"),
    );
    // What the forge actually received, not just how many writes there were: a create that
    // never arrives is a different defect from one that arrives wrong, and only one of those
    // is visible in a count.
    assert_eq!(
        harness.forge_writes(),
        1,
        "the delivery mirrored the issue; the forge saw: {:?}",
        harness.forge.seen()
    );
    assert_eq!(harness.forge_issues().len(), 1);

    let survey = harness.handler.survey(0).expect("a survey");

    assert_eq!(
        survey.writes(),
        0,
        "the sweep wanted to write after a delivery: {:?}",
        survey.entries
    );
    assert!(
        survey
            .entries
            .iter()
            .any(|entry| entry.action == Action::InStep),
        "{:?}",
        survey.entries
    );
    assert_eq!(harness.forge_writes(), 1, "and it wrote nothing itself");
}

#[test]
fn a_sweep_creates_a_copy_that_carries_our_marker() {
    // The marker is what makes a copy recognisable without the store, and therefore what
    // lets the next sweep adopt it instead of creating a duplicate.
    let mut harness = Harness::start();
    harness.existing("issue-A", "Deploy the widget");

    let survey = harness.handler.survey(0).expect("a survey");
    let written = harness.handler.apply_survey(0, &survey).expect("applied");
    assert_eq!(written, 1);

    let created = harness
        .forge
        .seen()
        .into_iter()
        .find(|record| record.method == "POST")
        .expect("a creation reached the forge");
    let body = created.body["body"].as_str().expect("a body");
    assert!(body.contains("linear-bridge:linear:issue-A"), "{body}");

    // The write recorded the revision, so the next sweep has nothing to do.
    let again = harness.handler.survey(0).expect("a survey");
    assert_eq!(again.writes(), 0, "{:?}", again.entries);
}

#[test]
fn only_the_field_that_differs_is_written() {
    let mut harness = Harness::start();
    harness.existing("issue-A", "Deploy the widget");
    harness.deliver(
        "linear",
        "Issue",
        &linear_issue_event("issue-A", "Deploy the widget", "create"),
    );

    // The edit happens on the platform *and* is announced, which is what a delivery
    // means: the bridge re-reads rather than trusting the payload, so both must move.
    harness.retitle("issue-A", "Deploy the widget (revised)");
    harness.deliver(
        "linear",
        "Issue",
        &linear_issue_event("issue-A", "Deploy the widget (revised)", "update"),
    );

    let patch = harness
        .forge
        .seen()
        .into_iter()
        .rfind(|record| record.method == "PATCH")
        .expect("an update reached the forge");
    assert_eq!(patch.body["title"], "Deploy the widget (revised)");
    for absent in ["labels", "assignees", "due_date", "body"] {
        assert!(
            !patch
                .body
                .as_object()
                .expect("an object")
                .contains_key(absent),
            "`{absent}` was not changed, so it must not be sent: {}",
            patch.body
        );
    }
}

/// A platform's issue list can lag its own write. When it does, a sweep finds a
/// recorded link whose counterpart it cannot read - and must treat that as "not seen
/// this pass", not "deleted", or it re-creates the copy it made moments ago.
///
/// This is GitHub's `GET /repos/{scope}/issues` lagging the issue it just accepted
/// (VED-291); the fake below stands in for that by dropping the created issue from
/// what the list returns while the store keeps the link.
#[test]
fn a_sweep_does_not_recreate_a_linked_copy_the_list_has_not_caught_up_with() {
    let mut harness = Harness::start();
    harness.existing("issue-A", "Deploy the widget");

    // Sweep 1 creates the mirror and records the link.
    let first = harness.handler.survey(0).expect("a survey");
    assert_eq!(harness.handler.apply_survey(0, &first).expect("applied"), 1);
    assert_eq!(harness.forge_issues().len(), 1);
    assert_eq!(harness.forge_writes(), 1);

    // Sweep 2 runs before the forge's list has caught up: the issue still exists, but
    // the list does not return it. The marker is unreadable for the same reason, so the
    // recorded link is the only proof the pair exists.
    harness.forge_world.lock().expect("not poisoned").issues.clear();

    let second = harness.handler.survey(0).expect("a survey");
    assert_eq!(
        second.writes(),
        0,
        "a lagging list made the sweep re-create a linked copy: {:?}",
        second.entries
    );
    assert_eq!(harness.handler.apply_survey(0, &second).expect("applied"), 0);
    assert_eq!(harness.forge_writes(), 1, "a second create reached the forge");
}

/// The same lag on the source list, which the sink side has to survive: the copy and
/// its link remain, the original is missing from what the sweep reads. The link is the
/// only proof left, and it lives on the *sink* entity, so a sweep that gathered links
/// only from the source would treat the copy as a stranger and re-create the original.
#[test]
fn a_sweep_does_not_recreate_an_original_the_source_list_has_not_caught_up_with() {
    let mut harness = Harness::start();
    harness.existing("issue-A", "Deploy the widget");

    let first = harness.handler.survey(0).expect("a survey");
    assert_eq!(harness.handler.apply_survey(0, &first).expect("applied"), 1);
    assert_eq!(harness.forge_issues().len(), 1);

    // Sweep 2 runs before the source's own list has caught up: the Linear issue is gone
    // from what the list returns, while the forge copy and the link remain. The marker is
    // stripped too, so the link is the only proof left - which is the shape a pair adopted
    // by `linear sync link` has, and the one a marker-only guard would miss.
    harness.linear_world.lock().expect("not poisoned").issues.clear();
    for issue in harness
        .forge_world
        .lock()
        .expect("not poisoned")
        .issues
        .iter_mut()
    {
        issue["body"] = json!("a plain body with no marker");
    }

    let second = harness.handler.survey(0).expect("a survey");
    assert_eq!(
        second.writes(),
        0,
        "a lagging source list made the sweep re-create the original: {:?}",
        second.entries
    );
    assert_eq!(harness.handler.apply_survey(0, &second).expect("applied"), 0);
    assert_eq!(
        harness.linear_world.lock().expect("not poisoned").issues.len(),
        0,
        "a second Linear issue was created"
    );
}
