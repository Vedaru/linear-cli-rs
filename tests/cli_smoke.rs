//! End-to-end smoke tests: run the real `linear` binary against the headless
//! mock server and assert on its output. These are the first slice of the
//! ported upstream command suites (see `test/commands/*.test.ts` upstream).

mod common;

use common::{run_cli, run_cli_full, MockLinearServer, MockResponse};
use serde_json::{json, Value};

/// Environment that hides any developer credentials on the machine.
fn clean_env(home: &str) -> Vec<(String, String)> {
    vec![
        ("HOME".to_string(), home.to_string()),
        ("LINEAR_IGNORE_ENV_FILE".to_string(), "1".to_string()),
    ]
}

const NO_CREDENTIAL_VARS: &[&str] = &["LINEAR_API_KEY", "LINEAR_TEAM_ID"];

#[test]
fn auth_token_prints_environment_key() {
    let dir = tempfile::tempdir().unwrap();
    let mut env = clean_env(dir.path().to_str().unwrap());
    env.push(("LINEAR_API_KEY".to_string(), "abc123".to_string()));

    let out = run_cli_full(&["auth", "token"], &env, NO_CREDENTIAL_VARS, None);
    assert!(out.success(), "stderr: {}", out.stderr);
    assert_eq!(out.stdout.trim(), "abc123");
}

#[test]
fn auth_list_reports_no_workspaces() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_cli_full(
        &["auth", "list"],
        &clean_env(dir.path().to_str().unwrap()),
        NO_CREDENTIAL_VARS,
        None,
    );
    assert!(out.success(), "stderr: {}", out.stderr);
    assert_eq!(
        out.stdout.trim(),
        "No workspaces configured\nRun `linear auth login` to add a workspace"
    );
}

#[test]
fn auth_whoami_without_key_explains_how_to_configure() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_cli_full(
        &["auth", "whoami"],
        &clean_env(dir.path().to_str().unwrap()),
        NO_CREDENTIAL_VARS,
        None,
    );
    assert!(!out.success());
    assert!(
        out.stderr.contains("No API key configured"),
        "stderr: {}",
        out.stderr
    );
    assert!(out.stderr.contains("LINEAR_API_KEY"), "stderr: {}", out.stderr);
}

fn members_response(nodes: Value) -> Value {
    json!({
        "data": { "viewer": { "organization": { "users": {
            "nodes": nodes,
            "pageInfo": { "hasNextPage": false, "endCursor": null }
        } } } }
    })
}

fn member(id: &str, name: &str, active: bool) -> Value {
    json!({
        "id": id,
        "name": name,
        "displayName": name,
        "email": format!("{}@example.com", name.to_lowercase()),
        "active": active,
        "initials": "XX",
        "description": null,
        "timezone": "UTC",
        "lastSeen": null,
        "statusEmoji": null,
        "statusLabel": null,
        "guest": false,
        "isAssignable": true,
        "admin": false,
        "owner": false,
        "isMe": false,
        "url": format!("https://linear.app/example/profiles/{id}")
    })
}

#[test]
fn user_list_json_returns_members() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "GetOrganizationMembers",
        members_response(json!([member("u1", "Ada", true), member("u2", "Grace", true)])),
    )]);

    let out = run_cli(&["user", "list", "--json"], &common::mock_env(&server));
    assert!(out.success(), "stderr: {}", out.stderr);

    let parsed: Value = serde_json::from_str(out.stdout.trim()).expect("json output");
    let nodes = parsed["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 2);
    assert_eq!(nodes[0]["name"], "Ada");
    assert_eq!(parsed["pageInfo"]["hasNextPage"], false);
}

#[test]
fn user_list_filters_inactive_members_without_all() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "GetOrganizationMembers",
        members_response(json!([member("u1", "Ada", true), member("u2", "Grace", false)])),
    )]);

    let out = run_cli(&["user", "list"], &common::mock_env(&server));
    assert!(out.success(), "stderr: {}", out.stderr);

    assert!(out.stdout.contains("Ada"), "stdout: {}", out.stdout);
    assert!(!out.stdout.contains("Grace"), "stdout: {}", out.stdout);
}

