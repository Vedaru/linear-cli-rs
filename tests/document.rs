//! End-to-end tests for the `linear document` group (list/view/create/
//! update/delete and the nested `comment add`/`comment list`), run against the
//! headless mock server.

mod common;

use common::{run_cli, run_cli_full, MockLinearServer, MockResponse};
use serde_json::json;

const NO_CREDENTIAL_VARS: &[&str] = &["LINEAR_API_KEY", "LINEAR_TEAM_ID"];

const DOCUMENT_NODE: fn(&str, &str) -> serde_json::Value = |slug, title| {
    json!({
        "id": format!("id-{slug}"),
        "title": title,
        "slugId": slug,
        "url": format!("https://linear.app/acme/document/{slug}"),
        "createdAt": "2024-01-01T00:00:00.000Z",
        "updatedAt": "2024-01-02T00:00:00.000Z",
        "project": { "name": "Launch", "slugId": "launch" },
        "issue": null,
        "initiative": null,
        "team": null,
        "cycle": null,
        "release": null,
        "creator": { "name": "Ada" }
    })
};

#[test]
fn document_list_renders_slug_title_and_attachment() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "ListDocuments",
        json!({ "data": { "documents": {
            "nodes": [DOCUMENT_NODE("launch-spec", "Launch spec")],
            "pageInfo": { "hasNextPage": false, "endCursor": null }
        } } }),
    )]);

    let out = run_cli(&["document", "list"], &common::mock_env(&server));
    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(out.stdout.contains("SLUG"), "stdout: {}", out.stdout);
    assert!(out.stdout.contains("TITLE"), "stdout: {}", out.stdout);
    assert!(out.stdout.contains("ATTACHMENT"), "stdout: {}", out.stdout);
    assert!(out.stdout.contains("launch-spec"), "stdout: {}", out.stdout);
    assert!(out.stdout.contains("Launch spec"), "stdout: {}", out.stdout);
    assert!(
        out.stdout.contains("Project: Launch"),
        "stdout: {}",
        out.stdout
    );
}

#[test]
fn document_list_json_returns_connection() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "ListDocuments",
        json!({ "data": { "documents": {
            "nodes": [DOCUMENT_NODE("launch-spec", "Launch spec")],
            "pageInfo": { "hasNextPage": false, "endCursor": null }
        } } }),
    )]);

    let out = run_cli(&["document", "list", "--json"], &common::mock_env(&server));
    assert!(out.success(), "stderr: {}", out.stderr);
    let parsed: serde_json::Value = serde_json::from_str(&out.stdout).expect("json output");
    assert_eq!(parsed["nodes"][0]["slugId"], "launch-spec");
    assert_eq!(parsed["pageInfo"]["hasNextPage"], false);
}

#[test]
fn document_list_rejects_two_target_flags() {
    let server = MockLinearServer::start(vec![]);
    let out = run_cli(
        &["document", "list", "--project", "P", "--issue", "ENG-1"],
        &common::mock_env(&server),
    );
    assert!(!out.success());
    assert!(
        out.stderr.contains("Only one attachment target may be set"),
        "stderr: {}",
        out.stderr
    );
}

#[test]
fn document_create_with_team_target_posts_create_input() {
    let server = MockLinearServer::start(vec![
        MockResponse::new(
            "FindTeam",
            json!({ "data": { "teams": { "nodes": [{
                "id": common::ENG_TEAM_ID,
                "key": common::ENG_TEAM_KEY,
                "name": common::ENG_TEAM_NAME
            }] } } }),
        )
        .with_variables(json!({ "reference": common::ENG_TEAM_KEY })),
        MockResponse::new(
            "CreateDocument",
            json!({ "data": { "documentCreate": {
                "success": true,
                "document": {
                    "id": "doc-1",
                    "slugId": "doc-1",
                    "title": "Runbook",
                    "url": "https://linear.app/acme/document/runbook"
                }
            } } }),
        )
        .with_variables(json!({
            "input": { "title": "Runbook", "teamId": common::ENG_TEAM_ID }
        })),
    ]);

    let out = run_cli(
        &["document", "create", "--title", "Runbook", "--team", "ENG"],
        &common::mock_env(&server),
    );
    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("✓ Created document: Runbook"),
        "stdout: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("https://linear.app/acme/document/runbook"),
        "stdout: {}",
        out.stdout
    );
}

#[test]
fn document_create_requires_a_target() {
    let server = MockLinearServer::start(vec![]);
    let out = run_cli(
        &["document", "create", "--title", "Runbook"],
        &common::mock_env(&server),
    );
    assert!(!out.success());
    assert!(
        out.stderr.contains("A document attachment target is required"),
        "stderr: {}",
        out.stderr
    );
}

#[test]
fn document_view_raw_prints_markdown_content() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "GetDocument",
        json!({ "data": { "document": {
            "id": "doc-1",
            "title": "Runbook",
            "slugId": "runbook",
            "content": "Hello **world**",
            "url": "https://linear.app/acme/document/runbook",
            "createdAt": "2024-01-01T00:00:00.000Z",
            "updatedAt": "2024-01-02T00:00:00.000Z",
            "creator": { "name": "Ada", "email": "ada@example.com" },
            "project": null, "issue": null, "initiative": null,
            "team": null, "cycle": null, "release": null
        } } }),
    )]);

    // stdout is piped (not a TTY), so view short-circuits to the raw content.
    let out = run_cli(&["document", "view", "doc-1"], &common::mock_env(&server));
    assert!(out.success(), "stderr: {}", out.stderr);
    assert_eq!(out.stdout.trim(), "Hello **world**");
}

