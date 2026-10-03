//! `linear view` - the CRUD half, and the apply half.
//!
//! Two different things are pinned here. The CRUD tests pin *shapes*: what `--json` prints, and -
//! through the mocked variables, which are compared as a subset rather than in full - exactly
//! what the command sent. The apply tests pin the thing a view exists for: a name resolves to a
//! view, the view's `filterData` becomes the query's filter, and the default team does *not*
//! narrow a list the view means to show wider. The mocks prove the last part by omission - a
//! `FindTeam` request has no answer in these tests, and an unanswered request is an error.

mod common;

use common::{mock_env, run_cli, MockLinearServer, MockResponse};
use serde_json::{json, Value};

/// The filter every test view saves: one state, in the API's `filterData` shape.
const STARTED: &str = r#"{"state": {"type": {"eq": "started"}}}"#;

fn view_node() -> Value {
    json!({
        "id": "view-1",
        "name": "My Sprint",
        "description": "What I am on this week",
        "slugId": "my-sprint",
        "shared": false,
        "filterData": { "state": { "type": { "eq": "started" } } },
        "team": { "id": "team-eng-id", "key": "ENG", "name": "Engineering" },
        "owner": { "id": "user-1", "name": "loner", "displayName": "loner" },
        "createdAt": "2026-10-01T00:00:00.000Z",
        "updatedAt": "2026-10-03T00:00:00.000Z"
    })
}

/// `--team ENG` resolves through `FindTeam` (by key, then name) - the same mock `issue query`
/// uses, because it is the same resolver.
fn find_team_mock() -> MockResponse {
    MockResponse::new(
        "FindTeam",
        json!({ "data": {
            "teams": { "nodes": [{ "id": "team-eng-id", "key": "ENG", "name": "Engineering" }] },
            "teamById": { "nodes": [] }
        } }),
    )
}

fn list_views_mock() -> MockResponse {
    MockResponse::new(
        "ListViews",
        json!({ "data": { "customViews": {
            "nodes": [view_node()],
            "pageInfo": { "hasNextPage": false, "endCursor": null }
        } } }),
    )
}

fn get_view_mock() -> MockResponse {
    MockResponse::new("GetView", json!({ "data": { "customView": view_node() } }))
}

fn issue(id: &str, identifier: &str, state: &str, state_type: &str) -> Value {
    json!({
        "id": id,
        "identifier": identifier,
        "title": format!("Issue {identifier}"),
        "priority": 0,
        "priorityLabel": "No priority",
        "estimate": null,
        "url": format!("https://linear.app/vedaru/issue/{identifier}"),
        "createdAt": "2026-10-01T00:00:00.000Z",
        "updatedAt": "2026-10-03T00:00:00.000Z",
        "state": {
            "id": format!("state-{state_type}"),
            "name": state,
            "type": state_type,
            "color": "#5e6ad2",
            "position": 1
        },
        "assignee": null,
        "team": {
            "id": "team-eng-id",
            "key": "ENG",
            "name": "Engineering",
            "cyclesEnabled": false,
            "activeCycle": null
        },
        "project": null,
        "projectMilestone": null,
        "cycle": null,
        "labels": { "nodes": [] },
        "inverseRelations": { "nodes": [] }
    })
}

#[test]
fn list_pins_the_json_shape() {
    let server = MockLinearServer::start(vec![list_views_mock()]);

    let output = run_cli(&["view", "list", "--json"], &mock_env(&server));

    assert!(output.success(), "stderr: {}", output.stderr);
    let printed: Value = serde_json::from_str(&output.stdout).expect("a views document");
    assert_eq!(printed["nodes"][0]["name"], "My Sprint");
    assert_eq!(printed["nodes"][0]["slugId"], "my-sprint");
    assert_eq!(printed["pageInfo"]["hasNextPage"], false);
}

#[test]
fn view_by_id_reads_one_object_not_a_list() {
    let server = MockLinearServer::start(vec![get_view_mock()]);

    let output = run_cli(
        &[
            "view",
            "view",
            "2f1a2b3c-4d5e-6f70-8192-a3b4c5d6e7f8",
            "--json",
        ],
        &mock_env(&server),
    );

    assert!(output.success(), "stderr: {}", output.stderr);
    let printed: Value = serde_json::from_str(&output.stdout).expect("a view");
    assert_eq!(printed["id"], "view-1");
    assert_eq!(printed["filterData"]["state"]["type"]["eq"], "started");
}

