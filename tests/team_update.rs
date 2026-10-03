//! `linear team` writes: `update` (new here - upstream can create and delete a team but never
//! correct one), plus the `--json` half of `create` and `delete` that VED-53's "done when" asks
//! for: a throwaway team created, read back and deleted without screen-scraping.

mod common;

use common::{run_cli, MockLinearServer, MockResponse};
use serde_json::{json, Value};

fn find_team_mock(reference: &str) -> MockResponse {
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
    .with_variables(json!({ "reference": reference }))
}

#[test]
fn team_update_sends_only_the_fields_given_and_names_them() {
    let server = MockLinearServer::start(vec![
        find_team_mock(common::ENG_TEAM_KEY),
        MockResponse::new(
            "UpdateTeam",
            json!({ "data": { "teamUpdate": {
                "success": true,
                "team": {
                    "id": common::ENG_TEAM_ID,
                    "key": common::ENG_TEAM_KEY,
                    "name": "Engineering Team",
                    "description": null,
                    "private": false,
                    "timezone": null
                }
            } } }),
        )
        // The gate is the assertion: an update that also sent a description, a key or a
        // visibility would not match this request.
        .with_variables(json!({
            "id": common::ENG_TEAM_ID,
            "input": { "name": "Engineering Team" }
        })),
    ]);

    let out = run_cli(
        &["team", "update", "ENG", "--name", "Engineering Team"],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("✓ Updated team ENG: Engineering Team"),
        "{}",
        out.stdout
    );
    assert!(out.stdout.contains("Changed: name"), "{}", out.stdout);
}

#[test]
fn team_update_sends_an_explicit_public_visibility() {
    let server = MockLinearServer::start(vec![
        find_team_mock(common::ENG_TEAM_KEY),
        MockResponse::new(
            "UpdateTeam",
            json!({ "data": { "teamUpdate": { "success": true, "team": {
                "id": common::ENG_TEAM_ID,
                "key": common::ENG_TEAM_KEY,
                "name": common::ENG_TEAM_NAME,
                "description": null,
                "private": true,
                "timezone": null
            } } } }),
        )
        .with_variables(json!({
            "id": common::ENG_TEAM_ID,
            "input": { "private": true }
        })),
    ]);

    let out = run_cli(
        &["team", "update", "ENG", "--private"],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("visibility (private)"),
        "{}",
        out.stdout
    );
}

#[test]
fn team_update_with_nothing_to_change_is_refused() {
    let server = MockLinearServer::start(vec![]);

    let out = run_cli(&["team", "update", "ENG"], &common::mock_env(&server));

    assert!(!out.success());
    assert!(
        out.stderr.contains("Nothing to update"),
        "stderr: {}",
        out.stderr
    );
}

#[test]
fn team_update_answers_json_with_the_api_payload() {
    let server = MockLinearServer::start(vec![
        find_team_mock(common::ENG_TEAM_KEY),
        MockResponse::new(
            "UpdateTeam",
            json!({ "data": { "teamUpdate": { "success": true, "team": {
                "id": common::ENG_TEAM_ID,
                "key": "ENGX",
                "name": "Engineering",
                "description": null,
                "private": false,
                "timezone": "Europe/Berlin"
            } } } }),
        )
        .with_variables(json!({ "id": common::ENG_TEAM_ID, "input": { "key": "ENGX" } })),
    ]);

    let out = run_cli(
        &["team", "update", "ENG", "--key", "ENGX", "--json"],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    let parsed: Value = serde_json::from_str(&out.stdout).expect("update --json is JSON");
    assert_eq!(parsed["teamUpdate"]["team"]["key"], json!("ENGX"));
}

#[test]
fn team_create_answers_json_with_the_resolved_key() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "CreateTeam",
        json!({ "data": { "teamCreate": { "success": true, "team": {
            "id": "team-new-id",
            "name": "Throwaway",
            "key": "THR"
        } } } }),
    )
    .with_variables(json!({ "input": { "name": "Throwaway" } }))]);

    let out = run_cli(
        &["team", "create", "--name", "Throwaway", "--json"],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    let parsed: Value = serde_json::from_str(&out.stdout).expect("create --json is JSON");
    assert_eq!(parsed["teamCreate"]["team"]["key"], json!("THR"));
}

#[test]
fn team_delete_with_force_answers_json() {
    let server = MockLinearServer::start(vec![
        find_team_mock(common::ENG_TEAM_KEY),
        MockResponse::new(
            "GetTeamDetails",
            json!({ "data": { "team": {
                "id": common::ENG_TEAM_ID,
                "key": common::ENG_TEAM_KEY,
                "name": common::ENG_TEAM_NAME,
                "issues": { "nodes": [] }
            } } }),
        ),
        MockResponse::new(
            "DeleteTeam",
            json!({ "data": { "teamDelete": { "success": true, "entityId": common::ENG_TEAM_ID } } }),
        )
        .with_variables(json!({ "id": common::ENG_TEAM_ID })),
    ]);

    let out = run_cli(
        &["team", "delete", "ENG", "--force", "--json"],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    let parsed: Value = serde_json::from_str(&out.stdout).expect("delete --json is JSON");
    assert_eq!(parsed["teamDelete"]["success"], json!(true));
    assert_eq!(parsed["teamDelete"]["entityId"], json!(common::ENG_TEAM_ID));
}