#[test]
fn document_view_missing_reports_against_raw_id() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "GetDocument",
        json!({ "data": { "document": null } }),
    )]);

    let out = run_cli(&["document", "view", "nope"], &common::mock_env(&server));
    assert!(!out.success());
    assert!(
        out.stderr.contains("Failed to view document"),
        "stderr: {}",
        out.stderr
    );
    assert!(
        out.stderr.contains("Document not found: nope"),
        "stderr: {}",
        out.stderr
    );
}

#[test]
fn document_comment_add_resolves_content_id_and_creates_comment() {
    let server = MockLinearServer::start(vec![
        MockResponse::new(
            "GetDocumentCommentTarget",
            json!({ "data": { "document": {
                "id": "doc-1",
                "title": "Runbook",
                "documentContentId": "dc-1"
            } } }),
        ),
        MockResponse::new(
            "AddComment",
            json!({ "data": { "commentCreate": {
                "success": true,
                "comment": { "id": "c1", "url": "https://linear.app/acme/comment/c1" }
            } } }),
        )
        .with_variables(json!({
            "input": { "body": "looks good", "documentContentId": "dc-1" }
        })),
    ]);

    let out = run_cli(
        &["document", "comment", "add", "doc-1", "-b", "looks good"],
        &common::mock_env(&server),
    );
    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("✓ Comment added to document doc-1"),
        "stdout: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("https://linear.app/acme/comment/c1"),
        "stdout: {}",
        out.stdout
    );
}

#[test]
fn document_comment_add_without_content_record_explains_why() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "GetDocumentCommentTarget",
        json!({ "data": { "document": {
            "id": "doc-1",
            "title": "Runbook",
            "documentContentId": null
        } } }),
    )]);

    let out = run_cli(
        &["document", "comment", "add", "doc-1", "-b", "hi"],
        &common::mock_env(&server),
    );
    assert!(!out.success());
    assert!(
        out.stderr
            .contains("has no content record to comment on"),
        "stderr: {}",
        out.stderr
    );
}

#[test]
fn document_comment_list_renders_threads() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "GetDocumentComments",
        json!({ "data": { "document": {
            "id": "doc-1",
            "comments": {
                "nodes": [{
                    "id": "c1",
                    "body": "First thought",
                    "quotedText": null,
                    "createdAt": "2024-01-01T00:00:00.000Z",
                    "updatedAt": "2024-01-01T00:00:00.000Z",
                    "editedAt": null,
                    "url": "https://linear.app/acme/comment/c1",
                    "user": { "id": "u1", "name": "Ada", "displayName": "Ada" },
                    "externalUser": null,
                    "botActor": null,
                    "parent": null
                }],
                "pageInfo": { "hasNextPage": false, "endCursor": null }
            }
        } } }),
    )]);

    let out = run_cli(
        &["document", "comment", "list", "doc-1"],
        &common::mock_env(&server),
    );
    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("@Ada commented"),
        "stdout: {}",
        out.stdout
    );
    assert!(out.stdout.contains("First thought"), "stdout: {}", out.stdout);
}

#[test]
fn document_comment_list_empty_prints_notice() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "GetDocumentComments",
        json!({ "data": { "document": {
            "id": "doc-1",
            "comments": { "nodes": [], "pageInfo": { "hasNextPage": false, "endCursor": null } }
        } } }),
    )]);

    let out = run_cli(
        &["document", "comment", "list", "doc-1"],
        &common::mock_env(&server),
    );
    assert!(out.success(), "stderr: {}", out.stderr);
    assert_eq!(out.stdout.trim(), "No comments found for this document");
}

#[test]
fn document_delete_without_yes_requires_confirmation_in_headless_run() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "GetDocumentForDelete",
        json!({ "data": { "document": {
            "id": "doc-1",
            "slugId": "runbook",
            "title": "Runbook"
        } } }),
    )]);

    let out = run_cli_full(
        &["document", "delete", "doc-1"],
        &common::mock_env(&server),
        NO_CREDENTIAL_VARS,
        None,
    );
    assert!(!out.success());
    assert!(
        out.stderr.contains("Failed to delete document"),
        "stderr: {}",
        out.stderr
    );
    assert!(
        out.stderr.contains("Interactive confirmation required"),
        "stderr: {}",
        out.stderr
    );
}

#[test]
fn document_delete_with_yes_deletes() {
    let server = MockLinearServer::start(vec![
        MockResponse::new(
            "GetDocumentForDelete",
            json!({ "data": { "document": {
                "id": "doc-1",
                "slugId": "runbook",
                "title": "Runbook"
            } } }),
        ),
        MockResponse::new(
            "DeleteDocument",
            json!({ "data": { "documentDelete": { "success": true } } }),
        )
        .with_variables(json!({ "id": "doc-1" })),
    ]);

    let out = run_cli(
        &["document", "delete", "doc-1", "--yes"],
        &common::mock_env(&server),
    );
    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("✓ Deleted document: Runbook"),
        "stdout: {}",
        out.stdout
    );
}
