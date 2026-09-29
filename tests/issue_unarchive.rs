//! `linear issue unarchive`, run against the headless mock server.
//!
//! The command exists for an API capability neither upstream nor Linear's own
//! clients expose: `issueUnarchive`, the inverse of `issue archive` and of
//! `issue delete`. Linear represents both an archived issue and a deleted
//! (trashed) one with `archivedAt` + `trashed`, so the tests above pin the two
//! states separately, plus the no-op case and the bulk path.

mod common;

use common::{run_cli, MockLinearServer, MockResponse};
use serde_json::json;

const DETAILS_QUERY: &str = "GetIssueUnarchiveDetails";
const BULK_DETAILS_QUERY: &str = "GetIssueDetailsForBulkUnarchive";

fn issue_details(
    identifier: &str,
    title: &str,
    archived_at: Option<&str>,
    trashed: bool,
) -> serde_json::Value {
    json!({ "data": { "issue": {
        "identifier": identifier,
        "title": title,
        "archivedAt": archived_at,
        "trashed": trashed
    } } })
}

#[test]
fn issue_unarchive_restores_a_trashed_issue() {
    let server = MockLinearServer::start(vec![
        MockResponse::new(
            DETAILS_QUERY,
            issue_details(
                "ENG-9",
                "Trashed issue",
                Some("2024-01-01T00:00:00.000Z"),
                true,
            ),
        )
        .with_variables(json!({ "id": "ENG-9" })),
        MockResponse::new(
            "UnarchiveIssue",
            json!({ "data": { "issueUnarchive": { "success": true } } }),
        )
        .with_variables(json!({ "id": "ENG-9" })),
    ]);

    let out = run_cli(
        &["issue", "unarchive", "ENG-9", "--confirm"],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout
            .contains("✓ Successfully unarchived issue: ENG-9: Trashed issue"),
        "stdout: {}",
        out.stdout
    );
}

/// An archived issue carries no `trashed` flag at all; it is still restorable.
#[test]
fn issue_unarchive_restores_an_archived_issue() {
    let server = MockLinearServer::start(vec![
        MockResponse::new(
            DETAILS_QUERY,
            issue_details(
                "ENG-9",
                "Archived issue",
                Some("2024-01-01T00:00:00.000Z"),
                false,
            ),
        )
        .with_variables(json!({ "id": "ENG-9" })),
        MockResponse::new(
            "UnarchiveIssue",
            json!({ "data": { "issueUnarchive": { "success": true } } }),
        )
        .with_variables(json!({ "id": "ENG-9" })),
    ]);

    let out = run_cli(
        &["issue", "unarchive", "ENG-9", "--confirm"],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("✓ Successfully unarchived issue"),
        "stdout: {}",
        out.stdout
    );
}

/// Nothing to restore: reported, not prompted for, and no mutation is sent —
/// there is no `UnarchiveIssue` mock to answer one.
#[test]
fn issue_unarchive_reports_a_live_issue_without_prompting() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        DETAILS_QUERY,
        issue_details("ENG-9", "Live issue", None, false),
    )
    .with_variables(json!({ "id": "ENG-9" }))]);

    let out = run_cli(
        &["issue", "unarchive", "ENG-9", "--confirm"],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    assert_eq!(
        out.stdout.trim(),
        "Issue \"ENG-9: Live issue\" is not archived."
    );
}

#[test]
fn issue_unarchive_unknown_issue_is_not_found() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        DETAILS_QUERY,
        json!({ "data": { "issue": null } }),
    )
    .with_variables(json!({ "id": "ENG-404" }))]);

    let out = run_cli(
        &["issue", "unarchive", "ENG-404", "--confirm"],
        &common::mock_env(&server),
    );

    assert!(!out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("Issue not found: ENG-404"),
        "stderr: {}",
        out.stderr
    );
}

/// Bulk restores every identifier given, one trashed and one archived.
#[test]
fn issue_unarchive_bulk_restores_every_identifier() {
    let server = MockLinearServer::start(vec![
        MockResponse::new(
            BULK_DETAILS_QUERY,
            issue_details(
                "ENG-1",
                "Trashed one",
                Some("2024-01-01T00:00:00.000Z"),
                true,
            ),
        )
        .with_variables(json!({ "id": "ENG-1" })),
        MockResponse::new(
            BULK_DETAILS_QUERY,
            issue_details(
                "ENG-2",
                "Archived two",
                Some("2024-02-01T00:00:00.000Z"),
                false,
            ),
        )
        .with_variables(json!({ "id": "ENG-2" })),
        MockResponse::new(
            "BulkUnarchiveIssue",
            json!({ "data": { "issueUnarchive": { "success": true } } }),
        )
        .with_variables(json!({ "id": "ENG-1" })),
        MockResponse::new(
            "BulkUnarchiveIssue",
            json!({ "data": { "issueUnarchive": { "success": true } } }),
        )
        .with_variables(json!({ "id": "ENG-2" })),
    ]);

    let out = run_cli(
        &[
            "issue",
            "unarchive",
            "--bulk",
            "ENG-1",
            "ENG-2",
            "--confirm",
        ],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("Found 2 issue(s) to unarchive."),
        "stdout: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("✓ Successfully unarchived 2 issues"),
        "stdout: {}",
        out.stdout
    );
}

/// A positional ID together with `--bulk` is rejected, as in `issue archive`.
#[test]
fn issue_unarchive_rejects_positional_id_with_bulk() {
    let server = MockLinearServer::start(vec![]);

    let out = run_cli(
        &[
            "issue",
            "unarchive",
            "ENG-1",
            "--bulk",
            "ENG-2",
            "--confirm",
        ],
        &common::mock_env(&server),
    );

    assert!(!out.success(), "stdout: {}", out.stdout);
    assert!(
        out.stderr
            .contains("Cannot combine a positional issue ID with --bulk"),
        "stderr: {}",
        out.stderr
    );
}
