//! End-to-end tests for the `linear project` group (list/view/create/update/
//! delete and the nested `comment add`/`comment list`), run against the
//! headless mock server.

#![recursion_limit = "512"]

mod common;

use common::{run_cli, run_cli_full, MockLinearServer, MockResponse};
use serde_json::json;

const NO_CREDENTIAL_VARS: &[&str] = &["LINEAR_API_KEY", "LINEAR_TEAM_ID"];

/// A syntactically valid Linear UUID, so references resolve without an extra
/// lookup call.
const PROJECT_ID: &str = "11111111-1111-1111-1111-111111111111";

const PROJECT_NODE: fn(&str, &str) -> serde_json::Value = |slug, name| {
    json!({
        "id": format!("id-{slug}"),
        "name": name,
        "description": null,
        "slugId": slug,
        "icon": null,
        "color": "#5e6ad2",
        "sortOrder": 1.0,
        "status": { "id": "st-1", "name": "In Progress", "color": "#5e6ad2", "type": "started" },
        "lead": { "name": "Ada", "displayName": "Ada", "initials": "AD" },
        "priority": 2,
        "health": "onTrack",
        "startDate": "2024-01-01",
        "targetDate": null,
        "startedAt": "2024-01-02T00:00:00.000Z",
        "completedAt": null,
        "canceledAt": null,
        "createdAt": "2024-01-01T00:00:00.000Z",
        "updatedAt": "2024-01-02T00:00:00.000Z",
        "url": format!("https://linear.app/acme/project/{slug}"),
        "teams": { "nodes": [{ "key": "ENG" }] }
    })
};

#[test]
fn project_list_renders_columns_and_rows() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "GetProjects",
        json!({ "data": { "projects": {
            "nodes": [PROJECT_NODE("launch", "Launch")],
            "pageInfo": { "hasNextPage": false, "endCursor": null }
        } } }),
    )]);

    let out = run_cli(
        &["project", "list", "--all-teams"],
        &common::mock_env(&server),
    );
    assert!(out.success(), "stderr: {}", out.stderr);
    for header in ["SLUG", "NAME", "STATUS", "PRIORITY", "HEALTH", "LEAD", "TEAMS", "DATE"] {
        assert!(out.stdout.contains(header), "missing {header}: {}", out.stdout);
    }
    assert!(out.stdout.contains("launch"), "stdout: {}", out.stdout);
    assert!(out.stdout.contains("Launch"), "stdout: {}", out.stdout);
    assert!(out.stdout.contains("In Progress"), "stdout: {}", out.stdout);
    assert!(out.stdout.contains("ENG"), "stdout: {}", out.stdout);
}

#[test]
fn project_list_json_returns_connection() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "GetProjects",
        json!({ "data": { "projects": {
            "nodes": [PROJECT_NODE("launch", "Launch")],
            "pageInfo": { "hasNextPage": false, "endCursor": null }
        } } }),
    )]);

    let out = run_cli(
        &["project", "list", "--all-teams", "--json"],
        &common::mock_env(&server),
    );
    assert!(out.success(), "stderr: {}", out.stderr);
    let parsed: serde_json::Value = serde_json::from_str(&out.stdout).expect("json output");
    assert_eq!(parsed["nodes"][0]["slugId"], "launch");
    assert_eq!(parsed["pageInfo"]["hasNextPage"], false);
}

#[test]
fn project_list_rejects_team_with_all_teams() {
    let server = MockLinearServer::start(vec![]);
    let out = run_cli(
        &["project", "list", "--team", "ENG", "--all-teams"],
        &common::mock_env(&server),
    );
    assert!(!out.success());
    assert!(
        out.stderr.contains("Cannot use both --team and --all-teams flags"),
        "stderr: {}",
        out.stderr
    );
}

