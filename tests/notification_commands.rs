//! `linear notification` - the listing, the two writes, and the refusals.
//!
//! Three things are pinned here that the live round trip cannot pin repeatably:
//!
//! * **`--unread` filters rows, not the count.** `NotificationFilter` has no read state, so the
//!   filter runs on the page here while `unreadCount` stays the API's own number. A test that only
//!   checked the rows would let someone "simplify" that into a server-side filter that does not
//!   exist; the count is asserted separately so the two cannot quietly become the same thing.
//! * **Which document went out.** The mocks match on operation name, so `MarkNotificationRead`
//!   answering proves the request *was* the mutation - and `NotificationViewer` being a separate
//!   mock is the pin on the fact that `viewer` cannot ride along inside a mutation document.
//! * **Whose state.** Every write says the viewer's name and email, because read state is per user
//!   and a bare "marked read" is the one claim a person cannot check.

mod common;

use common::{mock_env, run_cli, MockLinearServer, MockResponse};
use serde_json::{json, Value};

fn unread_notification(id: &str, identifier: &str) -> Value {
    json!({
        "id": id,
        "type": "issueMention",
        "readAt": null,
        "createdAt": "2026-10-03T09:00:00.000Z",
        "archivedAt": null,
        "issue": { "identifier": identifier, "title": "Columns on the board" }
    })
}

fn read_notification(id: &str) -> Value {
    json!({
        "id": id,
        "type": "workspaceWelcome",
        "readAt": "2026-10-02T02:46:26.934Z",
        "createdAt": "2026-10-02T01:22:42.936Z",
        "archivedAt": null
    })
}

fn list_mock(nodes: Vec<Value>) -> MockResponse {
    MockResponse::new(
        "ListNotifications",
        json!({ "data": {
            "notifications": {
                "nodes": nodes,
                "pageInfo": { "hasNextPage": false, "endCursor": null }
            },
            "viewer": { "name": "loner", "email": "l2859794@gmail.com" }
        } }),
    )
}

fn unread_count_mock(count: i64) -> MockResponse {
    MockResponse::new(
        "NotificationsUnreadCount",
        json!({ "data": { "notificationsUnreadCount": count } }),
    )
}

fn viewer_mock() -> MockResponse {
    MockResponse::new(
        "NotificationViewer",
        json!({ "data": { "viewer": { "name": "loner", "email": "l2859794@gmail.com" } } }),
    )
}

#[test]
fn list_prints_the_connection_shape_with_the_viewer_and_the_api_count() {
    let server = MockLinearServer::start(vec![
        list_mock(vec![unread_notification("n-1", "VED-47")]),
        unread_count_mock(3),
    ]);

    let output = run_cli(&["notification", "list", "--json"], &mock_env(&server));
    assert!(output.success(), "{}", output.stderr);

    let body: Value = serde_json::from_str(&output.stdout).expect("--json must be valid JSON");
    assert_eq!(body["nodes"][0]["id"], "n-1");
    assert_eq!(body["nodes"][0]["issue"]["identifier"], "VED-47");
    assert_eq!(body["pageInfo"]["hasNextPage"], false);
    // The API's count, not the row count: one node shown, three unread.
    assert_eq!(body["unreadCount"], 3);
    assert_eq!(body["viewer"]["name"], "loner");
}

#[test]
fn unread_filters_the_rows_but_the_count_stays_the_apis_number() {
    let server = MockLinearServer::start(vec![
        list_mock(vec![
            unread_notification("n-1", "VED-47"),
            read_notification("n-2"),
        ]),
        unread_count_mock(7),
    ]);

    let output = run_cli(
        &["notification", "list", "--unread", "--json"],
        &mock_env(&server),
    );
    assert!(output.success(), "{}", output.stderr);

    let body: Value = serde_json::from_str(&output.stdout).expect("--json must be valid JSON");
    assert_eq!(
        body["nodes"].as_array().unwrap().len(),
        1,
        "the read one is filtered out"
    );
    assert_eq!(body["nodes"][0]["id"], "n-1");
    assert_eq!(
        body["unreadCount"], 7,
        "the count is the API's, so it is not the number of rows"
    );
}

#[test]
fn since_is_sent_as_a_created_at_comparator() {
    let server = MockLinearServer::start(vec![
        MockResponse::new(
            "ListNotifications",
            json!({ "data": {
                "notifications": { "nodes": [], "pageInfo": { "hasNextPage": false, "endCursor": null } },
                "viewer": { "name": "loner", "email": "l2859794@gmail.com" }
            } }),
        )
        .with_variables(json!({ "filter": { "createdAt": { "gt": "2024-01-15T00:00:00.000Z" } } })),
        unread_count_mock(0),
    ]);

    let output = run_cli(
        &["notification", "list", "--since", "2024-01-15", "--json"],
        &mock_env(&server),
    );
    assert!(output.success(), "{}", output.stderr);
}

#[test]
fn a_written_since_age_is_accepted_and_converted() {
    // The relative form goes through the same parser `issue query --since` uses; the assertion here
    // is that it parses at all and reaches the request (the mock would have no answer otherwise).
    let server = MockLinearServer::start(vec![
        MockResponse::new(
            "ListNotifications",
            json!({ "data": {
                "notifications": { "nodes": [], "pageInfo": { "hasNextPage": false, "endCursor": null } },
                "viewer": { "name": "loner", "email": "l2859794@gmail.com" }
            } }),
        )
        .with_query_includes("ListNotifications"),
        unread_count_mock(0),
    ]);

    let output = run_cli(
        &["notification", "list", "--since", "7d", "--json"],
        &mock_env(&server),
    );
    assert!(output.success(), "{}", output.stderr);
}

