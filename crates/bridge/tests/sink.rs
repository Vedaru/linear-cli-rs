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

mod support;

use serde_json::{json, Value};

use linear_bridge::domain::{Change, IssueFields, Patch};
use linear_bridge::sink::Sink;
use linear_bridge::sources::presets;

use support::Fake;

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
        project: None,
    }
}

// ---------------------------------------------------------------------------
// A forge: REST, ids for labels, the state on the issue itself.

#[test]
fn an_emulated_due_date_survives_a_labels_only_update() {
    // The bug this pins: a labels-only patch has nothing to say about the due date, so a
    // sink that read the value out of the patch would send the label set *without* the
    // emulated label - and the target would lose a date nobody touched. On a platform that
    // carries the date in a label, writing the labels means writing the whole set.
    let fake = Fake::start_from("forgejo");
    let preset = presets::preset("forgejo").expect("the preset loads");
    let mut capabilities: linear_bridge::domain::Capabilities = preset.capabilities.into();
    capabilities.due_dates = false;
    let sink = fake.sink_with(
        "forgejo",
        preset.sink.clone().expect("forgejo writes"),
        capabilities,
    );

    // Only the labels move.
    let patch = Patch {
        labels: Change::Set(vec!["bug".into()]),
        ..Default::default()
    };
    let effective = IssueFields {
        labels: vec!["bug".into()],
        due_date: Some("2026-10-09".into()),
        ..Default::default()
    };
    sink.update_issue("Vedaru/linear-cli-rs", "12", &patch, &effective, None)
        .expect("update");

    // The label set the forge is asked to hold carries the date, resolved to the id the
    // platform knows it by.
    let sent = fake.only("PUT", "/api/v1/repos/Vedaru/linear-cli-rs/issues/12/labels");
    assert_eq!(
        sent.body["labels"],
        json!([3, 21]),
        "the emulated due date has to be in the set: {}",
        sent.body
    );
}