#[test]
fn user_list_all_includes_inactive_members() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "GetOrganizationMembers",
        members_response(json!([member("u1", "Ada", true), member("u2", "Grace", false)])),
    )]);

    let out = run_cli(&["user", "list", "--all"], &common::mock_env(&server));
    assert!(out.success(), "stderr: {}", out.stderr);

    assert!(out.stdout.contains("Ada"));
    assert!(out.stdout.contains("Grace"));
}

fn team(id: &str, name: &str, key: &str, archived: bool) -> Value {
    json!({
        "id": id,
        "name": name,
        "key": key,
        "description": null,
        "icon": null,
        "color": "#5e6ad2",
        "cyclesEnabled": true,
        "createdAt": "2024-01-01T00:00:00.000Z",
        "updatedAt": "2024-01-01T00:00:00.000Z",
        "archivedAt": if archived { json!("2024-02-01T00:00:00.000Z") } else { Value::Null },
        "organization": { "id": "org1", "name": "Example" }
    })
}

#[test]
fn team_list_json_filters_archived_and_sorts_by_name() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "GetTeams",
        json!({ "data": { "teams": {
            "nodes": [
                team("t2", "Zeta", "Z", false),
                team("t3", "Old", "OLD", true),
                team("t1", "Alpha", "A", false)
            ],
            "pageInfo": { "hasNextPage": false, "endCursor": null }
        } } }),
    )]);

    let out = run_cli(&["team", "list", "--json"], &common::mock_env(&server));
    assert!(out.success(), "stderr: {}", out.stderr);

    let parsed: Value = serde_json::from_str(out.stdout.trim()).expect("json output");
    let names: Vec<&str> = parsed["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| node["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["Alpha", "Zeta"]);
}

#[test]
fn team_list_empty_prints_notice() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "GetTeams",
        json!({ "data": { "teams": {
            "nodes": [],
            "pageInfo": { "hasNextPage": false, "endCursor": null }
        } } }),
    )]);

    let out = run_cli(&["team", "list"], &common::mock_env(&server));
    assert!(out.success(), "stderr: {}", out.stderr);
    assert_eq!(out.stdout.trim(), "No teams found.");
}

fn cycle(number: i64, name: &str, starts: &str, is_active: bool, is_past: bool) -> Value {
    json!({
        "id": format!("cycle-{number}"),
        "number": number,
        "name": name,
        "startsAt": starts,
        "endsAt": starts,
        "completedAt": if is_past { json!("2024-01-19T00:00:00.000Z") } else { Value::Null },
        "isActive": is_active,
        "isFuture": false,
        "isPast": is_past
    })
}

