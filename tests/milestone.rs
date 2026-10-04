//! `milestone update`/`delete` reference errors (VED-297).
//!
//! The mutations used to surface Linear's internal
//! "Could not find referenced ProjectMilestone." verbatim. These pin the
//! friendly replacement, and that it does not leak the GraphQL type name.

mod common;

use common::{run_cli, MockLinearServer, MockResponse};
use serde_json::json;

fn missing_milestone(operation: &str) -> MockResponse {
    MockResponse::new(
        operation,
        json!({ "errors": [{ "message": "Could not find referenced ProjectMilestone." }] }),
    )
}

#[test]
fn milestone_update_reports_a_missing_milestone_with_a_suggestion() {
    let server = MockLinearServer::start(vec![missing_milestone("UpdateProjectMilestone")]);

    let out = run_cli(
        &["milestone", "update", "ZZZ-NOPE-2026", "--name", "nope"],
        &common::mock_env(&server),
    );

    assert!(!out.success(), "stdout: {}", out.stdout);
    let complaint = format!("{}{}", out.stdout, out.stderr);
    assert!(complaint.contains("Milestone not found"), "{complaint}");
    assert!(
        !complaint.contains("ProjectMilestone"),
        "the internal GraphQL type name leaked: {complaint}"
    );
    assert!(
        complaint.contains("milestone list"),
        "and it must name a way out: {complaint}"
    );
}

#[test]
fn milestone_delete_reports_a_missing_milestone_with_a_suggestion() {
    let server = MockLinearServer::start(vec![missing_milestone("DeleteProjectMilestone")]);

    let out = run_cli(
        &["milestone", "delete", "ZZZ-NOPE-2026", "--force"],
        &common::mock_env(&server),
    );

    assert!(!out.success(), "stdout: {}", out.stdout);
    let complaint = format!("{}{}", out.stdout, out.stderr);
    assert!(complaint.contains("Milestone not found"), "{complaint}");
    assert!(
        !complaint.contains("ProjectMilestone"),
        "the internal GraphQL type name leaked: {complaint}"
    );
}
