//! `linear export` / `linear import` — the transfer pair, and the property the ticket names:
//! an export re-imports to a no-op, and a hand-edited field travels.
//!
//! The round-trip test is the one that matters: it exports through the real command, writes the
//! real CSV to a temp file, and imports that file back, so it fails if the two directions ever
//! disagree about a field's spelling - which is the failure mode a second serialiser would have.

mod common;

use common::{run_cli, MockLinearServer, MockResponse};
use serde_json::{json, Value};

const ISSUE_ID: &str = "issue-1";

fn issue() -> Value {
    json!({
        "id": ISSUE_ID,
        "identifier": "ENG-1",
        "title": "Ship it",
        // A comma and a newline: the two things a CSV cell must quote.
        "description": "one, two\nthree",
        "priority": 2,
        "estimate": 3,
        "dueDate": "2026-10-10",
        "url": "https://linear.app/example/issue/ENG-1",
        "createdAt": "2026-09-01T00:00:00.000Z",
        "updatedAt": "2026-10-01T00:00:00.000Z",
        "state": { "id": "state-1", "name": "In Progress", "type": "started" },
        "assignee": { "id": "user-1", "name": "Ada", "displayName": "Ada" },
        "team": { "id": common::ENG_TEAM_ID, "key": common::ENG_TEAM_KEY, "name": common::ENG_TEAM_NAME },
        "project": { "id": "project-1", "name": "Board" },
        "projectMilestone": { "id": "milestone-1", "name": "M7" },
        "cycle": { "id": "cycle-1", "number": 7, "name": null },
        "labels": { "nodes": [ { "id": "label-2", "name": "Improvement" }, { "id": "label-1", "name": "Bug" } ] },
        "parent": null
    })
}

fn issues_document(nodes: Vec<Value>) -> Value {
    json!({ "data": { "issues": {
        "nodes": nodes,
        "pageInfo": { "hasNextPage": false, "endCursor": null }
    } } })
}

fn export_mock(nodes: Vec<Value>) -> MockResponse {
    MockResponse::new("ExportIssues", issues_document(nodes))
}

fn find_team_mock() -> MockResponse {
    MockResponse::new(
        "FindTeam",
        json!({ "data": {
            "teams": { "nodes": [{
                "id": common::ENG_TEAM_ID,
                "key": common::ENG_TEAM_KEY,
                "name": common::ENG_TEAM_NAME
            }] },
            "teamById": { "nodes": [] }
        } }),
    )
}

fn temp_path(name: &str) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join(name);
    let path = path.to_string_lossy().to_string();
    (dir, path)
}