#[test]
fn cycle_list_resolves_team_and_sorts_recent_first() {
    let server = MockLinearServer::start(vec![
        MockResponse::new(
            "FindTeam",
            json!({ "data": { "teams": { "nodes": [
                { "id": common::ENG_TEAM_ID, "key": common::ENG_TEAM_KEY, "name": common::ENG_TEAM_NAME }
            ] } } }),
        )
        .with_variables(json!({ "reference": common::ENG_TEAM_KEY })),
        MockResponse::new(
            "GetTeamCycles",
            json!({ "data": { "team": {
                "id": common::ENG_TEAM_ID,
                "name": common::ENG_TEAM_NAME,
                "cycles": {
                    "nodes": [
                        cycle(1, "Cycle 1", "2024-01-05T00:00:00.000Z", false, true),
                        cycle(3, "Cycle 3", "2024-02-02T00:00:00.000Z", true, false)
                    ],
                    "pageInfo": { "hasNextPage": false, "endCursor": null }
                }
            } } }),
        )
        .with_variables(json!({ "teamId": common::ENG_TEAM_ID })),
    ]);

    let out = run_cli(
        &["cycle", "list", "--team", common::ENG_TEAM_KEY, "--json"],
        &common::mock_env(&server),
    );
    assert!(out.success(), "stderr: {}", out.stderr);

    let parsed: Value = serde_json::from_str(out.stdout.trim()).expect("json output");
    let numbers: Vec<i64> = parsed["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| node["number"].as_i64().unwrap())
        .collect();
    assert_eq!(numbers, vec![3, 1]);
}

#[test]
fn cycle_list_unknown_team_is_not_found() {
    let server = MockLinearServer::start(vec![
        MockResponse::new(
            "FindTeam",
            json!({ "data": { "teams": { "nodes": [] }, "teamById": { "nodes": [] } } }),
        )
        .with_variables(json!({ "reference": "NOPE" })),
        MockResponse::new(
            "GetAllTeams",
            json!({ "data": { "teams": { "nodes": [] } } }),
        ),
    ]);

    let out = run_cli(
        &["cycle", "list", "--team", "NOPE"],
        &common::mock_env(&server),
    );
    assert!(!out.success());
    assert!(
        out.stderr.contains("NOPE"),
        "stderr should name the missing team: {}",
        out.stderr
    );
}

fn label(id: &str, name: &str, team_key: Option<&str>) -> Value {
    json!({
        "id": id,
        "name": name,
        "description": null,
        "color": "#eb5757",
        "team": team_key.map(|key| json!({ "key": key, "name": "Team" }))
    })
}

#[test]
fn label_list_all_json_sorts_by_name() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "GetIssueLabels",
        json!({ "data": { "issueLabels": {
            "nodes": [
                label("l2", "zeta", None),
                label("l1", "Alpha", Some(common::ENG_TEAM_KEY))
            ],
            "pageInfo": { "hasNextPage": false, "endCursor": null }
        } } }),
    )]);

    let out = run_cli(&["label", "list", "--all", "--json"], &common::mock_env(&server));
    assert!(out.success(), "stderr: {}", out.stderr);

    let parsed: Value = serde_json::from_str(out.stdout.trim()).expect("json output");
    let names: Vec<&str> = parsed["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| node["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["Alpha", "zeta"]);
}

#[test]
fn issue_relation_add_reports_created_relation_in_user_order() {
    let server = MockLinearServer::start(vec![
        MockResponse::new(
            "GetIssueId",
            json!({ "data": { "issue": { "id": "issue-id-123" } } }),
        )
        .with_variables(json!({ "id": "ENG-123" })),
        MockResponse::new(
            "GetIssueId",
            json!({ "data": { "issue": { "id": "issue-id-456" } } }),
        )
        .with_variables(json!({ "id": "ENG-456" })),
        MockResponse::new(
            "CreateIssueRelation",
            json!({ "data": { "issueRelationCreate": {
                "success": true,
                "issueRelation": { "id": "relation-id-2" }
            } } }),
        ),
    ]);

    let out = run_cli(
        &["issue", "relation", "add", "ENG-123", "blocked-by", "ENG-456"],
        &common::mock_env(&server),
    );
    assert!(out.success(), "stderr: {}", out.stderr);
    assert_eq!(
        out.stdout.trim(),
        "✓ Created relation: ENG-123 blocked-by ENG-456"
    );
}

#[test]
fn label_list_team_applies_or_filter_with_workspace_labels() {
    let server = MockLinearServer::start(vec![
        MockResponse::new(
            "FindTeam",
            json!({ "data": { "teams": { "nodes": [
                { "id": common::ENG_TEAM_ID, "key": common::ENG_TEAM_KEY, "name": common::ENG_TEAM_NAME }
            ] } } }),
        )
        .with_variables(json!({ "reference": common::ENG_TEAM_KEY })),
        MockResponse::new(
            "GetIssueLabels",
            json!({ "data": { "issueLabels": {
                "nodes": [label("l1", "Bug", Some(common::ENG_TEAM_KEY))],
                "pageInfo": { "hasNextPage": false, "endCursor": null }
            } } }),
        )
        .with_variables(json!({
            "filter": { "or": [
                { "team": { "key": { "eq": common::ENG_TEAM_KEY } } },
                { "team": { "null": true } }
            ] },
            "first": 100
        })),
    ]);

    let out = run_cli(
        &["label", "list", "--team", common::ENG_TEAM_KEY],
        &common::mock_env(&server),
    );
    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(out.stdout.contains("Bug"), "stdout: {}", out.stdout);
    assert!(out.stdout.contains("1 labels found."), "stdout: {}", out.stdout);
}

