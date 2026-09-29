//! Self-tests for the mock Linear server. These do not invoke the `linear`
//! binary, so they exercise the harness contract in isolation.

mod common;

use std::io::{Read, Write};
use std::net::TcpStream;

use common::{MockLinearServer, MockResponse};
use serde_json::{json, Value};

/// Minimal HTTP/1.1 client so the harness tests do not depend on ureq's API.
fn http(method: &str, url: &str, headers: &[(&str, &str)], body: &[u8]) -> (u16, String) {
    let rest = url.strip_prefix("http://").expect("http url");
    let (authority, path) = match rest.split_once('/') {
        Some((authority, path)) => (authority, format!("/{path}")),
        None => (rest, "/".to_string()),
    };
    let mut stream = TcpStream::connect(authority).expect("connect");
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {authority}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).unwrap();
    stream.write_all(body).unwrap();

    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let (head, payload) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    (status, payload.to_string())
}

fn graphql(query: &str, variables: Value) -> Vec<u8> {
    json!({ "query": query, "variables": variables })
        .to_string()
        .into_bytes()
}

#[test]
fn matches_query_by_name_and_returns_response() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "GetTeam",
        json!({ "data": { "team": { "id": "t1", "key": "ENG", "name": "Engineering" } } }),
    )]);

    let (status, body) = http(
        "POST",
        &server.get_endpoint(),
        &[("Content-Type", "application/json")],
        &graphql(
            "query GetTeam($id: String!) { team(id: $id) { id key name } }",
            json!({ "id": "t1" }),
        ),
    );

    assert_eq!(status, 200);
    let parsed: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed["data"]["team"]["key"], "ENG");
}

#[test]
fn variable_filters_select_the_right_response() {
    let server = MockLinearServer::start(vec![
        MockResponse::new("GetIssue", json!({ "data": { "issue": { "id": "one" } } }))
            .with_variables(json!({ "id": "i1" })),
        MockResponse::new("GetIssue", json!({ "data": { "issue": { "id": "two" } } }))
            .with_variables(json!({ "id": "i2" })),
    ]);

    let (_, body) = http(
        "POST",
        &server.get_endpoint(),
        &[],
        &graphql("query GetIssue($id: String!) { issue(id: $id) { id } }", json!({ "id": "i2" })),
    );
    let parsed: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed["data"]["issue"]["id"], "two");
}

#[test]
fn unmatched_query_reports_no_mock_configured() {
    let server = MockLinearServer::start(vec![]);
    let (status, body) = http(
        "POST",
        &server.get_endpoint(),
        &[],
        &graphql("query SomethingElse { viewer { id } }", json!({})),
    );

    assert_eq!(status, 200);
    let parsed: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        parsed["errors"][0]["extensions"]["code"],
        "NO_MOCK_CONFIGURED"
    );
    assert_eq!(parsed["errors"][0]["extensions"]["query"], "SomethingElse");
}

#[test]
fn mutation_names_are_extracted() {
    let server = MockLinearServer::start(vec![MockResponse::new(
        "CreateIssue",
        json!({ "data": { "issueCreate": { "success": true } } }),
    )]);

    let (_, body) = http(
        "POST",
        &server.get_endpoint(),
        &[],
        &graphql("mutation CreateIssue($input: IssueCreateInput!) { issueCreate(input: $input) { success } }", json!({})),
    );
    let parsed: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed["data"]["issueCreate"]["success"], true);
}

#[test]
fn captures_upload_body_and_content_type() {
    let server = MockLinearServer::start(vec![]);
    let url = server.get_upload_url();

    let (status, _) = http(
        "PUT",
        &url,
        &[("Content-Type", "image/png")],
        b"\x89PNG fake bytes",
    );

    assert_eq!(status, 200);
    let uploads = server.uploads();
    assert_eq!(uploads.len(), 1);
    assert_eq!(uploads[0].content_type.as_deref(), Some("image/png"));
    assert_eq!(uploads[0].body, b"\x89PNG fake bytes");
    assert!(uploads[0].pathname.starts_with("/upload"));
}
