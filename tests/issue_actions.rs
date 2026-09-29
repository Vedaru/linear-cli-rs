//! API-only actions on issues and their comments, run against the mock server:
//! `issue comment resolve` / `unresolve` (`commentResolve`, `commentUnresolve`)
//! and `issue subscribe` / `unsubscribe` (`issueSubscribe`, `issueUnsubscribe`).
//!
//! Upstream has none of the four: it can add a comment but not resolve the
//! thread, and it cannot follow an issue at all.

mod common;

use common::{run_cli, MockLinearServer, MockResponse};
use serde_json::json;

const COMMENT_ID: &str = "22222222-2222-4222-8222-222222222222";

#[test]
fn issue_comment_resolve_reports_the_thread_as_resolved() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "ResolveComment",
        json!({ "data": { "commentResolve": { "success": true } } }),
    )
    .with_variables(json!({ "id": COMMENT_ID }))]);

    let out = run_cli(
        &["issue", "comment", "resolve", COMMENT_ID],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    assert_eq!(out.stdout.trim(), "✓ Comment resolved");
}

#[test]
fn issue_comment_unresolve_reports_the_thread_as_unresolved() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "UnresolveComment",
        json!({ "data": { "commentUnresolve": { "success": true } } }),
    )
    .with_variables(json!({ "id": COMMENT_ID }))]);

    let out = run_cli(
        &["issue", "comment", "unresolve", COMMENT_ID],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    assert_eq!(out.stdout.trim(), "✓ Comment unresolved");
}

#[test]
fn issue_subscribe_names_the_issue_from_the_mutation_payload() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "SubscribeToIssue",
        json!({ "data": { "issueSubscribe": {
            "success": true,
            "issue": { "identifier": "ENG-9", "title": "Flaky upload" }
        } } }),
    )
    .with_variables(json!({ "id": "ENG-9" }))]);

    let out = run_cli(&["issue", "subscribe", "ENG-9"], &common::mock_env(&server));

    assert!(out.success(), "stderr: {}", out.stderr);
    assert_eq!(
        out.stdout.trim(),
        "✓ Subscribed to issue: ENG-9: Flaky upload"
    );
}

#[test]
fn issue_unsubscribe_names_the_issue_from_the_mutation_payload() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "UnsubscribeFromIssue",
        json!({ "data": { "issueUnsubscribe": {
            "success": true,
            "issue": { "identifier": "ENG-9", "title": "Flaky upload" }
        } } }),
    )
    .with_variables(json!({ "id": "ENG-9" }))]);

    let out = run_cli(
        &["issue", "unsubscribe", "ENG-9"],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    assert_eq!(
        out.stdout.trim(),
        "✓ Unsubscribed from issue: ENG-9: Flaky upload"
    );
}
