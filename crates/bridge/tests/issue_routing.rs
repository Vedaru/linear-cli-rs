//! Placement: which repository an entity's mirror lives in.
//!
//! One mapping, two (and more) forge repositories: where a project's mirror lives is
//! configuration (`[[mapping.project]]`), and an issue follows its project. This checks
//! the whole path against stateful fakes where a create a later read must see really is
//! remembered, and asserts on the actual request paths:
//!
//! - a configured project is created in the repository its entry names;
//! - an issue in that project lands in the same repository as the project's mirror;
//! - a project with no entry is not mirrored, and its issue falls back to the mapping's
//!   own repository (as does an issue with no project at all);
//! - several entries may name the same repository;
//! - an existing pair whose project is now configured elsewhere is *not* relocated - the
//!   update still goes to the repository the pair lives in.
//!
//! The fakes are hand-written rather than fixture-driven because this is behaviour,
//! not a shape: a create has to appear on the next read.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use linear_bridge::connector::Source;
use linear_bridge::domain::{EntityKind, EntityRef, Secret, UserMap};
use linear_bridge::queue::Handler;
use linear_bridge::reconcile::handler::{default_policy, Endpoint, Mapping, ReconcileHandler};
use linear_bridge::reconcile::placement::{ProjectScope, ProjectScopes};
use linear_bridge::reconcile::{Sides, StateNames};
use linear_bridge::sink::Sink;
use linear_bridge::sources::declarative::DeclarativeSource;
use linear_bridge::sources::presets;
use linear_bridge::store::sqlite::SqliteStore;
use linear_bridge::store::{Delivery, Link, Store};

mod support;
use support::Fake;

const SECRET: &str = "0123456789abcdef";
const DEFAULT_SCOPE: &str = "Vedaru/linear-cli-rs";

/// Both platforms, as the fakes hold them. Writes land here, which is what makes a
/// later read see what an earlier delivery did.
#[derive(Default)]
struct World {
    linear_issues: BTreeMap<String, Value>,
    linear_projects: BTreeMap<String, Value>,
    /// Forge issues, keyed by (scope, number): the scope is the repository.
    forge_issues: BTreeMap<(String, i64), Value>,
    forge_projects: BTreeMap<String, Vec<Value>>,
    forge_next: i64,
    forge_project_next: i64,
}

fn state() -> Arc<Mutex<World>> {
    Arc::new(Mutex::new(World::default()))
}

fn linear_project(id: &str, name: &str, slug: &str) -> Value {
    json!({
        "id": id,
        "slugId": slug,
        "name": name,
        "description": "why it matters",
        "url": format!("https://linear.app/vedaru/project/{slug}"),
    })
}

