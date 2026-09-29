//! `linear cycle update` / `archive`, run against the headless mock server.
//!
//! Both are API-only: upstream's cycle group is list/view, so the CLI could read
//! a cycle but never change or retire one (`cycleUpdate`, `cycleArchive`). A
//! UUID reference skips team resolution entirely, which is what these tests use.

mod common;

use common::{run_cli, MockLinearServer, MockResponse};
use serde_json::json;

const CYCLE_ID: &str = "11111111-1111-1111-1111-111111111111";

#[test]
fn cycle_update_sends_the_new_name_and_dates() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "UpdateCycle",
        json!({ "data": { "cycleUpdate": {
            "success": true,
            "cycle": {
                "id": CYCLE_ID, "number": 4, "name": "Sprint 4",
                "startsAt": "2024-02-01T00:00:00.000Z",
                "endsAt": "2024-02-14T00:00:00.000Z"
            }
        } } }),
    )
    .with_variables(json!({
        "id": CYCLE_ID,
        "input": { "name": "Sprint 4", "endsAt": "2024-02-14" }
    }))]);

    let out = run_cli(
        &[
            "cycle",
            "update",
            CYCLE_ID,
            "--name",
            "Sprint 4",
            "--end-date",
            "2024-02-14",
        ],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("✓ Updated cycle: Sprint 4"),
        "stdout: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("End: 2024-02-14"),
        "stdout: {}",
        out.stdout
    );
}

#[test]
fn cycle_update_without_changes_is_rejected() {
    let server = MockLinearServer::start(vec![]);

    let out = run_cli(&["cycle", "update", CYCLE_ID], &common::mock_env(&server));

    assert!(!out.success());
    assert!(
        out.stderr.contains("Nothing to update"),
        "stderr: {}",
        out.stderr
    );
}

#[test]
fn cycle_archive_asks_before_an_irreversible_call() {
    let server = MockLinearServer::start(vec![]);

    // No `--confirm`, no terminal: the API has no cycleUnarchive, so the command
    // must never archive on a guess.
    let out = run_cli(&["cycle", "archive", CYCLE_ID], &common::mock_env(&server));

    assert!(!out.success());
    assert!(
        out.stderr.contains("Interactive confirmation required"),
        "stderr: {}",
        out.stderr
    );
}

#[test]
fn cycle_archive_confirmed_reports_the_archived_cycle() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "ArchiveCycle",
        json!({ "data": { "cycleArchive": {
            "success": true,
            "entity": { "id": CYCLE_ID, "number": 4, "name": "Sprint 4" }
        } } }),
    )
    .with_variables(json!({ "id": CYCLE_ID }))]);

    let out = run_cli(
        &["cycle", "archive", CYCLE_ID, "--confirm"],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("✓ Archived cycle: Sprint 4"),
        "stdout: {}",
        out.stdout
    );
}
