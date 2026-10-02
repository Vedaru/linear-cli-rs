//! The generic sink engine: executes a [`SinkSpec`] against a platform.
//!
//! The same shape as the source engine - one implementation, many platforms - but
//! for writing. What it adds over "render and send" is the two things a write path
//! cannot do without:
//!
//! - **name resolution with caching.** Most platforms want an id where a human
//!   writes a name (a Forgejo label id, a Linear state UUID, a Linear team UUID).
//!   The spec says how to list candidates and how to create a missing one; this
//!   engine memoises the answers per (kind, scope) because the write path resolves
//!   the same handful of names on every issue edit.
//! - **reads that produce neutral fields.** A reconciliation compares what the two
//!   sides hold, so the response of a `fetch` has to come back as the same
//!   [`IssueFields`] either platform would have produced.

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::{json, Value};

use crate::domain::{
    canonical_labels, labels_to_priority, priority_to_label, Capabilities, ConnectorId,
    IssueFields, Secret,
};
use crate::error::{Error, Result};
use crate::http_client::{HttpClient, Request, Response};
use crate::pointer::{resolve, resolve_string};
use crate::sink::spec::{LookupSpec, Operation, ReadField, ReadSpec, SinkSpec};
use crate::sink::{RemoteIssue, RemoteRef, Sink};

/// A lookup a spec declares: the kinds the engine will resolve.
const TEAM: &str = "team";
const LABEL: &str = "label";
const STATE: &str = "state";
const ASSIGNEE: &str = "assignee";

pub struct DeclarativeSink {
    id: ConnectorId,
    spec: SinkSpec,
    secret: Option<Secret>,
    capabilities: Capabilities,
    client: HttpClient,
    /// Name and candidate-list memoisation, per (kind, scope).
    lookups: Mutex<Lookups>,
}

impl DeclarativeSink {
    pub fn new(
        id: impl Into<ConnectorId>,
        spec: SinkSpec,
        secret: Option<Secret>,
        capabilities: Capabilities,
    ) -> Self {
        Self {
            id: id.into(),
            spec,
            secret,
            capabilities,
            client: HttpClient::new(),
            lookups: Mutex::new(Lookups::default()),
        }
    }

    pub fn spec(&self) -> &SinkSpec {
        &self.spec
    }

    fn send(&self, request: &Request) -> Result<Response> {
        let response = self.client.send(request)?;
        if !response.is_success() {
            return Err(Error::Upstream(format!(
                "{} {} -> {} {}",
                request.method.as_str(),
                request.url,
                response.status,
                response.summary()
            )));
        }
        // A transport-level success is not an operation-level success: GraphQL
        // reports a rejected mutation as `200 OK` with an `errors` array, and a
        // sync that believed the status code would be silently wrong.
        if let Some(pointer) = &self.spec.error_pointer {
            if let Some(errors) = resolve(&response.body, pointer) {
                if errors.as_array().is_some_and(|errors| !errors.is_empty()) {
                    return Err(Error::Upstream(format!(
                        "{} {} -> {} {}",
                        request.method.as_str(),
                        request.url,
                        response.status,
                        errors
                    )));
                }
            }
        }
        Ok(response)
    }