#[test]
fn export_csv_quotes_the_fields_that_need_it() {
    let server = MockLinearServer::start(vec![find_team_mock(), export_mock(vec![issue()])]);

    let out = run_cli(
        &[
            "export",
            "issues",
            "--team",
            common::ENG_TEAM_KEY,
            "--format",
            "csv",
        ],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    let lines: Vec<&str> = out.stdout.lines().collect();
    assert!(
        lines[0].starts_with("identifier,title,state,assignee"),
        "{}",
        lines[0]
    );
    assert!(
        lines[1].starts_with("ENG-1,Ship it,In Progress,Ada,2,3,Board,Bug|Improvement,"),
        "{}",
        lines[1]
    );
    // The description is one cell, quoted, with its newline intact.
    assert!(out.stdout.contains("\"one, two\nthree\""), "{}", out.stdout);
}

#[test]
fn export_json_is_the_document_the_query_layer_produces() {
    let server = MockLinearServer::start(vec![find_team_mock(), export_mock(vec![issue()])]);

    let out = run_cli(
        &[
            "export",
            "issues",
            "--team",
            common::ENG_TEAM_KEY,
            "--format",
            "json",
        ],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    let parsed: Value = serde_json::from_str(&out.stdout).expect("json export is JSON");
    assert_eq!(parsed["nodes"][0]["identifier"], json!("ENG-1"));
    assert!(parsed["pageInfo"].is_object(), "{}", out.stdout);
}

#[test]
fn export_ndjson_writes_one_node_per_line() {
    let server =
        MockLinearServer::start(vec![find_team_mock(), export_mock(vec![issue(), issue()])]);

    let out = run_cli(
        &[
            "export",
            "issues",
            "--team",
            common::ENG_TEAM_KEY,
            "--format",
            "ndjson",
        ],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    let lines: Vec<&str> = out.stdout.lines().filter(|line| !line.is_empty()).collect();
    assert_eq!(lines.len(), 2, "{}", out.stdout);
    let first: Value = serde_json::from_str(lines[0]).expect("one node per line");
    assert_eq!(first["identifier"], json!("ENG-1"));
}

#[test]
fn export_markdown_reads_as_a_document() {
    let server = MockLinearServer::start(vec![find_team_mock(), export_mock(vec![issue()])]);

    let out = run_cli(
        &[
            "export",
            "issues",
            "--team",
            common::ENG_TEAM_KEY,
            "--format",
            "markdown",
        ],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(out.stdout.contains("## ENG-1: Ship it"), "{}", out.stdout);
    assert!(
        out.stdout.contains("- State: In Progress"),
        "{}",
        out.stdout
    );
    assert!(
        out.stdout.contains("- Labels: Bug, Improvement"),
        "{}",
        out.stdout
    );
    assert!(out.stdout.contains("one, two\nthree"), "{}", out.stdout);
}

#[test]
fn export_projects_writes_the_same_team_keys_the_issue_export_does() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "ExportProjects",
        json!({ "data": { "projects": {
            "nodes": [{
                "id": "project-1",
                "name": "Board",
                "status": { "name": "In Progress", "type": "started" },
                "health": "onTrack",
                "priority": 2,
                "lead": { "displayName": "Ada" },
                "teams": { "nodes": [{ "key": "OPS" }, { "key": "ENG" }] },
                "startDate": "2026-10-01",
                "targetDate": "2026-12-01",
                "url": "https://linear.app/example/project/board-1",
                "createdAt": "2026-09-01T00:00:00.000Z",
                "updatedAt": "2026-10-01T00:00:00.000Z"
            }],
            "pageInfo": { "hasNextPage": false, "endCursor": null }
        } } }),
    )]);

    let out = run_cli(
        &["export", "projects", "--format", "csv"],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout
            .starts_with("name,status,health,priority,lead,teams"),
        "{}",
        out.stdout
    );
    assert!(
        out.stdout
            .contains("Board,In Progress,onTrack,2,Ada,ENG|OPS"),
        "{}",
        out.stdout
    );
}

/// The ticket's "done when": the CSV this CLI writes, read back by this CLI, is a no-op. Nothing
/// answers `ImportIssueUpdate` or `ImportIssueCreate` here, so a write that went out anyway would
/// fail the test instead of passing quietly.
#[test]
fn an_unchanged_export_re_imports_to_a_no_op() {
    let server =
        MockLinearServer::start(vec![find_team_mock(), export_mock(vec![issue(), issue()])]);
    let (_dir, path) = temp_path("issues.csv");

    let export = run_cli(
        &[
            "export",
            "issues",
            "--team",
            common::ENG_TEAM_KEY,
            "--format",
            "csv",
            "--output",
            &path,
        ],
        &common::mock_env(&server),
    );
    assert!(export.success(), "stderr: {}", export.stderr);

    // The same server answers the import's "what does Linear already have" read: the export came
    // from it, so the file and Linear agree field for field.
    let import = run_cli(
        &["import", "issues", &path, "--team", common::ENG_TEAM_KEY],
        &common::mock_env(&server),
    );

    assert!(import.success(), "stderr: {}", import.stderr);
    assert!(
        import.stdout.contains("Dry run: 0 issue(s) would change"),
        "stdout: {}",
        import.stdout
    );
    assert!(!import.stdout.contains("would update"), "{}", import.stdout);
    assert!(!import.stdout.contains("would create"), "{}", import.stdout);
    assert!(
        import.stdout.matches("unchanged").count() == 2,
        "{}",
        import.stdout
    );
}

/// A hand-edited cell is the difference the import must not lose: only that field is sent.
#[test]
fn a_hand_edited_field_travels_and_is_the_only_field_sent() {
    let server = MockLinearServer::start(vec![
        find_team_mock(),
        export_mock(vec![issue()]),
        MockResponse::new(
            "ImportIssueUpdate",
            json!({ "data": { "issueUpdate": { "success": true, "issue": {
                "id": ISSUE_ID, "identifier": "ENG-1"
            } } } }),
        )
        .with_variables(json!({
            "id": ISSUE_ID,
            "input": { "title": "Ship it today" }
        })),
    ]);

    let (_dir, path) = temp_path("edited.csv");
    let mut csv = String::from(
        "identifier,title,state,assignee,priority,estimate,project,labels,dueDate,cycle,milestone,team,description,url,id,updatedAt\n",
    );
    csv.push_str("\"ENG-1\",\"Ship it today\",\"In Progress\",\"Ada\",2,3,\"Board\",\"Bug|Improvement\",\"2026-10-10\",\"#7\",\"M7\",\"ENG\",\"one, two\nthree\",\"https://linear.app/example/issue/ENG-1\",\"issue-1\",\"2026-10-01T00:00:00.000Z\"\n");
    std::fs::write(&path, csv).expect("write the edited csv");

    let out = run_cli(
        &[
            "import",
            "issues",
            &path,
            "--team",
            common::ENG_TEAM_KEY,
            "--apply",
        ],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("✓ Updated ENG-1: title"),
        "{}",
        out.stdout
    );
}

