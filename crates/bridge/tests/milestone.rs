//! A milestone, in both directions, against the real preset.
//!
//! VED-285 shipped the field, both read halves, both lookups and both writes; this is the
//! proof it asked for and did not have. The value of the feature is entirely in one detail:
//! a forge addresses a milestone by **id**, while both platforms *name* it - so the thing
//! worth pinning is that what goes on the wire is the resolved id and never the name, and
//! never a one-element list (the label shape, which is what a neighbouring lookup returns).
//!
//! The fake is a route table and a recording, as `tests/sink.rs` describes it: assertions
//! are about the request that was sent.

mod support;

use serde_json::{json, Value};

use linear_bridge::domain::{Change, IssueFields, Patch};
use linear_bridge::sink::Sink;
use support::Fake;

const SCOPE: &str = "Vedaru/linear-cli-rs";
const NAME: &str = "M8 - Memory and logic";

/// The forge's routes, with the milestone collection answering `found`.
///
/// `found` is what the repository already has: the honest case is one matching milestone
/// (so a lookup happens and only a lookup), and the empty one is a repository that has
/// never heard of the name the mirror was told to apply.
fn forgejo_routes(found: Value) -> impl Fn(&str, &str, &Value) -> (u16, Value) {
    move |method: &str, path: &str, _body: &Value| {
        if method == "GET" && path.ends_with("/milestones?state=all") {
            return (200, found.clone());
        }
        if method == "POST" && path.ends_with("/milestones") {
            // The lookup's `create`: the platform decides the id, and the sink has to use
            // the one that came back rather than the one it guessed.
            return (201, json!({ "id": 9, "title": NAME }));
        }
        if method == "POST" && path.ends_with("/issues") {
            return (201, json!({ "number": 12 }));
        }
        if method == "PATCH" && path.contains("/issues/") {
            return (200, json!({ "number": 12 }));
        }
        if method == "GET" && path.contains("/issues/") {
            return (
                200,
                json!({
                    "number": 12,
                    "title": "Mirror the thing",
                    "body": "why it matters",
                    "state": "open",
                    "milestone": { "id": 7, "title": NAME }
                }),
            );
        }
        (
            404,
            json!({ "message": format!("no route for {method} {path}") }),
        )
    }
}

fn with_milestone() -> IssueFields {
    IssueFields {
        title: "Mirror the thing".into(),
        milestone: Some(NAME.into()),
        ..Default::default()
    }
}

#[test]
fn a_milestone_on_the_forge_reads_back_as_the_neutral_field() {
    let fake = Fake::start(forgejo_routes(json!([])));
    let sink = fake.sink("forgejo");

    let remote = sink
        .fetch_issue(SCOPE, "12")
        .expect("fetch")
        .expect("the issue exists");
    assert_eq!(
        remote.fields.milestone.as_deref(),
        Some(NAME),
        "the preset reads `/milestone/title`; a forge answers with the whole object"
    );
}

#[test]
fn an_update_sends_the_resolved_id_and_not_the_name() {
    let fake = Fake::start(forgejo_routes(json!([{ "id": 7, "title": NAME }])));
    let sink = fake.sink("forgejo");

    let patch = Patch {
        milestone: Change::Set(NAME.into()),
        ..Default::default()
    };
    sink.update_issue(SCOPE, "12", &patch, &with_milestone(), None)
        .expect("update");

    let sent = fake.only("PATCH", "/api/v1/repos/Vedaru/linear-cli-rs/issues/12");
    assert_eq!(
        sent.body["milestone"],
        json!(7),
        "must be the id: a name is not something the API can act on, and `[7]` is the \
         label shape, which this is not. Sent: {}",
        sent.body
    );
}

