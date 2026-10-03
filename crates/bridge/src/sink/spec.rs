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
use crate::net;
use crate::sink::template;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SinkSpec {
    /// Base URL of the API, e.g. `http://127.0.0.1:3000/api/v1`.
    pub base_url: String,
    /// How this platform recognises its own web URLs, so an entity that *declares* a
    /// location elsewhere can be resolved to a scope here. Optional: a platform with
    /// no such shape simply never has links consulted for it.
    #[serde(default)]
    pub location: Option<LocationSpec>,
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
    /// How to read a *board*: which column a card sits in. Absent means the platform
    /// does not report placement, and a sweep then has no opinion about it - the same
    /// degradation as a platform that cannot be enumerated at all.
    #[serde(default)]
    pub board: Option<BoardSpec>,
}

/// A board, read: a container (a project) whose members are its *columns*, each holding
/// the issues on it. Enough to answer "where is this card?", which is the one question a
/// sweep has about placement.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoardSpec {
    /// The request that lists the board. `{scope}` and `{id}` are the repository and the
    /// project, as they are for any other container operation.
    pub list: Operation,
    /// Pointer to the array of columns; the whole response when omitted.
    #[serde(default)]
    pub columns: Option<String>,
    /// Pointer inside a column to the title a mapping names columns by.
    pub title: String,
    /// Pointer inside a column to its cards, each being an issue's number.
    pub cards: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthSpec {
    pub header: String,
    #[serde(default)]
    pub prefix: Option<String>,
}

/// How a platform's own URLs name a scope (a repository, a board).
///
/// The pattern is a URL template with one `{scope}` capture, e.g.
/// `https://git.example.com/{scope}`. It is how the engine can tell a link that
/// points at this platform from one that points at another: a URL that does not
/// match is simply not this platform's, never an error.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocationSpec {
    /// A URL template carrying exactly one `{scope}` capture.
    pub url: String,
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
    /// Putting an issue on a project's board. Declared by a `[sink.project]` half:
    /// the issue is the *container's* member, so the path names the project and the
    /// issue (`/projects/{id}/issues/{index}`). A platform without projects simply
    /// does not declare these, and the mirror places nothing rather than failing.
    #[serde(default)]
    pub assign: Option<Operation>,
    /// Taking an issue off the project it is on.
    #[serde(default)]
    pub unassign: Option<Operation>,
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
    /// Pointer inside each element to the colour, when the platform carries one.
    #[serde(default)]
    pub pick_color: Option<String>,
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

    /// Where the colour lives inside the same element, when the platform carries one.
    pub fn color_pick(&self) -> Option<&str> {
        match self {
            ReadField::Pointer(_) => None,
            ReadField::Detailed(spec) => spec.pick_color.as_deref(),
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
    pub milestone: Option<ReadField>,
    #[serde(default)]
    pub assignee: Option<ReadField>,
    /// The project the issue is on, where the platform reports one. A platform
    /// whose issue carries no such field leaves this undeclared, and the mirror
    /// falls back to the project it recorded on the pairing when deciding whether
    /// the issue moved.
    #[serde(default)]
    pub project: Option<ReadField>,
    /// A container's alias (a project's slug), where the platform exposes one. Read
    /// only so a route may name a project by it; it is an identity field, never part
    /// of the content a mirror compares or writes.
    #[serde(default)]
    pub slug: Option<ReadField>,
    /// The identifier a person sees (`VED-119`), where the platform has one apart
    /// from the id. Read only so a route may name an issue by it; identity, not
    /// content.
    #[serde(default)]
    pub identifier: Option<ReadField>,
    /// Locations the entity declares elsewhere (a project's external links), as a list
    /// of URLs. Identity, not content: never compared or written, only consulted to
    /// resolve a scope from a link that points at the sink platform.
    #[serde(default)]
    pub links: Option<ReadField>,
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
        if let Some(location) = &self.location {
            if location.url.matches("{scope}").count() != 1 {
                return Err(format!(
                    "location.url `{}` must carry exactly one `{{scope}}` capture",
                    location.url
                ));
            }
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
            let rendered = template::render(body, values);
            return Ok(Request {
                method,
                url,
                headers,
                repeatable: Self::is_repeatable(method, Some(&rendered)),
                body: Some(rendered),
            });
        }
        Ok(Request {
            method,
            url,
            headers,
            repeatable: Self::is_repeatable(method, None),
            body: None,
        })
    }

    /// Whether this request may be sent twice.
    ///
    /// Two ways to know, and no third: the method is idempotent by definition, or the body is a
    /// GraphQL document that reads. A create is a POST carrying a `mutation`, and neither test
    /// lets it be repeated - a retried create is a duplicate issue.
    fn is_repeatable(method: Method, body: Option<&Value>) -> bool {
        if net::is_repeatable_method(method.as_str()) {
            return true;
        }
        body.and_then(|body| body.get("query"))
            .and_then(Value::as_str)
            .is_some_and(net::is_repeatable_document)
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
///
/// `index` is the *other* entity a membership path addresses
/// (`/projects/{id}/issues/{index}`): the container is `id`, the member `index`.
fn placeholders(values: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for key in ["scope", "scope_id", "id", "index", "name"] {
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
        ("assign", project.assign.as_ref()),
        ("unassign", project.unassign.as_ref()),
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
mod tests;
