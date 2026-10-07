//! Projects, end to end: a Linear project and a forge project, over real sockets.
//!
//! A project is not an issue: it has a title and a description and no state, and a
//! forge project has no body field, so the pairing marker rides in its *description*.
//! This checks the whole path the engine takes for one - a delivery creates the copy
//! with the marker, a sweep adopts the pair by that marker, and a rename on either
//! side converges onto the other - against stateful fakes, so a create really appears
//! on the next read.
//!
//! The two fakes are hand-written rather than fixture-driven because a project fake
//! has to *remember* what it was told (a create a later sweep must find), which is
//! behaviour, not a shape.

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use linear_bridge::connector::Source;
use linear_bridge::domain::{EntityKind, EntityRef, Secret, UserMap};
use linear_bridge::queue::Handler;
use linear_bridge::reconcile::handler::{default_policy, Endpoint, Mapping, ReconcileHandler};
use linear_bridge::reconcile::placement::{ProjectScope, ProjectScopes};
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

/// Both platforms' projects, as the fakes hold them. Writes are applied here, which
/// is what makes a sweep see what a delivery wrote.
#[derive(Default)]
struct World {
    /// The Linear project, as GraphQL returns it under `project`.
    linear: Option<Value>,
    /// The forge's projects.
    forgejo: Vec<Value>,
    forgejo_next: i64,
    /// Every `ProjectCreate`/`ProjectUpdate` input Linear received.
    linear_writes: Vec<Value>,
}

fn state() -> Arc<Mutex<World>> {
    Arc::new(Mutex::new(World {
        forgejo_next: 4,
        ..World::default()
    }))
}

fn linear_project(id: &str, name: &str, description: &str) -> Value {
    json!({
        "id": id,
        "name": name,
        "description": description,
        "url": format!("https://linear.app/vedaru/project/{id}"),
    })
}

fn forgejo_project(id: i64, title: &str, description: &str) -> Value {
    json!({
        "id": id,
        "title": title,
        "description": description,
        "html_url": format!("http://forge/{SCOPE}/projects/{id}"),
        "is_closed": false,
    })
}

