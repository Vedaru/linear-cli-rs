//! The write half of a platform, as data.
//!
//! Same principle as the read half (see [`crate::sources::declarative`]): there is
//! no `forgejo_client.rs`. A platform says where to send what, and the engine
//! executes it. The pieces a platform describes:
//!
//! - **auth**: one header, an optional prefix (`token `, `Bearer `), the secret
//!   itself coming from the deployment's environment;
//! - **operations**: create/update/fetch/delete an issue, comment on it, set its
//!   labels, transition its state, attach a link - each a method, a path with
//!   `{scope}`/`{id}` placeholders, and a body template;
//! - **read**: response pointers back into neutral fields, so a re-read produces
//!   the same [`crate::domain::IssueFields`] the other platform would;
//! - **lookup**: how a *name* becomes the id that platform needs (a Forgejo label
//!   id, a Linear state UUID), including creating the label when it is missing.
//!
//! GraphQL is not a special case: its `variables` object is a body template with
//! the same directives, and its response is addressed by the same JSON pointers.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

use crate::domain::Secret;
use crate::error::{Error, Result};
use crate::http_client::{Method, Request};
use crate::sink::template;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SinkSpec {
    /// Base URL of the API, e.g. `http://127.0.0.1:3000/api/v1`.
    pub base_url: String,
    #[serde(default)]
    pub auth: Option<AuthSpec>,
    /// Static headers sent with every request (`Accept`, an API version, ...).
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// JSON pointer to an error list that arrives inside a *successful* response.
    /// GraphQL is the reason this exists: Linear answers a failed mutation with
    /// `200 OK` and `{"errors": [...]}`, and a write path that trusts the status
    /// code would report a sync that never happened.
    #[serde(default)]
    pub error_pointer: Option<String>,
    pub issue: IssueSpec,
    /// The write half for *projects*, when this platform mirrors them. Absent means
    /// the platform does not do projects through this bridge, and a mapping that
    /// asks it to refuses by name.
    #[serde(default)]
    pub project: Option<IssueSpec>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthSpec {
    pub header: String,
    #[serde(default)]
    pub prefix: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IssueSpec {
    #[serde(default)]
    pub create: Option<Operation>,
    #[serde(default)]
    pub update: Option<Operation>,
    #[serde(default)]
    pub fetch: Option<Operation>,
    #[serde(default)]
    pub delete: Option<Operation>,
    #[serde(default)]
    pub comment: Option<CommentSpec>,
    /// Setting a state by name. `set.path`/`body` receive `$state` (the name) or
    /// `$state_id` (resolved through the `state` lookup) - whichever the platform
    /// needs, which is the difference between a forge and Linear written down.
    #[serde(default)]
    pub transition: Option<Operation>,
    /// Attaching a link to an issue (Linear's attachments; a forge has none).
    #[serde(default)]
    pub attach: Option<Operation>,
    /// Replacing an issue's labels wholesale.
    #[serde(default)]
    pub labels: Option<Operation>,
    /// Reading a whole scope, for a sweep. Absent means this platform cannot be
    /// enumerated, which a sweep reports rather than works around.
    #[serde(default)]
    pub list: Option<ListSpec>,
    #[serde(default)]
    pub read: Option<ReadSpec>,
    /// Name -> id resolution, keyed by the kind a preset asks for: `label`,
    /// `state`, `team`.
    #[serde(default)]
    pub lookup: BTreeMap<String, LookupSpec>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    pub method: String,
    /// Path appended to `base_url`; `{scope}`, `{id}` and `{name}` are
    /// substituted from the call's values.
    pub path: String,
    #[serde(default)]
    pub body: Option<Value>,
    /// JSON pointer to the created entity's id in the response.
    #[serde(default)]
    pub id: Option<String>,
    /// JSON pointer to its URL, for the link the other side attaches.
    #[serde(default)]
    pub url: Option<String>,
    /// JSON pointer to the array of candidates, for a `list` operation.
    #[serde(default)]
    pub items: Option<String>,
    /// Static query parameters, appended after the rendered path.
    #[serde(default)]
    pub query: BTreeMap<String, String>,
}

/// What a platform can do to a comment.
///
/// Three operations rather than one, because mirroring a comment is not over when
/// it is posted: an edit has to reach the copy, and so does a deletion - and they
/// are different requests. A preset declares the ones its platform can do, and the
/// engine refuses the rest by name rather than inventing a request.
///
/// `$id` means the *issue* in `create` and the *comment* in `update`/`delete`: the
/// create is addressed through its parent, and everything after it through itself.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommentSpec {
    pub create: Operation,
    #[serde(default)]
    pub update: Option<Operation>,
    #[serde(default)]
    pub delete: Option<Operation>,
}

/// Reading every issue in a scope.
///
/// A different question from `fetch` (one entity whose id is known) and from a
/// `lookup` (a name resolved to an id): a sweep starts from "everything here", so it
/// needs the collection, a way to read one item out of it, and how to walk to the
/// next page - because pagination is the one part of this that is genuinely each
/// platform's own.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListSpec {
    /// The request that lists, with `$page` or `$cursor` available to it whichever
    /// this platform pages with. Its `items` pointer says where the collection is in
    /// the response - the same place a `lookup` declares it, which is why this key is
    /// `request` and not a second `list`.
    pub request: Operation,
    /// Pointers **inside each item** - unlike `[sink.issue.read]`, which reads one
    /// issue from the root of a single-issue response.
    pub read: ReadSpec,
    #[serde(default)]
    pub paginate: Option<PaginateSpec>,
}

