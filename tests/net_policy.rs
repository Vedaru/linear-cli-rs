//! The request policy, from the CLI's side: a bounded failure on a hung upstream, and retries
//! only where a repeat is safe.
//!
//! The interesting half is the second clause, and it is proved as a biconditional: the same
//! "500 once, then answer properly" mock is queued for both a read and a mutation, so a command
//! that retried *would succeed*. The read must succeed; the mutation must not, because a retried
//! create is a duplicate - the failure this codebase spends the most effort avoiding.

mod common;

use std::time::{Duration, Instant};

use common::{mock_env, run_cli, MockLinearServer, MockResponse};
use serde_json::json;

fn views_page() -> serde_json::Value {
    json!({ "data": { "customViews": {
        "nodes": [],
        "pageInfo": { "hasNextPage": false, "endCursor": null }
    } } })
}

fn hiccup(query_name: &str) -> MockResponse {
    // Answers once and stops matching, so the second attempt sees the success below it.
    MockResponse::new(
        query_name,
        json!({ "errors": [{ "message": "upstream hiccup" }] }),
    )
    .with_status(500)
    .with_single_use()
}

#[test]
fn a_read_that_fails_once_is_retried_and_succeeds() {
    let server = MockLinearServer::start(vec![
        hiccup("ListViews"),
        MockResponse::new("ListViews", views_page()),
    ]);

    let output = run_cli(&["view", "list", "--json"], &mock_env(&server));

    assert!(
        output.success(),
        "a read is safe to repeat, so it should have been retried: {}",
        output.stderr
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&output.stdout).expect("views")["nodes"],
        json!([])
    );
}

#[test]
fn a_mutation_that_fails_once_is_not_retried() {
    let success = json!({ "data": { "customViewCreate": { "success": true, "customView": {
        "id": "view-1", "name": "X", "filterData": {}
    } } } });
    let server = MockLinearServer::start(vec![
        // Order does not matter to the matcher, but the success being *there* is the point: a
        // command that retried would consume it and pass.
        hiccup("CreateView"),
        MockResponse::new("CreateView", success),
        MockResponse::new(
            "FindTeam",
            json!({ "data": {
                "teams": { "nodes": [{ "id": "team-eng-id", "key": "ENG", "name": "Engineering" }] },
                "teamById": { "nodes": [] }
            } }),
        ),
    ]);

    let output = run_cli(
        &[
            "view", "create", "--name", "X", "--team", "ENG", "--filter", "{}", "--json",
        ],
        &mock_env(&server),
    );

    assert!(
        !output.success(),
        "a mutation is sent exactly once; this one succeeded on a retry: {}",
        output.stdout
    );
    assert!(
        output.stderr.contains("upstream hiccup") || output.stderr.contains("500"),
        "the failure should be the upstream one: {}",
        output.stderr
    );
}

#[test]
fn a_hanging_upstream_is_a_bounded_failure_not_a_wedged_process() {
    // The server waits far longer than the deadline; the deadline is a second. Three attempts of a
    // one-second deadline is a few seconds, which is the property being asserted: bounded, not
    // "eventually, after the real sixty".
    let server = MockLinearServer::start(vec![
        MockResponse::new("ListViews", views_page()).with_delay(Duration::from_secs(30))
    ]);
    let mut env = mock_env(&server);
    env.push(("LINEAR_REQUEST_TIMEOUT_SECS".into(), "1".into()));

    let started = Instant::now();
    let output = run_cli(&["view", "list", "--json"], &env);
    let elapsed = started.elapsed();

    assert!(
        !output.success(),
        "a hung upstream must not read as success"
    );
    assert!(
        elapsed < Duration::from_secs(15),
        "the deadline should have bitten: took {elapsed:?}"
    );
    assert!(
        output.stderr.contains("Failed to reach Linear API"),
        "stderr: {}",
        output.stderr
    );
}
