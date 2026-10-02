//! An issue landing on its mirrored project's board, end to end.
//!
//! A project is mirrored as its own entity (see `projects.rs`), but a board with no
//! cards is not much of a mirror: an issue whose *source* names a project has to
//! land on the project's counterpart too. This checks that whole path against
//! stateful fakes, where a create a later read must see really is remembered:
//!
//! - a paired project and an issue that names it: the issue is created on the forge
//!   *and* put on the mirrored project's board;
//! - a project the mapping does not have: nothing is placed, and - the point - the
//!   delivery is not an error;
//! - the project changing or being cleared: the issue is moved, then taken off.
//!
//! The fakes are hand-written rather than fixture-driven because this is behaviour,
//! not a shape: the forge has to remember what it was told so the next read sees it.

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use linear_bridge::connector::Source;
use linear_bridge::domain::{ConnectorId, EntityKind, EntityRef, Secret, UserMap};
use linear_bridge::queue::Handler;
use linear_bridge::reconcile::handler::{default_policy, Endpoint, Mapping, ReconcileHandler};
use linear_bridge::reconcile::{Sides, StateNames};
use linear_bridge::sink::Sink;
use linear_bridge::sources::declarative::DeclarativeSource;
use linear_bridge::sources::presets;
use linear_bridge::store::sqlite::SqliteStore;
use linear_bridge::store::{Delivery, Link, Store};

mod support;
use support::Fake;

const SECRET: &str = "0123456789abcdef";
const SCOPE: &str = "Vedaru/linear-cli-rs";

/// Both platforms' state, as the fakes hold it. Writes land here, which is what
/// makes a later read see what an earlier delivery did.
#[derive(Default)]
struct World {
    /// The Linear issue, as GraphQL returns it under `issue`.
    issue: Option<Value>,
    /// The forge issue the bridge created, as its own API returns it.
    forge_issue: Option<Value>,
}

fn state() -> Arc<Mutex<World>> {
    Arc::new(Mutex::new(World::default()))
}

/// The Linear issue, with `project` either an id or absent.
fn linear_issue(title: &str, body: &str, project: Option<&str>) -> Value {
    json!({
        "id": "issue-uuid",
        "identifier": "VED-1",
        "url": "https://linear.app/vedaru/issue/VED-1/x",
        "title": title,
        "description": body,
        "dueDate": null,
        "priority": 0,
        "state": { "name": "Todo" },
        "labels": { "nodes": [] },
        "assignee": null,
        "project": project.map(|id| json!({ "id": id })),
    })
}

fn linear_routes(world: Arc<Mutex<World>>) -> impl Fn(&str, &str, &Value) -> (u16, Value) {
    move |_method, path, body| {
        assert_eq!(path, "/graphql");
        let query = body["query"].as_str().unwrap_or_default();
        if query.contains("query Issue(") {
            let world = world.lock().expect("not poisoned");
            return (200, json!({ "data": { "issue": world.issue } }));
        }
        (
            200,
            json!({ "errors": [{ "message": format!("unrouted: {query}") }] }),
        )
    }
}

fn forgejo_routes(world: Arc<Mutex<World>>) -> impl Fn(&str, &str, &Value) -> (u16, Value) {
    move |method: &str, path: &str, body: &Value| {
        let mut world = world.lock().expect("not poisoned");
        let path = path.split('?').next().unwrap_or(path);
        let issues = format!("/api/v1/repos/{SCOPE}/issues");

        if path == issues && method == "POST" {
            let issue = json!({
                "number": 12,
                "html_url": format!("http://forge/{SCOPE}/issues/12"),
                "title": body["title"],
                "body": body["body"],
                "state": "open",
                "labels": [],
                "assignees": [],
                "due_date": "0001-01-01T00:00:00Z",
            });
            world.forge_issue = Some(issue.clone());
            return (201, issue);
        }
        if path.starts_with(&issues) {
            return match world.forge_issue.clone() {
                Some(issue) => (200, issue),
                None => (404, json!({ "message": "no such issue" })),
            };
        }
        // `POST|DELETE /projects/{id}/issues/{index}`: the membership the whole test
        // is about. Both answer the empty 204 Forgejo sends.
        if path.contains("/projects/") && path.contains("/issues/") {
            return (204, Value::Null);
        }
        (
            404,
            json!({ "message": format!("no route for {method} {path}") }),
        )
    }
}

struct Harness {
    handler: ReconcileHandler,
    store: Box<dyn Store>,
    world: Arc<Mutex<World>>,
    sources: Vec<Arc<dyn Source>>,
    /// Held: a dropped `Fake` shuts its server down.
    _linear: Fake,
    forgejo: Fake,
}