#[test]
fn a_nonsense_since_is_refused_before_any_request() {
    // No mocks at all: an unanswered request is an error, so passing this proves nothing was sent.
    let server = MockLinearServer::start(vec![]);

    let output = run_cli(
        &["notification", "list", "--since", "last tuesday"],
        &mock_env(&server),
    );
    assert!(!output.success());
    assert!(
        output.stderr.contains("--since"),
        "the refusal names the flag: {}",
        output.stderr
    );
}

#[test]
fn read_marks_the_notification_and_names_the_user_whose_state_changed() {
    let server = MockLinearServer::start(vec![
        list_mock(vec![unread_notification("n-1", "VED-47")]),
        MockResponse::new(
            "MarkNotificationRead",
            json!({ "data": { "notificationUpdate": {
                "success": true,
                "notification": { "id": "n-1", "readAt": "2026-10-03T10:00:00.000Z" }
            } } }),
        ),
        viewer_mock(),
    ]);

    let output = run_cli(&["notification", "read", "VED-47"], &mock_env(&server));
    assert!(output.success(), "{}", output.stderr);
    assert!(
        output.stdout.contains("loner <l2859794@gmail.com>"),
        "the sentence says whose read state changed: {}",
        output.stdout
    );
    assert!(output.stdout.contains("VED-47 Columns on the board"));
}

#[test]
fn read_json_is_the_payload_with_the_viewer_folded_in() {
    let server = MockLinearServer::start(vec![
        list_mock(vec![unread_notification("n-1", "VED-47")]),
        MockResponse::new(
            "MarkNotificationRead",
            json!({ "data": { "notificationUpdate": {
                "success": true,
                "notification": { "id": "n-1", "readAt": "2026-10-03T10:00:00.000Z" }
            } } }),
        ),
        viewer_mock(),
    ]);

    let output = run_cli(
        &["notification", "read", "n-1", "--json"],
        &mock_env(&server),
    );
    assert!(output.success(), "{}", output.stderr);

    let body: Value = serde_json::from_str(&output.stdout).expect("--json must be valid JSON");
    // The mutation's own fields, plus the viewer - not a wrapper object.
    assert_eq!(body["success"], true);
    assert_eq!(body["notification"]["id"], "n-1");
    assert_eq!(body["viewer"]["email"], "l2859794@gmail.com");
}

#[test]
fn archive_sends_the_archive_mutation() {
    let server = MockLinearServer::start(vec![
        list_mock(vec![unread_notification("n-1", "VED-47")]),
        MockResponse::new(
            "ArchiveNotification",
            json!({ "data": { "notificationArchive": {
                "success": true,
                "entity": { "id": "n-1", "archivedAt": "2026-10-03T10:09:46.729Z" }
            } } }),
        ),
        viewer_mock(),
    ]);

    let output = run_cli(&["notification", "archive", "VED-47"], &mock_env(&server));
    assert!(output.success(), "{}", output.stderr);
    assert!(output.stdout.contains("Archived for loner"));
}

#[test]
fn an_unknown_reference_is_refused_rather_than_guessed() {
    let server =
        MockLinearServer::start(vec![list_mock(vec![unread_notification("n-1", "VED-47")])]);

    let output = run_cli(&["notification", "read", "VED-999"], &mock_env(&server));
    assert!(!output.success());
    assert!(
        output.stderr.contains("VED-999"),
        "the refusal names what it could not find: {}",
        output.stderr
    );
}

#[test]
fn an_ambiguous_identifier_is_refused_rather_than_guessed() {
    let server = MockLinearServer::start(vec![list_mock(vec![
        unread_notification("n-1", "VED-47"),
        unread_notification("n-2", "VED-47"),
    ])]);

    let output = run_cli(&["notification", "archive", "VED-47"], &mock_env(&server));
    assert!(!output.success());
    assert!(
        output.stderr.contains("2 notifications"),
        "the refusal says how many matched: {}",
        output.stderr
    );
}

#[test]
fn a_negative_limit_is_refused_without_a_request() {
    // `=` rather than a space: `--limit -1` is rejected by the parser as an unknown argument before
    // the command sees it, which is a different refusal than the one under test here.
    let server = MockLinearServer::start(vec![]);

    let output = run_cli(&["notification", "list", "--limit=-1"], &mock_env(&server));
    assert!(!output.success());
    assert!(
        output.stderr.contains("--limit must be 0 or greater"),
        "{}",
        output.stderr
    );
}

#[test]
fn the_human_listing_says_whose_they_are_and_how_many_are_unread() {
    let server = MockLinearServer::start(vec![
        list_mock(vec![
            unread_notification("n-1", "VED-47"),
            read_notification("n-2"),
        ]),
        unread_count_mock(1),
    ]);

    let output = run_cli(&["notification", "list"], &mock_env(&server));
    assert!(output.success(), "{}", output.stderr);
    assert!(
        output.stdout.contains("READ"),
        "the table has a read column"
    );
    assert!(
        output
            .stdout
            .contains("2 notifications shown, 1 unread for loner <l2859794@gmail.com>."),
        "the summary says whose and how many: {}",
        output.stdout
    );
}