#[test]
fn project_view_prints_markdown() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "GetProjectDetails",
        json!({ "data": { "project": {
            "id": PROJECT_ID,
            "name": "Launch",
            "identifier": "LNCH",
            "description": "Ship it",
            "content": null,
            "slugId": "launch",
            "icon": null,
            "color": "#5e6ad2",
            "progress": 0.5,
            "scope": 0,
            "url": "https://linear.app/acme/project/launch",
            "priority": 2,
            "health": "onTrack",
            "healthUpdatedAt": null,
            "startDate": null,
            "startDateResolution": null,
            "targetDate": null,
            "targetDateResolution": null,
            "startedAt": null,
            "completedAt": null,
            "canceledAt": null,
            "archivedAt": null,
            "autoArchivedAt": null,
            "createdAt": "2024-01-01T00:00:00.000Z",
            "updatedAt": "2024-01-02T00:00:00.000Z",
            "status": { "id": "st-1", "name": "In Progress", "color": "#5e6ad2", "type": "started", "position": 1.0 },
            "creator": { "id": "u1", "name": "Ada", "displayName": "Ada" },
            "lead": null,
            "teams": { "nodes": [], "pageInfo": { "hasNextPage": false, "endCursor": null } },
            "labels": { "nodes": [], "pageInfo": { "hasNextPage": false, "endCursor": null } },
            "members": { "nodes": [], "pageInfo": { "hasNextPage": false, "endCursor": null } },
            "initiatives": { "nodes": [], "pageInfo": { "hasNextPage": false, "endCursor": null } },
            "projectMilestones": { "nodes": [], "pageInfo": { "hasNextPage": false, "endCursor": null } },
            "externalLinks": { "nodes": [], "pageInfo": { "hasNextPage": false, "endCursor": null } },
            "documents": { "nodes": [], "pageInfo": { "hasNextPage": false, "endCursor": null } },
            "attachments": { "nodes": [], "pageInfo": { "hasNextPage": false, "endCursor": null } },
            "relations": { "nodes": [], "pageInfo": { "hasNextPage": false, "endCursor": null } },
            "inverseRelations": { "nodes": [], "pageInfo": { "hasNextPage": false, "endCursor": null } },
            "issues": { "nodes": [], "pageInfo": { "hasNextPage": false, "endCursor": null } },
            "lastUpdate": null
        } } }),
    )]);

    // stdout is piped (not a TTY); the markdown renderer is the only branch.
    let out = run_cli(&["project", "view", PROJECT_ID], &common::mock_env(&server));
    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(out.stdout.contains("# Launch [LNCH]"), "stdout: {}", out.stdout);
    assert!(out.stdout.contains("**Status:** In Progress"), "stdout: {}", out.stdout);
    assert!(out.stdout.contains("Ship it"), "stdout: {}", out.stdout);
}

#[test]
fn project_view_missing_reports_against_raw_id() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "GetProjectDetails",
        json!({ "data": { "project": null } }),
    )]);

    let out = run_cli(
        &["project", "view", PROJECT_ID],
        &common::mock_env(&server),
    );
    assert!(!out.success());
    assert!(
        out.stderr.contains("Failed to view project"),
        "stderr: {}",
        out.stderr
    );
    assert!(
        out.stderr.contains(&format!("Project not found: {PROJECT_ID}")),
        "stderr: {}",
        out.stderr
    );
}

#[test]
fn project_create_posts_create_input() {
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
        MockResponse::new(
            "CreateProject",
            json!({ "data": { "projectCreate": {
                "success": true,
                "project": {
                    "id": PROJECT_ID,
                    "slugId": "runbook",
                    "name": "Runbook",
                    "url": "https://linear.app/acme/project/runbook"
                }
            } } }),
        )
        .with_variables(json!({
            "input": { "name": "Runbook", "teamIds": [common::ENG_TEAM_ID] }
        })),
    ]);

    let out = run_cli(
        &["project", "create", "--name", "Runbook", "--team", "ENG"],
        &common::mock_env(&server),
    );
    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("✓ Created project: Runbook"),
        "stdout: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("https://linear.app/acme/project/runbook"),
        "stdout: {}",
        out.stdout
    );
}