#[test]
fn a_forge_issue_is_created_with_the_ids_the_forge_wants() {
    let fake = Fake::start_from("forgejo");
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
fn a_patch_sends_only_what_it_changes() {
    let fake = Fake::start_from("forgejo");
    let sink = fake.sink("forgejo");

    // A patch that mentions the title and nothing else. The rest must not reach the
    // request: restating a field is how a mirror overwrites something it did not look
    // at, and the labels in particular are one field with the priority on a forge.
    let patch = Patch {
        title: Change::Set("Renamed".into()),
        ..Patch::default()
    };
    sink.update_issue("Vedaru/linear-cli-rs", "12", &patch, &fields(), None)
        .expect("update");

    let update = fake.only("PATCH", "/api/v1/repos/Vedaru/linear-cli-rs/issues/12");
    assert_eq!(update.body["title"], "Renamed");
    for absent in ["body", "labels", "due_date", "assignees", "state"] {
        assert!(
            !update
                .body
                .as_object()
                .expect("an object")
                .contains_key(absent),
            "`{absent}` was not changed, so it must not be sent: {}",
            update.body
        );
    }
    // No label write either: the spec declares a separate labels operation, and this
    // patch says nothing about labels.
    assert!(
        fake.seen().iter().all(|record| record.method != "PUT"),
        "a label write happened"
    );
}

#[test]
fn a_patch_clears_exactly_what_it_says_it_clears() {
    let fake = Fake::start_from("forgejo");
    let sink = fake.sink("forgejo");

    let patch = Patch {
        due_date: Change::Clear,
        assignee: Change::Clear,
        ..Patch::default()
    };
    sink.update_issue(
        "Vedaru/linear-cli-rs",
        "12",
        &patch,
        &fields(),
        Some("closed"),
    )
    .expect("update");

    let update = fake.only("PATCH", "/api/v1/repos/Vedaru/linear-cli-rs/issues/12");
    // A present null is the instruction "clear it" - the one thing that must not be
    // dropped from the request, or the two sides keep a value neither wants.
    assert_eq!(update.body["due_date"], Value::Null);
    assert_eq!(update.body["assignees"], json!([]));
    assert_eq!(update.body["state"], "closed");
    // Nothing else was touched.
    assert!(!update.body.as_object().unwrap().contains_key("title"));
    assert!(!update.body.as_object().unwrap().contains_key("body"));
}

#[test]
fn a_priority_change_rewrites_the_label_set_it_travels_in() {
    let fake = Fake::start_from("forgejo");
    let sink = fake.sink("forgejo");

    // On a platform with no priority field the priority *is* a label, so the label
    // set has to be restated whenever it changes - with the real labels kept.
    let patch = Patch {
        priority: Change::Set(2),
        labels: Change::Set(vec!["bug".into()]),
        ..Patch::default()
    };
    sink.update_issue("Vedaru/linear-cli-rs", "12", &patch, &fields(), None)
        .expect("update");

    let labels = fake.only("PUT", "/api/v1/repos/Vedaru/linear-cli-rs/issues/12/labels");
    // A forge takes label *ids*, so the names went through the same lookup the create
    // path uses - and the set is the real label plus the priority, not one or the
    // other.
    let sent: Vec<i64> = labels.body["labels"]
        .as_array()
        .expect("a list")
        .iter()
        .map(|value| value.as_i64().expect("an id"))
        .collect();
    assert_eq!(sent.len(), 2, "{sent:?}");
}

#[test]
fn a_sweep_reads_every_page_and_every_field() {
    let fake = Fake::start_from("forgejo");
    // A page size of two, so a three-issue scope exercises the walk without a test
    // fixture of fifty issues. The number is the preset's business, not the engine's.
    let forgejo = presets::preset("forgejo").expect("the preset");
    let capabilities: linear_bridge::domain::Capabilities =
        forgejo.capabilities.resolve(forgejo.sink.as_ref());
    let mut spec = forgejo.sink.clone().expect("the preset has a write half");
    spec.issue
        .list
        .as_mut()
        .expect("a list section")
        .paginate
        .as_mut()
        .expect("pagination")
        .page
        .as_mut()
        .expect("numbered pages")
        .size = 2;
    let sink = fake.sink_with("forgejo", spec, capabilities);

    let issues = sink.list_issues("Vedaru/linear-cli-rs").expect("a sweep");

    assert_eq!(issues.len(), 3, "every page was read");
    assert_eq!(issues[0].reference.id, "12");
    assert_eq!(issues[0].fields.title, "Mirror the thing");
    // The priority came back out of the label set, which is where a forge keeps it.
    assert_eq!(issues[0].fields.priority, 2);
    assert_eq!(issues[0].fields.labels, vec!["bug"]);
    assert_eq!(issues[0].fields.assignee.as_deref(), Some("vedaru"));
    assert_eq!(issues[0].fields.due_date.as_deref(), Some("2026-10-09"));
    assert_eq!(issues[0].state.as_deref(), Some("open"));
    // A forge spells "no due date" as a zero timestamp; the neutral model says None,
    // so a sweep does not report a due date that is not there.
    assert_eq!(issues[1].fields.due_date, None);
    assert_eq!(issues[2].reference.id, "14");

    // Two requests, not three: the short page ended the walk.
    let seen = fake.seen();
    let pages: Vec<&str> = seen
        .iter()
        .filter(|record| record.path.contains("/issues?"))
        .map(|record| record.path.as_str())
        .collect();
    assert_eq!(pages.len(), 2, "{pages:?}");
}

#[test]
fn a_sweep_follows_a_cursor_and_believes_has_next_page() {
    let fake = Fake::start(linear_routes);
    let sink = fake.sink("linear");

    let issues = sink.list_issues("VED").expect("a sweep");

    assert_eq!(issues.len(), 2);
    assert_eq!(issues[0].reference.id, "issue-1");
    assert_eq!(issues[0].fields.title, "First");
    assert_eq!(issues[0].fields.priority, 3);
    assert_eq!(issues[0].state.as_deref(), Some("Todo"));
    assert_eq!(issues[1].reference.id, "issue-2");
    // `priority: 0` is "no priority", and no due date is no due date.
    assert_eq!(issues[1].fields.priority, 0);
    assert_eq!(issues[1].fields.assignee, None);
    assert_eq!(issues[1].fields.due_date.as_deref(), Some("2026-11-01"));

    // Two requests: the last page also carried a cursor, and was believed when it
    // said there is no more.
    let seen = fake.seen();
    let pages: Vec<&str> = seen
        .iter()
        .filter(|record| {
            record
                .body
                .get("query")
                .and_then(Value::as_str)
                .is_some_and(|query| query.contains("query Issues"))
        })
        .map(|record| record.body["variables"]["after"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(pages, vec!["", "cursor-2"], "{pages:?}");
}

#[test]
fn a_platform_that_cannot_be_enumerated_says_so_by_name() {
    let fake = Fake::start_from("forgejo");
    let forgejo = presets::preset("forgejo").expect("the preset");
    let capabilities: linear_bridge::domain::Capabilities =
        forgejo.capabilities.resolve(forgejo.sink.as_ref());
    let mut spec = forgejo.sink.clone().expect("the preset has a write half");
    spec.issue.list = None;
    let sink = fake.sink_with("forgejo", spec, capabilities);

    // "There is nothing there" and "I cannot look" must not be the same answer: a
    // sweep that took an empty list for an empty scope would create everything again.
    let error = sink
        .list_issues("Vedaru/linear-cli-rs")
        .expect_err("no list operation");
    let message = error.to_string();
    assert!(message.contains("list"), "{message}");
    assert!(message.contains("forgejo"), "{message}");
}

#[test]
fn a_fetched_issue_comes_back_as_the_neutral_field_set() {
    let fake = Fake::start_from("forgejo");
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
    let fake = Fake::start_from("forgejo");
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
fn a_mirrored_comment_can_be_edited_and_removed() {
    let fake = Fake::start_from("forgejo");
    let sink = fake.sink("forgejo");

    // A comment is created through its issue...
    let created = sink
        .comment("Vedaru/linear-cli-rs", "12", "looks good")
        .expect("comment");
    assert_eq!(created.id, "77");

    // ...and everything after that is addressed by its own id, on its own path: an
    // edit that went to the issue's comment *collection* would post a second comment.
    sink.update_comment("Vedaru/linear-cli-rs", "77", "looks good (edited)")
        .expect("update");
    let patch = fake.only(
        "PATCH",
        "/api/v1/repos/Vedaru/linear-cli-rs/issues/comments/77",
    );
    assert_eq!(patch.body["body"], "looks good (edited)");

    sink.delete_comment("Vedaru/linear-cli-rs", "77")
        .expect("delete");
    fake.only(
        "DELETE",
        "/api/v1/repos/Vedaru/linear-cli-rs/issues/comments/77",
    );
}

#[test]
fn a_platform_that_cannot_edit_a_comment_says_so_by_name() {
    // The engine refuses an operation the spec does not declare rather than
    // inventing a request: a mapping that needs comment edits on a platform that
    // has none should hear about it, not silently post duplicates.
    let fake = Fake::start_from("forgejo");
    // The preset's write half, with the one operation taken away: a platform that
    // cannot edit a comment is a real case (an older API, a stricter token).
    let forgejo = presets::preset("forgejo").expect("the preset");
    let capabilities: linear_bridge::domain::Capabilities = forgejo.capabilities.into();
    let mut spec = forgejo.sink.expect("the preset has a write half");
    spec.issue
        .comment
        .as_mut()
        .expect("a comment section")
        .update = None;
    let sink = fake.sink_with("forgejo", spec, capabilities);

    let error = sink
        .update_comment("Vedaru/linear-cli-rs", "77", "edited")
        .expect_err("the spec declares no comment.update");
    let message = error.to_string();
    assert!(message.contains("comment.update"), "{message}");
    assert!(message.contains("forgejo"), "{message}");
}

#[test]
fn an_operation_the_preset_does_not_declare_is_refused_by_name() {
    let fake = Fake::start_from("forgejo");
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
        // A cursor-paged sweep: the second page reports `hasNextPage = false`, which
        // is what ends the walk - even though a cursor is still handed back, as some
        // APIs do on the last page.
        _ if query.contains("query Issues") => {
            let after = body["variables"]["after"].as_str().unwrap_or_default();
            assert_eq!(body["variables"]["teamId"], "uuid-team");
            if after.is_empty() {
                (
                    200,
                    json!({ "data": { "issues": {
                    "nodes": [
                        {
                            "id": "issue-1",
                            "url": "https://linear.app/vedaru/issue/VED-1",
                            "title": "First",
                            "description": "body",
                            "dueDate": null,
                            "priority": 3,
                            "state": { "name": "Todo" },
                            "labels": { "nodes": [{ "name": "Bug" }] },
                            "assignee": { "email": "vedaru@example.com" }
                        }
                    ],
                    "pageInfo": { "hasNextPage": true, "endCursor": "cursor-2" }
                } } }),
                )
            } else {
                assert_eq!(after, "cursor-2", "the cursor came from the previous page");
                (
                    200,
                    json!({ "data": { "issues": {
                    "nodes": [
                        {
                            "id": "issue-2",
                            "url": "https://linear.app/vedaru/issue/VED-2",
                            "title": "Second",
                            "description": "",
                            "dueDate": "2026-11-01",
                            "priority": 0,
                            "state": { "name": "Done" },
                            "labels": { "nodes": [] },
                            "assignee": null
                        }
                    ],
                    "pageInfo": { "hasNextPage": false, "endCursor": "cursor-3" }
                } } }),
                )
            }
        }
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
        project: None,
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
    let fake = Fake::start_from("forgejo");
    let sink = fake.sink("forgejo");

    // No route for this one: the fake answers 404, and the write path must not
    // treat that as "nothing to do".
    let error = sink
        .transition("Vedaru/linear-cli-rs", "999", "closed")
        .expect_err("404");
    let message = error.to_string();
    assert!(message.contains("404"), "{message}");
}