#[test]
fn a_create_sends_it_too() {
    let fake = Fake::start(forgejo_routes(json!([{ "id": 7, "title": NAME }])));
    let sink = fake.sink("forgejo");

    sink.create_issue(SCOPE, &with_milestone(), None)
        .expect("create");

    let sent = fake.only("POST", "/api/v1/repos/Vedaru/linear-cli-rs/issues");
    assert_eq!(sent.body["milestone"], json!(7), "sent: {}", sent.body);
}

#[test]
fn a_milestone_the_repository_lacks_is_created_and_its_id_used() {
    // The preset declares `lookup.milestone.create`, for the same reason the label lookup
    // does: a repository that has never heard of the name is a repository the mirror has to
    // teach, not a write to drop. What matters is that the *created* id is what travels.
    let fake = Fake::start(forgejo_routes(json!([])));
    let sink = fake.sink("forgejo");

    let patch = Patch {
        milestone: Change::Set(NAME.into()),
        ..Default::default()
    };
    sink.update_issue(SCOPE, "12", &patch, &with_milestone(), None)
        .expect("update");

    let created = fake.only("POST", "/api/v1/repos/Vedaru/linear-cli-rs/milestones");
    assert_eq!(created.body["title"], json!(NAME), "the create names it");
    let sent = fake.only("PATCH", "/api/v1/repos/Vedaru/linear-cli-rs/issues/12");
    assert_eq!(
        sent.body["milestone"],
        json!(9),
        "the id the platform answered with, not the one the bridge would have guessed: {}",
        sent.body
    );
}

#[test]
fn a_cleared_milestone_is_sent_as_null_rather_than_omitted() {
    // Omitting it would mean "leave it alone", which is the one thing a clear is not.
    let fake = Fake::start(forgejo_routes(json!([{ "id": 7, "title": NAME }])));
    let sink = fake.sink("forgejo");

    let patch = Patch {
        milestone: Change::Set(String::new()),
        ..Default::default()
    };
    sink.update_issue(SCOPE, "12", &patch, &with_milestone(), None)
        .expect("update");

    let sent = fake.only("PATCH", "/api/v1/repos/Vedaru/linear-cli-rs/issues/12");
    assert_eq!(sent.body["milestone"], Value::Null, "sent: {}", sent.body);
}

// ---------------------------------------------------------------------------
// The other direction: Linear takes `projectMilestoneId`, and its milestones are
// queryable workspace-wide rather than through the team the other lookups use.

fn linear_routes(_method: &str, path: &str, body: &Value) -> (u16, Value) {
    assert_eq!(path, "/graphql", "Linear is one endpoint");
    let query = body["query"].as_str().unwrap_or_default();
    if query.contains("IssueUpdate") {
        return (
            200,
            json!({ "data": { "issueUpdate": { "success": true, "issue": { "id": "issue-1" } } } }),
        );
    }
    if query.contains("ProjectMilestones") {
        return (
            200,
            json!({ "data": { "projectMilestones": { "nodes": [{ "id": "ms-7", "name": NAME }] } } }),
        );
    }
    (
        404,
        json!({ "errors": [{ "message": format!("no route for: {query}") }] }),
    )
}

#[test]
fn linear_is_sent_the_resolved_milestone_uuid() {
    let fake = Fake::start(linear_routes);
    let sink = fake.sink("linear");

    let patch = Patch {
        milestone: Change::Set(NAME.into()),
        ..Default::default()
    };
    let mut effective = with_milestone();
    effective.title = "Mirror the thing".into();
    sink.update_issue("VED", "issue-1", &patch, &effective, None)
        .expect("update");

    let updates = fake.graphql("IssueUpdate");
    assert_eq!(updates.len(), 1, "one update, not a storm: {updates:?}");
    assert_eq!(
        updates[0].body["variables"]["input"]["projectMilestoneId"],
        json!("ms-7"),
        "Linear wants the milestone's id in the issue input, not its name: {}",
        updates[0].body
    );
    assert_eq!(
        fake.graphql("ProjectMilestones").len(),
        1,
        "the workspace-wide collection, resolved once"
    );
}