    fn operation<'a>(&self, name: &str, operation: Option<&'a Operation>) -> Result<&'a Operation> {
        operation.ok_or_else(|| Error::Unsupported(name.to_string(), self.id.clone()))
    }

    /// The cache, used in short scopes only - never held across a request.
    fn lookups(&self) -> std::sync::MutexGuard<'_, Lookups> {
        // A poisoned cache still holds valid data (append-only ids), and refusing
        // every later write because one earlier one panicked would turn a single
        // failure into a permanent one.
        self.lookups
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The labels to send to *this* platform.
    ///
    /// The neutral set excludes the `priority:*` labels, because on a platform
    /// with a priority field the priority is not a label. On a platform without
    /// one - the capability says so - the priority travels as a label instead, so
    /// it is added back here: dropping it would quietly lose the priority of every
    /// issue the moment it crossed to a forge.
    fn outbound_labels(&self, fields: &IssueFields) -> Vec<String> {
        let mut labels = fields.canonical_labels();
        if !self.capabilities.priorities {
            if let Some(label) = priority_to_label(fields.priority) {
                labels.push(label.to_string());
            }
        }
        labels
    }

    /// The values a template may use, resolving only what the template asks for:
    /// a request that never mentions `$label_ids` never costs a label lookup.
    fn context(&self, operation: &Operation, call: &Call<'_>) -> Result<Value> {
        let scope = call.scope;
        let mut values = json!({ "scope": scope });
        if let Some(id) = call.id {
            values["id"] = json!(id);
        }
        if let Some(comment) = call.comment {
            values["body"] = json!(comment);
        }
        if let Some(title) = call.title {
            values["title"] = json!(title);
        }
        if let Some(url) = call.url {
            values["url"] = json!(url);
        }

        if let Some(fields) = call.fields {
            values["title"] = json!(fields.title);
            values["body"] = json!(fields.body);
            values["labels"] = json!(self.outbound_labels(fields));
            values["priority"] = if fields.priority == 0 {
                Value::Null
            } else {
                json!(fields.priority)
            };
            values["due_date"] = optional(fields.due_date.as_deref());
            values["assignee"] = optional(fields.assignee.as_deref());
            // Forges take a list of assignees; the neutral model carries one, so
            // the list form is derived rather than asked of the sync engine.
            values["assignees"] = json!(fields.assignee.iter().collect::<Vec<_>>());

            if uses(&operation.body, "$label_ids") {
                let names = self.outbound_labels(fields);
                let mut ids = Vec::with_capacity(names.len());
                for name in &names {
                    ids.push(self.resolve(LABEL, scope, name)?);
                }
                values["label_ids"] = json!(ids);
            }
        }

        if let Some(state) = call.state {
            values["state"] = json!(state);
            if uses(&operation.body, "$state_id") {
                values["state_id"] = json!(self.resolve(STATE, scope, state)?);
            }
        }
        if uses(&operation.body, "$assignee_id") {
            // Only resolved when the platform needs an id: a forge takes the login
            // as written, so it never pays for this lookup.
            if let Some(assignee) = call.fields.and_then(|fields| fields.assignee.as_deref()) {
                values["assignee_id"] = json!(self.resolve(ASSIGNEE, scope, assignee)?);
            }
        }
        if uses(&operation.body, "$scope_id") {
            // The team/space the scope names, resolved through the same
            // mechanism: Linear's mutations take a team UUID where a human writes
            // a team key.
            values["scope_id"] = json!(self.resolve(TEAM, scope, scope)?);
        }
        Ok(values)
    }

    /// Resolve a name to a platform id, memoised per (kind, scope).
    ///
    /// The id keeps the *JSON type* the platform answered with: a forge numbers
    /// its labels (`"labels": [3, 9]`), Linear names them with UUID strings, and
    /// the preset said which pointer to take - so stringifying here would be the
    /// engine overruling the platform.
    fn resolve(&self, kind: &str, scope: &str, name: &str) -> Result<Value> {
        let lookup = self.spec.issue.lookup.get(kind).ok_or_else(|| {
            Error::Config(format!(
                "connector `{}` has no `{kind}` lookup, but a template asked for `{kind}` ids",
                self.id
            ))
        })?;
        if let Some(id) = self.lookups().resolved(kind, scope, name) {
            return Ok(id);
        }

        for candidate in self.candidates(kind, lookup, scope)? {
            let found = resolve_string(&candidate, &lookup.name);
            if found
                .as_deref()
                .is_some_and(|found| found.eq_ignore_ascii_case(name))
            {
                if let Some(id) = resolve(&candidate, &lookup.id).cloned() {
                    self.lookups().remember(kind, scope, name, &id);
                    return Ok(id);
                }
            }
        }

        let create = lookup.create.as_ref().ok_or_else(|| {
            Error::Upstream(format!(
                "no {kind} named `{name}` exists on {} scope `{scope}`, and the spec cannot create one",
                self.id
            ))
        })?;
        let values = self.lookup_values(kind, scope, Some(name), &create.body)?;
        let request = self.spec.request(create, &values, self.secret.as_ref())?;
        let response = self.send(&request)?;
        let id = create
            .id
            .as_deref()
            .and_then(|pointer| resolve(&response.body, pointer).cloned())
            .ok_or_else(|| {
                Error::Upstream(format!(
                    "{} created `{name}` but returned no id at `{}`",
                    self.id,
                    create.id.as_deref().unwrap_or("-")
                ))
            })?;
        // The new one is remembered by name, so the next issue carrying the same
        // label resolves from memory instead of re-listing and re-creating.
        self.lookups().remember(kind, scope, name, &id);
        Ok(id)
    }

    /// The candidate list for a lookup, fetched at most once per (kind, scope).
    ///
    /// One fetch serves every name that needs it: resolving three labels must not
    /// list a repository's labels three times, and a webhook storm must not become
    /// an API storm.
    fn candidates(&self, kind: &str, lookup: &LookupSpec, scope: &str) -> Result<Vec<Value>> {
        if let Some(cached) = self.lookups().candidates(kind, scope) {
            return Ok(cached);
        }
        let values = self.lookup_values(kind, scope, None, &lookup.list.body)?;
        let request = self
            .spec
            .request(&lookup.list, &values, self.secret.as_ref())?;
        let response = self.send(&request)?;
        let candidates = match &lookup.items {
            Some(pointer) => resolve(&response.body, pointer),
            None => Some(&response.body),
        }
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
        self.lookups().store_candidates(kind, scope, &candidates);
        Ok(candidates)
    }

    fn lookup_values(
        &self,
        kind: &str,
        scope: &str,
        name: Option<&str>,
        body: &Option<Value>,
    ) -> Result<Value> {
        let mut values = json!({ "scope": scope });
        if let Some(name) = name {
            values["name"] = json!(name);
        }
        if kind != TEAM && uses(body, "$scope_id") {
            // A lookup can need the *containing* scope resolved first (Linear lists
            // a team's states, and a team is named by its key, not its UUID). The
            // team lookup itself is the one lookup that must not recurse.
            values["scope_id"] = json!(self.resolve(TEAM, scope, scope)?);
        }
        Ok(values)
    }

    /// Run an operation and return the platform's id and url for what it touched.
    fn execute(&self, operation: &Operation, values: &Value) -> Result<RemoteRef> {
        let request = self.spec.request(operation, values, self.secret.as_ref())?;
        let response = self.send(&request)?;
        let id = operation
            .id
            .as_deref()
            .and_then(|pointer| resolve_string(&response.body, pointer))
            .unwrap_or_default();
        let url = operation
            .url
            .as_deref()
            .and_then(|pointer| resolve_string(&response.body, pointer));
        Ok(RemoteRef { id, url })
    }
}

