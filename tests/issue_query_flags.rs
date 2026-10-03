//! The three levers `issue query` grew for agents: `--count-only`, `--since`, `--group-by`.
//!
//! The mocks here are deliberately thin. A request a test did not configure answers "No mock
//! response configured for this query", which is an error, so a command that fetched what it
//! should not have fetched fails the test instead of quietly passing it - which is what makes
//! the `--count-only` tests about *what was asked for* rather than only about the number.

mod common;

use common::{mock_env, run_cli, MockLinearServer, MockResponse};
use serde_json::{json, Value};

/// `--team ENG` resolves through `FindTeam` (by key, then name), so that is the query that has
/// to answer - the `ResolveTeam` helper several other suites use is a different operation.
fn find_team_mock() -> MockResponse {
    MockResponse::new(
        "FindTeam",
        json!({ "data": {
            "teams": { "nodes": [{
                "id": "team-eng-id", "key": "ENG", "name": "Engineering"
            }] },
            "teamById": { "nodes": [] }
        } }),
    )
}

/// An issue as `FetchIssues` returns one: the fields the table renderer and the grouping read.
fn issue(id: &str, identifier: &str, title: &str, state: &str, state_type: &str) -> Value {
    json!({
        "id": id,
        "identifier": identifier,
        "title": title,
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

/// A `FetchIssues` answer holding `nodes`.
fn issues_response(nodes: Vec<Value>) -> MockResponse {
    MockResponse::new(
        "FetchIssues",
        json!({ "data": { "issues": {
            "nodes": nodes,
            "pageInfo": { "hasNextPage": false, "endCursor": null }
        } } }),
    )
}

/// The first page of a longer answer: the cursor is what makes a second request happen.
fn first_page(nodes: Vec<Value>) -> MockResponse {
    MockResponse::new(
        "FetchIssues",
        json!({ "data": { "issues": {
            "nodes": nodes,
            "pageInfo": { "hasNextPage": true, "endCursor": "cursor-1" }
        } } }),
    )
    // The first request carries no `after`, which the harness reads as null.
    .with_variables(json!({ "after": null }))
}

/// The page after `first_page`.
fn second_page(nodes: Vec<Value>) -> MockResponse {
    MockResponse::new(
        "FetchIssues",
        json!({ "data": { "issues": {
            "nodes": nodes,
            "pageInfo": { "hasNextPage": false, "endCursor": null }
        } } }),
    )
    .with_variables(json!({ "after": "cursor-1" }))
}

#[test]
fn count_only_takes_the_number_the_api_states() {
    // Only the stated-count request is configured: a full issue fetch would have no answer and
    // would fail this test, which is how "no page requests" is asserted rather than claimed.
    let server = MockLinearServer::start(vec![
        find_team_mock(),
        MockResponse::new(
            "TeamIssueCounts",
            json!({ "data": { "teams": { "nodes": [{ "key": "ENG", "issueCount": 41 }] } } }),
        )
        .with_variables(json!({ "keys": ["ENG"] })),
    ]);

    let output = run_cli(
        &["issue", "query", "--team", "ENG", "--count-only", "--json"],
        &mock_env(&server),
    );

    assert!(output.success(), "stderr: {}", output.stderr);
    assert_eq!(
        serde_json::from_str::<Value>(&output.stdout).expect("a count"),
        json!({ "count": 41 })
    );
}

#[test]
fn count_only_with_a_filter_counts_ids_instead() {
    // A filter has no stated count behind it, so the smallest possible request is `nodes { id }`
    // - and the filter still has to travel, which the mocked variables assert.
    let server = MockLinearServer::start(vec![
        find_team_mock(),
        MockResponse::new(
            "CountIssues",
            json!({ "data": { "issues": {
                "nodes": [{ "id": "a" }, { "id": "b" }, { "id": "c" }],
                "pageInfo": { "hasNextPage": false, "endCursor": null }
            } } }),
        )
        // The filter the request must carry: the team scope *and* the flag, whole - a count
        // that filtered differently from the list would answer a question nobody asked.
        .with_variables(json!({ "filter": {
            "team": { "key": { "in": ["ENG"] } },
            "assignee": { "null": true }
        } })),
    ]);

    let output = run_cli(
        &[
            "issue",
            "query",
            "--team",
            "ENG",
            "--unassigned",
            "--count-only",
            "--json",
        ],
        &mock_env(&server),
    );

    assert!(output.success(), "stderr: {}", output.stderr);
    assert_eq!(
        serde_json::from_str::<Value>(&output.stdout).expect("a count"),
        json!({ "count": 3 })
    );
}

#[test]
fn since_is_the_relative_form_of_the_same_bound() {
    let server = MockLinearServer::start(vec![
        find_team_mock(),
        issues_response(vec![issue(
            "id-1",
            "ENG-1",
            "One",
            "In Progress",
            "started",
        )]),
    ]);
    let env = mock_env(&server);

    let relative = run_cli(
        &["issue", "query", "--team", "ENG", "--since", "7d", "--json"],
        &env,
    );
    // The same bound written the way it used to have to be written by hand. The two runs are a
    // few milliseconds apart (each resolves its own "now"), so this pins the *route* - both
    // notations reach the same filter - rather than the instant.
    let a_week_ago = (chrono::Utc::now() - chrono::Duration::days(7))
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string();
    let absolute = run_cli(
        &[
            "issue",
            "query",
            "--team",
            "ENG",
            "--updated-after",
            &a_week_ago,
            "--json",
        ],
        &env,
    );

    assert!(relative.success(), "stderr: {}", relative.stderr);
    assert!(absolute.success(), "stderr: {}", absolute.stderr);
    assert_eq!(
        relative.stdout, absolute.stdout,
        "the same bound should give the same answer in both notations"
    );
}

#[test]
fn since_refuses_a_mistyped_age_and_a_duplicated_bound() {
    let server = MockLinearServer::start(vec![find_team_mock()]);
    let env = mock_env(&server);

    let mistyped = run_cli(&["issue", "query", "--team", "ENG", "--since", "7x"], &env);
    assert!(!mistyped.success(), "7x is not an age");
    assert!(
        mistyped.stderr.contains("An age is a number and a unit"),
        "the error should teach the grammar: {}",
        mistyped.stderr
    );

    let both = run_cli(
        &[
            "issue",
            "query",
            "--team",
            "ENG",
            "--since",
            "7d",
            "--updated-after",
            "2026-01-01",
        ],
        &env,
    );
    assert!(!both.success(), "one bound, not two");
    assert!(
        both.stderr
            .contains("Cannot use both --since and --updated-after"),
        "stderr: {}",
        both.stderr
    );
}

#[test]
fn group_by_arranges_the_json_it_was_asked_to_group() {
    let server = MockLinearServer::start(vec![
        find_team_mock(),
        issues_response(vec![
            issue("a", "ENG-1", "One", "In Progress", "started"),
            issue("b", "ENG-2", "Two", "Done", "completed"),
            issue("c", "ENG-3", "Three", "In Progress", "started"),
        ]),
    ]);

    let output = run_cli(
        &[
            "issue",
            "query",
            "--team",
            "ENG",
            "--group-by",
            "state",
            "--json",
        ],
        &mock_env(&server),
    );

    assert!(output.success(), "stderr: {}", output.stderr);
    let parsed: Value = serde_json::from_str(&output.stdout).expect("grouped json");
    assert_eq!(parsed["groupedBy"], "state");
    assert_eq!(parsed["total"], 3);

    let groups = parsed["groups"].as_array().expect("groups");
    assert_eq!(groups.len(), 2, "{parsed}");
    // First seen, not sorted: the query's own order decided what came first.
    assert_eq!(groups[0]["label"], "In Progress");
    assert_eq!(groups[0]["count"], 2);
    assert_eq!(groups[0]["issues"].as_array().map(Vec::len), Some(2));
    assert_eq!(groups[1]["label"], "Done");
    assert_eq!(groups[1]["count"], 1);
}

#[test]
fn group_by_prints_one_table_per_group() {
    let server = MockLinearServer::start(vec![
        find_team_mock(),
        issues_response(vec![
            issue("a", "ENG-1", "One", "In Progress", "started"),
            issue("b", "ENG-2", "Two", "Done", "completed"),
            issue("c", "ENG-3", "Three", "In Progress", "started"),
        ]),
    ]);

    let output = run_cli(
        &[
            "issue",
            "query",
            "--team",
            "ENG",
            "--group-by",
            "state",
            "--no-pager",
        ],
        &mock_env(&server),
    );

    assert!(output.success(), "stderr: {}", output.stderr);
    assert!(
        output.stdout.contains("In Progress (2)"),
        "a group is labelled and counted: {}",
        output.stdout
    );
    assert!(output.stdout.contains("Done (1)"), "{}", output.stdout);
    assert_eq!(
        output.stdout.matches("TITLE").count(),
        2,
        "each group is a table of its own: {}",
        output.stdout
    );
}

#[test]
fn group_by_refuses_a_field_it_does_not_group_by() {
    let server = MockLinearServer::start(vec![find_team_mock()]);

    let output = run_cli(
        &["issue", "query", "--team", "ENG", "--group-by", "banana"],
        &mock_env(&server),
    );

    assert!(!output.success(), "an unknown field is not a grouping");
    assert!(
        output.stderr.contains("Unknown --group-by field"),
        "stderr: {}",
        output.stderr
    );
}

#[test]
fn ndjson_writes_one_line_per_issue_across_pages() {
    let server = MockLinearServer::start(vec![
        find_team_mock(),
        first_page(vec![issue("a", "ENG-1", "One", "In Progress", "started")]),
        second_page(vec![issue("b", "ENG-2", "Two", "Done", "completed")]),
    ]);

    let output = run_cli(
        &[
            "issue",
            "query",
            "--team",
            "ENG",
            "--ndjson",
            "--limit",
            "0",
            "--no-pager",
        ],
        &mock_env(&server),
    );

    assert!(output.success(), "stderr: {}", output.stderr);
    let lines: Vec<&str> = output
        .stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert_eq!(lines.len(), 2, "one line per issue: {}", output.stdout);
    // Each line has to stand on its own: that is what a line-oriented reader parses.
    let first: Value = serde_json::from_str(lines[0]).expect("a document of its own");
    let second: Value = serde_json::from_str(lines[1]).expect("a document of its own");
    assert_eq!(first["identifier"], "ENG-1");
    assert_eq!(second["identifier"], "ENG-2");
}

#[test]
fn ndjson_streams_what_it_has_before_a_later_page_fails() {
    // Page one is answered and page two deliberately is not. A command that *streams* has
    // already written page one when the second request fails; one that buffers the result
    // prints nothing at all. This is the only place the difference is visible from outside,
    // which is why it is the assertion the implementation has to survive.
    let server = MockLinearServer::start(vec![
        find_team_mock(),
        first_page(vec![issue("a", "ENG-1", "One", "In Progress", "started")]),
    ]);

    let output = run_cli(
        &[
            "issue",
            "query",
            "--team",
            "ENG",
            "--ndjson",
            "--limit",
            "0",
            "--no-pager",
        ],
        &mock_env(&server),
    );

    assert!(!output.success(), "the missing page is an error");
    assert!(
        output.stdout.contains("ENG-1"),
        "page one should already be out: {}",
        output.stdout
    );
}

#[test]
fn ndjson_stops_at_the_limit_without_fetching_the_next_page() {
    // The limit is applied before a page is handed over, and the walk stops there. Page two is
    // not configured, so a command that fetched it anyway would fail this test.
    let server = MockLinearServer::start(vec![
        find_team_mock(),
        first_page(vec![
            issue("a", "ENG-1", "One", "In Progress", "started"),
            issue("b", "ENG-2", "Two", "Done", "completed"),
        ]),
    ]);

    let output = run_cli(
        &[
            "issue",
            "query",
            "--team",
            "ENG",
            "--ndjson",
            "--limit",
            "1",
            "--no-pager",
        ],
        &mock_env(&server),
    );

    assert!(output.success(), "stderr: {}", output.stderr);
    assert_eq!(
        output
            .stdout
            .lines()
            .filter(|line| !line.trim().is_empty())
            .count(),
        1,
        "the limit belongs to the consumer too: {}",
        output.stdout
    );
}

#[test]
fn ndjson_refuses_what_it_cannot_stream() {
    let server = MockLinearServer::start(vec![find_team_mock()]);
    let env = mock_env(&server);

    for (flags, expected) in [
        (
            vec!["--ndjson", "--json"],
            "Cannot use both --ndjson and --json",
        ),
        (
            vec!["--ndjson", "--count-only"],
            "Cannot use --ndjson with --count-only",
        ),
        (
            vec!["--ndjson", "--group-by", "state"],
            "Cannot use --ndjson with --group-by",
        ),
        (
            vec!["--ndjson", "--search", "oauth"],
            "Cannot use --ndjson with --search",
        ),
    ] {
        let mut args = vec!["issue", "query", "--team", "ENG"];
        args.extend(flags.iter().copied());
        let output = run_cli(&args, &env);
        assert!(!output.success(), "{flags:?} must be refused");
        assert!(
            output.stderr.contains(expected),
            "{flags:?}: {}",
            output.stderr
        );
    }
}
