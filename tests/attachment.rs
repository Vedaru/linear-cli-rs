//! `linear issue attachment` — reading back, correcting and removing a sidebar link.
//!
//! The create half (`issue attach`, `issue link`) is covered elsewhere; these pin the three
//! quarters the API had and the CLI did not, plus the two shape facts that decide how the writes
//! are built: `AttachmentUpdateInput.title` is required (so a subtitle-only edit still sends the
//! title it found) and an attachment's URL is its identity (so `--url` is a create-then-delete
//! re-link, proven here by the order the requests have to arrive in).

mod common;

use common::{run_cli, MockLinearServer, MockResponse};
use serde_json::{json, Value};

const ATTACHMENT_ID: &str = "att-11111111-2222-3333-4444-555555555555";

fn attachment(title: &str, url: &str) -> Value {
    json!({
        "id": ATTACHMENT_ID,
        "title": title,
        "subtitle": null,
        "url": url,
        "sourceType": null,
        "metadata": null,
        "createdAt": "2026-10-01T00:00:00.000Z",
        "updatedAt": "2026-10-01T00:00:00.000Z",
        "issue": { "id": "issue-uuid-1", "identifier": "ENG-1" }
    })
}

/// A reply to `attachmentUpdate`/`attachmentCreate` carrying the attachment it wrote.
fn written(mutation: &str, node: Value) -> Value {
    json!({ "data": { mutation: { "success": true, "attachment": node } } })
}

#[test]
fn attachment_list_prints_the_ids_the_other_commands_take() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "IssueAttachments",
        json!({ "data": { "issue": {
            "id": "issue-uuid-1",
            "identifier": "ENG-1",
            "attachments": {
                "nodes": [attachment("Build log", "https://ci.example.com/run/7")],
                "pageInfo": { "hasNextPage": false, "endCursor": null }
            }
        } } }),
    )
    // The query declares `$id: String!`; a mock that accepts any variables would pass even when
    // the CLI never sends it (which is how the command shipped broken - see VED-482).
    .with_variables(json!({ "id": "ENG-1" }))]);

    let out = run_cli(
        &["issue", "attachment", "list", "ENG-1"],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("Attachments on ENG-1"),
        "{}",
        out.stdout
    );
    assert!(out.stdout.contains(ATTACHMENT_ID), "{}", out.stdout);
    assert!(out.stdout.contains("Build log"), "{}", out.stdout);
    assert!(
        out.stdout.contains("https://ci.example.com/run/7"),
        "{}",
        out.stdout
    );

    let json_out = run_cli(
        &["issue", "attachment", "list", "ENG-1", "--json"],
        &common::mock_env(&server),
    );
    assert!(json_out.success(), "stderr: {}", json_out.stderr);
    let parsed: Value = serde_json::from_str(&json_out.stdout).expect("list --json is JSON");
    assert_eq!(parsed["nodes"][0]["id"], json!(ATTACHMENT_ID));
    assert_eq!(
        parsed["nodes"][0]["url"],
        json!("https://ci.example.com/run/7")
    );
}

#[test]
fn attachment_get_prints_one_attachment_and_its_issue() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "GetAttachment",
        json!({ "data": { "attachment": attachment("Build log", "https://ci.example.com/run/7") } }),
    )]);

    let out = run_cli(
        &["issue", "attachment", "get", ATTACHMENT_ID, "--json"],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    let parsed: Value = serde_json::from_str(&out.stdout).expect("get --json is JSON");
    assert_eq!(parsed["id"], json!(ATTACHMENT_ID));
    assert_eq!(parsed["issue"]["identifier"], json!("ENG-1"));
}

#[test]
fn attachment_update_sends_the_new_title_and_reports_the_change() {
    let server = MockLinearServer::start(vec![
        MockResponse::new(
            "GetAttachment",
            json!({ "data": { "attachment": attachment("Buid log", "https://ci.example.com/run/7") } }),
        ),
        MockResponse::new(
            "AttachmentUpdate",
            written(
                "attachmentUpdate",
                attachment("Build log", "https://ci.example.com/run/7"),
            ),
        )
        .with_variables(json!({
            "id": ATTACHMENT_ID,
            "input": { "title": "Build log" }
        })),
    ]);

    let out = run_cli(
        &[
            "issue",
            "attachment",
            "update",
            ATTACHMENT_ID,
            "--title",
            "Build log",
        ],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout
            .contains(&format!("✓ Updated attachment {ATTACHMENT_ID}")),
        "{}",
        out.stdout
    );
    assert!(
        out.stdout.contains("title: \"Buid log\" → \"Build log\""),
        "{}",
        out.stdout
    );
}

/// The update input's `title` is non-null in the schema, so a subtitle-only edit has to send the
/// title it read. The mock only answers when that title is on the request, which is how a
/// regression to a bare `{"subtitle": ...}` input fails here rather than on the live API.
#[test]
fn attachment_update_carries_the_existing_title_when_only_the_subtitle_changes() {
    let server = MockLinearServer::start(vec![
        MockResponse::new(
            "GetAttachment",
            json!({ "data": { "attachment": attachment("Build log", "https://ci.example.com/run/7") } }),
        ),
        MockResponse::new(
            "AttachmentUpdate",
            written(
                "attachmentUpdate",
                attachment("Build log", "https://ci.example.com/run/7"),
            ),
        )
        .with_variables(json!({
            "id": ATTACHMENT_ID,
            "input": { "title": "Build log", "subtitle": "run 7" }
        })),
    ]);

    let out = run_cli(
        &[
            "issue",
            "attachment",
            "update",
            ATTACHMENT_ID,
            "--subtitle",
            "run 7",
        ],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("subtitle: \"\" → \"run 7\""),
        "{}",
        out.stdout
    );
}