/// The lookup cache: per (kind, scope), the candidate list as the platform gave
/// it, and the names already resolved out of that list.
///
/// Both halves earn their place. The list is what a lookup *costs* - one request
/// fetches the whole set - while the write path asks for names one at a time,
/// repeatedly, and every one of them must not pay for that request again.
#[derive(Default)]
struct Lookups {
    lists: HashMap<String, Vec<Value>>,
    names: HashMap<String, Value>,
}

impl Lookups {
    fn list_key(kind: &str, scope: &str) -> String {
        format!("{kind}\u{0}{scope}")
    }

    /// Names are matched the way the platform matches them: case-insensitively,
    /// so `Bug` and `bug` are one label.
    fn name_key(kind: &str, scope: &str, name: &str) -> String {
        format!("{kind}\u{0}{scope}\u{0}{}", name.to_lowercase())
    }

    fn candidates(&self, kind: &str, scope: &str) -> Option<Vec<Value>> {
        self.lists.get(&Self::list_key(kind, scope)).cloned()
    }

    fn store_candidates(&mut self, kind: &str, scope: &str, candidates: &[Value]) {
        self.lists
            .insert(Self::list_key(kind, scope), candidates.to_vec());
    }

    fn resolved(&self, kind: &str, scope: &str, name: &str) -> Option<Value> {
        self.names.get(&Self::name_key(kind, scope, name)).cloned()
    }

    fn remember(&mut self, kind: &str, scope: &str, name: &str, id: &Value) {
        self.names
            .insert(Self::name_key(kind, scope, name), id.clone());
    }
}