/// How to ask for the next page.
///
/// Two shapes rather than a general grammar, because they are what the platforms in
/// use actually offer: a page number in a query string, or an opaque cursor in a
/// GraphQL variable. Guessing at a third would be inventing requirements.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaginateSpec {
    #[serde(default)]
    pub page: Option<NumberedPage>,
    #[serde(default)]
    pub cursor: Option<CursorPage>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NumberedPage {
    /// The query parameter carrying the page number (1-based).
    pub param: String,
    /// How many items a full page holds. A short page is how a numbered API says
    /// "that was the last one", and it costs no extra request to find out.
    pub size: usize,
    /// The parameter carrying the size, when the platform wants to be told.
    #[serde(default)]
    pub size_param: Option<String>,
    /// Stop after this many pages whatever the platform claims, so a platform that
    /// always reports a full page cannot spin forever.
    #[serde(default = "default_max_pages")]
    pub max_pages: usize,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CursorPage {
    /// Pointer to the next cursor in the response. Where it *goes* is the request's
    /// own business: `$cursor` in its `variables` or its query.
    pub next: String,
    /// Pointer to the platform's "there are more" flag, when it has one. Without it
    /// the walk stops at the first empty cursor.
    #[serde(default)]
    pub more: Option<String>,
    #[serde(default = "default_max_pages")]
    pub max_pages: usize,
}

fn default_max_pages() -> usize {
    50
}

impl PaginateSpec {
    /// How many pages one walk may read, whichever way this platform pages.
    pub fn max_pages(&self) -> usize {
        self.page
            .as_ref()
            .map(|page| page.max_pages)
            .or_else(|| self.cursor.as_ref().map(|cursor| cursor.max_pages))
            .unwrap_or(1)
    }

    /// Exactly one way of paging, and the fields that way needs.
    pub fn validate(&self, label: &str) -> std::result::Result<(), String> {
        match (&self.page, &self.cursor) {
            (Some(_), Some(_)) => Err(format!(
                "{label}: `paginate` declares both a page number and a cursor, and a platform pages one way"
            )),
            (None, None) => Err(format!(
                "{label}: `paginate` declares neither a page number nor a cursor"
            )),
            (Some(page), None) if page.size == 0 || page.max_pages == 0 => Err(format!(
                "{label}: `paginate.page` needs a size and a page budget above zero"
            )),
            (None, Some(cursor)) if cursor.max_pages == 0 => Err(format!(
                "{label}: `paginate.cursor.max_pages` is 0, so a sweep would read nothing"
            )),
            _ => Ok(()),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LookupSpec {
    /// The request that lists candidates (a REST collection, or a GraphQL query
    /// whose `variables` carry the scope).
    pub list: Operation,
    /// JSON pointer to the array of candidates; the whole response when omitted.
    #[serde(default)]
    pub items: Option<String>,
    /// Pointer inside a candidate to the name being matched (case-insensitive).
    pub name: String,
    /// Pointer inside a candidate to the value to return.
    pub id: String,
    /// What to do when nothing matches. Without it, an unknown name is an error:
    /// silently skipping a label is worse than refusing to sync the issue.
    #[serde(default)]
    pub create: Option<Operation>,
}

/// How to read an issue back. One entry per neutral field, each either a plain
/// JSON pointer (`title = "/title"`) or a description when the platform's shape
/// differs from the neutral one.
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum ReadField {
    /// A pointer to a scalar or an array of strings.
    Pointer(String),
    Detailed(ReadFieldSpec),
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ReadFieldSpec {
    #[serde(default)]
    pub path: Option<String>,
    /// Pointer into each element of the array at `path`.
    #[serde(default)]
    pub pick: Option<String>,
    /// Derive the value from the labels instead of reading a field of its own
    /// (how a priority survives on a platform that has no priority field).
    #[serde(default)]
    pub from_labels: bool,
}

impl ReadField {
    pub fn path(&self) -> Option<&str> {
        match self {
            ReadField::Pointer(path) => Some(path),
            ReadField::Detailed(spec) => spec.path.as_deref(),
        }
    }

    pub fn pick(&self) -> Option<&str> {
        match self {
            ReadField::Pointer(_) => None,
            ReadField::Detailed(spec) => spec.pick.as_deref(),
        }
    }

    pub fn from_labels(&self) -> bool {
        match self {
            ReadField::Pointer(_) => false,
            ReadField::Detailed(spec) => spec.from_labels,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadSpec {
    #[serde(default)]
    pub title: Option<ReadField>,
    #[serde(default)]
    pub body: Option<ReadField>,
    #[serde(default)]
    pub labels: Option<ReadField>,
    #[serde(default)]
    pub priority: Option<ReadField>,
    #[serde(default)]
    pub due_date: Option<ReadField>,
    #[serde(default)]
    pub assignee: Option<ReadField>,
    /// The platform's state, as a name (`/state/name` on Linear, `/state` on a
    /// forge).
    #[serde(default)]
    pub state: Option<ReadField>,
    /// Where the issue's id and URL live, for `fetch`.
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

impl SinkSpec {
    pub fn from_toml(text: &str) -> Result<Self> {
        let spec: Self = toml::from_str(text)
            .map_err(|error| Error::Config(format!("cannot parse the sink spec: {error}")))?;
        spec.validate()
            .map_err(|error| Error::Config(format!("invalid sink spec: {error}")))?;
        Ok(spec)
    }

    /// Everything checkable without a request. Called at load time so a typo in a
    /// preset fails `linear webhook serve --check` instead of a live sync.
    pub fn validate(&self) -> std::result::Result<(), String> {
        if !self.base_url.starts_with("http://") && !self.base_url.starts_with("https://") {
            return Err(format!("base_url `{}` must be http(s)", self.base_url));
        }
        if let Some(auth) = &self.auth {
            if auth.header.trim().is_empty() {
                return Err("auth.header must name a header".into());
            }
        }
        if let Some(pointer) = &self.error_pointer {
            if !pointer.starts_with('/') {
                return Err(format!("error_pointer `{pointer}` must be a JSON pointer"));
            }
        }
        let issue = &self.issue;
        let operations = [
            ("create", issue.create.as_ref()),
            ("update", issue.update.as_ref()),
            ("fetch", issue.fetch.as_ref()),
            ("delete", issue.delete.as_ref()),
            ("transition", issue.transition.as_ref()),
            ("attach", issue.attach.as_ref()),
            ("labels", issue.labels.as_ref()),
        ];
        if operations.iter().all(|(_, operation)| operation.is_none()) {
            return Err("an issue spec needs at least one operation".into());
        }
        for (name, operation) in operations {
            if let Some(operation) = operation {
                validate_operation(name, operation)?;
            }
        }
        if let Some(list) = &issue.list {
            validate_operation("list", &list.request)?;
            if list.read.id.is_none() {
                // Every item has to be identifiable, or a sweep cannot tell what it
                // is looking at and cannot pair anything.
                return Err("list.read.id is required: a sweep pairs by id".into());
            }
            if let Some(paginate) = &list.paginate {
                paginate.validate("list.paginate")?;
            }
        }
        if let Some(comment) = &issue.comment {
            validate_operation("comment.create", &comment.create)?;
            if let Some(update) = &comment.update {
                validate_operation("comment.update", update)?;
            }
            if let Some(delete) = &comment.delete {
                validate_operation("comment.delete", delete)?;
            }
        }
        for (kind, lookup) in &issue.lookup {
            validate_operation(&format!("lookup.{kind}.list"), &lookup.list)?;
            if lookup.name.is_empty() || lookup.id.is_empty() {
                return Err(format!("lookup.{kind} needs `name` and `id` pointers"));
            }
            if let Some(create) = &lookup.create {
                validate_operation(&format!("lookup.{kind}.create"), create)?;
            }
        }
        if let Some(project) = &self.project {
            validate_project(project)?;
        }
        Ok(())
    }

    /// The concrete request for an operation, with `values` substituted.
    pub fn request(
        &self,
        operation: &Operation,
        values: &Value,
        secret: Option<&Secret>,
    ) -> Result<Request> {
        let method = Method::parse(&operation.method).ok_or_else(|| {
            Error::Config(format!("`{}` is not an HTTP method", operation.method))
        })?;
        let mut url = self.url(operation, values)?;
        if !operation.query.is_empty() {
            let query: Vec<String> = operation
                .query
                .iter()
                // Both key and value are single components here: a `/` inside a
                // query parameter is data, unlike in a path.
                // Values are templates, so a paginated preset can write
                // `page = "$page"`; a literal passes through unchanged.
                .map(|(key, value)| {
                    let rendered = template::render_component(value, values).unwrap_or_default();
                    format!("{}={}", encode(key), encode(&rendered))
                })
                .collect();
            url.push(if url.contains('?') { '&' } else { '?' });
            url.push_str(&query.join("&"));
        }

        let mut headers: Vec<(String, String)> = self
            .headers
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        if let Some(auth) = &self.auth {
            let secret = secret.ok_or_else(|| {
                Error::Config(format!(
                    "connector `{}` needs a token for its `{}` header",
                    self.base_url, auth.header
                ))
            })?;
            let value = match &auth.prefix {
                Some(prefix) => format!("{prefix}{}", secret.expose()),
                None => secret.expose().to_string(),
            };
            headers.push((auth.header.clone(), value));
        }
        // JSON in, JSON out; a body-less DELETE must not claim a content type.
        if let Some(body) = &operation.body {
            headers.push(("Content-Type".into(), "application/json".into()));
            return Ok(Request {
                method,
                url,
                headers,
                body: Some(template::render(body, values)),
            });
        }
        Ok(Request {
            method,
            url,
            headers,
            body: None,
        })
    }

    /// `base_url` + the rendered path.
    ///
    /// An empty path is the base URL itself, which is how a GraphQL endpoint is
    /// written (`base_url = "https://api.linear.app/graphql"`, no path).
    pub fn url(&self, operation: &Operation, values: &Value) -> Result<String> {
        let base = self.base_url.trim_end_matches('/');
        if operation.path.is_empty() {
            return Ok(base.to_string());
        }
        let mut path = operation.path.clone();
        for (key, value) in placeholders(values) {
            path = path.replace(&format!("{{{key}}}"), &encode_path(&value));
        }
        if path.contains('{') {
            return Err(Error::Config(format!(
                "path `{}` still has an unsubstituted placeholder",
                operation.path
            )));
        }
        if !path.starts_with('/') {
            path.insert(0, '/');
        }
        Ok(format!("{base}{path}"))
    }
}

/// The values a path may name. Paths are configuration (not user input), but a
/// missing value is still an error rather than a literal `{id}` sent upstream.
fn placeholders(values: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for key in ["scope", "scope_id", "id", "name"] {
        if let Some(value) = values.get(key).and_then(scalar) {
            out.push((key.to_string(), value));
        }
    }
    out
}

fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

/// The project half of a spec: its own create/update/fetch/list.
///
/// A project spec may declare only some of these - a platform read but not written
/// is served by the source half - but whatever it declares must be well-formed, and a
/// declared list must be able to identify its items or a sweep cannot pair them.
fn validate_project(project: &IssueSpec) -> std::result::Result<(), String> {
    let operations = [
        ("create", project.create.as_ref()),
        ("update", project.update.as_ref()),
        ("fetch", project.fetch.as_ref()),
        ("delete", project.delete.as_ref()),
        ("transition", project.transition.as_ref()),
        ("attach", project.attach.as_ref()),
        ("labels", project.labels.as_ref()),
    ];
    for (name, operation) in operations {
        if let Some(operation) = operation {
            validate_operation(&format!("project.{name}"), operation)?;
        }
    }
    if let Some(comment) = &project.comment {
        validate_operation("project.comment.create", &comment.create)?;
    }
    if let Some(list) = &project.list {
        validate_operation("project.list", &list.request)?;
        if list.read.id.is_none() {
            return Err("project.list.read.id is required: a sweep pairs by id".into());
        }
        if let Some(paginate) = &list.paginate {
            paginate.validate("project.list.paginate")?;
        }
    }
    Ok(())
}

fn validate_operation(name: &str, operation: &Operation) -> std::result::Result<(), String> {
    if Method::parse(&operation.method).is_none() {
        return Err(format!(
            "{name}: `{}` is not an HTTP method",
            operation.method
        ));
    }
    if operation.path.contains('{') && !operation.path.contains('}') {
        return Err(format!("{name}: path has an unclosed placeholder"));
    }
    if let Some(body) = &operation.body {
        template::validate(body).map_err(|error| format!("{name}: {error}"))?;
    }
    // Query values are rendered too (that is how `$page` reaches a request), so a
    // typo there is as much a load error as one in the body.
    for (key, value) in &operation.query {
        template::validate_component(value)
            .map_err(|error| format!("{name}: query `{key}`: {error}"))?;
    }
    Ok(())
}

/// Percent-encode everything a URL cannot carry, but keep `/`: a scope *is* a
/// path prefix on the platforms this ships (`Vedaru/linear-cli-rs`), and encoding
/// it would route the request to a repository that does not exist.
fn encode_path(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Query keys and values are single components: `/` in them is data, not
/// structure.
fn encode(value: &str) -> String {
    encode_path(value).replace('/', "%2F")
}

#[cfg(test)]
mod tests {
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
}
