//! The shared bulk path (`src/bulk.rs`) as three different command groups reach
//! it: `issue archive`, `document delete`, and `initiative archive`.
//!
//! Upstream keeps one `src/utils/bulk.ts`; the port had grown a copy per command
//! (and per group) until they were folded back into one module. These tests pin
//! the parts an agent actually reads — the `Found N ...` preamble, the summary
//! line, and the per-id failure rows — so a regression in the shared code is
//! caught from more than one caller.

mod common;

use common::{run_cli, MockLinearServer, MockResponse};
use serde_json::json;

const ISSUE_DETAILS: &str = "GetIssueDetailsForBulkArchive";
const DOCUMENT_DETAILS: &str = "GetDocumentForBulkDelete";
const INITIATIVE_DETAILS: &str = "GetInitiativeNameForBulkArchive";

#[test]
fn issue_archive_bulk_reports_the_summary() {
    let server = MockLinearServer::start(vec![
        MockResponse::new(
            ISSUE_DETAILS,
            json!({ "data": { "issue": { "identifier": "ENG-1", "title": "First", "archivedAt": null } } }),
        )
        .with_variables(json!({ "id": "ENG-1" })),
        MockResponse::new(
            ISSUE_DETAILS,
            json!({ "data": { "issue": { "identifier": "ENG-2", "title": "Second", "archivedAt": null } } }),
        )
        .with_variables(json!({ "id": "ENG-2" })),
        MockResponse::new(
            "BulkArchiveIssue",
            json!({ "data": { "issueArchive": { "success": true } } }),
        )
        .with_variables(json!({ "id": "ENG-1" })),
        MockResponse::new(
            "BulkArchiveIssue",
            json!({ "data": { "issueArchive": { "success": true } } }),
        )
        .with_variables(json!({ "id": "ENG-2" })),
    ]);

    let out = run_cli(
        &["issue", "archive", "--bulk", "ENG-1", "ENG-2", "--confirm"],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("Found 2 issue(s) to archive."),
        "stdout: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("✓ Successfully archived 2 issues"),
        "stdout: {}",
        out.stdout
    );
}

/// A not-found identifier is a failed row, not an aborted run: the other ids are
/// still archived and the summary names the failure.
#[test]
fn issue_archive_bulk_reports_a_missing_issue_per_row() {
    let server = MockLinearServer::start(vec![
        MockResponse::new(
            ISSUE_DETAILS,
            json!({ "data": { "issue": { "identifier": "ENG-1", "title": "First", "archivedAt": null } } }),
        )
        .with_variables(json!({ "id": "ENG-1" })),
        MockResponse::new("BulkArchiveIssue", json!({ "data": { "issueArchive": { "success": true } } }))
            .with_variables(json!({ "id": "ENG-1" })),
    ]);

    let out = run_cli(
        &[
            "issue",
            "archive",
            "--bulk",
            "ENG-1",
            "ENG-404",
            "--confirm",
        ],
        &common::mock_env(&server),
    );

    assert!(!out.success(), "a failed row exits non-zero");
    assert!(
        out.stdout.contains("Completed: 1/2 issues archived"),
        "stdout: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("Failed operations:"),
        "stdout: {}",
        out.stdout
    );
    assert!(out.stdout.contains("ENG-404"), "stdout: {}", out.stdout);
}

#[test]
fn document_delete_bulk_reports_the_summary() {
    let server = MockLinearServer::start(vec![
        MockResponse::new(
            DOCUMENT_DETAILS,
            json!({ "data": { "document": { "id": "doc-1", "title": "Spec" } } }),
        )
        .with_variables(json!({ "id": "spec" })),
        MockResponse::new(
            "BulkDeleteDocument",
            json!({ "data": { "documentDelete": { "success": true } } }),
        )
        .with_variables(json!({ "id": "doc-1" })),
    ]);

    let out = run_cli(
        &["document", "delete", "--bulk", "spec", "--yes"],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("Found 1 document(s) to delete."),
        "stdout: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("✓ Successfully deleted 1 document"),
        "stdout: {}",
        out.stdout
    );
}

#[test]
fn initiative_archive_bulk_reads_ids_from_stdin() {
    let server = MockLinearServer::start(vec![
        MockResponse::new(
            INITIATIVE_DETAILS,
            json!({ "data": { "initiative": { "id": "11111111-1111-1111-1111-111111111111", "name": "Mobile" } } }),
        )
        .with_variables(json!({ "id": "11111111-1111-1111-1111-111111111111" })),
        MockResponse::new(
            "BulkArchiveInitiative",
            json!({ "data": { "initiativeArchive": { "success": true } } }),
        )
        .with_variables(json!({ "id": "11111111-1111-1111-1111-111111111111" })),
    ]);

    let out = common::run_cli_stdin(
        &["initiative", "archive", "--bulk-stdin", "--force"],
        &common::mock_env(&server),
        Some("11111111-1111-1111-1111-111111111111\n"),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("✓ Successfully archived 1 initiative"),
        "stdout: {:?} stderr: {:?}",
        out.stdout,
        out.stderr
    );
}