/// Read a platform response back into neutral fields.
///
/// Free functions rather than methods: they are pure, they are the part of the
/// write path worth testing without a server, and keeping them out of the engine
/// means a test cannot accidentally reach the network.
fn read_fields(body: &Value, read: &ReadSpec) -> IssueFields {
    let raw_labels = read_labels(body, read.labels.as_ref());
    // The priority is derived from the labels *before* they are normalised: the
    // `priority:*` label is the value being read, and the neutral field set does
    // not carry it.
    let priority = match read.priority.as_ref() {
        Some(field) if field.from_labels() => labels_to_priority(&raw_labels).unwrap_or(0),
        field => read_text(body, field)
            .and_then(|value| value.parse::<u8>().ok())
            .unwrap_or(0),
    };
    IssueFields {
        // Normalised exactly as the read side normalises an inbound payload, so a
        // field that made it across unchanged reads back unchanged instead of
        // looking like an edit.
        labels: canonical_labels(&raw_labels),
        title: read_text(body, read.title.as_ref()).unwrap_or_default(),
        body: read_text(body, read.body.as_ref()).unwrap_or_default(),
        priority,
        due_date: read_text(body, read.due_date.as_ref()),
        assignee: read_text(body, read.assignee.as_ref()),
    }
}

fn read_text(body: &Value, field: Option<&ReadField>) -> Option<String> {
    let field = field?;
    if field.from_labels() {
        return None;
    }
    let path = field.path()?;
    match field.pick() {
        None => resolve_string(body, path),
        Some(pick) => resolve(body, path)?
            .as_array()?
            .first()
            .and_then(|first| resolve_string(first, pick)),
    }
}

fn read_labels(body: &Value, field: Option<&ReadField>) -> Vec<String> {
    let Some(field) = field else {
        return Vec::new();
    };
    let Some(path) = field.path() else {
        return Vec::new();
    };
    let Some(items) = resolve(body, path).and_then(Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| match field.pick() {
            Some(pick) => resolve_string(item, pick),
            None => match item {
                Value::String(text) => Some(text.clone()),
                other => scalar(other),
            },
        })
        .collect()
}

impl Sink for DeclarativeSink {
    fn id(&self) -> &ConnectorId {
        &self.id
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities.clone()
    }

    fn fetch_issue(&self, scope: &str, id: &str) -> Result<Option<RemoteIssue>> {
        let fetch = self.operation("fetch", self.spec.issue.fetch.as_ref())?;
        let values = self.context(fetch, &Call::new(scope).id(id))?;
        let request = self.spec.request(fetch, &values, self.secret.as_ref())?;
        let response = self.client.send(&request)?;
        if response.status == 404 {
            // The platform says it is gone; that is an answer, not a failure.
            return Ok(None);
        }
        if !response.is_success() {
            return Err(Error::Upstream(format!(
                "{} {} -> {} {}",
                request.method.as_str(),
                request.url,
                response.status,
                response.summary()
            )));
        }

        let read = self.spec.issue.read.clone().unwrap_or_default();
        let reference = RemoteRef {
            id: read
                .id
                .as_deref()
                .and_then(|pointer| resolve_string(&response.body, pointer))
                .unwrap_or_else(|| id.to_string()),
            url: read
                .url
                .as_deref()
                .and_then(|pointer| resolve_string(&response.body, pointer)),
        };
        let state = read_text(&response.body, read.state.as_ref());
        Ok(Some(RemoteIssue {
            reference,
            fields: read_fields(&response.body, &read),
            state,
        }))
    }

    fn create_issue(
        &self,
        scope: &str,
        fields: &IssueFields,
        state: Option<&str>,
    ) -> Result<RemoteRef> {
        let create = self.operation("create", self.spec.issue.create.as_ref())?;
        let values = self.context(create, &Call::new(scope).fields(fields).state(state))?;
        let reference = self.execute(create, &values)?;
        if reference.id.is_empty() {
            return Err(Error::Upstream(format!(
                "{} created an issue but the response had no id at `{}`",
                self.id,
                create.id.as_deref().unwrap_or("-")
            )));
        }
        Ok(reference)
    }