#[test]
fn view_by_name_asks_for_that_name() {
    // The name lookup carries the filter that makes it a lookup: a request for a different one
    // would be answered by the same mock only by accident, which is why the gate is here.
    let server = MockLinearServer::start(vec![list_views_mock().with_variables(json!({
        "filter": { "name": { "eq": "My Sprint" } }
    }))]);

    let output = run_cli(&["view", "view", "My Sprint", "--json"], &mock_env(&server));

    assert!(output.success(), "stderr: {}", output.stderr);
    let printed: Value = serde_json::from_str(&output.stdout).expect("a view");
    assert_eq!(printed["name"], "My Sprint");
}

#[test]
fn a_name_two_views_share_is_refused_rather_than_guessed() {
    let mut first = view_node();
    let mut second = view_node();
    first["id"] = json!("view-1");
    second["id"] = json!("view-2");
    let server = MockLinearServer::start(vec![MockResponse::new(
        "ListViews",
        json!({ "data": { "customViews": {
            "nodes": [first, second],
            "pageInfo": { "hasNextPage": false, "endCursor": null }
        } } }),
    )]);

    let output = run_cli(&["view", "view", "My Sprint"], &mock_env(&server));

    assert!(!output.success());
    assert!(
        output.stderr.contains("More than one view"),
        "stderr: {}",
        output.stderr
    );
}

#[test]
fn create_sends_the_filter_verbatim() {
    let created =
        json!({ "data": { "customViewCreate": { "success": true, "customView": view_node() } } });
    let server = MockLinearServer::start(vec![
        find_team_mock(),
        // The whole input, so a field the command invents - or drops - is a failure.
        MockResponse::new("CreateView", created).with_variables(json!({ "input": {
            "name": "My Sprint",
            "teamId": "team-eng-id",
            "filterData": { "state": { "type": { "eq": "started" } } },
            "description": "What I am on this week",
            "shared": true
        } })),
    ]);

    let output = run_cli(
        &[
            "view",
            "create",
            "--name",
            "My Sprint",
            "--team",
            "ENG",
            "--description",
            "What I am on this week",
            "--shared",
            "true",
            "--filter",
            STARTED,
            "--json",
        ],
        &mock_env(&server),
    );

    assert!(output.success(), "stderr: {}", output.stderr);
    let printed: Value = serde_json::from_str(&output.stdout).expect("the created view");
    assert_eq!(printed["filterData"]["state"]["type"]["eq"], "started");
}

#[test]
fn create_refuses_a_filter_that_is_not_json_and_a_view_with_no_filter() {
    // Validation happens before any request, which is why no mock is configured: a command that
    // asked the API first would fail here on the unanswered request instead.
    let output = run_cli(
        &["view", "create", "--name", "X", "--filter", "{not json"],
        &[],
    );
    assert!(!output.success());
    assert!(
        output.stderr.contains("not valid JSON"),
        "stderr: {}",
        output.stderr
    );

    let output = run_cli(&["view", "create", "--name", "X"], &[]);
    assert!(!output.success());
    assert!(
        output.stderr.contains("needs a filter"),
        "stderr: {}",
        output.stderr
    );
}

#[test]
fn update_sends_only_what_was_given() {
    // The mock answers with the view as the API would return it - renamed - because the command
    // reports what came back, not what it asked for.
    let mut renamed = view_node();
    renamed["name"] = json!("Renamed");
    let updated =
        json!({ "data": { "customViewUpdate": { "success": true, "customView": renamed } } });
    let server = MockLinearServer::start(vec![
        get_view_mock(),
        MockResponse::new("UpdateView", updated).with_variables(json!({
            "id": "view-1",
            "input": { "name": "Renamed" }
        })),
    ]);

    let output = run_cli(
        &[
            "view",
            "update",
            "2f1a2b3c-4d5e-6f70-8192-a3b4c5d6e7f8",
            "--name",
            "Renamed",
        ],
        &mock_env(&server),
    );

    assert!(output.success(), "stderr: {}", output.stderr);
    assert!(
        output.stdout.contains("Renamed"),
        "stdout: {}",
        output.stdout
    );
}

#[test]
fn update_with_nothing_to_change_is_refused() {
    let output = run_cli(
        &["view", "update", "2f1a2b3c-4d5e-6f70-8192-a3b4c5d6e7f8"],
        &[],
    );
    assert!(!output.success());
    assert!(
        output.stderr.contains("Nothing to update"),
        "stderr: {}",
        output.stderr
    );
}