fn linear_issue(id: &str, identifier: &str, project: Option<&str>, labels: &[&str]) -> Value {
    json!({
        "id": id,
        "identifier": identifier,
        "url": format!("https://linear.app/vedaru/issue/{identifier}"),
        "title": "Mirror the widget",
        "description": "why it matters",
        "dueDate": Value::Null,
        "priority": 0,
        "state": { "name": "Todo" },
        "labels": { "nodes": labels.iter().map(|name| json!({ "name": name })).collect::<Vec<_>>() },
        "assignee": Value::Null,
        "project": project.map(|id| json!({ "id": id })),
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
        if query.contains("TeamStates") {
            return (
                200,
                json!({ "data": { "team": { "states": { "nodes": [{ "id": "state-todo", "name": "Todo" }] } } } }),
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
        if query.contains("query Projects") {
            let nodes: Vec<Value> = world.linear_projects.values().cloned().collect();
            return (
                200,
                json!({ "data": { "projects": { "nodes": nodes,
                "pageInfo": { "hasNextPage": false, "endCursor": Value::Null } } } }),
            );
        }
        if query.contains("query Project(") {
            let wanted = body["variables"]["id"].clone();
            let found = world
                .linear_projects
                .values()
                .find(|p| p["id"] == wanted)
                .cloned();
            return (200, json!({ "data": { "project": found } }));
        }
        if query.contains("query Issue(") {
            let wanted = body["variables"]["id"].clone();
            let found = world
                .linear_issues
                .values()
                .find(|i| i["id"] == wanted)
                .cloned();
            return (200, json!({ "data": { "issue": found } }));
        }
        if query.contains("query Issues") {
            let nodes: Vec<Value> = world.linear_issues.values().cloned().collect();
            return (
                200,
                json!({ "data": { "issues": { "nodes": nodes,
                "pageInfo": { "hasNextPage": false, "endCursor": Value::Null } } } }),
            );
        }
        if query.contains("mutation ProjectCreate") {
            let input = body["variables"]["input"].clone();
            let id = format!("project-{}", world.linear_projects.len() + 1);
            let project = linear_project(&id, input["name"].as_str().unwrap_or(""), &id);
            world.linear_projects.insert(id.clone(), project.clone());
            return (
                200,
                json!({ "data": { "projectCreate": { "success": true, "project": {
                "id": id, "url": project["url"] } } } }),
            );
        }
        if query.contains("mutation IssueCreate") {
            let input = body["variables"]["input"].clone();
            let id = format!("issue-{}", world.linear_issues.len() + 1);
            let issue = linear_issue(&id, &id.to_uppercase(), None, &[]);
            world.linear_issues.insert(id.clone(), issue);
            let _ = input;
            return (
                200,
                json!({ "data": { "issueCreate": { "success": true, "issue": {
                "id": id, "identifier": id.to_uppercase(), "url": format!("https://linear.app/vedaru/issue/{id}") } } } }),
            );
        }
        if query.contains("mutation IssueUpdate") {
            return (
                200,
                json!({ "data": { "issueUpdate": { "success": true } } }),
            );
        }
        if query.contains("mutation IssueArchive") {
            return (
                200,
                json!({ "data": { "issueArchive": { "success": true } } }),
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
        let rest = path.strip_prefix("/api/v1/repos/").unwrap_or(path);

        // A membership path (`/projects/{id}/issues/{index}`) names both a project and
        // an issue; both are a no-op returning the empty 204 Forgejo sends.
        if rest.contains("/projects/") && rest.contains("/issues/") {
            return (204, Value::Null);
        }

        // Label name -> id, for an issue whose labels must travel. The collection
        // answers with the labels a repository already has.
        if let Some((_scope, tail)) = rest.split_once("/labels") {
            if tail.is_empty() && method == "GET" {
                return (200, json!([{ "id": 7, "name": "urgent" }]));
            }
            if tail.is_empty() && method == "POST" {
                return (201, json!({ "id": 7, "name": body["name"] }));
            }
            if method == "PUT" {
                return (200, json!([]));
            }
        }

        if let Some((scope, tail)) = rest.split_once("/projects") {
            if tail.is_empty() && method == "GET" {
                return (
                    200,
                    json!(world.forge_projects.get(scope).cloned().unwrap_or_default()),
                );
            }
            if tail.is_empty() && method == "POST" {
                world.forge_project_next += 1;
                let id = world.forge_project_next;
                let project = json!({
                    "id": id,
                    "title": body["title"],
                    "description": body["description"],
                    "html_url": format!("http://forge/{scope}/projects/{id}"),
                    "is_closed": false,
                });
                world
                    .forge_projects
                    .entry(scope.to_string())
                    .or_default()
                    .push(project.clone());
                return (201, project);
            }
            if let Some(id) = tail
                .strip_prefix('/')
                .and_then(|last| last.parse::<i64>().ok())
            {
                let found = world
                    .forge_projects
                    .get(scope)
                    .and_then(|projects| projects.iter().find(|project| project["id"] == json!(id)))
                    .cloned();
                match (method, found) {
                    ("GET", Some(project)) => return (200, project),
                    ("GET", None) => return (404, json!({ "message": "no such project" })),
                    (_, Some(project)) => return (200, project),
                    _ => {}
                }
            }
        }

        if let Some((scope, tail)) = rest.split_once("/issues") {
            if tail.is_empty() && method == "GET" {
                let list: Vec<Value> = world
                    .forge_issues
                    .iter()
                    .filter(|((issue_scope, _), _)| issue_scope == scope)
                    .map(|(_, issue)| issue.clone())
                    .collect();
                return (200, Value::Array(list));
            }
            if tail.is_empty() && method == "POST" {
                world.forge_next += 1;
                let number = world.forge_next;
                let issue = json!({
                    "number": number,
                    "html_url": format!("http://forge/{scope}/issues/{number}"),
                    "title": body["title"],
                    "body": body["body"],
                    "state": "open",
                    "labels": [],
                    "assignees": [],
                    "due_date": "0001-01-01T00:00:00Z",
                });
                world
                    .forge_issues
                    .insert((scope.to_string(), number), issue.clone());
                return (
                    201,
                    json!({ "number": number, "html_url": issue["html_url"] }),
                );
            }
            if let Some(id) = tail
                .strip_prefix('/')
                .and_then(|last| last.parse::<i64>().ok())
            {
                let found = world.forge_issues.get(&(scope.to_string(), id)).cloned();
                match (method, found) {
                    ("GET", Some(issue)) => return (200, issue),
                    ("GET", None) => return (404, json!({ "message": "no such issue" })),
                    (_, Some(mut issue)) => {
                        for key in ["title", "body", "state", "assignees", "due_date"] {
                            if let Some(value) = body.get(key) {
                                issue[key] = value.clone();
                            }
                        }
                        world
                            .forge_issues
                            .insert((scope.to_string(), id), issue.clone());
                        return (200, issue);
                    }
                    _ => {}
                }
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
    let path = support::test_database("bridge-issue-routing");
    path.to_string_lossy().to_string()
}

impl Harness {
    fn start() -> Self {
        Self::with_projects(ProjectScopes::new(vec![ProjectScope {
            project: "project-kuro".into(),
            scope: "Vedaru/kuro".into(),
        }]))
    }

    /// A harness whose mapping carries exactly these project entries, so a test can show
    /// where configuration places a project and its issues.
    fn with_projects(projects: ProjectScopes) -> Self {
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
            name: "routing".into(),
            users: UserMap::default(),
            source: Endpoint::parse("linear:VED").unwrap(),
            sink: Endpoint::parse(&format!("forgejo:{DEFAULT_SCOPE}")).unwrap(),
            // Placement is configuration: a `[[mapping.project]]` entry says where a
            // project's mirror lives. No default for a project, no link reading.
            project_scopes: projects,
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

    fn set_linear_project(&self, id: &str, name: &str, slug: &str) {
        self.world
            .lock()
            .expect("not poisoned")
            .linear_projects
            .insert(id.to_string(), linear_project(id, name, slug));
    }

    fn set_linear_issue(&self, id: &str, identifier: &str, project: Option<&str>, labels: &[&str]) {
        self.world
            .lock()
            .expect("not poisoned")
            .linear_issues
            .insert(
                id.to_string(),
                linear_issue(id, identifier, project, labels),
            );
    }

    fn add_forge_issue(&self, scope: &str, number: i64, title: &str) {
        self.world
            .lock()
            .expect("not poisoned")
            .forge_issues
            .insert(
                (scope.to_string(), number),
                json!({
                    "number": number,
                    "html_url": format!("http://forge/{scope}/issues/{number}"),
                    "title": title,
                    "body": "why it matters",
                    "state": "open",
                    "labels": [],
                    "assignees": [],
                    "due_date": "0001-01-01T00:00:00Z",
                }),
            );
    }

    fn pair(&mut self, issue_id: &str, scope: &str, number: i64) {
        self.store
            .upsert_link(&Link::new(
                linear_issue_ref(issue_id),
                forge_issue_ref(scope, number),
            ))
            .expect("the issues pair");
    }

    /// One webhook delivery through the same path the service uses.
    fn deliver(&mut self, event: &str, body: &str) {
        let source = self
            .sources
            .iter()
            .find(|source| source.id().as_str() == "linear")
            .expect("the linear source");
        let parsed = source
            .parse(&source.replay_headers(event), body.as_bytes())
            .expect("the body parses");
        let first = parsed.first().expect("one event");
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

    /// Every request the forge received, as `(method, path)`.
    fn forge_requests(&self) -> Vec<(String, String)> {
        self.forgejo
            .seen()
            .into_iter()
            .map(|record| {
                (
                    record.method,
                    record
                        .path
                        .split('?')
                        .next()
                        .unwrap_or(&record.path)
                        .to_string(),
                )
            })
            .collect()
    }
}

/// A Linear issue on the `VED` team.
fn linear_issue_ref(id: &str) -> EntityRef {
    EntityRef {
        connector: linear_bridge::domain::ConnectorId::new("linear"),
        kind: EntityKind::Issue,
        scope: Some("VED".to_string()),
        native_id: id.to_string(),
        url: None,
    }
}

/// A forge issue in a named repository.
fn forge_issue_ref(scope: &str, number: i64) -> EntityRef {
    EntityRef {
        connector: linear_bridge::domain::ConnectorId::new("forgejo"),
        kind: EntityKind::Issue,
        scope: Some(scope.to_string()),
        native_id: number.to_string(),
        url: None,
    }
}

fn linear_issue_event(id: &str, action: &str) -> String {
    json!({
        "action": action,
        "type": "Issue",
        "webhookTimestamp": linear_bridge::clock::now_millis(),
        "url": format!("https://linear.app/vedaru/issue/{id}"),
        "actor": { "id": "u-1", "name": "vedaru" },
        "data": { "id": id, "identifier": id, "team": { "key": "VED" } }
    })
    .to_string()
}

fn linear_project_event(id: &str, action: &str) -> String {
    json!({
        "action": action,
        "type": "Project",
        "webhookTimestamp": linear_bridge::clock::now_millis(),
        "url": format!("https://linear.app/vedaru/project/{id}"),
        "actor": { "id": "u-1", "name": "vedaru" },
        "data": { "id": id, "name": "Kuro", "description": "why it matters" }
    })
    .to_string()
}

#[test]
fn a_configured_projects_entry_places_it_and_its_issues_and_the_rest_fall_to_the_mapping_scope() {
    let mut harness = Harness::start();

    // The harness configures `project-kuro` into `Vedaru/kuro`. Another project has no
    // entry, and a bare issue has no project at all: those still have a home - the
    // mapping's own scope - because an issue that lives nowhere is worse than one that
    // lives in the team's repository. A project with no entry is not mirrored at all.
    harness.set_linear_project("project-kuro", "Kuro", "kuro");
    harness.set_linear_project("project-plain", "Plain", "plain");
    harness.deliver("Project", &linear_project_event("project-kuro", "create"));
    harness.deliver("Project", &linear_project_event("project-plain", "create"));

    harness.set_linear_issue("issue-kuro", "VED-100", Some("project-kuro"), &[]);
    harness.set_linear_issue("issue-default", "VED-101", Some("project-plain"), &[]);
    harness.set_linear_issue("issue-bare", "VED-102", None, &[]);

    for id in ["issue-kuro", "issue-default", "issue-bare"] {
        harness.deliver("Issue", &linear_issue_event(id, "create"));
    }

    let posts: Vec<String> = harness
        .forge_requests()
        .into_iter()
        .filter(|(method, path)| method == "POST" && path.ends_with("/issues"))
        .map(|(_, path)| path)
        .collect();
    let count = |scope: &str| {
        let wanted = format!("/api/v1/repos/{scope}/issues");
        posts.iter().filter(|path| **path == wanted).count()
    };

    let projects: Vec<String> = harness
        .forge_requests()
        .into_iter()
        .filter(|(method, path)| method == "POST" && path.ends_with("/projects"))
        .map(|(_, path)| path)
        .collect();

    assert_eq!(
        projects,
        vec!["/api/v1/repos/Vedaru/kuro/projects".to_string()],
        "only the linked project is mirrored: {projects:?}"
    );
    assert_eq!(count("Vedaru/kuro"), 1, "the linked issue: {posts:?}");
    assert_eq!(
        count(DEFAULT_SCOPE),
        2,
        "a project-less issue and an unlinked project's issue fall to the mapping scope: {posts:?}"
    );
}

#[test]
fn a_paired_issue_is_not_relocated_when_its_project_is_configured_elsewhere() {
    let mut harness = Harness::start();
    // The pair already lives in the default repository, but the issue's project is
    // configured into kuro. Moving it would delete the forge copy and its history, so the
    // update must go to the repository the pair is in.
    harness.add_forge_issue(DEFAULT_SCOPE, 12, "Old title");
    harness.pair("issue-moved", DEFAULT_SCOPE, 12);
    harness.set_linear_project("project-kuro", "Kuro", "kuro");
    harness.set_linear_issue("issue-moved", "VED-300", Some("project-kuro"), &[]);

    harness.deliver("Issue", &linear_issue_event("issue-moved", "update"));

    let requests = harness.forge_requests();
    assert!(
        requests.contains(&(
            "PATCH".to_string(),
            format!("/api/v1/repos/{DEFAULT_SCOPE}/issues/12")
        )),
        "the update did not go to the pair's own repo: {requests:?}"
    );
    assert!(
        !requests
            .iter()
            .any(|(method, path)| method == "POST" && path.contains("/repos/Vedaru/kuro/")),
        "the pair was relocated into the routed repo: {requests:?}"
    );
    // The pairing is still the one it was.
    let links = harness
        .store
        .find_links(&linear_issue_ref("issue-moved"))
        .expect("a readable store");
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].right.scope.as_deref(), Some(DEFAULT_SCOPE));
}

#[test]
fn a_sweep_compares_each_issue_against_its_own_repository() {
    let mut harness = Harness::start();
    harness.set_linear_project("project-kuro", "Kuro", "kuro");
    harness.set_linear_issue("issue-kuro", "VED-100", Some("project-kuro"), &[]);
    harness.add_forge_issue("Vedaru/kuro", 5, "Old title");
    harness.pair("issue-kuro", "Vedaru/kuro", 5);

    let survey = harness.handler.survey(0).expect("a survey");
    let writing = survey
        .entries
        .iter()
        .find(|entry| entry.step.is_some())
        .expect("the sweep has a write");
    assert_eq!(
        writing.sink_scope, "Vedaru/kuro",
        "the issue was compared against the wrong repo: {:?}",
        survey.entries
    );

    let written = harness.handler.apply_survey(0, &survey).expect("applied");
    // The issue update, and (sync_projects is on) the project the source names but the
    // forge does not yet have - both in the routed repo.
    assert!(written >= 1, "the sweep wrote nothing");
    assert!(
        harness.forge_requests().contains(&(
            "PATCH".to_string(),
            "/api/v1/repos/Vedaru/kuro/issues/5".to_string()
        )),
        "the sweep wrote to the wrong repo: {:?}",
        harness.forge_requests()
    );
}

#[test]
fn a_configured_entry_places_its_issues_and_an_unconfigured_project_is_not_a_candidate() {
    // Placement is configuration. One project is configured into the forge; the other is
    // not configured at all, so it is not mirrored - and its issue, with no entry to
    // follow, falls to the mapping's own scope.
    let mut harness = Harness::with_projects(ProjectScopes::new(vec![ProjectScope {
        project: "linked".into(),
        scope: "Vedaru/linked".into(),
    }]));
    harness.set_linear_project("project-linked", "Linked", "linked");
    harness.set_linear_project("project-ghost", "Ghost", "ghost");
    harness.deliver("Project", &linear_project_event("project-linked", "create"));

    harness.set_linear_issue("issue-linked", "VED-400", Some("project-linked"), &[]);
    harness.set_linear_issue("issue-ghost", "VED-401", Some("project-ghost"), &[]);
    harness.deliver("Issue", &linear_issue_event("issue-linked", "create"));
    harness.deliver("Issue", &linear_issue_event("issue-ghost", "create"));

    let posts: Vec<String> = harness
        .forge_requests()
        .into_iter()
        .filter(|(method, path)| method == "POST" && path.ends_with("/issues"))
        .map(|(_, path)| path)
        .collect();
    assert_eq!(
        posts,
        vec![
            "/api/v1/repos/Vedaru/linked/issues".to_string(),
            format!("/api/v1/repos/{DEFAULT_SCOPE}/issues"),
        ],
        "the linked project's issue goes to its link, the other to the mapping scope: {posts:?}"
    );
    // The project itself was created in the repo it links to.
    assert!(
        harness.forge_requests().contains(&(
            "POST".to_string(),
            "/api/v1/repos/Vedaru/linked/projects".to_string()
        )),
        "the linked project was not created in its linked repo: {:?}",
        harness.forge_requests()
    );
}

/// A sweep of an unpaired project that links to a repository: the pass that creates both
/// the project and its issue, and the guarantee that the issue comes with its project -
/// both in the linked repository, and the issue on its board *there*.
#[test]
fn a_sweep_places_a_configured_project_and_carries_its_unpaired_issue_with_it() {
    let mut harness = Harness::start();
    harness.set_linear_project("project-kuro", "Kuro", "kuro");
    harness.set_linear_issue("issue-kuro", "VED-100", Some("project-kuro"), &[]);

    let survey = harness.handler.survey(0).expect("a survey");
    harness.handler.apply_survey(0, &survey).expect("applied");

    let requests = harness.forge_requests();
    // The project itself was created in its linked repository...
    assert!(
        requests.contains(&(
            "POST".to_string(),
            "/api/v1/repos/Vedaru/kuro/projects".to_string()
        )),
        "the project was not created in its linked repo: {requests:?}"
    );
    // ...the issue was created in that same repository, not the default...
    let posts: Vec<&String> = requests
        .iter()
        .filter(|(method, path)| method == "POST" && path.ends_with("/issues"))
        .map(|(_, path)| path)
        .collect();
    assert!(
        posts
            .iter()
            .any(|path| path.as_str() == "/api/v1/repos/Vedaru/kuro/issues"),
        "the issue was not created in the linked repo: {posts:?}"
    );
    assert!(
        !posts
            .iter()
            .any(|path| path.as_str() == format!("/api/v1/repos/{DEFAULT_SCOPE}/issues").as_str()),
        "the issue fell back to the default repo: {posts:?}"
    );
    // ...and it landed on its board, in that same linked repository.
    assert!(
        requests.iter().any(|(method, path)| {
            method == "POST"
                && path.starts_with("/api/v1/repos/Vedaru/kuro/projects/")
                && path.ends_with("/issues/1")
        }),
        "the issue did not land on its board in the linked repo: {requests:?}"
    );
}

#[test]
fn a_sweep_skips_a_project_with_no_entry() {
    // The project names no repository in configuration, so it is not mirrored; its issue
    // still is, in the mapping's own scope.
    let mut harness = Harness::start();
    harness.set_linear_project("project-plain", "Plain", "plain");
    harness.set_linear_issue("issue-plain", "VED-101", Some("project-plain"), &[]);

    let survey = harness.handler.survey(0).expect("a survey");
    harness.handler.apply_survey(0, &survey).expect("applied");

    let requests = harness.forge_requests();
    assert!(
        requests
            .iter()
            .all(|(method, path)| !(method == "POST" && path.ends_with("/projects"))),
        "the unlinked project must not be mirrored: {requests:?}"
    );
    assert!(
        requests
            .iter()
            .any(|(method, path)| method == "POST"
                && path == &format!("/api/v1/repos/{DEFAULT_SCOPE}/issues")),
        "its issue still lands in the mapping scope: {requests:?}"
    );
}