    fn update_issue(
        &self,
        scope: &str,
        id: &str,
        fields: &IssueFields,
        state: Option<&str>,
    ) -> Result<()> {
        let update = self.operation("update", self.spec.issue.update.as_ref())?;
        let values = self.context(update, &Call::new(scope).id(id).fields(fields).state(state))?;
        self.execute(update, &values)?;

        // Labels are their own operation on most platforms (a forge replaces the
        // whole set, Linear takes ids in the same mutation and would have rendered
        // `$label_ids` above) - so only run it when the spec declares one.
        if let Some(labels) = &self.spec.issue.labels {
            let values =
                self.context(labels, &Call::new(scope).id(id).fields(fields).state(state))?;
            self.execute(labels, &values)?;
        }
        Ok(())
    }

    fn comment(&self, scope: &str, id: &str, body: &str) -> Result<RemoteRef> {
        let comment = self.operation("comment", self.spec.issue.comment.as_ref())?;
        let values = self.context(comment, &Call::new(scope).id(id).comment(body))?;
        let reference = self.execute(comment, &values)?;
        Ok(reference)
    }

    fn transition(&self, scope: &str, id: &str, state: &str) -> Result<()> {
        let transition = self.operation("transition", self.spec.issue.transition.as_ref())?;
        let values = self.context(transition, &Call::new(scope).id(id).state(Some(state)))?;
        self.execute(transition, &values)?;
        Ok(())
    }

    fn delete_issue(&self, scope: &str, id: &str) -> Result<()> {
        let delete = self.operation("delete", self.spec.issue.delete.as_ref())?;
        let values = self.context(delete, &Call::new(scope).id(id))?;
        self.execute(delete, &values)?;
        Ok(())
    }

    fn attach(&self, scope: &str, id: &str, url: &str, title: &str) -> Result<()> {
        let attach = self.operation("attach", self.spec.issue.attach.as_ref())?;
        let values = self.context(attach, &Call::new(scope).id(id).attachment(url, title))?;
        self.execute(attach, &values)?;
        Ok(())
    }
}

/// What one call knows. A struct rather than a list of `Option`s: the call sites
/// read as the operation they are performing, and adding a directive later does
/// not lengthen every one of them.
#[derive(Clone, Copy, Debug, Default)]
struct Call<'a> {
    scope: &'a str,
    id: Option<&'a str>,
    fields: Option<&'a IssueFields>,
    state: Option<&'a str>,
    comment: Option<&'a str>,
    title: Option<&'a str>,
    url: Option<&'a str>,
}

impl<'a> Call<'a> {
    fn new(scope: &'a str) -> Self {
        Self {
            scope,
            ..Self::default()
        }
    }

    fn id(mut self, id: &'a str) -> Self {
        self.id = Some(id);
        self
    }

    fn fields(mut self, fields: &'a IssueFields) -> Self {
        self.fields = Some(fields);
        self
    }

    fn state(mut self, state: Option<&'a str>) -> Self {
        self.state = state;
        self
    }

    fn comment(mut self, comment: &'a str) -> Self {
        self.comment = Some(comment);
        self
    }

    fn attachment(mut self, url: &'a str, title: &'a str) -> Self {
        self.url = Some(url);
        self.title = Some(title);
        self
    }
}

/// A JSON value for an optional string, so `null` and "absent" stay distinct in
/// the directives (`$field` drops the key, `$field!` sends null).
fn optional(value: Option<&str>) -> Value {
    value.map(|value| json!(value)).unwrap_or(Value::Null)
}

fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

