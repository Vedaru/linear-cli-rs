//! `issue pull-request` names the `gh` dependency (VED-298).
//!
//! `gh` is installed on many machines, so proving the missing case needs a
//! controlled `PATH`. This flow reaches `gh` without touching `git`, so the
//! path can be an empty directory and nothing else needs to be faked.

mod common;

use std::process::Command;

use common::{mock_env, MockLinearServer, MockResponse};
use serde_json::json;

fn issue_details(operation: &str) -> MockResponse {
    MockResponse::new(
        operation,
        json!({ "data": { "issue": {
            "id": "issue-1",
            "identifier": "ENG-1",
            "title": "A pull request subject",
            "url": "https://linear.app/example/issue/ENG-1"
        } } }),
    )
}

#[test]
fn a_missing_gh_is_named_with_a_way_to_install_it() {
    // Both spellings, because the fetch picks its query from whether a spinner
    // is shown, which depends on the terminal.
    let server = MockLinearServer::start(vec![
        issue_details("GetIssueDetails"),
        issue_details("GetIssueDetailsWithComments"),
    ]);

    let empty = tempfile::tempdir().expect("temp dir");
    let mut command = Command::new(env!("CARGO_BIN_EXE_linear"));
    command.args(["issue", "pull-request", "ENG-1", "--no-template"]);
    command.env("NO_COLOR", "1");
    // No tools on PATH: `gh` cannot be found.
    command.env("PATH", empty.path());
    for (key, value) in mock_env(&server) {
        command.env(key, value);
    }

    let output = command.output().expect("linear runs");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    assert_eq!(output.status.code(), Some(1), "{combined}");
    assert!(
        combined.contains("gh is not installed"),
        "the missing dependency must be named: {combined}"
    );
    assert!(
        combined.contains("cli.github.com"),
        "and the way to install it: {combined}"
    );
    assert!(
        !combined.contains("Failed to create pull request: Failed to create pull request"),
        "the tautology must be gone: {combined}"
    );
}
