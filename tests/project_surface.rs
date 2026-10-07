//! The project surface that only reads and writes sets: members, labels, and the two reversible
//! retirement verbs.
//!
//! The gates are the assertions here. `ProjectUpdateInput.memberIds` / `labelIds` carry the whole
//! set, so an `add` that sent only what it was given would drop everyone else - which is what the
//! expected `memberIds`/`labelIds` arrays in each mock pin.

mod common;

use common::{run_cli, MockLinearServer, MockResponse};
use serde_json::{json, Value};

const PROJECT: &str = "11111111-2222-3333-4444-555555555555";

fn members_reply(ids: &[(&str, &str, &str)]) -> MockResponse {
    let nodes: Vec<Value> = ids
        .iter()
        .map(|(id, name, email)| json!({ "id": id, "name": name, "displayName": name, "email": email }))
        .collect();
    MockResponse::new(
        "GetProjectMembers",
        json!({ "data": { "project": {
            "id": PROJECT,
            "name": "Board",
            "members": { "nodes": nodes, "pageInfo": { "hasNextPage": false, "endCursor": null } }
        } } }),
    )
}

fn labels_reply(ids: &[(&str, &str)]) -> MockResponse {
    let nodes: Vec<Value> = ids
        .iter()
        .map(|(id, name)| json!({ "id": id, "name": name, "color": "#4EA7FC" }))
        .collect();
    MockResponse::new(
        "GetProjectLabels",
        json!({ "data": { "project": {
            "id": PROJECT,
            "name": "Board",
            "labels": { "nodes": nodes, "pageInfo": { "hasNextPage": false, "endCursor": null } }
        } } }),
    )
}

fn update_reply() -> MockResponse {
    MockResponse::new(
        "UpdateProjectMembership",
        json!({ "data": { "projectUpdate": { "success": true, "project": {
            "id": PROJECT, "name": "Board", "labelIds": { "nodes": [] }, "members": { "nodes": [] }
        } } } }),
    )
}

#[test]
fn project_members_lists_names_and_emails() {
    let server = MockLinearServer::start(vec![members_reply(&[
        ("u-1", "Ada", "ada@example.com"),
        ("u-2", "Bob", "bob@example.com"),
    ])]);

    let out = run_cli(&["project", "members", PROJECT], &common::mock_env(&server));

    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(out.stdout.contains("Ada"), "{}", out.stdout);
    assert!(out.stdout.contains("bob@example.com"), "{}", out.stdout);

    let json_out = run_cli(
        &["project", "members", PROJECT, "--json"],
        &common::mock_env(&server),
    );
    let parsed: Value = serde_json::from_str(&json_out.stdout).expect("members --json is JSON");
    assert_eq!(parsed["nodes"][0]["id"], json!("u-1"));
}

