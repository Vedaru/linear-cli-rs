use super::*;
use serde_json::json;

const FORGEJO: &str = r#"
base_url = "http://127.0.0.1:3000/api/v1"
[auth]
header = "Authorization"
prefix = "token "
[issue.create]
method = "POST"
path = "/repos/{scope}/issues"
id = "/number"
url = "/html_url"
[issue.create.body]
title = "$title"
body = "$body"
due_date = "$due_date"
[issue.fetch]
method = "GET"
path = "/repos/{scope}/issues/{id}"
"#;

fn spec() -> SinkSpec {
    SinkSpec::from_toml(FORGEJO).expect("the fixture is valid")
}

#[test]
fn a_request_is_built_from_the_operation_and_the_values() {
    let spec = spec();
    let values = json!({ "scope": "Vedaru/linear-cli-rs", "title": "T", "body": "B" });
    let request = spec
        .request(
            spec.issue.create.as_ref().unwrap(),
            &values,
            Some(&Secret::new("token-value")),
        )
        .unwrap();

    assert_eq!(request.method, Method::Post);
    assert_eq!(
        request.url,
        "http://127.0.0.1:3000/api/v1/repos/Vedaru/linear-cli-rs/issues"
    );
    assert_eq!(
        request.body.as_ref().unwrap(),
        &json!({ "title": "T", "body": "B" }),
        "an absent due_date drops the key"
    );
    assert!(request
        .headers
        .contains(&("Authorization".to_string(), "token token-value".to_string())));
    assert!(request
        .headers
        .contains(&("Content-Type".to_string(), "application/json".to_string())));
}

#[test]
fn a_missing_token_is_an_error_naming_the_header() {
    let spec = spec();
    let values = json!({ "scope": "a/b", "title": "T", "body": "B" });
    let error = spec
        .request(spec.issue.create.as_ref().unwrap(), &values, None)
        .unwrap_err()
        .to_string();
    assert!(error.contains("Authorization"), "{error}");
}

#[test]
fn a_path_placeholder_keeps_the_scope_hierarchy() {
    let spec = spec();
    let values = json!({ "scope": "owner/repo", "id": "12" });
    let request = spec
        .request(
            spec.issue.fetch.as_ref().unwrap(),
            &values,
            Some(&Secret::new("token-value")),
        )
        .unwrap();
    assert_eq!(
        request.url,
        "http://127.0.0.1:3000/api/v1/repos/owner/repo/issues/12"
    );
    assert_eq!(request.body, None);
    assert!(
        !request
            .headers
            .iter()
            .any(|(name, _)| name == "Content-Type"),
        "a body-less request must not claim a content type"
    );
}

#[test]
fn an_unsubstituted_placeholder_is_an_error_not_a_url() {
    let spec = spec();
    let values = json!({ "scope": "a/b" });
    let error = spec
        .request(spec.issue.fetch.as_ref().unwrap(), &values, None)
        .unwrap_err()
        .to_string();
    assert!(error.contains("{id}"), "{error}");
}

#[test]
fn a_graphql_operation_is_the_same_shape() {
    let text = r#"
base_url = "https://api.linear.app/graphql"
[auth]
header = "Authorization"
[issue.create]
method = "POST"
path = ""
id = "/data/issueCreate/issue/id"
url = "/data/issueCreate/issue/url"
[issue.create.body]
query = "mutation ($input: IssueCreateInput!) { issueCreate(input: $input) { success } }"
[issue.create.body.variables.input]
teamId = "$scope_id"
title = "$title"
labelIds = "$label_ids"
priority = "$priority"
"#;
    let spec = SinkSpec::from_toml(text).unwrap();
    let values = json!({
        "scope_id": "uuid-1", "title": "T", "label_ids": [2, 3], "priority": 2
    });
    let request = spec
        .request(
            spec.issue.create.as_ref().unwrap(),
            &values,
            Some(&Secret::new("lin_api_x")),
        )
        .unwrap();
    assert_eq!(request.url, "https://api.linear.app/graphql");
    let body = request.body.unwrap();
    assert_eq!(body["variables"]["input"]["teamId"], "uuid-1");
    assert_eq!(body["variables"]["input"]["labelIds"], json!([2, 3]));
    assert_eq!(body["variables"]["input"]["priority"], 2);
    assert!(body["query"].as_str().unwrap().contains("issueCreate"));
}

#[test]
fn query_parameters_are_appended_and_encoded() {
    let text = r#"
base_url = "http://x/api"
[issue.fetch]
method = "GET"
path = "/issues"
query = { state = "open", filter = "a/b" }
"#;
    let spec = SinkSpec::from_toml(text).unwrap();
    let request = spec
        .request(spec.issue.fetch.as_ref().unwrap(), &json!({}), None)
        .unwrap();
    assert!(
        request.url.starts_with("http://x/api/issues?"),
        "{}",
        request.url
    );
    assert!(request.url.contains("state=open"), "{}", request.url);
    assert!(request.url.contains("filter=a%2Fb"), "{}", request.url);
}

#[test]
fn validation_catches_the_mistakes_a_preset_will_make() {
    let bad_method = FORGEJO.replace("method = \"POST\"", "method = \"FETCH\"");
    let error = SinkSpec::from_toml(&bad_method).unwrap_err().to_string();
    assert!(error.contains("FETCH"), "{error}");

    let bad_directive = FORGEJO.replace("title = \"$title\"", "title = \"$titel\"");
    let error = SinkSpec::from_toml(&bad_directive).unwrap_err().to_string();
    assert!(error.contains("$titel"), "{error}");

    let no_ops = r#"
base_url = "http://x"
[issue]
"#;
    let error = SinkSpec::from_toml(no_ops).unwrap_err().to_string();
    assert!(error.contains("at least one operation"), "{error}");

    let bad_base = FORGEJO.replace("http://127.0.0.1:3000/api/v1", "not-a-url");
    let error = SinkSpec::from_toml(&bad_base).unwrap_err().to_string();
    assert!(error.contains("must be http"), "{error}");

    let unknown_key = FORGEJO.replace("[issue.fetch]", "[issue.fetch]\nunexpected = 1");
    assert!(SinkSpec::from_toml(&unknown_key).is_err());
}

#[test]
fn read_fields_accept_both_the_shorthand_and_the_description() {
    let text = r#"
base_url = "http://x"
[issue.fetch]
method = "GET"
path = "/issue/{id}"
[issue.read]
title = "/title"
labels = { path = "/labels", pick = "/name" }
priority = { from_labels = true }
"#;
    let spec = SinkSpec::from_toml(text).unwrap();
    let read = spec.issue.read.expect("a read spec");
    assert_eq!(
        read.title.as_ref().and_then(ReadField::path),
        Some("/title")
    );
    assert_eq!(
        read.labels.as_ref().and_then(ReadField::pick),
        Some("/name")
    );
    assert!(read.priority.as_ref().is_some_and(ReadField::from_labels));
}
