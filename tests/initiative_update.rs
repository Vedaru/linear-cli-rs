//! End-to-end tests for the `linear initiative-update` group (`create`/
//! `list`), run against the headless mock server.

mod common;

use common::{run_cli, MockLinearServer, MockResponse};
use serde_json::json;

/// A UUID short-circuits initiative resolution, so these tests never need a
/// resolution mock.
const INITIATIVE_ID: &str = "11111111-1111-1111-1111-111111111111";

#[test]
fn initiative_update_list_renders_health_author_and_body() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "ListInitiativeUpdates",
        json!({ "data": { "initiative": {
            "name": "Mobile",
            "slugId": "mobile",
            "initiativeUpdates": {
                "nodes": [{
                    "id": "abcdef12-3456-7890-abcd-ef1234567890",
                    "body": "Shipped the beta\nacross two platforms",
                    "health": "onTrack",
                    "url": "https://linear.app/acme/initiative/mobile/update/1",
                    "createdAt": "2024-01-01T00:00:00.000Z",
                    "user": { "name": "Ada" }
                }]
            }
        } } }),
    )]);

    let out = run_cli(
        &["initiative-update", "list", INITIATIVE_ID],
        &common::mock_env(&server),
    );
    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("Status updates for: Mobile"),
        "stdout: {}",
        out.stdout
    );
    assert!(out.stdout.contains("HEALTH"), "stdout: {}", out.stdout);
    assert!(out.stdout.contains("On Track"), "stdout: {}", out.stdout);
    assert!(out.stdout.contains("Ada"), "stdout: {}", out.stdout);
    assert!(
        out.stdout.contains("Shipped the beta across two platforms"),
        "stdout: {}",
        out.stdout
    );
    assert!(out.stdout.contains("abcdef12"), "stdout: {}", out.stdout);
}

#[test]
fn initiative_update_list_alias_and_json_returns_initiative() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "ListInitiativeUpdates",
        json!({ "data": { "initiative": {
            "name": "Mobile",
            "slugId": "mobile",
            "initiativeUpdates": { "nodes": [{
                "id": "abcdef12-3456-7890-abcd-ef1234567890",
                "body": "Beta",
                "health": "atRisk",
                "url": "https://linear.app/acme/initiative/mobile/update/1",
                "createdAt": "2024-01-01T00:00:00.000Z",
                "user": { "name": "Ada" }
            }] }
        } } }),
    )]);

    let out = run_cli(
        &["initiative-update", "l", INITIATIVE_ID, "-j"],
        &common::mock_env(&server),
    );
    assert!(out.success(), "stderr: {}", out.stderr);
    let parsed: serde_json::Value = serde_json::from_str(&out.stdout).expect("json output");
    assert_eq!(parsed["name"], "Mobile");
    assert_eq!(parsed["initiativeUpdates"]["nodes"][0]["health"], "atRisk");
}

#[test]
fn initiative_update_list_empty_prints_notice() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "ListInitiativeUpdates",
        json!({ "data": { "initiative": {
            "name": "Mobile",
            "slugId": "mobile",
            "initiativeUpdates": { "nodes": [] }
        } } }),
    )]);

    let out = run_cli(
        &["initiative-update", "list", INITIATIVE_ID],
        &common::mock_env(&server),
    );
    assert!(out.success(), "stderr: {}", out.stderr);
    assert_eq!(out.stdout.trim(), "No status updates found for: Mobile");
}

#[test]
fn initiative_update_list_missing_reports_against_raw_input() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "ListInitiativeUpdates",
        json!({ "data": { "initiative": null } }),
    )]);

    let out = run_cli(
        &["initiative-update", "list", INITIATIVE_ID],
        &common::mock_env(&server),
    );
    assert!(!out.success());
    assert!(
        out.stderr.contains("Failed to fetch initiative updates"),
        "stderr: {}",
        out.stderr
    );
    assert!(
        out.stderr
            .contains(&format!("Initiative not found: {INITIATIVE_ID}")),
        "stderr: {}",
        out.stderr
    );
}

#[test]
fn initiative_update_create_posts_input_and_reports_health() {
    let server = MockLinearServer::start(vec![
        MockResponse::new(
            "GetInitiativeNameForStatusUpdate",
            json!({ "data": { "initiative": { "name": "Mobile", "slugId": "mobile" } } }),
        ),
        MockResponse::new(
            "CreateInitiativeUpdate",
            json!({ "data": { "initiativeUpdateCreate": {
                "success": true,
                "initiativeUpdate": {
                    "id": "u1",
                    "body": "Beta shipped",
                    "health": "onTrack",
                    "url": "https://linear.app/acme/initiative/mobile/update/1",
                    "createdAt": "2024-01-01T00:00:00.000Z",
                    "initiative": { "name": "Mobile", "slugId": "mobile" }
                }
            } } }),
        )
        .with_variables(json!({
            "input": {
                "initiativeId": INITIATIVE_ID,
                "body": "Beta shipped",
                "health": "onTrack"
            }
        })),
    ]);

    let out = run_cli(
        &[
            "initiative-update",
            "create",
            INITIATIVE_ID,
            "--body",
            "Beta shipped",
            "--health",
            "onTrack",
        ],
        &common::mock_env(&server),
    );
    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("Created status update for: Mobile"),
        "stdout: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("Health: onTrack"),
        "stdout: {}",
        out.stdout
    );
    assert!(
        out.stdout
            .contains("https://linear.app/acme/initiative/mobile/update/1"),
        "stdout: {}",
        out.stdout
    );
}

#[test]
fn initiative_update_create_rejects_invalid_health() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "GetInitiativeNameForStatusUpdate",
        json!({ "data": { "initiative": { "name": "Mobile", "slugId": "mobile" } } }),
    )]);

    let out = run_cli(
        &[
            "initiative-update",
            "create",
            INITIATIVE_ID,
            "--body",
            "hi",
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
            .contains("Valid values: onTrack, atRisk, offTrack"),
        "stderr: {}",
        out.stderr
    );
}

#[test]
fn initiative_update_create_surfaces_failed_mutation() {
    let server = MockLinearServer::start(vec![
        MockResponse::new(
            "GetInitiativeNameForStatusUpdate",
            json!({ "data": { "initiative": { "name": "Mobile", "slugId": "mobile" } } }),
        ),
        MockResponse::new(
            "CreateInitiativeUpdate",
            json!({ "data": { "initiativeUpdateCreate": {
                "success": false,
                "initiativeUpdate": null
            } } }),
        ),
    ]);

    let out = run_cli(
        &["initiative-update", "create", INITIATIVE_ID, "--body", "hi"],
        &common::mock_env(&server),
    );
    assert!(!out.success());
    assert!(
        out.stderr
            .contains("Failed to create initiative status update"),
        "stderr: {}",
        out.stderr
    );
}