/// A single `FetchIssues` issue node, shaped like the fields the port reads.
fn issue_node(identifier: &str, title: &str, assignee: Option<&str>) -> Value {
    json!({
        "id": format!("id-{identifier}"),
        "identifier": identifier,
        "title": title,
        "priority": 2,
        "priorityLabel": "High",
        "estimate": 3,
        "url": format!("https://linear.app/example/issue/{identifier}"),
        "createdAt": "2024-01-01T00:00:00.000Z",
        "updatedAt": "2024-01-02T00:00:00.000Z",
        "state": { "id": "state-1", "name": "In Progress", "type": "started",
                   "color": "#f2c94c", "position": 1.0 },
        "assignee": assignee.map(|name| json!({
            "id": "user-1", "name": name, "displayName": name,
            "initials": name.chars().take(2).collect::<String>(),
            "avatarUrl": null
        })),
        "team": { "id": common::ENG_TEAM_ID, "key": common::ENG_TEAM_KEY,
                  "name": common::ENG_TEAM_NAME, "cyclesEnabled": false,
                  "activeCycle": null },
        "project": null,
        "projectMilestone": null,
        "cycle": null,
        "labels": { "nodes": [] },
        "inverseRelations": { "nodes": [] }
    })
}

fn issues_response(nodes: Vec<Value>) -> Value {
    json!({ "data": { "issues": {
        "nodes": nodes,
        "pageInfo": { "hasNextPage": false, "endCursor": null }
    } } })
}

#[test]
fn issue_query_all_teams_json_returns_fetched_issues() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "FetchIssues",
        issues_response(vec![
            issue_node("ENG-1", "First issue", Some("Ada Lovelace")),
            issue_node("ENG-2", "Second issue", None),
        ]),
    )]);

    let out = run_cli(
        &["issue", "query", "--all-teams", "--json"],
        &common::mock_env(&server),
    );
    assert!(out.success(), "stderr: {}", out.stderr);

    let parsed: Value = serde_json::from_str(out.stdout.trim()).expect("json output");
    let nodes = parsed["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 2);
    assert_eq!(nodes[0]["identifier"], "ENG-1");
    assert_eq!(nodes[1]["assignee"], Value::Null);
}