#[test]
fn delete_needs_force_where_nothing_can_be_asked() {
    let server = MockLinearServer::start(vec![get_view_mock()]);

    let refused = run_cli(
        &["view", "delete", "2f1a2b3c-4d5e-6f70-8192-a3b4c5d6e7f8"],
        &mock_env(&server),
    );
    assert!(!refused.success());
    assert!(
        refused.stderr.contains("--force"),
        "stderr: {}",
        refused.stderr
    );

    let deleted = MockLinearServer::start(vec![
        get_view_mock(),
        MockResponse::new(
            "DeleteView",
            json!({ "data": { "customViewDelete": { "success": true } } }),
        )
        .with_variables(json!({ "id": "view-1" })),
    ]);
    let output = run_cli(
        &[
            "view",
            "delete",
            "2f1a2b3c-4d5e-6f70-8192-a3b4c5d6e7f8",
            "--force",
            "--json",
        ],
        &mock_env(&deleted),
    );
    assert!(output.success(), "stderr: {}", output.stderr);
    assert_eq!(
        serde_json::from_str::<Value>(&output.stdout).expect("a delete result"),
        json!({ "success": true, "id": "view-1" })
    );
}

#[test]
fn query_refuses_the_filter_flags_beside_a_view() {
    // A view is already a filter. The flags actually given are the ones named, and validation
    // runs before any request - so no mock is configured and nothing was fetched.
    let output = run_cli(
        &[
            "issue",
            "query",
            "--view",
            "My Sprint",
            "--state",
            "started",
        ],
        &[],
    );
    assert!(!output.success());
    assert!(
        output.stderr.contains("Cannot combine --view with --state"),
        "stderr: {}",
        output.stderr
    );

    let output = run_cli(
        &["issue", "query", "--view", "My Sprint", "--team", "ENG"],
        &[],
    );
    assert!(!output.success());
    assert!(
        output.stderr.contains("Cannot combine --view with --team"),
        "stderr: {}",
        output.stderr
    );
}

#[test]
fn query_applies_the_saved_filter_and_not_the_default_team() {
    // Two mocks, and both of them are assertions. `ListViews` gated on the name proves the view
    // was looked up by the name that was given; `FetchIssues` answers the issues the view's
    // filter selects. No `FindTeam` is configured, so a query that fell back to the default team
    // scope - the one thing `--view` must not do - fails on an unanswered request rather than
    // quietly narrowing the list.
    let server = MockLinearServer::start(vec![
        list_views_mock().with_variables(json!({
            "filter": { "name": { "eq": "My Sprint" } }
        })),
        MockResponse::new(
            "FetchIssues",
            json!({ "data": { "issues": {
                "nodes": [
                    issue("issue-1", "VED-1", "In Progress", "started"),
                    issue("issue-2", "VED-2", "In Review", "started")
                ],
                "pageInfo": { "hasNextPage": false, "endCursor": null }
            } } }),
        ),
    ]);

    let output = run_cli(
        &["issue", "query", "--view", "My Sprint", "--json"],
        &mock_env(&server),
    );

    assert!(output.success(), "stderr: {}", output.stderr);
    let printed: Value = serde_json::from_str(&output.stdout).expect("issues");
    let identifiers: Vec<&str> = printed["nodes"]
        .as_array()
        .expect("nodes")
        .iter()
        .filter_map(|node| node["identifier"].as_str())
        .collect();
    assert_eq!(identifiers, vec!["VED-1", "VED-2"]);
}

#[test]
fn query_composes_a_view_with_a_count() {
    // `--count-only` and `--view` are both about the filter, so they compose: the count is the
    // view's count. It has to be counted rather than asked for, because a filtered count has no
    // stated number behind it - which is what the smallest-possible request here proves.
    let server = MockLinearServer::start(vec![
        list_views_mock(),
        MockResponse::new(
            "CountIssues",
            json!({ "data": { "issues": {
                "nodes": [{ "id": "a" }, { "id": "b" }],
                "pageInfo": { "hasNextPage": false, "endCursor": null }
            } } }),
        )
        .with_variables(json!({ "filter": { "state": { "type": { "eq": "started" } } } })),
    ]);

    let output = run_cli(
        &[
            "issue",
            "query",
            "--view",
            "view-1",
            "--count-only",
            "--json",
        ],
        &mock_env(&server),
    );

    assert!(output.success(), "stderr: {}", output.stderr);
    assert_eq!(
        serde_json::from_str::<Value>(&output.stdout).expect("a count"),
        json!({ "count": 2 })
    );
}
