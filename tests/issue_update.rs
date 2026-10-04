//! `linear issue update`: the no-field guard (VED-296).
//!
//! The command used to send an `issueUpdate` carrying only the issue's own
//! `teamId` and report "✓ Updated" for a write that changed nothing. This pins
//! the refusal, and that it happens before any request.

mod common;

use common::{run_cli, MockLinearServer};

#[test]
fn issue_update_with_no_field_flags_is_refused_before_any_request() {
    // No responses: if the command reached the API the mock would answer
    // "No mock response configured", so passing the guard is proven by the
    // message, not by a mock that happened to match.
    let server = MockLinearServer::start(vec![]);

    let out = run_cli(
        &["issue", "update", "ENG-1"],
        &common::mock_env(&server),
    );

    assert!(!out.success(), "stdout: {}", out.stdout);
    let complaint = format!("{}{}", out.stdout, out.stderr);
    assert!(
        complaint.contains("No update fields provided"),
        "the message must name the missing fields: {complaint}"
    );
    assert!(
        complaint.contains("--title") || complaint.contains("--state"),
        "and suggest a flag to pass: {complaint}"
    );
    assert!(
        !complaint.contains("No mock response configured"),
        "the guard must run before any request: {complaint}"
    );
}