#[test]
fn project_member_add_sends_the_existing_members_and_the_new_one() {
    let server = MockLinearServer::start(vec![
        members_reply(&[("u-1", "Ada", "ada@example.com")]),
        MockResponse::new(
            "LookupUser",
            json!({ "data": { "users": { "nodes": [
                { "id": "u-2", "name": "Bob", "displayName": "Bob", "email": "bob@example.com" }
            ] } } }),
        ),
        update_reply().with_variables(json!({
            "id": PROJECT,
            "input": { "memberIds": ["u-1", "u-2"] }
        })),
    ]);

    let out = run_cli(
        &["project", "member", "add", PROJECT, "Bob"],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(out.stdout.contains("✓ Added Bob"), "{}", out.stdout);
}

#[test]
fn project_member_add_reports_a_member_who_is_already_there() {
    // No write is configured: a command that sent one anyway would answer the harness's
    // "No mock response configured" error instead of this refusal.
    let server = MockLinearServer::start(vec![
        members_reply(&[("u-1", "Ada", "ada@example.com")]),
        MockResponse::new(
            "LookupUser",
            json!({ "data": { "users": { "nodes": [
                { "id": "u-1", "name": "Ada", "displayName": "Ada", "email": "ada@example.com" }
            ] } } }),
        ),
    ]);

    let out = run_cli(
        &["project", "member", "add", PROJECT, "ada@example.com"],
        &common::mock_env(&server),
    );

    assert!(!out.success());
    assert!(
        out.stderr.contains("Nothing to change"),
        "stderr: {}",
        out.stderr
    );
    assert!(out.stderr.contains("already on"), "stderr: {}", out.stderr);
}

#[test]
fn project_member_remove_sends_the_set_without_that_member() {
    let server = MockLinearServer::start(vec![
        members_reply(&[
            ("u-1", "Ada", "ada@example.com"),
            ("u-2", "Bob", "bob@example.com"),
        ]),
        MockResponse::new(
            "LookupUser",
            json!({ "data": { "users": { "nodes": [
                { "id": "u-2", "name": "Bob", "displayName": "Bob", "email": "bob@example.com" }
            ] } } }),
        ),
        update_reply().with_variables(json!({
            "id": PROJECT,
            "input": { "memberIds": ["u-1"] }
        })),
    ]);

    let out = run_cli(
        &["project", "member", "remove", PROJECT, "Bob"],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(out.stdout.contains("✓ Removed Bob from"), "{}", out.stdout);
}

#[test]
fn project_label_list_reads_the_projects_labels() {
    let server = MockLinearServer::start(vec![labels_reply(&[("l-1", "Platform")])]);

    let out = run_cli(
        &["project", "label", "list", PROJECT, "--json"],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    let parsed: Value = serde_json::from_str(&out.stdout).expect("label list --json is JSON");
    assert_eq!(parsed["nodes"][0]["name"], json!("Platform"));
}

#[test]
fn project_label_add_keeps_the_labels_it_found() {
    let server = MockLinearServer::start(vec![
        labels_reply(&[("l-1", "Platform")]),
        MockResponse::new(
            "FindProjectLabel",
            json!({ "data": { "projectLabels": { "nodes": [
                { "id": "l-2", "name": "Customer", "color": "#4EA7FC" }
            ] } } }),
        ),
        update_reply().with_variables(json!({
            "id": PROJECT,
            "input": { "labelIds": ["l-1", "l-2"] }
        })),
    ]);

    let out = run_cli(
        &["project", "label", "add", PROJECT, "Customer"],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(out.stdout.contains("✓ Added Customer on"), "{}", out.stdout);
}

#[test]
fn project_label_remove_sends_the_labels_without_that_one() {
    let server = MockLinearServer::start(vec![
        labels_reply(&[("l-1", "Platform"), ("l-2", "Customer")]),
        MockResponse::new(
            "FindProjectLabel",
            json!({ "data": { "projectLabels": { "nodes": [
                { "id": "l-2", "name": "Customer", "color": "#4EA7FC" }
            ] } } }),
        ),
        update_reply().with_variables(json!({
            "id": PROJECT,
            "input": { "labelIds": ["l-1"] }
        })),
    ]);

    let out = run_cli(
        &["project", "label", "remove", PROJECT, "Customer"],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("✓ Removed Customer on"),
        "{}",
        out.stdout
    );
}

/// `set` is the only verb in the group that can drop a label nobody named, so it is the only one
/// that asks - and off a terminal it refuses rather than waiting for an answer.
#[test]
fn project_label_set_asks_before_replacing_the_set() {
    let server = MockLinearServer::start(vec![]);

    let out = run_cli(
        &["project", "label", "set", PROJECT, "Customer"],
        &common::mock_env(&server),
    );

    assert!(!out.success());
    assert!(
        out.stderr.contains("Interactive confirmation required"),
        "stderr: {}",
        out.stderr
    );
}

#[test]
fn project_label_set_with_force_replaces_the_whole_set() {
    let server = MockLinearServer::start(vec![
        labels_reply(&[("l-1", "Platform")]),
        MockResponse::new(
            "FindProjectLabel",
            json!({ "data": { "projectLabels": { "nodes": [
                { "id": "l-2", "name": "Customer", "color": "#4EA7FC" }
            ] } } }),
        ),
        // Exactly one id: the label that was there is gone, which is what `set` means and what
        // `add`/`remove` are for avoiding.
        update_reply().with_variables(json!({
            "id": PROJECT,
            "input": { "labelIds": ["l-2"] }
        })),
    ]);

    let out = run_cli(
        &["project", "label", "set", PROJECT, "Customer", "--force"],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(out.stdout.contains("✓ Set Customer on"), "{}", out.stdout);
}

#[test]
fn project_archive_reports_the_way_back() {
    // Addressed by name, not UUID: the hint must name the UUID, because a slug/name stops
    // resolving once the project is archived (VED-483).
    let server = MockLinearServer::start(vec![
        MockResponse::new(
            "GetProjectByName",
            json!({ "data": { "projects": { "nodes": [
                { "id": PROJECT, "name": "Board" }
            ] } } }),
        ),
        MockResponse::new(
            "ArchiveProject",
            json!({ "data": { "projectArchive": { "success": true, "entity": {
                "id": PROJECT, "name": "Board"
            } } } }),
        )
        .with_variables(json!({ "id": PROJECT, "trash": false })),
    ]);

    let out = run_cli(&["project", "archive", "Board"], &common::mock_env(&server));

    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("✓ Archived project: Board"),
        "{}",
        out.stdout
    );
    assert!(
        out.stdout
            .contains(&format!("linear project unarchive {PROJECT}")),
        "the hint must be the UUID, not the name: {}",
        out.stdout
    );
}

#[test]
fn project_archive_trash_uses_the_successors_behaviour_by_name() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "ArchiveProject",
        json!({ "data": { "projectArchive": { "success": true, "entity": {
            "id": PROJECT, "name": "Board"
        } } } }),
    )
    .with_variables(json!({ "id": PROJECT, "trash": true }))]);

    let out = run_cli(
        &["project", "archive", PROJECT, "--trash", "--json"],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    let parsed: Value = serde_json::from_str(&out.stdout).expect("archive --json is JSON");
    assert_eq!(parsed["projectArchive"]["success"], json!(true));
}

#[test]
fn project_unarchive_restores_a_trashed_project() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "UnarchiveProject",
        json!({ "data": { "projectUnarchive": { "success": true, "entity": {
            "id": PROJECT, "name": "Board"
        } } } }),
    )
    .with_variables(json!({ "id": PROJECT }))]);

    let out = run_cli(
        &["project", "unarchive", PROJECT],
        &common::mock_env(&server),
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("✓ Unarchived project: Board"),
        "{}",
        out.stdout
    );
}
