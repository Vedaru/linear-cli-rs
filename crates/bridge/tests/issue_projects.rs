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
use linear_bridge::reconcile::survey::Action;
use linear_bridge::reconcile::{Sides, StateNames, Step};
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
    /// Where the board says the mirrored card sits. `Some` only when a test cares: a board
    /// the fake never had anybody drag a card across reports the card on no column, which is
    /// the truth about a placement this fake never recorded.
    card_in: Option<String>,
    /// A second source/sink pair, for a test that needs two cards on one board. `None`
    /// everywhere else, so the single-issue tests read exactly what they did before.
    second_issue: Option<Value>,
    second_forge_issue: Option<Value>,
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
        // A sweep lists a team's issues and resolves the team by key first; a delivery never
        // had to, which is why these two only appeared when a survey ran in this harness.
        if query.contains("query Teams") {
            return (
                200,
                json!({ "data": { "teams": { "nodes": [{ "id": "team-uuid", "key": "VED" }] } } }),
            );
        }
        if query.contains("query Projects(") {
            // The survey mirrors projects too, so it lists them. The harness's paired project
            // exists only when a test said so - which is the issue carrying it.
            let world = world.lock().expect("not poisoned");
            let carries =
                |issue: &Value| issue.get("project").map(|p| !p.is_null()).unwrap_or(false);
            let carries_a_project = world
                .issue
                .iter()
                .chain(world.second_issue.iter())
                .any(&carries);
            let nodes: Vec<Value> = if carries_a_project {
                vec![json!({
                    "id": "project-uuid",
                    "slugId": "widget",
                    "name": "The widget",
                    "description": "",
                    "state": "started",
                    "url": "https://linear.app/vedaru/project/widget",
                    "externalLinks": { "nodes": [] }
                })]
            } else {
                Vec::new()
            };
            return (
                200,
                json!({ "data": { "projects": { "nodes": nodes, "pageInfo": {
                    "hasNextPage": false, "endCursor": null } } } }),
            );
        }
        if query.contains("query Issues(") {
            let world = world.lock().expect("not poisoned");
            let nodes: Vec<Value> = world
                .issue
                .clone()
                .into_iter()
                .chain(world.second_issue.clone())
                .collect();
            return (
                200,
                json!({ "data": { "issues": { "nodes": nodes, "pageInfo": {
                    "hasNextPage": false, "endCursor": null } } } }),
            );
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
        // The collection: a survey reads it to find the counterpart of each source issue, where
        // a delivery only ever read back the one issue it had just written.
        if path == issues && method == "GET" {
            let listed: Vec<Value> = world
                .forge_issue
                .clone()
                .into_iter()
                .chain(world.second_forge_issue.clone())
                .collect();
            return (200, json!(listed));
        }
        if path.starts_with(&issues) {
            return match world.forge_issue.clone() {
                Some(issue) => (200, issue),
                None => (404, json!({ "message": "no such issue" })),
            };
        }
        // `GET /projects` and `GET /projects/{id}`: a survey lists the boards (the mirror keeps
        // them paired) where a delivery only ever needed to place a card on one.
        if path.ends_with("/projects") && method == "GET" {
            return (
                200,
                json!([{
                    "id": 4,
                    "title": "The widget",
                    "description": "",
                    "is_closed": false,
                    "html_url": "http://forge/Vedaru/linear-cli-rs/projects/4",
                    "created_at": "2026-01-01T00:00:00Z",
                    "updated_at": "2026-01-01T00:00:00Z"
                }]),
            );
        }
        if path.ends_with("/projects/4") && method == "GET" {
            return (
                200,
                json!({
                    "id": 4,
                    "title": "The widget",
                    "description": "",
                    "is_closed": false,
                    "html_url": "http://forge/Vedaru/linear-cli-rs/projects/4",
                    "created_at": "2026-01-01T00:00:00Z",
                    "updated_at": "2026-01-01T00:00:00Z"
                }),
            );
        }
        // `GET /projects/{id}/columns`: what a board calls its columns, which is what a
        // placement resolves a column *name* against. Per project, as the real endpoint
        // is: the harness has one board, but the path names it. Each column carries its
        // cards, because that is what a *sweep* reads to see a card somebody moved.
        if path.ends_with("/columns") && method == "GET" {
            let numbers: Vec<Value> = world
                .forge_issue
                .iter()
                .chain(world.second_forge_issue.iter())
                .filter_map(|issue| issue.get("number").cloned())
                .collect();
            let cards = |title: &str| -> Value {
                if world.card_in.as_deref() == Some(title) {
                    json!(numbers)
                } else {
                    json!([])
                }
            };
            return (
                200,
                json!([
                    { "id": 30, "title": "Backlog", "default": true, "cards": cards("Backlog") },
                    { "id": 31, "title": "To Do", "default": false, "cards": cards("To Do") },
                    { "id": 32, "title": "In Progress", "default": false, "cards": cards("In Progress") },
                    { "id": 33, "title": "Done", "default": false, "cards": cards("Done") }
                ]),
            );
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
    let path = support::test_database("bridge-issue-projects");
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
        // What the board calls the states the source names. This table is the only thing
        // that makes a placement name a column: with it, a card follows its issue's state;
        // without it, the placement says nothing and the card keeps its column.
        policy.columns = [
            ("Todo".to_string(), "To Do".to_string()),
            ("In Progress".to_string(), "In Progress".to_string()),
        ]
        .into_iter()
        .collect();
        let mapping = Mapping {
            name: "issue-projects".into(),
            users: UserMap::default(),
            source: Endpoint::parse("linear:VED").unwrap(),
            sink: Endpoint::parse(&format!("forgejo:{SCOPE}")).unwrap(),
            project_scopes: Default::default(),
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

    /// The bodies of the placements the forge was sent, in arrival order.
    fn placements(&self) -> Vec<Value> {
        self.forgejo
            .seen()
            .into_iter()
            .filter(|record| {
                record.method == "POST"
                    && record.path.contains("/projects/")
                    && record.path.contains("/issues/")
            })
            .map(|record| record.body)
            .collect()
    }

    /// How many times the forge was asked for a board's columns.
    fn board_reads(&self) -> usize {
        self.forgejo
            .seen()
            .into_iter()
            .filter(|record| record.method == "GET" && record.path.ends_with("/columns"))
            .count()
    }

    /// Somebody drags the card to another column, as the board would then report it.
    ///
    /// Not a request the bridge makes: this is the human edit no delivery announces, which
    /// is exactly why placement is something only a sweep can notice.
    fn card_dragged_to(&self, column: &str) {
        self.world.lock().expect("not poisoned").card_in = Some(column.to_string());
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

/// A card lands in the column the issue's state names, not in the board's default.
///
/// This is the whole difference between a board that says what Linear says and one
/// where every card reads Backlog, however many states the issues are in. The column
/// travels as a *name* - that is what a mapping can state - and the id the request needs
/// is resolved against the board, which is why the placement body carries a number.
#[test]
fn a_mirrored_issue_lands_in_the_column_its_state_names() {
    let mut harness = Harness::start();
    harness.pair_projects("project-uuid", 4);
    let mut issue = linear_issue("In flight", "why it matters", Some("project-uuid"));
    issue["state"] = json!({ "name": "In Progress" });
    harness.set_issue(issue);

    harness.deliver_issue("create");

    let placements = harness.placements();
    assert_eq!(placements.len(), 1, "one placement: {placements:?}");
    assert_eq!(
        placements[0]["column_id"], 32,
        "the column the state named, resolved to the board's own id: {:?}",
        placements[0]
    );
}

/// A state the mapping says nothing about leaves the card where it is: the placement
/// carries no column at all, so the key is absent rather than null. A mapping that has
/// not thought about a state must not move somebody's card to the default column - and
/// on the wire, "leave it" and "clear it" are different requests.
#[test]
fn a_state_with_no_column_named_places_the_card_without_touching_its_column() {
    let mut harness = Harness::start();
    harness.pair_projects("project-uuid", 4);
    let mut issue = linear_issue("In flight", "why it matters", Some("project-uuid"));
    issue["state"] = json!({ "name": "Backlog" });
    harness.set_issue(issue);

    harness.deliver_issue("create");

    let placements = harness.placements();
    assert_eq!(placements.len(), 1, "one placement: {placements:?}");
    assert!(
        placements[0].get("column_id").is_none(),
        "the body must say nothing about the column: {:?}",
        placements[0]
    );
}

/// A card somebody dragged stays where they put it - until a sweep says otherwise, and one
/// only moves it when it is *asked* to.
///
/// Placement is not a field of the issue, so no delivery announces a drag and no field diff
/// can see one: the board is the only place that evidence lives, which is what makes this the
/// sweep's half of "the board reads what Linear reads". The sweep's own contract survives
/// intact - a dry run reports the move, applying the plan makes it - which is the line this
/// test draws: the *report* is the deliverable, the move is a separate, deliberate act.
#[test]
fn a_sweep_reports_a_card_that_drifted_and_moves_it_only_when_asked() {
    let mut harness = Harness::start();
    harness.pair_projects("project-uuid", 4);
    let mut issue = linear_issue("In flight", "why it matters", Some("project-uuid"));
    issue["state"] = json!({ "name": "In Progress" });
    harness.set_issue(issue);

    // A delivery puts the card where the state says, and records the board it sits on.
    harness.deliver_issue("create");
    assert_eq!(
        harness.placements().len(),
        1,
        "the delivery placed the card"
    );

    // Then somebody drags it to Backlog. Nothing tells the bridge; the board is where it shows.
    harness.card_dragged_to("Backlog");

    let survey = harness.handler.survey(0).expect("a survey");
    let planned: Vec<(String, String)> = survey
        .entries
        .iter()
        .filter_map(|entry| match &entry.step {
            Some(Step::Place { project, column }) => Some((project.clone(), column.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        planned,
        vec![("4".to_string(), "In Progress".to_string())],
        "the plan has to carry the move, and only the move: {:?}",
        survey.entries
    );
    assert_eq!(
        harness.placements().len(),
        1,
        "a dry run reports the move and writes nothing"
    );

    // Applying what it planned is what moves the card - back to the column the state names.
    harness
        .handler
        .apply_survey(0, &survey)
        .expect("the plan applies");
    let moves = harness.placements();
    assert_eq!(moves.len(), 2, "one more placement: {moves:?}");
    assert_eq!(
        moves[1]["column_id"], 32,
        "resolved to the board's own id, not the name: {:?}",
        moves[1]
    );

    // And a board that agrees has nothing left to say.
    harness.card_dragged_to("In Progress");
    let quiet = harness.handler.survey(0).expect("a survey");
    assert!(
        !quiet
            .entries
            .iter()
            .any(|entry| matches!(entry.step, Some(Step::Place { .. }))),
        "a card already in the right column is not a difference: {:?}",
        quiet.entries
    );
}

/// A sweep over two cards on one board reads the board **once** (VED-301).
///
/// A board is the same for every card on it, so a per-card read is N HTTP requests
/// for one answer. Both pairs are in step and both names a project, so both reach
/// the placement check - which is what makes the request count mean something: with
/// the cache, two checks are one read; without it, two.
#[test]
fn a_sweep_reads_a_board_once_for_every_card_on_it() {
    let mut harness = Harness::start();
    harness.pair_projects("project-uuid", 4);

    let linear = |id: &str, number: i64, title: &str, body: &str| {
        json!({
            "id": id,
            "identifier": format!("VED-{number}"),
            "url": format!("https://linear.app/vedaru/issue/VED-{number}/x"),
            "title": title,
            "description": body,
            "dueDate": null,
            "priority": 0,
            "state": { "name": "In Progress" },
            "labels": { "nodes": [] },
            "assignee": null,
            "project": { "id": "project-uuid" }
        })
    };
    let forge = |number: i64, title: &str, body: &str| {
        json!({
            "number": number,
            "html_url": format!("http://forge/{SCOPE}/issues/{number}"),
            "title": title,
            "body": body,
            "state": "open",
            "labels": [],
            "assignees": [],
            "due_date": "0001-01-01T00:00:00Z"
        })
    };

    {
        let mut world = harness.world.lock().expect("not poisoned");
        world.issue = Some(linear("issue-a", 1, "One", "body one"));
        world.second_issue = Some(linear("issue-b", 2, "Two", "body two"));
        world.forge_issue = Some(forge(12, "One", "body one"));
        world.second_forge_issue = Some(forge(13, "Two", "body two"));
        // Both cards sit where their state names, so both pairs are in step.
        world.card_in = Some("In Progress".to_string());
    }

    let issue_ref = |connector: &str, scope: &str, id: &str| EntityRef {
        connector: ConnectorId::new(connector),
        kind: EntityKind::Issue,
        scope: Some(scope.to_string()),
        native_id: id.to_string(),
        url: None,
    };
    for (linear_id, forge_number) in [("issue-a", 12), ("issue-b", 13)] {
        harness
            .store
            .upsert_link(
                &Link::new(
                    issue_ref("linear", "VED", linear_id),
                    issue_ref("forgejo", SCOPE, &forge_number.to_string()),
                )
                .with_project(Some("4")),
            )
            .expect("the issue pair is recorded");
    }

    let before = harness.board_reads();
    let survey = harness.handler.survey(0).expect("a survey");
    let after = harness.board_reads();

    let in_step = survey
        .entries
        .iter()
        .filter(|entry| entry.action == Action::InStep)
        .count();
    assert!(
        in_step >= 2,
        "both pairs must be in step or the count means nothing: {:?}",
        survey.entries
    );
    assert_eq!(
        after - before,
        1,
        "two cards on one board must be one read, not two: {:?}",
        survey.entries
    );
}
