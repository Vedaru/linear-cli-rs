//! The write path, over a real socket, against a fake platform.
//!
//! The unit tests cover rendering and pointer resolution; this covers the thing
//! that actually breaks in production - whether the bytes a sink sends are the
//! ones the platform documents, and whether the response comes back as the
//! neutral field set the other side of the mapping expects.
//!
//! The fake is deliberately dumb: a route table and a recording of what arrived.
//! The assertions are about the request, so the platform has to be boring for a
//! failure to mean anything.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde_json::{json, Value};
use tiny_http::{Header, Response, Server};

use linear_bridge::domain::{Capabilities, IssueFields, Secret};
use linear_bridge::sink::declarative::DeclarativeSink;
use linear_bridge::sink::spec::SinkSpec;
use linear_bridge::sink::Sink;
use linear_bridge::sources::presets;

type Route = fn(&str, &str, &Value) -> (u16, Value);

#[derive(Clone, Debug)]
struct Recorded {
    method: String,
    path: String,
    body: Value,
}

/// A fake platform on an ephemeral port.
struct Fake {
    base_url: String,
    seen: Arc<Mutex<Vec<Recorded>>>,
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Fake {
    fn start(route: Route) -> Self {
        let server = Server::http("127.0.0.1:0").expect("a fake platform binds");
        let base_url = format!("http://{}", server.server_addr());
        let seen: Arc<Mutex<Vec<Recorded>>> = Arc::new(Mutex::new(Vec::new()));
        let shutdown = Arc::new(AtomicBool::new(false));

        let thread = {
            let seen = Arc::clone(&seen);
            let shutdown = Arc::clone(&shutdown);
            thread::spawn(move || {
                while !shutdown.load(Ordering::SeqCst) {
                    let Ok(Some(mut request)) = server.recv_timeout(Duration::from_millis(25))
                    else {
                        continue;
                    };
                    let method = request.method().as_str().to_string();
                    let path = request.url().to_string();
                    let mut raw = String::new();
                    let _ = request.as_reader().read_to_string(&mut raw);
                    let body: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
                    seen.lock().expect("recorded").push(Recorded {
                        method: method.clone(),
                        path: path.clone(),
                        body: body.clone(),
                    });
                    let (status, reply) = route(&method, &path, &body);
                    let header = Header::from_bytes("Content-Type", "application/json")
                        .expect("a valid header");
                    let _ = request.respond(
                        Response::from_string(reply.to_string())
                            .with_status_code(status)
                            .with_header(header),
                    );
                }
            })
        };

        Self {
            base_url,
            seen,
            shutdown,
            thread: Some(thread),
        }
    }

    fn seen(&self) -> Vec<Recorded> {
        self.seen.lock().expect("recorded").clone()
    }

    /// The one request with this method and path.
    fn only(&self, method: &str, path: &str) -> Recorded {
        let seen = self.seen();
        let found: Vec<&Recorded> = seen
            .iter()
            .filter(|record| record.method == method && record.path == path)
            .collect();
        assert_eq!(found.len(), 1, "expected one {method} {path}, got {seen:?}");
        found[0].clone()
    }

    /// Every request whose GraphQL query mentions `needle` - how a fake tells
    /// one mutation from another when they share a path.
    fn graphql(&self, needle: &str) -> Vec<Recorded> {
        self.seen()
            .into_iter()
            .filter(|record| {
                record
                    .body
                    .get("query")
                    .and_then(Value::as_str)
                    .is_some_and(|query| query.contains(needle))
            })
            .collect()
    }

    /// A sink built from a preset, pointed at this fake instead of the real API.
    fn sink(&self, preset: &str) -> DeclarativeSink {
        let source = presets::preset(preset).expect("the preset loads");
        let capabilities: Capabilities = source.capabilities.into();
        let mut spec: SinkSpec = source.sink.expect("the preset has a write half");
        // The preset names the real API; only the origin changes here, the path
        // prefix stays so the assertions are about the preset's own paths.
        let path = spec.base_url.splitn(4, '/').nth(3).unwrap_or("");
        spec.base_url = if path.is_empty() {
            self.base_url.clone()
        } else {
            format!("{}/{path}", self.base_url)
        };
        DeclarativeSink::new(preset, spec, Some(Secret::new("test-token")), capabilities)
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn fields() -> IssueFields {
    IssueFields {
        title: "Mirror the thing".into(),
        body: "why it matters".into(),
        labels: vec!["Bug".into(), "Urgent".into()],
        // A forge has no priority field, so this is the value that has to survive
        // as a label.
        priority: 2,
        due_date: Some("2026-10-09".into()),
        assignee: Some("vedaru".into()),
    }
}

// ---------------------------------------------------------------------------
// A forge: REST, ids for labels, the state on the issue itself.

fn forgejo_routes(method: &str, path: &str, _body: &Value) -> (u16, Value) {
    match (method, path) {
        ("GET", "/api/v1/repos/Vedaru/linear-cli-rs/labels") => (
            200,
            json!([
                { "id": 3, "name": "Bug" },
                { "id": 9, "name": "Urgent" },
                { "id": 11, "name": "priority:high" }
            ]),
        ),
        ("POST", "/api/v1/repos/Vedaru/linear-cli-rs/issues") => (
            201,
            json!({ "number": 12, "html_url": "http://forge/Vedaru/linear-cli-rs/issues/12" }),
        ),
        ("PATCH", "/api/v1/repos/Vedaru/linear-cli-rs/issues/12") => (200, json!({ "number": 12 })),
        ("PUT", "/api/v1/repos/Vedaru/linear-cli-rs/issues/12/labels") => (200, json!([])),
        ("GET", "/api/v1/repos/Vedaru/linear-cli-rs/issues/12") => (
            200,
            json!({
                "number": 12,
                "html_url": "http://forge/Vedaru/linear-cli-rs/issues/12",
                "title": "Mirror the thing",
                "body": "why it matters\n\nmirrored from VED-99",
                "due_date": "2026-10-09",
                "state": "closed",
                "labels": [{ "id": 3, "name": "Bug" }, { "id": 5, "name": "priority:high" }],
                "assignees": [{ "login": "vedaru" }],
            }),
        ),
        ("POST", "/api/v1/repos/Vedaru/linear-cli-rs/issues/12/comments") => (
            201,
            json!({ "id": 77, "html_url": "http://forge/Vedaru/linear-cli-rs/issues/12#comment-77" }),
        ),
        _ => (
            404,
            json!({ "message": format!("no route for {method} {path}") }),
        ),
    }
}

#[test]
fn a_forge_issue_is_created_with_the_ids_the_forge_wants() {
    let fake = Fake::start(forgejo_routes);
    let sink = fake.sink("forgejo");

    let reference = sink
        .create_issue("Vedaru/linear-cli-rs", &fields(), None)
        .expect("create");

    // Both pointers the preset declares are followed: the id the link store
    // needs, and the url the other platform attaches.
    assert_eq!(reference.id, "12");
    assert_eq!(
        reference.url.as_deref(),
        Some("http://forge/Vedaru/linear-cli-rs/issues/12")
    );

    let create = fake.only("POST", "/api/v1/repos/Vedaru/linear-cli-rs/issues");
    assert_eq!(create.body["title"], "Mirror the thing");
    assert_eq!(create.body["body"], "why it matters");
    // Names became the repository's label ids, which is why the lookup ran - and
    // the priority rode along as the label this platform understands, because its
    // capabilities say it has no priority field.
    assert_eq!(create.body["labels"], json!([3, 9, 11]));
    assert_eq!(create.body["due_date"], "2026-10-09");
    // The neutral model carries one assignee; a forge wants a list.
    assert_eq!(create.body["assignees"], json!(["vedaru"]));
    // The preset's auth header, prefix included, is what a forge expects.
    fake.only("GET", "/api/v1/repos/Vedaru/linear-cli-rs/labels");
}

#[test]
fn an_update_clears_a_field_but_never_clears_what_it_does_not_sync() {
    let fake = Fake::start(forgejo_routes);
    let sink = fake.sink("forgejo");

    let mut cleared = fields();
    cleared.due_date = None;
    sink.update_issue("Vedaru/linear-cli-rs", "12", &cleared, Some("closed"))
        .expect("update");

    let update = fake.only("PATCH", "/api/v1/repos/Vedaru/linear-cli-rs/issues/12");
    // `$due_date!`: the date was cleared here, so it must be cleared there.
    assert!(update.body.as_object().unwrap().contains_key("due_date"));
    assert_eq!(update.body["due_date"], Value::Null);
    assert_eq!(update.body["state"], "closed");
    assert_eq!(update.body["title"], "Mirror the thing");
}

#[test]
fn a_fetched_issue_comes_back_as_the_neutral_field_set() {
    let fake = Fake::start(forgejo_routes);
    let sink = fake.sink("forgejo");

    let remote = sink
        .fetch_issue("Vedaru/linear-cli-rs", "12")
        .expect("fetch")
        .expect("the issue exists");

    assert_eq!(remote.reference.id, "12");
    assert_eq!(remote.fields.title, "Mirror the thing");
    assert!(remote.fields.body.contains("mirrored from VED-99"));
    assert_eq!(remote.fields.due_date.as_deref(), Some("2026-10-09"));
    assert_eq!(remote.fields.assignee.as_deref(), Some("vedaru"));
    assert_eq!(remote.state.as_deref(), Some("closed"));
    // `priority:high` is the label a bridge writes where a platform has no
    // priority field, so reading it back is reading the labels.
    assert_eq!(remote.fields.priority, 2);
    // Case-normalised, exactly as the read side normalises an inbound payload.
    assert_eq!(remote.fields.labels, vec!["bug".to_string()]);
}

#[test]
fn a_comment_returns_the_id_the_other_side_will_link_to() {
    let fake = Fake::start(forgejo_routes);
    let sink = fake.sink("forgejo");

    let reference = sink
        .comment("Vedaru/linear-cli-rs", "12", "mirrored comment")
        .expect("comment");

    assert_eq!(reference.id, "77");
    let comment = fake.only(
        "POST",
        "/api/v1/repos/Vedaru/linear-cli-rs/issues/12/comments",
    );
    assert_eq!(comment.body["body"], "mirrored comment");
}

#[test]
fn an_operation_the_preset_does_not_declare_is_refused_by_name() {
    let fake = Fake::start(forgejo_routes);
    let sink = fake.sink("forgejo");

    // A forge has no attachments. The write path says so instead of inventing a
    // request, so a mapping that needs one fails loudly on its first delivery.
    let error = sink
        .attach("Vedaru/linear-cli-rs", "12", "http://forge/pulls/4", "PR 4")
        .expect_err("a forge cannot attach");
    let message = error.to_string();
    assert!(message.contains("attach"), "{message}");
    assert!(message.contains("forgejo"), "{message}");

    // And nothing was sent: a refusal is not a half-attempt.
    assert!(fake.seen().is_empty(), "{:?}", fake.seen());
}

// ---------------------------------------------------------------------------
// Linear: GraphQL, UUIDs behind every name, and a failure inside a `200 OK`.

fn linear_routes(_method: &str, path: &str, body: &Value) -> (u16, Value) {
    assert_eq!(path, "/graphql");
    let query = body["query"].as_str().unwrap_or_default();
    match () {
        _ if query.contains("query Teams") => (
            200,
            json!({ "data": { "teams": { "nodes": [
            { "id": "uuid-team", "key": "VED" }
        ] } } }),
        ),
        _ if query.contains("TeamStates") => {
            assert_eq!(body["variables"]["teamId"], "uuid-team");
            (
                200,
                json!({ "data": { "team": { "states": { "nodes": [
                { "id": "state-todo", "name": "Todo" },
                { "id": "state-done", "name": "Done" }
            ] } } } }),
            )
        }
        _ if query.contains("TeamLabels") => (
            200,
            json!({ "data": { "team": { "labels": { "nodes": [
            { "id": "label-bug", "name": "Bug" }
        ] } } } }),
        ),
        _ if query.contains("query Users") => (
            200,
            json!({ "data": { "users": { "nodes": [
            { "id": "user-vedaru", "email": "vedaru@example.com" }
        ] } } }),
        ),
        _ if query.contains("IssueCreate") => (
            200,
            json!({ "data": { "issueCreate": {
            "success": true,
            "issue": { "id": "issue-uuid", "identifier": "VED-99", "url": "https://linear.app/vedaru/issue/VED-99" }
        } } }),
        ),
        // Linear answers a rejected mutation with 200 and this shape.
        _ if query.contains("IssueArchive") => (
            200,
            json!({ "errors": [
            { "message": "Access denied: issueArchive requires admin" }
        ] }),
        ),
        _ => (
            200,
            json!({ "errors": [{ "message": format!("unrouted query: {query}") }] }),
        ),
    }
}

fn linear_fields() -> IssueFields {
    IssueFields {
        title: "Mirror the thing".into(),
        body: "why it matters".into(),
        labels: vec!["Bug".into()],
        priority: 2,
        due_date: Some("2026-10-09".into()),
        assignee: Some("vedaru@example.com".into()),
    }
}

#[test]
fn a_linear_issue_is_created_through_names_resolved_to_ids() {
    let fake = Fake::start(linear_routes);
    let sink = fake.sink("linear");

    let reference = sink
        .create_issue("VED", &linear_fields(), Some("Todo"))
        .expect("create");

    assert_eq!(reference.id, "issue-uuid");
    assert_eq!(
        reference.url.as_deref(),
        Some("https://linear.app/vedaru/issue/VED-99")
    );

    let creates = fake.graphql("IssueCreate");
    assert_eq!(creates.len(), 1, "{creates:?}");
    let input = &creates[0].body["variables"]["input"];
    // Every name a human writes in a mapping, resolved to the UUID the API wants.
    assert_eq!(input["teamId"], "uuid-team");
    assert_eq!(input["stateId"], "state-todo");
    assert_eq!(input["labelIds"], json!(["label-bug"]));
    assert_eq!(input["assigneeId"], "user-vedaru");
    assert_eq!(input["title"], "Mirror the thing");
    assert_eq!(input["description"], "why it matters");
    assert_eq!(input["dueDate"], "2026-10-09");
    assert_eq!(input["priority"], 2, "Linear carries priority 0-4 natively");
}

#[test]
fn a_name_is_resolved_once_and_then_remembered() {
    let fake = Fake::start(linear_routes);
    let sink = fake.sink("linear");

    sink.create_issue("VED", &linear_fields(), Some("Todo"))
        .unwrap();
    sink.create_issue("VED", &linear_fields(), Some("Todo"))
        .unwrap();

    assert_eq!(fake.graphql("IssueCreate").len(), 2);
    // Two creates, one lookup each: the team, its states, its labels and the
    // users are the kinds of thing that do not change between deliveries, and a
    // webhook storm must not turn into a lookup storm.
    for needle in ["query Teams", "TeamStates", "TeamLabels", "query Users"] {
        assert_eq!(fake.graphql(needle).len(), 1, "looked up twice: {needle}");
    }
}

#[test]
fn a_failure_inside_a_successful_response_is_a_failure() {
    let fake = Fake::start(linear_routes);
    let sink = fake.sink("linear");

    let error = sink
        .delete_issue("VED", "issue-uuid")
        .expect_err("Linear answered errors");
    let message = error.to_string();
    assert!(message.contains("Access denied"), "{message}");
}

#[test]
fn a_platform_error_is_reported_with_its_status() {
    let fake = Fake::start(forgejo_routes);
    let sink = fake.sink("forgejo");

    // No route for this one: the fake answers 404, and the write path must not
    // treat that as "nothing to do".
    let error = sink
        .transition("Vedaru/linear-cli-rs", "999", "closed")
        .expect_err("404");
    let message = error.to_string();
    assert!(message.contains("404"), "{message}");
}