#[test]
fn an_edited_field_is_reported_by_the_dry_run() {
    let server = MockLinearServer::start(vec![find_team_mock(), export_mock(vec![issue()])]);

    let (_dir, path) = temp_path("edited.csv");
    let csv = "identifier,title,team\nENG-1,Ship it today,ENG\n";
    std::fs::write(&path, csv).expect("write the edited csv");

    let out = run_cli(
        &[
            "import",
            "issues",
            &path,
            "--team",
            common::ENG_TEAM_KEY,
            "--json",
        ],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    let parsed: Value = serde_json::from_str(&out.stdout).expect("plan --json is JSON");
    assert_eq!(parsed["applied"], json!(false));
    assert_eq!(parsed["update"], json!(1));
    assert_eq!(parsed["plan"][0]["changes"][0]["field"], json!("title"));
    assert_eq!(parsed["plan"][0]["changes"][0]["from"], json!("Ship it"));
    assert_eq!(
        parsed["plan"][0]["changes"][0]["to"],
        json!("Ship it today")
    );
}

#[test]
fn a_row_no_issue_matches_is_created() {
    let server = MockLinearServer::start(vec![
        export_mock(vec![issue()]),
        find_team_mock(),
        MockResponse::new(
            "GetWorkflowStates",
            json!({ "data": { "team": { "states": { "nodes": [
                { "id": "state-1", "name": "Todo", "type": "unstarted", "position": 1.0, "color": "#eee" },
                { "id": "state-2", "name": "In Progress", "type": "started", "position": 2.0, "color": "#eee" }
            ] } } } }),
        ),
        MockResponse::new(
            "LookupUser",
            json!({ "data": { "users": { "nodes": [
                { "id": "user-1", "name": "Ada", "displayName": "Ada", "email": "ada@example.com" }
            ] } } }),
        ),
        MockResponse::new(
            "ImportIssueCreate",
            json!({ "data": { "issueCreate": { "success": true, "issue": {
                "id": "issue-2", "identifier": "ENG-2"
            } } } }),
        )
        .with_variables(json!({ "input": {
            "title": "A brand new issue",
            "teamId": common::ENG_TEAM_ID,
            "stateId": "state-1",
            "assigneeId": "user-1",
            "priority": 3
        } })),
    ]);

    let (_dir, path) = temp_path("new.csv");
    let csv = "identifier,title,team,state,assignee,priority\n,A brand new issue,ENG,Todo,Ada,3\n";
    std::fs::write(&path, csv).expect("write the csv");

    let out = run_cli(
        &[
            "import",
            "issues",
            &path,
            "--team",
            common::ENG_TEAM_KEY,
            "--apply",
        ],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("✓ Created ENG-2: A brand new issue"),
        "{}",
        out.stdout
    );
}

#[test]
fn a_row_with_no_title_cannot_be_created() {
    let server = MockLinearServer::start(vec![find_team_mock(), export_mock(vec![issue()])]);

    let (_dir, path) = temp_path("empty.csv");
    std::fs::write(&path, "identifier,title,team\n,,\n").expect("write the csv");

    let out = run_cli(
        &["import", "issues", &path, "--team", common::ENG_TEAM_KEY],
        &common::mock_env(&server),
    );

    assert!(!out.success());
    assert!(
        out.stderr.contains("has no title"),
        "stderr: {}",
        out.stderr
    );
}

#[test]
fn a_row_that_does_not_line_up_with_the_header_is_refused() {
    let server = MockLinearServer::start(vec![]);

    let (_dir, path) = temp_path("ragged.csv");
    std::fs::write(&path, "identifier,title\nENG-1\n").expect("write the csv");

    let out = run_cli(
        &["import", "issues", &path, "--team", common::ENG_TEAM_KEY],
        &common::mock_env(&server),
    );

    assert!(!out.success());
    assert!(
        out.stderr.contains("has 1 cells but the header has 2"),
        "stderr: {}",
        out.stderr
    );
}
