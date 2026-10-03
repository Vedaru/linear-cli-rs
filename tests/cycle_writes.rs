//! `linear cycle create` and `linear cycle complete` — the two writes that turn reading cycles
//! into running them.
//!
//! The overlap check is what these tests are mostly about: it happens *before* the create, so a
//! window that clashes is refused here with the cycle named, and the tests that expect a refusal
//! configure no `CreateCycle` reply at all - a request that went out anyway would surface the
//! harness's "no mock response" error instead.

mod common;

use common::{run_cli, MockLinearServer, MockResponse};
use serde_json::{json, Value};

const CYCLE_ID: &str = "11111111-2222-3333-4444-555555555555";

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
    .with_variables(json!({ "reference": common::ENG_TEAM_KEY }))
}

fn windows_mock(cycles: Vec<Value>, cycles_enabled: bool) -> MockResponse {
    MockResponse::new(
        "GetTeamCycleWindows",
        json!({ "data": { "team": {
            "id": common::ENG_TEAM_ID,
            "key": common::ENG_TEAM_KEY,
            "name": common::ENG_TEAM_NAME,
            "cyclesEnabled": cycles_enabled,
            "cycles": {
                "nodes": cycles,
                "pageInfo": { "hasNextPage": false, "endCursor": null }
            }
        } } }),
    )
}

fn cycle(number: i64, starts: &str, ends: &str) -> Value {
    json!({
        "id": format!("cycle-{number}"),
        "number": number,
        "name": null,
        "startsAt": starts,
        "endsAt": ends,
        "completedAt": null,
        "archivedAt": null
    })
}

#[test]
fn cycle_create_posts_a_window_that_does_not_clash() {
    let server = MockLinearServer::start(vec![
        find_team_mock(),
        windows_mock(
            vec![cycle(
                7,
                "2026-09-17T00:00:00.000Z",
                "2026-09-30T00:00:00.000Z",
            )],
            true,
        ),
        MockResponse::new(
            "CreateCycle",
            json!({ "data": { "cycleCreate": { "success": true, "cycle": {
                "id": CYCLE_ID,
                "number": 8,
                "name": null,
                "startsAt": "2026-10-01T00:00:00.000Z",
                "endsAt": "2026-10-14T00:00:00.000Z",
                "isActive": false,
                "isFuture": true
            } } } }),
        )
        .with_variables(json!({ "input": {
            "teamId": common::ENG_TEAM_ID,
            "startsAt": "2026-10-01",
            "endsAt": "2026-10-14"
        } })),
    ]);

    let out = run_cli(
        &[
            "cycle",
            "create",
            "--team",
            common::ENG_TEAM_KEY,
            "--start-date",
            "2026-10-01",
            "--end-date",
            "2026-10-14",
        ],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout
            .contains("✓ Created cycle #8 in team ENG (2026-10-01 → 2026-10-14)"),
        "{}",
        out.stdout
    );
}

#[test]
fn cycle_create_refuses_a_window_that_overlaps_by_naming_the_cycle() {
    let server = MockLinearServer::start(vec![
        find_team_mock(),
        windows_mock(
            vec![cycle(
                7,
                "2026-09-20T00:00:00.000Z",
                "2026-10-04T00:00:00.000Z",
            )],
            true,
        ),
    ]);

    let out = run_cli(
        &[
            "cycle",
            "create",
            "--team",
            common::ENG_TEAM_KEY,
            "--start-date",
            "2026-10-01",
            "--end-date",
            "2026-10-14",
        ],
        &common::mock_env(&server),
    );

    assert!(!out.success());
    assert!(
        out.stderr
            .contains("overlaps cycle #7 (2026-09-20 → 2026-10-04)"),
        "stderr: {}",
        out.stderr
    );
}

#[test]
fn cycle_create_refuses_an_end_before_its_start_before_any_request() {
    let server = MockLinearServer::start(vec![]);

    let out = run_cli(
        &[
            "cycle",
            "create",
            "--team",
            common::ENG_TEAM_KEY,
            "--start-date",
            "2026-10-14",
            "--end-date",
            "2026-10-01",
        ],
        &common::mock_env(&server),
    );

    assert!(!out.success());
    assert!(
        out.stderr
            .contains("The cycle's end must be after its start"),
        "stderr: {}",
        out.stderr
    );
}

#[test]
fn cycle_create_refuses_a_team_that_does_not_use_cycles() {
    let server = MockLinearServer::start(vec![find_team_mock(), windows_mock(vec![], false)]);

    let out = run_cli(
        &[
            "cycle",
            "create",
            "--team",
            common::ENG_TEAM_KEY,
            "--start-date",
            "2026-10-01",
            "--end-date",
            "2026-10-14",
        ],
        &common::mock_env(&server),
    );

    assert!(!out.success());
    assert!(
        out.stderr.contains("Cycles are not enabled for team ENG"),
        "stderr: {}",
        out.stderr
    );
}

#[test]
fn cycle_complete_sets_completed_at() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "CompleteCycle",
        json!({ "data": { "cycleUpdate": { "success": true, "cycle": {
            "id": CYCLE_ID,
            "number": 7,
            "name": null,
            "startsAt": "2026-09-17T00:00:00.000Z",
            "endsAt": "2026-09-30T00:00:00.000Z",
            "completedAt": "2026-10-03T16:00:00.000Z"
        } } } }),
    )
    .with_variables(json!({ "id": CYCLE_ID }))]);

    let out = run_cli(
        &["cycle", "complete", CYCLE_ID, "--json"],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    let parsed: Value = serde_json::from_str(&out.stdout).expect("complete --json is JSON");
    assert_eq!(parsed["cycleUpdate"]["cycle"]["number"], json!(7));
}
