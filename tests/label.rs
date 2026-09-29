//! Regressions for `linear label` and for passing a workspace label to an
//! issue, run against the headless mock server.
//!
//! Two defects are pinned here:
//!
//! * `linear label delete <name>` never worked. `GetLabelByName` declared a
//!   `$teamKey` variable it never used, and Linear rejects an operation that
//!   declares an unused variable before running it, so the request always
//!   failed — and `resolve_label_id` reported that failure as "Label not
//!   found". The mock cannot validate a document, so the first test simulates
//!   Linear's rejection by gating a reply on the document still carrying the
//!   accidental declaration; the second pins the error-surfacing half.
//! * A workspace-level label (one whose `team` is null) could not be applied:
//!   label resolution filtered on `team: { key: ... }` only, so the stock
//!   workspace labels were unrepresentable. Upstream's filter accepts the
//!   team's labels *or* the workspace's, and the third test gates the lookup on
//!   exactly that clause.

mod common;

use common::{run_cli, MockLinearServer, MockResponse};
use serde_json::{json, Value};

const WORKSPACE_LABEL_ID: &str = "label-workspace-bug";

/// A label with no team: a workspace label, shared by every team.
fn workspace_label(id: &str, name: &str) -> Value {
    json!({
        "id": id,
        "name": name,
        "color": "#EB5757",
        "team": null
    })
}

/// The lookup request upstream sends for a name: the team's labels or the
/// workspace's. Gating a mock on this clause is how the harness asserts the
/// filter, since it cannot inspect a filter that never reaches it.
const WORKSPACE_OR_TEAM_CLAUSE: &str = "team: { null: true }";

#[test]
fn label_delete_by_name_does_not_declare_an_unused_variable() {
    let server = MockLinearServer::start(vec![
        // Linear's own answer to an operation declaring an unused variable.
        // While `GetLabelByName` still declares `$teamKey`, this reply is the
        // one the command sees and the delete never happens.
        MockResponse::new(
            "GetLabelByName",
            json!({ "errors": [{
                "message": "Variable \"$teamKey\" is never used in operation \"GetLabelByName\".",
                "extensions": { "code": "GRAPHQL_VALIDATION_FAILED" }
            }] }),
        )
        .with_query_includes("$teamKey"),
        MockResponse::new(
            "GetLabelByName",
            json!({ "data": { "issueLabels": {
                "nodes": [workspace_label("label-scratch-id", "ZZZ-scratch-label")]
            } } }),
        )
        .with_variables(json!({ "name": "ZZZ-scratch-label" })),
        MockResponse::new(
            "DeleteIssueLabel",
            json!({ "data": { "issueLabelDelete": { "success": true } } }),
        )
        .with_variables(json!({ "id": "label-scratch-id" })),
    ]);

    let out = run_cli(
        &["label", "delete", "ZZZ-scratch-label", "--force"],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout
            .contains("✓ Deleted label: ZZZ-scratch-label (Workspace)"),
        "stdout: {}",
        out.stdout
    );
}

#[test]
fn label_delete_surfaces_a_failed_lookup_instead_of_reporting_not_found() {
    // No delete mock: reaching the mutation is itself a failure. The lookup
    // fails, and that failure is not a missing label.
    let server = MockLinearServer::start(vec![MockResponse::new(
        "GetLabelByName",
        json!({ "errors": [{ "message": "Name lookup exploded" }] }),
    )]);

    let out = run_cli(
        &["label", "delete", "Bug", "--force"],
        &common::mock_env(&server),
    );

    assert!(!out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("Name lookup exploded"),
        "the request failure must surface as itself: {}",
        out.stderr
    );
    assert!(
        !out.stderr.contains("Label not found"),
        "a failed request is not a missing label: {}",
        out.stderr
    );
}

#[test]
fn issue_create_resolves_a_workspace_label_by_name() {
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
        // The label lookup only matches once it asks for the team's labels or
        // the workspace's; a team-only filter never reaches this mock.
        MockResponse::new(
            "GetIssueLabelByName",
            json!({ "data": { "issueLabels": {
                "nodes": [{ "id": WORKSPACE_LABEL_ID, "name": "Bug" }]
            } } }),
        )
        .with_query_includes(WORKSPACE_OR_TEAM_CLAUSE)
        .with_variables(json!({ "name": "Bug", "teamKey": common::ENG_TEAM_KEY })),
        // Gated on the whole create input, so the resolved workspace label
        // really has to be in `labelIds`.
        MockResponse::new(
            "CreateIssue",
            json!({ "data": { "issueCreate": {
                "success": true,
                "issue": {
                    "id": "issue-1",
                    "identifier": "ENG-1",
                    "url": "https://linear.app/example/issue/ENG-1",
                    "team": { "key": common::ENG_TEAM_KEY }
                }
            } } }),
        )
        .with_variables(json!({ "input": {
            "title": "Carry a workspace label",
            "labelIds": [WORKSPACE_LABEL_ID],
            "teamId": common::ENG_TEAM_ID,
            "useDefaultTemplate": true
        } })),
    ]);

    let out = run_cli(
        &[
            "issue",
            "create",
            "--team",
            common::ENG_TEAM_KEY,
            "--title",
            "Carry a workspace label",
            "--label",
            "Bug",
        ],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    // The gated `CreateIssue` reply above only matches when `labelIds` carries
    // the resolved workspace label, so reaching the URL means the label was on
    // the mutation.
    assert!(
        out.stdout
            .contains("https://linear.app/example/issue/ENG-1"),
        "stdout: {}",
        out.stdout
    );
}