fn linear_routes(world: Arc<Mutex<World>>) -> impl Fn(&str, &str, &Value) -> (u16, Value) {
    move |_method, path, body| {
        assert_eq!(path, "/graphql");
        let query = body["query"].as_str().unwrap_or_default();
        let mut world = world.lock().expect("not poisoned");

        if query.contains("query Teams") {
            return (
                200,
                json!({ "data": { "teams": { "nodes": [{ "id": "team-uuid", "key": "VED" }] } } }),
            );
        }
        if query.contains("query Issues") {
            // A sweep lists issues too; this deployment has none.
            return (
                200,
                json!({ "data": { "issues": { "nodes": [],
                    "pageInfo": { "hasNextPage": false, "endCursor": Value::Null } } } }),
            );
        }
        if query.contains("query Projects") {
            let nodes = world.linear.clone().into_iter().collect::<Vec<_>>();
            return (
                200,
                json!({ "data": { "projects": { "nodes": nodes,
                    "pageInfo": { "hasNextPage": false, "endCursor": Value::Null } } } }),
            );
        }
        if query.contains("query Project(") {
            let wanted = body["variables"]["id"].clone();
            let found = world
                .linear
                .clone()
                .filter(|project| project["id"] == wanted);
            return (200, json!({ "data": { "project": found } }));
        }
        if query.contains("mutation ProjectCreate") {
            let input = body["variables"]["input"].clone();
            world.linear_writes.push(input.clone());
            let project = json!({
                "id": "project-linear-1",
                "name": input["name"],
                "description": input["description"],
                "url": "https://linear.app/vedaru/project/project-linear-1",
            });
            world.linear = Some(project.clone());
            return (
                200,
                json!({ "data": { "projectCreate": { "success": true, "project": {
                    "id": project["id"], "url": project["url"] } } } }),
            );
        }
        if query.contains("mutation ProjectUpdate") {
            let input = body["variables"]["input"].clone();
            world.linear_writes.push(input.clone());
            if let Some(project) = world.linear.as_mut() {
                for key in ["name", "description"] {
                    if let Some(value) = input.get(key) {
                        project[key] = value.clone();
                    }
                }
            }
            return (
                200,
                json!({ "data": { "projectUpdate": { "success": true } } }),
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

        if path.ends_with("/issues") {
            return (200, json!([]));
        }
        if path == format!("/api/v1/repos/{SCOPE}/projects") && method == "GET" {
            return (200, Value::Array(world.forgejo.clone()));
        }
        if path == format!("/api/v1/repos/{SCOPE}/projects") && method == "POST" {
            world.forgejo_next += 1;
            let id = world.forgejo_next;
            let project = forgejo_project(
                id,
                body["title"].as_str().unwrap_or_default(),
                body["description"].as_str().unwrap_or_default(),
            );
            world.forgejo.push(project.clone());
            return (201, project);
        }
        if let Some(id) = path
            .rsplit('/')
            .next()
            .and_then(|last| last.parse::<i64>().ok())
        {
            let found = world
                .forgejo
                .iter()
                .position(|project| project["id"] == json!(id));
            match (method, found) {
                ("GET", Some(index)) => return (200, world.forgejo[index].clone()),
                ("GET", None) => return (404, json!({ "message": "no such project" })),
                (_, Some(index)) => {
                    let project = &mut world.forgejo[index];
                    for key in ["title", "description"] {
                        if let Some(value) = body.get(key) {
                            project[key] = value.clone();
                        }
                    }
                    return (200, project.clone());
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
    let path = support::test_database("bridge-projects");
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
            name: "projects".into(),
            users: UserMap::default(),
            source: Endpoint::parse("linear:VED").unwrap(),
            sink: Endpoint::parse(&format!("forgejo:{SCOPE}")).unwrap(),
            // Placement is configuration: the project this file mirrors is named here.
            project_scopes: ProjectScopes::new(vec![ProjectScope {
                project: "project-uuid".into(),
                scope: SCOPE.into(),
            }]),
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

    fn forgejo_projects(&self) -> Vec<Value> {
        self.world.lock().expect("not poisoned").forgejo.clone()
    }

    /// How many writes the forge was asked to make - reads are expected of a sweep.
    fn forgejo_writes(&self) -> usize {
        self.forgejo
            .seen()
            .iter()
            .filter(|record| matches!(record.method.as_str(), "POST" | "PATCH" | "PUT"))
            .count()
    }

    fn linear(&self) -> Option<Value> {
        self.world.lock().expect("not poisoned").linear.clone()
    }

    fn linear_writes(&self) -> Vec<Value> {
        self.world
            .lock()
            .expect("not poisoned")
            .linear_writes
            .clone()
    }

    fn links(&mut self, reference: EntityRef) -> Vec<linear_bridge::store::Link> {
        self.store.find_links(&reference).expect("a readable store")
    }
}

/// A Linear project webhook, as `type = "Project"` carries it.
fn linear_project_event(action: &str, id: &str, name: &str) -> String {
    json!({
        "action": action,
        "type": "Project",
        "webhookTimestamp": linear_bridge::clock::now_millis(),
        "url": format!("https://linear.app/vedaru/project/{id}"),
        "actor": { "id": "u-1", "name": "vedaru" },
        "data": { "id": id, "name": name, "description": "why it matters" }
    })
    .to_string()
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
fn a_project_event_is_no_longer_the_inert_catch_all() {
    // Intake reads the resource, not the event name: a `Project` delivery is a
    // project, where before it fell through to the `event-name` catch-all and the
    // reconciler answered `NotOurKind`.
    let source = DeclarativeSource::new(
        "linear",
        Secret::new(SECRET),
        presets::preset("linear").expect("the preset loads"),
    );
    let body = linear_project_event("create", "project-uuid", "Mirror the projects");
    let events = source
        .parse(&source.replay_headers("Project"), body.as_bytes())
        .expect("the body parses");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, EntityKind::Project);
    assert_eq!(events[0].subject.native_id, "project-uuid");
}

#[test]
fn a_project_delivery_creates_it_on_the_forge_with_the_marker() {
    let mut harness = Harness::start();
    harness.world.lock().unwrap().linear = Some(linear_project(
        "project-uuid",
        "Mirror the widget",
        "why it matters",
    ));

    harness.deliver(
        "linear",
        "Project",
        &linear_project_event("create", "project-uuid", "Mirror the widget"),
    );

    let projects = harness.forgejo_projects();
    assert_eq!(projects.len(), 1, "the project was mirrored once");
    assert_eq!(projects[0]["title"], "Mirror the widget");
    // A forge project has no body field, so the marker rides in its description -
    // which is what lets a sweep adopt the copy without the store.
    let description = projects[0]["description"].as_str().expect("a description");
    assert!(description.starts_with("why it matters"), "{description}");
    assert!(
        description.contains("linear-bridge:linear:project-uuid"),
        "{description}"
    );

    // The pairing is recorded, keyed by the project's identity on both ends.
    let links = harness.links(project_ref("linear", "project-uuid"));
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].right.kind, EntityKind::Project);
    assert!(links[0].last_synced_hash.is_some());
}

#[test]
fn a_sweep_after_a_project_delivery_finds_it_in_step() {
    // The sweep lists projects on both sides, pairs them by the stored link, and
    // writes nothing: the delivery and the sweep share the record.
    let mut harness = Harness::start();
    harness.world.lock().unwrap().linear = Some(linear_project(
        "project-uuid",
        "Mirror the widget",
        "why it matters",
    ));
    harness.deliver(
        "linear",
        "Project",
        &linear_project_event("create", "project-uuid", "Mirror the widget"),
    );
    let writes_before = harness.forgejo_writes();

    let survey = harness.handler.survey(0).expect("a survey");

    assert_eq!(survey.writes(), 0, "{:?}", survey.entries);
    assert_eq!(
        harness.forgejo_writes(),
        writes_before,
        "a sweep reads, it does not write"
    );
}

#[test]
fn a_project_rename_on_linear_converges_on_the_forge() {
    let mut harness = Harness::start();
    harness.world.lock().unwrap().linear = Some(linear_project(
        "project-uuid",
        "Mirror the widget",
        "why it matters",
    ));
    harness.deliver(
        "linear",
        "Project",
        &linear_project_event("create", "project-uuid", "Mirror the widget"),
    );

    // Linear renames the project and announces it.
    harness.world.lock().unwrap().linear = Some(linear_project(
        "project-uuid",
        "Mirror the widget, properly",
        "why it matters",
    ));
    harness.deliver(
        "linear",
        "Project",
        &linear_project_event("update", "project-uuid", "Mirror the widget, properly"),
    );

    let projects = harness.forgejo_projects();
    assert_eq!(
        projects.len(),
        1,
        "an edit must not create a second project"
    );
    assert_eq!(projects[0]["title"], "Mirror the widget, properly");

    // And re-delivering the same edit is a no-op: the link records it.
    let patches = harness
        .forgejo
        .seen()
        .into_iter()
        .filter(|record| record.method == "PATCH")
        .count();
    harness.deliver(
        "linear",
        "Project",
        &linear_project_event("update", "project-uuid", "Mirror the widget, properly"),
    );
    assert_eq!(
        harness
            .forgejo
            .seen()
            .into_iter()
            .filter(|record| record.method == "PATCH")
            .count(),
        patches,
        "a re-sent edit wrote again: the recorded revision never converged"
    );
}

#[test]
fn a_project_rename_on_the_forge_converges_on_linear() {
    let mut harness = Harness::start();
    harness.world.lock().unwrap().linear = Some(linear_project(
        "project-uuid",
        "Mirror the widget",
        "why it matters",
    ));
    harness.deliver(
        "linear",
        "Project",
        &linear_project_event("create", "project-uuid", "Mirror the widget"),
    );

    // The forge renames the project - the same change, the other way round.
    harness.world.lock().unwrap().forgejo[0]["title"] = json!("Renamed on the forge");

    let survey = harness.handler.survey(0).expect("a survey");
    assert_eq!(survey.writes(), 1, "{:?}", survey.entries);
    let written = harness.handler.apply_survey(0, &survey).expect("applied");
    assert_eq!(written, 1);

    let linear = harness
        .linear()
        .expect("the forge's project reached Linear");
    assert_eq!(linear["name"], "Renamed on the forge");
    let updates: Vec<Value> = harness
        .linear_writes()
        .into_iter()
        .filter(|input| input.get("name") == Some(&json!("Renamed on the forge")))
        .collect();
    assert_eq!(updates.len(), 1, "one update, not one per sweep");
}

#[test]
fn a_project_created_on_the_forge_is_created_on_linear() {
    // The other direction of "created on either side": a project that exists only on
    // the forge is swept across, carrying the marker Linear's copy will be adopted by.
    let mut harness = Harness::start();
    harness.world.lock().unwrap().forgejo.push(forgejo_project(
        4,
        "Born on the forge",
        "no linear twin",
    ));

    let survey = harness.handler.survey(0).expect("a survey");
    assert_eq!(survey.writes(), 1, "{:?}", survey.entries);
    let creating = survey
        .entries
        .iter()
        .find(|entry| entry.side == Side::Sink)
        .expect("the creation is on the sink side");
    assert_eq!(creating.subject.kind, EntityKind::Project);

    harness.handler.apply_survey(0, &survey).expect("applied");

    let linear = harness.linear().expect("Linear has the new project");
    assert_eq!(linear["name"], "Born on the forge");
    let description = linear["description"].as_str().expect("a description");
    assert!(
        description.contains("linear-bridge:forgejo:4"),
        "Linear's copy carries the marker the forge's project will pair by: {description}"
    );
}