#[test]
fn attachment_update_with_nothing_to_change_is_refused() {
    let server = MockLinearServer::start(vec![]);

    let out = run_cli(
        &["issue", "attachment", "update", ATTACHMENT_ID],
        &common::mock_env(&server),
    );

    assert!(!out.success());
    assert!(
        out.stderr.contains("Nothing to update"),
        "stderr: {}",
        out.stderr
    );
}

#[test]
fn attachment_update_with_an_unchanged_value_is_refused_before_the_write() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "GetAttachment",
        json!({ "data": { "attachment": attachment("Build log", "https://ci.example.com/run/7") } }),
    )]);

    let out = run_cli(
        &[
            "issue",
            "attachment",
            "update",
            ATTACHMENT_ID,
            "--title",
            "Build log",
        ],
        &common::mock_env(&server),
    );

    assert!(!out.success());
    assert!(
        out.stderr.contains("already has every value given"),
        "stderr: {}",
        out.stderr
    );
}

/// A URL is an attachment's identity and the update input has no `url` field, so `--url` is a
/// re-link. The order is the point: the create must be attempted first, so a create that fails
/// leaves the link in place instead of deleting it and then discovering it cannot replace it.
/// Nothing answers `AttachmentDelete` here, so a delete sent first would surface as
/// "No mock response configured for this query" instead of the create's own failure.
#[test]
fn attachment_update_url_relinks_after_the_create_succeeds() {
    let server = MockLinearServer::start(vec![
        MockResponse::new(
            "GetAttachment",
            json!({ "data": { "attachment": attachment("Build log", "https://ci.example.com/run/7") } }),
        ),
        MockResponse::new(
            "AttachmentCreateForRelink",
            json!({ "errors": [{ "message": "create refused" }] }),
        ),
    ]);

    let out = run_cli(
        &[
            "issue",
            "attachment",
            "update",
            ATTACHMENT_ID,
            "--url",
            "https://ci.example.com/run/8",
        ],
        &common::mock_env(&server),
    );

    assert!(!out.success());
    assert!(
        out.stderr.contains("create refused"),
        "stderr: {}",
        out.stderr
    );
    assert!(
        !out.stderr.contains("AttachmentDelete"),
        "the old link was deleted before the new one existed: {}",
        out.stderr
    );
}

#[test]
fn attachment_update_url_reports_the_relink_when_both_writes_succeed() {
    let server = MockLinearServer::start(vec![
        MockResponse::new(
            "GetAttachment",
            json!({ "data": { "attachment": attachment("Build log", "https://ci.example.com/run/7") } }),
        ),
        MockResponse::new(
            "AttachmentCreateForRelink",
            written(
                "attachmentCreate",
                json!({
                    "id": "att-new",
                    "title": "Build log",
                    "subtitle": null,
                    "url": "https://ci.example.com/run/8",
                    "issue": { "identifier": "ENG-1" }
                }),
            ),
        )
        .with_variables(json!({
            "input": {
                "issueId": "issue-uuid-1",
                "url": "https://ci.example.com/run/8",
                "title": "Build log"
            }
        })),
        MockResponse::new(
            "AttachmentDelete",
            json!({ "data": { "attachmentDelete": { "success": true, "entityId": ATTACHMENT_ID } } }),
        )
        .with_variables(json!({ "id": ATTACHMENT_ID })),
    ]);

    let out = run_cli(
        &[
            "issue",
            "attachment",
            "update",
            ATTACHMENT_ID,
            "--url",
            "https://ci.example.com/run/8",
        ],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("✓ Re-linked ENG-1: att-new"),
        "{}",
        out.stdout
    );
    assert!(
        out.stdout
            .contains("url: \"https://ci.example.com/run/7\" → \"https://ci.example.com/run/8\""),
        "{}",
        out.stdout
    );
    assert!(
        out.stdout
            .contains(&format!("Deleted attachment {ATTACHMENT_ID}")),
        "{}",
        out.stdout
    );
}

#[test]
fn attachment_delete_requires_force_off_a_terminal() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "GetAttachment",
        json!({ "data": { "attachment": attachment("Build log", "https://ci.example.com/run/7") } }),
    )]);

    let out = run_cli(
        &["issue", "attachment", "delete", ATTACHMENT_ID],
        &common::mock_env(&server),
    );

    assert!(!out.success());
    assert!(
        out.stderr.contains("Interactive confirmation required"),
        "stderr: {}",
        out.stderr
    );
}

#[test]
fn attachment_delete_with_force_answers_json() {
    let server = MockLinearServer::start(vec![
        MockResponse::new(
            "GetAttachment",
            json!({ "data": { "attachment": attachment("Build log", "https://ci.example.com/run/7") } }),
        ),
        MockResponse::new(
            "AttachmentDelete",
            json!({ "data": { "attachmentDelete": { "success": true, "entityId": ATTACHMENT_ID } } }),
        )
        .with_variables(json!({ "id": ATTACHMENT_ID })),
    ]);

    let out = run_cli(
        &[
            "issue",
            "attachment",
            "delete",
            ATTACHMENT_ID,
            "--force",
            "--json",
        ],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    let parsed: Value = serde_json::from_str(&out.stdout).expect("delete --json is JSON");
    assert_eq!(parsed["attachmentDelete"]["success"], json!(true));
    assert_eq!(parsed["attachmentDelete"]["entityId"], json!(ATTACHMENT_ID));
}