/// Whether a template mentions a directive at all. Used to avoid paying for a
/// lookup a request does not need.
fn uses(template: &Option<Value>, directive: &str) -> bool {
    let Some(template) = template else {
        return false;
    };
    match template {
        Value::String(text) => {
            text == directive || text.strip_prefix(directive).is_some_and(|rest| rest == "!")
        }
        Value::Array(items) => items
            .iter()
            .any(|item| uses(&Some(item.clone()), directive)),
        Value::Object(map) => map
            .values()
            .any(|value| uses(&Some(value.clone()), directive)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_finds_a_directive_anywhere_in_a_template() {
        let template = json!({
            "query": "mutation { x }",
            "variables": { "input": { "teamId": "$scope_id", "labels": ["$label_ids"] } }
        });
        assert!(uses(&Some(template.clone()), "$scope_id"));
        assert!(uses(&Some(template.clone()), "$label_ids"));
        assert!(!uses(&Some(template.clone()), "$state_id"));
        assert!(!uses(&None, "$scope_id"));
        // The explicit-null form counts as the same directive.
        assert!(uses(&Some(json!({ "due": "$due_date!" })), "$due_date"));
    }

    #[test]
    fn names_resolve_case_insensitively_and_only_inside_their_scope() {
        let mut lookups = Lookups::default();
        lookups.remember("label", "a/b", "Bug", &json!(3));

        assert_eq!(lookups.resolved("label", "a/b", "bug"), Some(json!(3)));
        assert_eq!(lookups.resolved("label", "a/b", "BUG"), Some(json!(3)));
        // A label id means nothing in another repository, so it must not leak.
        assert_eq!(lookups.resolved("label", "c/d", "bug"), None);
        assert_eq!(lookups.resolved("state", "a/b", "bug"), None);
    }

    #[test]
    fn read_fields_map_a_flat_response_and_a_rich_one() {
        let read = ReadSpec {
            title: Some(ReadField::Pointer("/title".into())),
            body: Some(ReadField::Pointer("/body".into())),
            labels: Some(ReadField::Detailed(crate::sink::spec::ReadFieldSpec {
                path: Some("/labels".into()),
                pick: Some("/name".into()),
                from_labels: false,
            })),
            priority: Some(ReadField::Detailed(crate::sink::spec::ReadFieldSpec {
                from_labels: true,
                ..Default::default()
            })),
            due_date: Some(ReadField::Pointer("/due_date".into())),
            assignee: Some(ReadField::Pointer("/assignees/0/login".into())),
            state: Some(ReadField::Pointer("/state".into())),
            id: Some("/number".into()),
            url: Some("/html_url".into()),
        };

        // A forge-shaped response: labels are objects, priority is absent and has
        // to come out of the labels a bridge wrote there.
        let body = json!({
            "number": 7,
            "title": "T",
            "body": "B",
            "labels": [{ "id": 1, "name": "bug" }, { "id": 2, "name": "priority:high" }],
            "due_date": "2026-10-02T00:00:00Z",
            "assignees": [{ "login": "vedaru" }],
            "state": "open"
        });
        let fields = read_fields(&body, &read);
        assert_eq!(fields.title, "T");
        // The neutral field set excludes the priority labels: there the priority
        // *is* the label, here it is the priority field, and carrying both would
        // make one issue look like two.
        assert_eq!(fields.labels, vec!["bug"]);
        assert_eq!(fields.priority, 2, "priority comes from the label");
        assert_eq!(fields.assignee.as_deref(), Some("vedaru"));
        assert_eq!(fields.due_date.as_deref(), Some("2026-10-02T00:00:00Z"));
    }

    #[test]
    fn a_linear_shaped_response_reads_the_same_fields() {
        let read = ReadSpec {
            title: Some(ReadField::Pointer("/title".into())),
            body: Some(ReadField::Pointer("/description".into())),
            labels: Some(ReadField::Detailed(crate::sink::spec::ReadFieldSpec {
                path: Some("/labels/nodes".into()),
                pick: Some("/name".into()),
                from_labels: false,
            })),
            priority: Some(ReadField::Pointer("/priority".into())),
            due_date: Some(ReadField::Pointer("/dueDate".into())),
            assignee: Some(ReadField::Pointer("/assignee/email".into())),
            state: Some(ReadField::Pointer("/state/name".into())),
            ..ReadSpec::default()
        };
        let body = json!({
            "title": "T",
            "description": "B",
            "labels": { "nodes": [{ "name": "Bug" }] },
            "priority": 3,
            "dueDate": "2026-10-02",
            "assignee": { "email": "loner@example.com" },
            "state": { "name": "Todo" }
        });
        let fields = read_fields(&body, &read);
        assert_eq!(fields.priority, 3);
        assert_eq!(fields.canonical_labels(), vec!["bug"]);
        assert_eq!(fields.assignee.as_deref(), Some("loner@example.com"));
    }
}
