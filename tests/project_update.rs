//! End-to-end tests for the `linear project-update` group (`create`/`list`),
//! run against the headless mock server.

mod common;

use common::{run_cli, MockLinearServer, MockResponse};
use serde_json::json;

/// A UUID so `resolve_project_id` short-circuits without a name lookup.
const PROJECT_UUID: &str = "11111111-2222-3333-4444-555555555555";

fn update_node(id: &str, health: &str, body: &str, author: &str) -> serde_json::Value {
    json!({
        "id": id,
        "body": body,
        "health": health,
        "url": format!("https://linear.app/acme/project-update/{id}"),
        "createdAt": "2024-01-01T00:00:00.000Z",
        "user": { "name": author, "displayName": author }
    })
}

#[test]
fn project_update_list_renders_health_date_author_and_body() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "ListProjectUpdates",
        json!({ "data": { "project": {
            "name": "Launch",
            "slugId": "launch",
            "projectUpdates": {
                "nodes": [update_node("abcdef123456", "onTrack", "Shipping on time", "Ada")],
                "pageInfo": { "hasNextPage": false, "endCursor": null }
            }
        } } }),
    )]);

    let out = run_cli(
        &["project-update", "list", PROJECT_UUID],
        &common::mock_env(&server),
    );
    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("Status updates for: Launch"),
        "stdout: {}",
        out.stdout
    );
    assert!(out.stdout.contains("HEALTH"), "stdout: {}", out.stdout);
    assert!(out.stdout.contains("AUTHOR"), "stdout: {}", out.stdout);
    assert!(out.stdout.contains("onTrack"), "stdout: {}", out.stdout);
    assert!(out.stdout.contains("Ada"), "stdout: {}", out.stdout);
    assert!(
        out.stdout.contains("Shipping on time"),
        "stdout: {}",
        out.stdout
    );
}

#[test]
fn project_update_list_alias_and_json() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "ListProjectUpdates",
        json!({ "data": { "project": {
            "name": "Launch",
            "slugId": "launch",
            "projectUpdates": {
                "nodes": [update_node("abcdef123456", "atRisk", "Slip", "Ada")],
                "pageInfo": { "hasNextPage": false, "endCursor": null }
            }
        } } }),
    )]);

    let out = run_cli(
        &["project-update", "l", PROJECT_UUID, "--json"],
        &common::mock_env(&server),
    );
    assert!(out.success(), "stderr: {}", out.stderr);
    let parsed: serde_json::Value = serde_json::from_str(&out.stdout).expect("json output");
    assert_eq!(parsed["name"], "Launch");
    assert_eq!(parsed["projectUpdates"]["nodes"][0]["health"], "atRisk");
}

#[test]
fn project_update_list_empty_prints_notice() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "ListProjectUpdates",
        json!({ "data": { "project": {
            "name": "Launch",
            "slugId": "launch",
            "projectUpdates": {
                "nodes": [],
                "pageInfo": { "hasNextPage": false, "endCursor": null }
            }
        } } }),
    )]);

    let out = run_cli(
        &["project-update", "list", PROJECT_UUID],
        &common::mock_env(&server),
    );
    assert!(out.success(), "stderr: {}", out.stderr);
    assert_eq!(
        out.stdout.trim(),
        "No status updates found for project: Launch"
    );
}

#[test]
fn project_update_list_missing_project_reports_against_input() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "ListProjectUpdates",
        json!({ "data": { "project": null } }),
    )]);

    let out = run_cli(
        &["project-update", "list", PROJECT_UUID],
        &common::mock_env(&server),
    );
    assert!(!out.success());
    assert!(
        out.stderr.contains("Failed to fetch project updates"),
        "stderr: {}",
        out.stderr
    );
    assert!(
        out.stderr
            .contains(&format!("Project not found: {PROJECT_UUID}")),
        "stderr: {}",
        out.stderr
    );
}

#[test]
fn project_update_create_posts_input_and_reports_health() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "CreateProjectUpdate",
        json!({ "data": { "projectUpdateCreate": {
            "success": true,
            "projectUpdate": {
                "id": "pu-1",
                "body": "hello",
                "health": "onTrack",
                "url": "https://linear.app/acme/project-update/pu-1",
                "createdAt": "2024-01-01T00:00:00.000Z",
                "project": { "name": "Launch", "slugId": "launch" }
            }
        } } }),
    )
    .with_variables(json!({
        "input": { "projectId": PROJECT_UUID, "body": "hello", "health": "onTrack" }
    }))]);

    let out = run_cli(
        &[
            "project-update",
            "create",
            PROJECT_UUID,
            "--body",
            "hello",
            "--health",
            "onTrack",
        ],
        &common::mock_env(&server),
    );
    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("Created status update for: Launch"),
        "stdout: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("Health: onTrack"),
        "stdout: {}",
        out.stdout
    );
}

#[test]
fn project_update_create_rejects_invalid_health() {
    let server = MockLinearServer::start(vec![]);
    let out = run_cli(
        &[
            "project-update",
            "create",
            PROJECT_UUID,
            "--body",
            "hello",
            "--health",
            "bogus",
        ],
        &common::mock_env(&server),
    );
    assert!(!out.success());
    assert!(
        out.stderr.contains("Invalid health value: bogus"),
        "stderr: {}",
        out.stderr
    );
    assert!(
        out.stderr
            .contains("Must be one of: onTrack, atRisk, offTrack"),
        "stderr: {}",
        out.stderr
    );
}

#[test]
fn project_update_create_surfaces_failed_mutation() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "CreateProjectUpdate",
        json!({ "data": { "projectUpdateCreate": {
            "success": false,
            "projectUpdate": null
        } } }),
    )]);

    let out = run_cli(
        &["project-update", "create", PROJECT_UUID, "--body", "hello"],
        &common::mock_env(&server),
    );
    assert!(!out.success());
    assert!(
        out.stderr.contains("Failed to create project update"),
        "stderr: {}",
        out.stderr
    );
}