/// `--include-archived` has to reach the API on the filter path too.
///
/// The `FetchIssues` document used to declare no `$includeArchived` variable and
/// never pass one to `issues(...)`, while the request sent `includeArchived`
/// anyway: Linear ignores a variable the operation does not declare, so archived
/// issues stayed hidden unless `--search` (whose document did declare it) was
/// also given. Gating each reply on the document carrying the variable and the
/// argument is how the harness pins it — while the argument is missing, neither
/// reply matches and the run fails.
#[test]
fn issue_query_include_archived_reaches_the_filter_path() {
    let server = MockLinearServer::start(vec![
        MockResponse::new(
            "FetchIssues",
            issues_response(vec![issue_node("ENG-9", "Archived issue", None)]),
        )
        .with_query_includes("includeArchived: $includeArchived")
        .with_variables(json!({ "includeArchived": true })),
        MockResponse::new(
            "FetchIssues",
            issues_response(vec![issue_node("ENG-1", "Live issue", None)]),
        )
        .with_query_includes("includeArchived: $includeArchived")
        .with_variables(json!({ "includeArchived": false })),
    ]);

    let archived = run_cli(
        &["issue", "query", "--all-teams", "--include-archived", "--json"],
        &common::mock_env(&server),
    );
    assert!(archived.success(), "stderr: {}", archived.stderr);
    let parsed: Value = serde_json::from_str(archived.stdout.trim()).expect("json output");
    let identifiers: Vec<&str> = parsed["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| node["identifier"].as_str().unwrap())
        .collect();
    assert_eq!(identifiers, ["ENG-9"], "stdout: {}", archived.stdout);

    let live = run_cli(
        &["issue", "query", "--all-teams", "--json"],
        &common::mock_env(&server),
    );
    assert!(live.success(), "stderr: {}", live.stderr);
    let parsed: Value = serde_json::from_str(live.stdout.trim()).expect("json output");
    let identifiers: Vec<&str> = parsed["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| node["identifier"].as_str().unwrap())
        .collect();
    assert_eq!(identifiers, ["ENG-1"], "stdout: {}", live.stdout);
}

#[test]
fn issue_query_empty_prints_notice() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "FetchIssues",
        issues_response(vec![]),
    )]);

    let out = run_cli(
        &["issue", "query", "--all-teams"],
        &common::mock_env(&server),
    );
    assert!(out.success(), "stderr: {}", out.stderr);
    assert_eq!(out.stdout.trim(), "No issues found.");
}

#[test]
fn issue_query_renders_table_with_identifier_and_title() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "FetchIssues",
        issues_response(vec![issue_node("ENG-7", "Ship the port", Some("Grace Hopper"))]),
    )]);

    let out = run_cli(
        &["issue", "query", "--all-teams"],
        &common::mock_env(&server),
    );
    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(out.stdout.contains("ENG-7"), "stdout: {}", out.stdout);
    assert!(out.stdout.contains("Ship the port"), "stdout: {}", out.stdout);
    assert!(out.stdout.contains("UPDATED"), "stdout: {}", out.stdout);
}

#[test]
fn issue_query_rejects_team_with_all_teams() {
    let server = MockLinearServer::start(vec![]);
    let out = run_cli(
        &["issue", "query", "--team", common::ENG_TEAM_KEY, "--all-teams"],
        &common::mock_env(&server),
    );
    assert!(!out.success());
    assert!(
        out.stderr.contains("--team and --all-teams"),
        "stderr: {}",
        out.stderr
    );
}

#[test]
fn issue_mine_resolves_team_and_renders_assigned_issues() {
    let server = MockLinearServer::start(vec![
        MockResponse::new(
            "FindTeam",
            json!({ "data": { "teams": { "nodes": [
                { "id": common::ENG_TEAM_ID, "key": common::ENG_TEAM_KEY, "name": common::ENG_TEAM_NAME }
            ] } } }),
        )
        .with_variables(json!({ "reference": common::ENG_TEAM_KEY })),
        MockResponse::new(
            "FetchIssues",
            issues_response(vec![issue_node("ENG-3", "Mine to do", Some("Ada Lovelace"))]),
        ),
    ]);

    let out = run_cli(
        &["issue", "mine", "--team", common::ENG_TEAM_KEY],
        &common::mock_env(&server),
    );
    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(out.stdout.contains("ENG-3"), "stdout: {}", out.stdout);
    assert!(out.stdout.contains("Mine to do"), "stdout: {}", out.stdout);
}

#[test]
fn issue_mine_rejects_removed_assignee_flag() {
    let server = MockLinearServer::start(vec![]);
    let out = run_cli(
        &["issue", "mine", "--team", common::ENG_TEAM_KEY, "--assignee", "Ada"],
        &common::mock_env(&server),
    );
    assert!(!out.success());
    assert!(
        out.stderr.contains("--assignee has been removed from 'issue mine'"),
        "stderr: {}",
        out.stderr
    );
    assert!(
        out.stderr.contains("linear issue query --assignee"),
        "stderr: {}",
        out.stderr
    );
}
