//! `issue create`'s flag path: the input it builds, and the validations that stop it
//! before any request (VED-290).
//!
//! The suite pins the mutation's `variables` rather than only its response: the mock
//! answers `CreateIssue` only when the input deep-equals what the test expects, so a
//! field that is added, dropped or mis-serialised fails the test instead of quietly
//! matching a loose stub.

mod common;

use common::{run_cli, MockLinearServer, MockResponse};
use serde_json::json;

/// The team lookup `--team` makes before the mutation.
fn team_mock() -> MockResponse {
    MockResponse::new(
        "FindTeam",
        json!({ "data": { "teams": { "nodes": [{
            "id": common::ENG_TEAM_ID, "key": common::ENG_TEAM_KEY, "name": common::ENG_TEAM_NAME
        }] } } }),
    )
    .with_variables(json!({ "reference": common::ENG_TEAM_KEY }))
}

/// A successful `issueCreate`, with the url the non-`--json` path prints.
fn created(identifier: &str) -> MockResponse {
    MockResponse::new(
        "CreateIssue",
        json!({ "data": { "issueCreate": {
            "success": true,
            "issue": {
                "id": format!("id-{identifier}"),
                "identifier": identifier,
                "url": format!("https://linear.app/example/issue/{identifier}"),
                "team": { "key": common::ENG_TEAM_KEY }
            }
        } } }),
    )
}

#[test]
fn issue_create_sends_the_flag_built_input() {
    let server = MockLinearServer::start(vec![
        team_mock(),
        created("ENG-1").with_variables(json!({
            "input": {
                "title": "A flag-built issue",
                "labelIds": [],
                "teamId": common::ENG_TEAM_ID,
                "useDefaultTemplate": true
            }
        })),
    ]);

    let out = run_cli(
        &[
            "issue",
            "create",
            "--team",
            common::ENG_TEAM_KEY,
            "--title",
            "A flag-built issue",
        ],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("https://linear.app/example/issue/ENG-1"),
        "stdout: {}",
        out.stdout
    );
}

#[test]
fn issue_create_no_use_default_template_reaches_the_input() {
    // `--no-use-default-template` is a flag whose *absence* is not the same as `false`:
    // the field only exists because the team's default template would otherwise fill the
    // issue a second time.
    let server = MockLinearServer::start(vec![
        team_mock(),
        created("ENG-2").with_variables(json!({
            "input": {
                "title": "No default template",
                "labelIds": [],
                "teamId": common::ENG_TEAM_ID,
                "useDefaultTemplate": false
            }
        })),
    ]);

    let out = run_cli(
        &[
            "issue",
            "create",
            "--team",
            common::ENG_TEAM_KEY,
            "--title",
            "No default template",
            "--no-use-default-template",
        ],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
}

#[test]
fn issue_create_carries_description_priority_and_due_date() {
    let server = MockLinearServer::start(vec![
        team_mock(),
        created("ENG-3").with_variables(json!({
            "input": {
                "title": "Everything from flags",
                "description": "why it matters",
                "priority": 2,
                "dueDate": "2026-10-09",
                "labelIds": [],
                "teamId": common::ENG_TEAM_ID,
                "useDefaultTemplate": true
            }
        })),
    ]);

    let out = run_cli(
        &[
            "issue",
            "create",
            "--team",
            common::ENG_TEAM_KEY,
            "--title",
            "Everything from flags",
            "--description",
            "why it matters",
            "--priority",
            "2",
            "--due-date",
            "2026-10-09",
        ],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
}

#[test]
fn issue_create_resolves_a_label_name_to_its_id() {
    let server = MockLinearServer::start(vec![
        team_mock(),
        MockResponse::new(
            "GetIssueLabelByName",
            json!({ "data": { "issueLabels": { "nodes": [
                { "id": "label-bug", "name": "Bug" }
            ] } } }),
        )
        .with_variables(json!({ "name": "Bug", "teamKey": common::ENG_TEAM_KEY })),
        created("ENG-4").with_variables(json!({
            "input": {
                "title": "Labelled",
                "labelIds": ["label-bug"],
                "teamId": common::ENG_TEAM_ID,
                "useDefaultTemplate": true
            }
        })),
    ]);

    let out = run_cli(
        &[
            "issue",
            "create",
            "--team",
            common::ENG_TEAM_KEY,
            "--title",
            "Labelled",
            "--label",
            "Bug",
        ],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
}

#[test]
fn issue_create_missing_title_in_a_headless_run_names_the_flag() {
    // A flag beyond nothing-but-parent/project means flag mode, so the title is required;
    // the error must name the way out instead of dropping into a prompt no agent can answer.
    let server = MockLinearServer::start(vec![]);

    let out = run_cli(
        &["issue", "create", "--team", common::ENG_TEAM_KEY],
        &common::mock_env(&server),
    );

    assert!(!out.success(), "stdout: {}", out.stdout);
    let complaint = format!("{}{}", out.stdout, out.stderr);
    assert!(complaint.contains("Title is required"), "{complaint}");
    assert!(complaint.contains("--title"), "{complaint}");
}

#[test]
fn issue_create_milestone_without_project_is_refused() {
    let server = MockLinearServer::start(vec![team_mock()]);

    let out = run_cli(
        &[
            "issue",
            "create",
            "--team",
            common::ENG_TEAM_KEY,
            "--title",
            "Needs a project",
            "--milestone",
            "M1",
        ],
        &common::mock_env(&server),
    );

    assert!(!out.success(), "stdout: {}", out.stdout);
    let complaint = format!("{}{}", out.stdout, out.stderr);
    assert!(
        complaint.contains("--milestone requires --project"),
        "{complaint}"
    );
}

#[test]
fn issue_create_start_rejects_a_non_self_assignee() {
    // `--start` means "assign it to me and move it to the started state"; pairing it with
    // somebody else is contradictory, and refusing beats silently starting it for the
    // wrong person.
    let server = MockLinearServer::start(vec![team_mock()]);

    let out = run_cli(
        &[
            "issue",
            "create",
            "--team",
            common::ENG_TEAM_KEY,
            "--title",
            "Someone else starts it",
            "--start",
            "--assignee",
            "alice",
        ],
        &common::mock_env(&server),
    );

    assert!(!out.success(), "stdout: {}", out.stdout);
    let complaint = format!("{}{}", out.stdout, out.stderr);
    assert!(
        complaint.contains("--start") && complaint.contains("--assignee"),
        "{complaint}"
    );
}

/// A response that is not `success: true` is a failed creation, not a silent one.
#[test]
fn issue_create_reports_a_rejected_mutation() {
    let server = MockLinearServer::start(vec![team_mock(), {
        let mut rejected = created("ENG-5");
        rejected.response = json!({ "data": { "issueCreate": { "success": false } } });
        rejected
    }]);

    let out = run_cli(
        &[
            "issue",
            "create",
            "--team",
            common::ENG_TEAM_KEY,
            "--title",
            "Rejected",
        ],
        &common::mock_env(&server),
    );

    assert!(!out.success(), "stdout: {}", out.stdout);
    let complaint = format!("{}{}", out.stdout, out.stderr);
    assert!(complaint.contains("Issue creation failed"), "{complaint}");
}