fn database() -> String {
    let path = std::env::temp_dir().join(format!(
        "bridge-issue-projects-{}-{}.db",
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
        let world = state();
        let linear = Fake::start(linear_routes(Arc::clone(&world)));
        let forgejo = Fake::start(forgejo_routes(Arc::clone(&world)));

        let sources: Vec<Arc<dyn Source>> = vec![
            Arc::new(DeclarativeSource::new(
                "linear",
                Secret::new(SECRET),
                presets::preset("linear").expect("the linear preset loads"),
            )),
            Arc::new(DeclarativeSource::new(
                "forgejo",
                Secret::new(SECRET),
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
        policy.sync_projects = true;
        let mapping = Mapping {
            name: "issue-projects".into(),
            users: UserMap::default(),
            source: Endpoint::parse("linear:VED").unwrap(),
            sink: Endpoint::parse(&format!("forgejo:{SCOPE}")).unwrap(),
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
            _linear: linear,
            forgejo,
        }
    }

    /// Pair a Linear project with a forge project, as mirroring the projects would.
    fn pair_projects(&mut self, linear_id: &str, forgejo_id: i64) {
        self.store
            .upsert_link(&Link::new(
                project_ref("linear", linear_id),
                project_ref("forgejo", &forgejo_id.to_string()),
            ))
            .expect("the projects pair");
    }

    fn set_issue(&self, issue: Value) {
        self.world.lock().expect("not poisoned").issue = Some(issue);
    }

    fn deliver_issue(&mut self, action: &str) {
        let body = json!({
            "action": action,
            "type": "Issue",
            "webhookTimestamp": linear_bridge::clock::now_millis(),
            "url": "https://linear.app/vedaru/issue/VED-1/x",
            "data": { "id": "issue-uuid", "identifier": "VED-1", "team": { "key": "VED" } },
        })
        .to_string();
        let source = self
            .sources
            .iter()
            .find(|source| source.id().as_str() == "linear")
            .expect("the linear source");
        let parsed = source
            .parse(&source.replay_headers("Issue"), body.as_bytes())
            .expect("the body parses");
        let first = parsed.first().expect("one event");
        let delivery = Delivery {
            id: 1,
            connector: first.connector.clone(),
            delivery_id: first.delivery.as_str().to_string(),
            event: "Issue".into(),
            kind: first.kind.clone(),
            action: first.action.clone(),
            scope: first.subject.scope.clone(),
            native_id: first.subject.native_id.clone(),
            body,
            attempts: 1,
            last_error: None,
        };
        self.handler.handle(&delivery).expect("handled");
    }

    /// Every request the forge was sent whose path names a project membership.
    fn membership_requests(&self) -> Vec<(String, String)> {
        self.forgejo
            .seen()
            .into_iter()
            .filter(|record| record.path.contains("/projects/") && record.path.contains("/issues/"))
            .map(|record| (record.method, record.path))
            .collect()
    }
}

fn project_ref(connector: &str, id: &str) -> EntityRef {
    EntityRef {
        connector: linear_bridge::domain::ConnectorId::new(connector),
        kind: EntityKind::Project,
        scope: Some(if connector == "linear" {
            "VED".to_string()
        } else {
            SCOPE.to_string()
        }),
        native_id: id.to_string(),
        url: None,
    }
}

#[test]
fn a_mirrored_issue_lands_on_its_paired_project_board() {
    let mut harness = Harness::start();
    harness.pair_projects("project-uuid", 4);
    harness.set_issue(linear_issue(
        "Mirror the widget",
        "why it matters",
        Some("project-uuid"),
    ));

    harness.deliver_issue("create");

    // The issue was created on the forge...
    assert!(
        harness
            .forgejo
            .seen()
            .iter()
            .any(|record| record.method == "POST" && record.path.ends_with("/issues")),
        "the issue itself was not created: {:?}",
        harness.membership_requests()
    );
    // ...and put on the board of the project it names, by the *forge's* id for it.
    assert_eq!(
        harness.membership_requests(),
        vec![(
            "POST".to_string(),
            format!("/api/v1/repos/{SCOPE}/projects/4/issues/12")
        )],
        "the issue did not land on its mirrored project"
    );
    // And the pairing remembers where it landed, which is what lets a later clear
    // find the board to take it off.
    let links = harness
        .store
        .find_links(&EntityRef {
            connector: ConnectorId::new("linear"),
            kind: EntityKind::Issue,
            scope: Some("VED".into()),
            native_id: "issue-uuid".into(),
            url: None,
        })
        .expect("a readable store");
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].project.as_deref(), Some("4"));
}

#[test]
fn an_issue_naming_a_project_the_mapping_does_not_have_is_a_no_op() {
    let mut harness = Harness::start();
    // No project pairing exists for this id: the issue names a project the mirror
    // has never seen. That must place nothing - and, the point, must not be an error
    // that parks the delivery.
    harness.set_issue(linear_issue(
        "Mirror the widget",
        "why it matters",
        Some("unknown-project"),
    ));

    harness.deliver_issue("create");

    assert!(
        harness.membership_requests().is_empty(),
        "an unpaired project must place nothing: {:?}",
        harness.membership_requests()
    );
    // The issue itself still crossed: an unknown container is not a reason to drop
    // the issue.
    assert!(
        harness
            .forgejo
            .seen()
            .iter()
            .any(|record| record.method == "POST" && record.path.ends_with("/issues")),
        "the issue should still have been mirrored"
    );
}

#[test]
fn moving_and_clearing_the_project_moves_and_removes_the_issue() {
    let mut harness = Harness::start();
    harness.pair_projects("project-uuid", 4);
    harness.pair_projects("project-two", 5);
    harness.set_issue(linear_issue(
        "Mirror the widget",
        "why it matters",
        Some("project-uuid"),
    ));
    harness.deliver_issue("create");

    // The issue is moved to a different project's board: assigning the new project
    // takes it off the old one, because a forge issue sits on one project.
    harness.set_issue(linear_issue(
        "Mirror the widget",
        "why it matters",
        Some("project-two"),
    ));
    harness.deliver_issue("update");
    assert!(
        harness.membership_requests().iter().any(|(method, path)| {
            method == "POST" && path == &format!("/api/v1/repos/{SCOPE}/projects/5/issues/12")
        }),
        "the issue was not moved to the new project: {:?}",
        harness.membership_requests()
    );

    // And clearing it takes the issue off the board the pairing recorded.
    harness.set_issue(linear_issue("Mirror the widget", "why it matters", None));
    harness.deliver_issue("update");
    assert!(
        harness.membership_requests().iter().any(|(method, path)| {
            method == "DELETE" && path == &format!("/api/v1/repos/{SCOPE}/projects/5/issues/12")
        }),
        "the issue was not taken off the cleared project: {:?}",
        harness.membership_requests()
    );
}