#[test]
fn project_update_sends_update_input() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "UpdateProject",
        json!({ "data": { "projectUpdate": {
            "success": true,
            "project": {
                "id": PROJECT_ID,
                "slugId": "launch",
                "name": "Launch v2",
                "description": null,
                "url": "https://linear.app/acme/project/launch",
                "updatedAt": "2024-02-01T00:00:00.000Z"
            }
        } } }),
    )
    .with_variables(json!({ "id": PROJECT_ID, "input": { "name": "Launch v2" } }))]);

    let out = run_cli(
        &["project", "update", PROJECT_ID, "--name", "Launch v2"],
        &common::mock_env(&server),
    );
    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("✓ Updated project: Launch v2"),
        "stdout: {}",
        out.stdout
    );
}

#[test]
fn project_delete_without_force_requires_confirmation_in_headless_run() {
    let server = MockLinearServer::start(vec![]);

    let out = run_cli_full(
        &["project", "delete", PROJECT_ID],
        &common::mock_env(&server),
        NO_CREDENTIAL_VARS,
        None,
    );
    assert!(!out.success());
    assert!(
        out.stderr.contains("Failed to delete project"),
        "stderr: {}",
        out.stderr
    );
    assert!(
        out.stderr.contains("Interactive confirmation required"),
        "stderr: {}",
        out.stderr
    );
}

#[test]
fn project_delete_with_force_deletes() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "DeleteProject",
        json!({ "data": { "projectDelete": {
            "success": true,
            "entity": { "id": PROJECT_ID, "name": "Launch" }
        } } }),
    )
    .with_variables(json!({ "id": PROJECT_ID }))]);

    let out = run_cli(
        &["project", "delete", PROJECT_ID, "--force"],
        &common::mock_env(&server),
    );
    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("✓ Deleted project: Launch"),
        "stdout: {}",
        out.stdout
    );
}

#[test]
fn project_comment_add_resolves_project_and_creates_comment() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "AddComment",
        json!({ "data": { "commentCreate": {
            "success": true,
            "comment": { "id": "c1", "url": "https://linear.app/acme/comment/c1" }
        } } }),
    )
    .with_variables(json!({
        "input": { "body": "looks good", "projectId": PROJECT_ID }
    }))]);

    let out = run_cli(
        &["project", "comment", "add", PROJECT_ID, "-b", "looks good"],
        &common::mock_env(&server),
    );
    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout
            .contains(&format!("✓ Comment added to project {PROJECT_ID}")),
        "stdout: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("https://linear.app/acme/comment/c1"),
        "stdout: {}",
        out.stdout
    );
}

#[test]
fn project_comment_list_renders_threads() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "GetProjectComments",
        json!({ "data": {
            "project": { "id": PROJECT_ID, "name": "Launch" },
            "comments": {
                "nodes": [{
                    "id": "c1",
                    "body": "First thought",
                    "quotedText": null,
                    "createdAt": "2024-01-01T00:00:00.000Z",
                    "updatedAt": "2024-01-01T00:00:00.000Z",
                    "editedAt": null,
                    "url": "https://linear.app/acme/comment/c1",
                    "user": { "id": "u1", "name": "Ada", "displayName": "Ada" },
                    "externalUser": null,
                    "botActor": null,
                    "parent": null
                }],
                "pageInfo": { "hasNextPage": false, "endCursor": null }
            }
        } }),
    )]);

    let out = run_cli(
        &["project", "comment", "list", PROJECT_ID],
        &common::mock_env(&server),
    );
    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("@Ada commented"),
        "stdout: {}",
        out.stdout
    );
    assert!(out.stdout.contains("First thought"), "stdout: {}", out.stdout);
}

#[test]
fn project_comment_list_empty_prints_notice() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "GetProjectComments",
        json!({ "data": {
            "project": { "id": PROJECT_ID, "name": "Launch" },
            "comments": { "nodes": [], "pageInfo": { "hasNextPage": false, "endCursor": null } }
        } }),
    )]);

    let out = run_cli(
        &["project", "comment", "list", PROJECT_ID],
        &common::mock_env(&server),
    );
    assert!(out.success(), "stderr: {}", out.stderr);
    assert_eq!(out.stdout.trim(), "No comments found for this project");
}
