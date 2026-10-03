//! `linear sprint` — the five figures, run against the mock server.
//!
//! The arithmetic is the thing under test: which issues count as done, what "scope" means on a
//! given day, and which cycles an average is allowed to include. The fixtures are cycles in the
//! past so "today" (the real clock) cannot make an assertion flaky.

mod common;

use common::{run_cli, MockLinearServer, MockResponse};
use serde_json::{json, Value};

const CYCLE_8: &str = "cycle-8";
const CYCLE_9: &str = "cycle-9";

/// A team whose active cycle is #8, with #9 waiting after it.
fn cycles_mock(active: bool) -> MockResponse {
    MockResponse::new(
        "GetTeamCycleWindows",
        json!({ "data": { "team": {
            "id": common::ENG_TEAM_ID,
            "key": common::ENG_TEAM_KEY,
            "name": common::ENG_TEAM_NAME,
            "cyclesEnabled": true,
            "activeCycle": {
                "id": CYCLE_8, "number": 8, "name": null,
                "startsAt": "2026-09-17T00:00:00.000Z", "endsAt": "2026-09-30T23:59:59.000Z"
            },
            "cycles": {
                "nodes": [
                    {
                        "id": CYCLE_8, "number": 8, "name": "Sprint 8",
                        "startsAt": "2026-09-17T00:00:00.000Z", "endsAt": "2026-09-30T23:59:59.000Z",
                        "completedAt": null, "archivedAt": null,
                        "isActive": active, "isNext": false, "isPast": !active, "isFuture": false
                    },
                    {
                        "id": CYCLE_9, "number": 9, "name": null,
                        "startsAt": "2026-10-01T00:00:00.000Z", "endsAt": "2026-10-14T23:59:59.000Z",
                        "completedAt": null, "archivedAt": null,
                        "isActive": false, "isNext": true, "isPast": false, "isFuture": true
                    }
                ],
                "pageInfo": { "hasNextPage": false, "endCursor": null }
            }
        } } }),
    )
}

fn find_team_mock() -> MockResponse {
    MockResponse::new(
        "FindTeam",
        json!({ "data": {
            "teams": { "nodes": [{
                "id": common::ENG_TEAM_ID, "key": common::ENG_TEAM_KEY, "name": common::ENG_TEAM_NAME
            }] },
            "teamById": { "nodes": [] }
        } }),
    )
}

fn issue(
    identifier: &str,
    state_type: &str,
    estimate: i64,
    created_at: &str,
    completed_at: Option<&str>,
) -> Value {
    json!({
        "id": format!("issue-{identifier}"),
        "identifier": identifier,
        "title": format!("Work on {identifier}"),
        "description": null,
        "priority": 2,
        "estimate": estimate,
        "dueDate": null,
        "completedAt": completed_at,
        "url": format!("https://linear.app/example/issue/{identifier}"),
        "createdAt": created_at,
        "updatedAt": created_at,
        "state": { "id": "state-1", "name": state_type, "type": state_type },
        "assignee": null,
        "team": { "id": common::ENG_TEAM_ID, "key": common::ENG_TEAM_KEY, "name": common::ENG_TEAM_NAME },
        "project": null,
        "projectMilestone": null,
        "cycle": { "id": CYCLE_8, "number": 8, "name": "Sprint 8" },
        "labels": { "nodes": [] }
    })
}

fn issues_mock(nodes: Vec<Value>) -> MockResponse {
    MockResponse::new(
        "ExportIssues",
        json!({ "data": { "issues": {
            "nodes": nodes,
            "pageInfo": { "hasNextPage": false, "endCursor": null }
        } } }),
    )
}

/// A cycle with one of everything: done, started, and canceled.
fn mixed_issues_mock() -> MockResponse {
    issues_mock(vec![
        issue(
            "ENG-1",
            "completed",
            3,
            "2026-09-18T00:00:00.000Z",
            Some("2026-09-20T00:00:00.000Z"),
        ),
        issue("ENG-2", "started", 5, "2026-09-19T00:00:00.000Z", None),
        issue("ENG-3", "canceled", 1, "2026-09-19T00:00:00.000Z", None),
    ])
}

