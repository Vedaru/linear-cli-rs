//! What `--json` means for a mutation: the API's own payload, verbatim - the same
//! rule the read commands follow. A caller gets the identifier, the id and the url
//! the server already returned, so it can act on the result without a second query
//! and without parsing a sentence.

mod common;

use common::{run_cli, MockLinearServer, MockResponse};
use serde_json::json;

/// The team lookup both paths make before their mutation. The operation is
/// `FindTeam`, not the `ResolveTeam` helper some other suites use.
fn team_mock() -> MockResponse {
    MockResponse::new(
        "FindTeam",
        json!({ "data": { "teams": { "nodes": [{
            "id": common::ENG_TEAM_ID, "key": common::ENG_TEAM_KEY, "name": common::ENG_TEAM_NAME
        }] } } }),
    )
    .with_variables(json!({ "reference": common::ENG_TEAM_KEY }))
}
#[test]
fn issue_create_json_emits_the_api_payload() {
    let server = MockLinearServer::start(vec![
        team_mock(),
        MockResponse::new(
            "CreateIssue",
            json!({ "data": { "issueCreate": {
                "success": true,
                "issue": {
                    "id": "issue-1",
                    "identifier": "ENG-1",
                    "title": "A machine-readable creation",
                    "url": "https://linear.app/example/issue/ENG-1",
                    "team": { "key": common::ENG_TEAM_KEY }
                }
            } } }),
        ),
    ]);

    let out = run_cli(
        &[
            "issue",
            "create",
            "--team",
            common::ENG_TEAM_KEY,
            "--title",
            "A machine-readable creation",
            "--json",
        ],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    let parsed: serde_json::Value = serde_json::from_str(&out.stdout).expect("json output");
    assert_eq!(parsed["issueCreate"]["issue"]["identifier"], "ENG-1");
    assert_eq!(parsed["issueCreate"]["issue"]["id"], "issue-1");
    assert_eq!(parsed["issueCreate"]["success"], true);
    // One document and nothing else on stdout: no "Creating issue in ..." above it.
    assert!(
        out.stdout.trim_start().starts_with('{'),
        "stdout: {}",
        out.stdout
    );
}

/// `issue update --json` emits `issueUpdate`, and the prose it replaces is gone.
#[test]
fn issue_update_json_emits_the_api_payload() {
    let server = MockLinearServer::start(vec![
        team_mock(),
        MockResponse::new(
            "UpdateIssue",
            json!({ "data": { "issueUpdate": {
                "success": true,
                "issue": {
                    "id": "issue-9",
                    "identifier": "ENG-9",
                    "title": "Renamed",
                    "url": "https://linear.app/example/issue/ENG-9"
                }
            } } }),
        ),
    ]);

    let out = run_cli(
        &["issue", "update", "ENG-9", "--title", "Renamed", "--json"],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    let parsed: serde_json::Value = serde_json::from_str(&out.stdout).expect("json output");
    assert_eq!(parsed["issueUpdate"]["issue"]["identifier"], "ENG-9");
    assert_eq!(parsed["issueUpdate"]["issue"]["title"], "Renamed");
    assert!(
        !out.stdout.contains("Updating issue"),
        "the progress line has to stay out of the document: {}",
        out.stdout
    );
}