#[test]
fn sprint_status_counts_what_is_done_and_what_is_left() {
    let server = MockLinearServer::start(vec![
        find_team_mock(),
        cycles_mock(true),
        mixed_issues_mock(),
    ]);

    let out = run_cli(
        &["sprint", "status", "--team", common::ENG_TEAM_KEY, "--json"],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    let parsed: Value = serde_json::from_str(&out.stdout).expect("status --json is JSON");
    assert_eq!(parsed["by"], json!("points"));
    assert_eq!(parsed["issues"]["completed"], json!(1));
    assert_eq!(parsed["issues"]["remaining"], json!(1));
    assert_eq!(parsed["issues"]["canceled"], json!(1));
    assert_eq!(parsed["issues"]["total"], json!(2));
    assert_eq!(parsed["work"]["total"], json!(8));
    assert_eq!(parsed["work"]["completed"], json!(3));
    assert_eq!(parsed["work"]["remaining"], json!(5));
}

#[test]
fn sprint_status_without_an_active_cycle_names_the_suggestion() {
    let server = MockLinearServer::start(vec![find_team_mock(), cycles_mock(false)]);

    let out = run_cli(
        &["sprint", "status", "--team", common::ENG_TEAM_KEY],
        &common::mock_env(&server),
    );

    assert!(!out.success());
    assert!(
        out.stderr.contains("has no active cycle"),
        "stderr: {}",
        out.stderr
    );
    assert!(out.stderr.contains("--cycle"), "stderr: {}", out.stderr);
}

#[test]
fn sprint_progress_reports_the_scope_that_arrived_after_the_start() {
    let server = MockLinearServer::start(vec![
        find_team_mock(),
        cycles_mock(true),
        // ENG-2 was created two days into the cycle; ENG-1 before it started.
        issues_mock(vec![
            issue(
                "ENG-1",
                "completed",
                3,
                "2026-09-01T00:00:00.000Z",
                Some("2026-09-20T00:00:00.000Z"),
            ),
            issue("ENG-2", "started", 5, "2026-09-19T00:00:00.000Z", None),
        ]),
    ]);

    let out = run_cli(
        &[
            "sprint",
            "progress",
            "--team",
            common::ENG_TEAM_KEY,
            "--json",
        ],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    let parsed: Value = serde_json::from_str(&out.stdout).expect("progress --json is JSON");
    assert_eq!(parsed["addedAfterStart"]["count"], json!(1));
    assert_eq!(parsed["addedAfterStart"]["identifiers"][0], json!("ENG-2"));
    assert_eq!(parsed["work"]["completed"], json!(3));
    assert_eq!(parsed["work"]["total"], json!(8));
    assert_eq!(parsed["work"]["percent"], json!(37.5));
}

#[test]
fn sprint_carry_over_is_a_dry_run_by_default() {
    // No `CarryOverIssue` reply is configured: a move that went out anyway would fail this test.
    let server = MockLinearServer::start(vec![
        find_team_mock(),
        cycles_mock(true),
        mixed_issues_mock(),
    ]);

    let out = run_cli(
        &["sprint", "carry-over", "--team", common::ENG_TEAM_KEY],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(out.stdout.contains("would move  ENG-2"), "{}", out.stdout);
    assert!(!out.stdout.contains("ENG-1"), "{}", out.stdout);
    assert!(
        out.stdout
            .contains("Dry run: 1 issue(s) would move from #8 (Sprint 8) to #9"),
        "{}",
        out.stdout
    );
}

#[test]
fn sprint_carry_over_with_apply_moves_into_the_next_cycle() {
    let server = MockLinearServer::start(vec![
        find_team_mock(),
        cycles_mock(true),
        mixed_issues_mock(),
        MockResponse::new(
            "CarryOverIssue",
            json!({ "data": { "issueUpdate": { "success": true, "issue": {
                "identifier": "ENG-2", "cycle": { "number": 9 }
            } } } }),
        )
        .with_variables(json!({
            "id": "issue-ENG-2",
            "input": { "cycleId": CYCLE_9 }
        })),
    ]);

    let out = run_cli(
        &[
            "sprint",
            "carry-over",
            "--team",
            common::ENG_TEAM_KEY,
            "--apply",
        ],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(out.stdout.contains("✓ Moved ENG-2 → #9"), "{}", out.stdout);
    assert!(
        out.stdout.contains("Applied: 1 issue(s) moved to #9"),
        "{}",
        out.stdout
    );
}

#[test]
fn sprint_carry_over_with_nothing_unfinished_says_so() {
    let server = MockLinearServer::start(vec![
        find_team_mock(),
        cycles_mock(true),
        issues_mock(vec![issue(
            "ENG-1",
            "completed",
            3,
            "2026-09-18T00:00:00.000Z",
            Some("2026-09-20T00:00:00.000Z"),
        )]),
    ]);

    let out = run_cli(
        &["sprint", "carry-over", "--team", common::ENG_TEAM_KEY],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("Nothing to carry over"),
        "{}",
        out.stdout
    );
}

/// The window is 2026-09-28 → 2026-09-30 (all in the past, so the real clock cannot make this
/// flaky). Scope grows on the 29th when ENG-2 arrives, and ENG-1 completes the same day.
#[test]
fn sprint_burndown_steps_down_as_work_completes_and_scope_arrives() {
    let server = MockLinearServer::start(vec![
        find_team_mock(),
        MockResponse::new(
            "GetTeamCycleWindows",
            json!({ "data": { "team": {
                "id": common::ENG_TEAM_ID, "key": common::ENG_TEAM_KEY, "name": common::ENG_TEAM_NAME,
                "cyclesEnabled": true,
                "activeCycle": null,
                "cycles": { "nodes": [{
                    "id": CYCLE_8, "number": 8, "name": null,
                    "startsAt": "2026-09-28T00:00:00.000Z", "endsAt": "2026-09-30T23:59:59.000Z",
                    "completedAt": null, "archivedAt": null,
                    "isActive": true, "isNext": false, "isPast": false, "isFuture": false
                }], "pageInfo": { "hasNextPage": false, "endCursor": null } }
            } } }),
        ),
        // `--cycle 8` is a number, so the resolver looks it up before the cycle is read.
        MockResponse::new(
            "GetTeamCycles",
            json!({ "data": { "team": {
                "id": common::ENG_TEAM_ID, "key": common::ENG_TEAM_KEY, "name": common::ENG_TEAM_NAME,
                "cyclesEnabled": true,
                "activeCycle": null,
                "cycles": {
                    "nodes": [{
                        "id": CYCLE_8, "number": 8, "name": null,
                        "startsAt": "2026-09-28T00:00:00.000Z",
                        "isNext": false, "isPrevious": false
                    }],
                    "pageInfo": { "hasNextPage": false, "endCursor": null }
                }
            } } }),
        ),
        issues_mock(vec![
            issue(
                "ENG-1",
                "completed",
                3,
                "2026-09-01T00:00:00.000Z",
                Some("2026-09-29T10:00:00.000Z"),
            ),
            issue("ENG-2", "started", 2, "2026-09-29T12:00:00.000Z", None),
            issue("ENG-3", "started", 5, "2026-08-30T00:00:00.000Z", None),
        ]),
    ]);

    let out = run_cli(
        &[
            "sprint",
            "burndown",
            "--team",
            common::ENG_TEAM_KEY,
            "--cycle",
            "8",
            "--json",
        ],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    let parsed: Value = serde_json::from_str(&out.stdout).expect("burndown --json is JSON");
    assert_eq!(parsed["by"], json!("points"));
    assert_eq!(parsed["total"], json!(10));
    let days = parsed["days"].as_array().expect("days");
    assert_eq!(days.len(), 3, "{}", out.stdout);
    assert_eq!(days[0]["date"], json!("2026-09-28"));
    assert_eq!(days[0]["remaining"], json!(8));
    assert_eq!(days[1]["remaining"], json!(7));
    assert_eq!(days[2]["remaining"], json!(7));
    assert_eq!(days[2]["future"], json!(false));
}

#[test]
fn sprint_velocity_leaves_the_cycle_in_flight_out_of_the_average() {
    let server = MockLinearServer::start(vec![
        find_team_mock(),
        cycles_mock(true),
        mixed_issues_mock(),
    ]);

    let out = run_cli(
        &[
            "sprint",
            "velocity",
            "--team",
            common::ENG_TEAM_KEY,
            "--json",
        ],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    let parsed: Value = serde_json::from_str(&out.stdout).expect("velocity --json is JSON");
    assert_eq!(parsed["by"], json!("points"));
    assert_eq!(parsed["cycles"][0]["completed"], json!(3));
    assert_eq!(parsed["cycles"][0]["committed"], json!(8));
    assert_eq!(parsed["cycles"][0]["active"], json!(true));
    assert_eq!(parsed["average"]["over"], json!(0));
    assert_eq!(parsed["average"]["work"], json!(0.0));
}

#[test]
fn sprint_velocity_averages_a_cycle_that_has_ended() {
    let server = MockLinearServer::start(vec![
        find_team_mock(),
        cycles_mock(false),
        mixed_issues_mock(),
    ]);

    let out = run_cli(
        &["sprint", "velocity", "--team", common::ENG_TEAM_KEY],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout
            .contains("average 3 points per cycle over 1 finished cycle(s)"),
        "{}",
        out.stdout
    );
}
