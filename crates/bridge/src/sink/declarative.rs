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

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use serde_json::{json, Value};

use crate::domain::{
    canonical_labels, due_date_to_label, labels_to_due_date, labels_to_priority, normalise_color,
    normalise_due_date, priority_to_label, Capabilities, Change, ConnectorId, IssueFields, Label,
    Patch, Secret,
};
use crate::error::{Error, Result};
use crate::http_client::{HttpClient, Request, Response};
use crate::pointer::{resolve, resolve_string};
use crate::sink::spec::{LookupSpec, Operation, PaginateSpec, ReadField, ReadSpec, SinkSpec};
use crate::sink::{BoardCard, RemoteIssue, RemoteRef, Sink};

/// A lookup a spec declares: the kinds the engine will resolve.
const TEAM: &str = "team";
const LABEL: &str = "label";
/// A milestone, which a forge addresses by id - the same name-to-id shape as a
/// label, except that an issue carries one rather than a set.
const MILESTONE: &str = "milestone";
const STATE: &str = "state";
const ASSIGNEE: &str = "assignee";
/// A card's column on a board: a *project* lookup, and the only one whose answer is
/// scoped to something smaller than the repository.
const COLUMN: &str = "column";

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
        self.check(request, response)
    }

    /// Turn a transport response into an operation result.
    ///
    /// A transport-level success is not an operation-level success: GraphQL
    /// reports a rejected mutation as `200 OK` with an `errors` array, and a
    /// sync that believed the status code would be silently wrong.
    fn check(&self, request: &Request, response: Response) -> Result<Response> {
        if !response.is_success() {
            return Err(Error::Upstream(format!(
                "{} {} -> {} {}",
                request.method.as_str(),
                request.url,
                response.status,
                response.summary()
            )));
        }
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

    /// Run a deletion, treating "it is already gone" as done.
    ///
    /// A delete states a desired end state - the remote entity is absent - and a
    /// `404` is that state already holding. A retry of a delete whose response
    /// was lost, or a delivery replayed after the copy was removed by hand, must
    /// converge rather than fail forever. Only the entity's own absence is
    /// tolerated: every other status, and a GraphQL error inside a `200`, still
    /// fails through [`Self::check`].
    fn execute_delete(&self, operation: &Operation, values: &Value) -> Result<()> {
        let request = self.spec.request(operation, values, self.secret.as_ref())?;
        let response = self.client.send(&request)?;
        if response.status == 404 {
            return Ok(());
        }
        self.check(&request, response)?;
        Ok(())
    }

    fn operation<'a>(&self, name: &str, operation: Option<&'a Operation>) -> Result<&'a Operation> {
        self.declared(name, operation)
    }

    /// A spec section this platform has to have declared, or an error naming it.
    fn declared<'a, T>(&self, name: &str, section: Option<&'a T>) -> Result<&'a T> {
        section.ok_or_else(|| Error::Unsupported(name.to_string(), self.id.clone()))
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
    fn outbound_labels(&self, fields: &IssueFields) -> Vec<Label> {
        self.outbound_labels_for(
            &fields.canonical_labels(),
            Some(fields.priority),
            fields.due_date.as_deref(),
        )
    }

    /// The label set to send for a given priority.
    ///
    /// The priority is passed separately because on a forge the two are one field:
    /// a write that names the labels and forgets the priority does not omit it, it
    /// *clears* it.
    fn outbound_labels_for(
        &self,
        labels: &[Label],
        priority: Option<u8>,
        due_date: Option<&str>,
    ) -> Vec<Label> {
        let mut labels = canonical_labels(labels);
        if !self.capabilities.priorities {
            if let Some(label) = priority.and_then(priority_to_label) {
                labels.push(Label::named(label));
            }
        }
        // And the same for a due date: the platform has no field for it, so it travels
        // as a `due:*` label - which is what its read half reads back, so the two ends
        // still agree about the date instead of one of them silently losing it.
        if !self.capabilities.due_dates {
            if let Some(date) = due_date {
                labels.push(Label::named(due_date_to_label(date)));
            }
        }
        // Order is left alone: the names arrived canonical (sorted, deduped, synthetic
        // labels removed) and the emulated ones are appended, so resolving them in order
        // asks the platform for the same ids in the same order every time.
        labels
    }

    /// Fill the directives a partial update mentions - and only those.
    ///
    /// A field the patch leaves alone must not reach the request at all. That is the
    /// point of sending a patch: restating a field is what overwrites one the other
    /// side changed on its own, and this bridge has no business rewriting what it
    /// did not look at.
    fn apply_patch(
        &self,
        values: &mut Value,
        patch: &Patch,
        call: &Call<'_>,
        operation: &Operation,
    ) -> Result<()> {
        if let Change::Set(title) = &patch.title {
            values["title"] = json!(title);
        }
        if let Change::Set(body) = &patch.body {
            values["body"] = json!(body);
        }
        if let Change::Set(names) = &patch.labels {
            let labels = self.outbound_labels_for(names, call.priority, call.due_date);
            // The template wants names here; the ids it needs come from the lookup below.
            values["labels"] = json!(labels
                .iter()
                .map(|label| label.name.clone())
                .collect::<Vec<_>>());
            if uses(&operation.body, "$label_ids") {
                let mut ids = Vec::with_capacity(labels.len());
                for label in &labels {
                    ids.push(self.resolve_colored(
                        LABEL,
                        call.scope,
                        &label.name,
                        label.color.as_deref(),
                    )?);
                }
                values["label_ids"] = json!(ids);
            }
        }
        if let Change::Set(name) = &patch.milestone {
            // Clearing a milestone is a value the platform understands, not an omission:
            // the same reason a cleared due date is sent as null rather than left out.
            values["milestone_id"] = if name.is_empty() {
                Value::Null
            } else {
                self.resolve(MILESTONE, call.scope, name)?
            };
        }
        if let Change::Set(priority) = &patch.priority {
            // 0 is "no priority" on the wire, which is how one is cleared.
            values["priority"] = if *priority == 0 {
                Value::Null
            } else {
                json!(priority)
            };
        }
        match &patch.due_date {
            Change::Set(date) => values["due_date"] = json!(date),
            Change::Clear => values["due_date"] = Value::Null,
            Change::Leave => {}
        }
        match &patch.assignee {
            Change::Set(assignee) => {
                values["assignee"] = json!(assignee);
                values["assignees"] = json!([assignee]);
                if uses(&operation.body, "$assignee_id") {
                    values["assignee_id"] = json!(self.resolve(ASSIGNEE, call.scope, assignee)?);
                }
            }
            Change::Clear => {
                // Every shape "nobody" takes: the single value, the id a platform
                // wants, and the list a forge wants.
                values["assignee"] = Value::Null;
                values["assignee_id"] = Value::Null;
                values["assignees"] = json!([]);
            }
            Change::Leave => {}
        }
        Ok(())
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
        if let Some(page) = call.page {
            values["page"] = json!(page);
        }
        if let Some(cursor) = call.cursor {
            values["cursor"] = json!(cursor);
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
            values["labels"] = json!(self
                .outbound_labels(fields)
                .iter()
                .map(|label| label.name.clone())
                .collect::<Vec<_>>());
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
            if uses(&operation.body, "$milestone_id") {
                // One id, not a set: `resolve` already answers with a single value, which is
                // why the plural `$label_ids` beside it is a loop and not another mechanism.
                // Absent is sent as null, the same way a cleared due date is.
                values["milestone_id"] = match fields.milestone.as_deref() {
                    Some(name) => self.resolve(MILESTONE, scope, name)?,
                    None => Value::Null,
                };
            }

            if uses(&operation.body, "$label_ids") {
                let names = self.outbound_labels(fields);
                let mut ids = Vec::with_capacity(names.len());
                for label in &names {
                    ids.push(self.resolve_colored(
                        LABEL,
                        scope,
                        &label.name,
                        label.color.as_deref(),
                    )?);
                }
                values["label_ids"] = json!(ids);
            }
        }

        if let Some(patch) = call.patch {
            self.apply_patch(&mut values, patch, call, operation)?;
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
        if uses(&operation.body, "$due_date_timestamp") {
            // Derived from `due_date` after every path has set it (the field model and a patch
            // alike), so a date an update never mentions stays absent here too - and
            // `$due_date_timestamp` drops the key exactly as `$due_date` would.
            if let Some(value) = values.get("due_date").cloned() {
                values["due_date_timestamp"] = due_date_timestamp(&value);
            }
        }
        Ok(values)
    }

    /// Resolve a name to a platform id out of the *issue* half's lookups.
    fn resolve(&self, kind: &str, scope: &str, name: &str) -> Result<Value> {
        self.resolve_in(&self.spec.issue.lookup, None, kind, scope, name, None)
    }

    /// The same, for a label that has a colour: the colour is sent when the lookup has to
    /// *create* the thing, which is the only moment either side learns it.
    fn resolve_colored(
        &self,
        kind: &str,
        scope: &str,
        name: &str,
        color: Option<&str>,
    ) -> Result<Value> {
        self.resolve_in(&self.spec.issue.lookup, None, kind, scope, name, color)
    }

    /// Resolve a name to a platform id, memoised per (kind, scope, container).
    ///
    /// The id keeps the *JSON type* the platform answered with: a forge numbers
    /// its labels (`"labels": [3, 9]`), Linear names them with UUID strings, and
    /// the preset said which pointer to take - so stringifying here would be the
    /// engine overruling the platform.
    ///
    /// `table` is which half of the preset declared the lookup - labels and states live
    /// under `[sink.issue]`, a board's columns under `[sink.project]` - and `container`
    /// is what the answer is filed under when a scope is not a fine enough key. One
    /// implementation rather than two, because the two differ only in which table they
    /// read and what they label the answer with.
    fn resolve_in(
        &self,
        table: &BTreeMap<String, LookupSpec>,
        container: Option<&str>,
        kind: &str,
        scope: &str,
        name: &str,
        color: Option<&str>,
    ) -> Result<Value> {
        let lookup = table.get(kind).ok_or_else(|| {
            Error::Config(format!(
                "connector `{}` has no `{kind}` lookup, but a template asked for `{kind}` ids",
                self.id
            ))
        })?;
        if let Some(id) = self.lookups().resolved(kind, scope, container, name) {
            return Ok(id);
        }

        for candidate in self.candidates_in(kind, lookup, scope, container)? {
            let found = resolve_string(&candidate, &lookup.name);
            if found
                .as_deref()
                .is_some_and(|found| found.eq_ignore_ascii_case(name))
            {
                if let Some(id) = resolve(&candidate, &lookup.id).cloned() {
                    self.lookups().remember(kind, scope, container, name, &id);
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
        let values = self.lookup_values(kind, scope, container, Some(name), color, &create.body)?;
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
        self.lookups().remember(kind, scope, container, name, &id);
        Ok(id)
    }

    /// The candidate list for a lookup, fetched at most once per (kind, scope).
    ///
    /// One fetch serves every name that needs it: resolving three labels must not
    /// list a repository's labels three times, and a webhook storm must not become
    /// an API storm.
    fn candidates_in(
        &self,
        kind: &str,
        lookup: &LookupSpec,
        scope: &str,
        container: Option<&str>,
    ) -> Result<Vec<Value>> {
        if let Some(cached) = self.lookups().candidates(kind, scope, container) {
            return Ok(cached);
        }
        let values = self.lookup_values(kind, scope, container, None, None, &lookup.list.body)?;
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
        self.lookups()
            .store_candidates(kind, scope, container, &candidates);
        Ok(candidates)
    }

    /// The values a lookup request may use. `container` is the thing a set of names can
    /// belong to within a scope - a project's columns - and reaches the request as `{id}`,
    /// so a list path can name it (`/repos/{scope}/projects/{id}/columns`).
    fn lookup_values(
        &self,
        kind: &str,
        scope: &str,
        container: Option<&str>,
        name: Option<&str>,
        color: Option<&str>,
        body: &Option<Value>,
    ) -> Result<Value> {
        let mut values = json!({ "scope": scope });
        if let Some(container) = container {
            values["id"] = json!(container);
        }
        if let Some(name) = name {
            values["name"] = json!(name);
        }
        // Only a lookup that creates something has a use for a colour, and only the label
        // one is ever given it: this is the whole of what "a label keeps its colour" needs.
        if let Some(color) = color {
            values["color"] = json!(color);
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

    /// Run the operation that puts an issue on a project's board, or takes it off.
    ///
    /// A platform that declares no `assign`/`unassign` under `[sink.project]` does
    /// nothing here, and that is deliberate: a forge with no projects at all (one
    /// that keeps milestones, or nothing) must degrade to doing nothing rather than
    /// fail a mirror over a container it does not have. `id` is the project, `index`
    /// the issue - the path names the container and its member.
    fn membership(
        &self,
        name: &str,
        scope: &str,
        issue: &str,
        project: &str,
        column: Option<&str>,
    ) -> Result<()> {
        let operation = self.spec.project.as_ref().and_then(|project| match name {
            "project.assign" => project.assign.as_ref(),
            _ => project.unassign.as_ref(),
        });
        let Some(operation) = operation else {
            return Ok(());
        };
        let mut values = json!({ "scope": scope, "id": project, "index": issue });
        if let Some(column) = column {
            // The column travels as a *name*: the preset knows what its board calls its
            // columns, and the id a request needs is resolved against that board. Only
            // resolved when the preset's body asks for it, so a platform that places
            // cards without columns never pays for a lookup it does not have.
            if uses(&operation.body, "$column_id") {
                let table = &self.declared("project", self.spec.project.as_ref())?.lookup;
                values["column_id"] =
                    json!(self.resolve_in(table, Some(project), COLUMN, scope, column, None)?);
            }
            values["column"] = json!(column);
        }
        let request = self
            .spec
            .request(operation, &values, self.secret.as_ref())?;
        self.send(&request)?;
        Ok(())
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
    /// What a candidate list is filed under.
    ///
    /// `container` is the extra thing a set of names can belong to when a scope is not
    /// enough: a board's columns are per *project*, and two boards in one repository may
    /// both have an "In Progress". A lookup with no container keys on (kind, scope) as
    /// it always did, so nothing that resolves a label or a state pays for the extra
    /// component.
    fn list_key(kind: &str, scope: &str, container: Option<&str>) -> String {
        match container {
            Some(container) => format!("{kind}\u{0}{scope}\u{0}{container}"),
            None => format!("{kind}\u{0}{scope}"),
        }
    }

    /// Names are matched the way the platform matches them: case-insensitively,
    /// so `Bug` and `bug` are one label.
    fn name_key(kind: &str, scope: &str, container: Option<&str>, name: &str) -> String {
        format!(
            "{}\u{0}{}",
            Self::list_key(kind, scope, container),
            name.to_lowercase()
        )
    }

    fn candidates(&self, kind: &str, scope: &str, container: Option<&str>) -> Option<Vec<Value>> {
        self.lists
            .get(&Self::list_key(kind, scope, container))
            .cloned()
    }

    fn store_candidates(
        &mut self,
        kind: &str,
        scope: &str,
        container: Option<&str>,
        candidates: &[Value],
    ) {
        self.lists
            .insert(Self::list_key(kind, scope, container), candidates.to_vec());
    }

    fn resolved(
        &self,
        kind: &str,
        scope: &str,
        container: Option<&str>,
        name: &str,
    ) -> Option<Value> {
        self.names
            .get(&Self::name_key(kind, scope, container, name))
            .cloned()
    }

    fn remember(
        &mut self,
        kind: &str,
        scope: &str,
        container: Option<&str>,
        name: &str,
        id: &Value,
    ) {
        self.names
            .insert(Self::name_key(kind, scope, container, name), id.clone());
    }
}

/// Read a platform response back into neutral fields.
///
/// Free functions rather than methods: they are pure, they are the part of the
/// write path worth testing without a server, and keeping them out of the engine
/// means a test cannot accidentally reach the network.
/// Read one issue out of whatever the response already narrowed to.
///
/// The root is the whole response for `fetch` and a single item for a sweep, which is
/// the only difference between the two reads: the pointers in a
/// `[sink.issue.list.read]` are relative to the item.
fn read_issue(root: &Value, read: &ReadSpec, fallback_id: &str) -> RemoteIssue {
    let reference = RemoteRef {
        id: read
            .id
            .as_deref()
            .and_then(|pointer| resolve_string(root, pointer))
            .unwrap_or_else(|| fallback_id.to_string()),
        url: read
            .url
            .as_deref()
            .and_then(|pointer| resolve_string(root, pointer)),
    };
    RemoteIssue {
        reference,
        fields: read_fields(root, read),
        state: read_text(root, read.state.as_ref()),
    }
}

/// Which request to make next, if any.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Next {
    Page(usize),
    Cursor(String),
}

/// Where the walk goes after a page - the one part of enumeration that is genuinely
/// each platform's own.
fn next_page(
    paginate: Option<&PaginateSpec>,
    body: &Value,
    items: usize,
    page: usize,
) -> Option<Next> {
    let paginate = paginate?;
    if let Some(cursor) = &paginate.cursor {
        // A platform that says "no more" is believed, even if it also handed back a
        // cursor - which some do on the last page.
        if let Some(more) = cursor
            .more
            .as_deref()
            .and_then(|pointer| resolve(body, pointer))
        {
            if more.as_bool() != Some(true) {
                return None;
            }
        }
        let next = resolve_string(body, &cursor.next)?;
        return (!next.is_empty()).then_some(Next::Cursor(next));
    }
    let numbered = paginate.page.as_ref()?;
    // A short page is how a numbered API says "that was the last one", and it costs
    // no extra request to find out.
    (items >= numbered.size).then_some(Next::Page(page + 1))
}

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
    // Same shape as the priority above: the preset says where the value lives, and a
    // platform with no due-date field says it lives in a `due:*` label.
    let due_date = match read.due_date.as_ref() {
        Some(field) if field.from_labels() => labels_to_due_date(&raw_labels),
        field => normalise_due_date(read_text(body, field).as_deref()),
    };
    IssueFields {
        // Normalised exactly as the read side normalises an inbound payload, so a
        // field that made it across unchanged reads back unchanged instead of
        // looking like an edit.
        labels: canonical_labels(&raw_labels),
        title: read_text(body, read.title.as_ref()).unwrap_or_default(),
        // Where the preset says the milestone lives, by name; `None` when it says nothing.
        milestone: read_text(body, read.milestone.as_ref()),
        body: read_text(body, read.body.as_ref()).unwrap_or_default(),
        priority,
        // Normalised, not read raw: a forge spells "no due date" as
        // `0001-01-01T00:00:00Z`, and taken literally that is a due date the source
        // does not have - a difference that never converges, which is the same trap
        // an unwritable assignee sets.
        due_date,
        assignee: read_text(body, read.assignee.as_ref()),
        // A platform that reports an issue's project only if its preset says where.
        // A forge that reports it nowhere yields `None`, and the reconciler falls
        // back to the project it recorded when it placed the issue.
        project: read_text(body, read.project.as_ref()),
        // A container's alias, for routing only - not part of the content a mirror
        // compares.
        slug: read_text(body, read.slug.as_ref()),
        // The identifier a person sees, likewise for routing only.
        identifier: read_text(body, read.identifier.as_ref()),
        // A list of URLs the entity declares elsewhere, for resolving a scope from a
        // link that points at the sink platform. Read with the same path/pick shape a
        // label list uses; identity, never content.
        links: read_names(body, read.links.as_ref()),
    }
}

/// The names of a path/pick list, for the fields that are identity rather than content.
fn read_names(body: &Value, field: Option<&ReadField>) -> Vec<String> {
    read_labels(body, field)
        .into_iter()
        .map(|label| label.name)
        .collect()
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

fn read_labels(body: &Value, field: Option<&ReadField>) -> Vec<Label> {
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
        .filter_map(|item| {
            let name = match field.pick() {
                Some(pick) => resolve_string(item, pick),
                None => match item {
                    Value::String(text) => Some(text.clone()),
                    other => scalar(other),
                },
            }?;
            // The colour is optional: a platform that does not carry one reads exactly as
            // it did before, and the neutral form is normalised so the two sides agree.
            let color = field
                .color_pick()
                .and_then(|pick| resolve_string(item, pick))
                .and_then(|value| normalise_color(Some(&value)));
            Some(Label { name, color })
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
        Ok(Some(read_issue(&response.body, &read, id)))
    }

    fn list_issues(&self, scope: &str) -> Result<Vec<RemoteIssue>> {
        let list = self
            .declared("list", self.spec.issue.list.as_ref())?
            .clone();
        let mut issues = Vec::new();
        let mut page = 1usize;
        let mut cursor: Option<String> = None;

        // Bounded by construction: a platform that always answers "there is more"
        // must not be able to turn one sweep into an unbounded walk.
        let budget = list.paginate.as_ref().map_or(1, PaginateSpec::max_pages);
        for _ in 0..budget {
            let values = self.context(
                &list.request,
                &Call::new(scope).page(page).cursor(cursor.as_deref()),
            )?;
            let request = self
                .spec
                .request(&list.request, &values, self.secret.as_ref())?;
            let response = self.send(&request)?;
            // An empty page is a legitimate answer (nothing in this scope yet), so a
            // missing collection is not worth failing the walk over.
            let items: Vec<Value> = match &list.request.items {
                Some(pointer) => resolve(&response.body, pointer).and_then(Value::as_array),
                None => response.body.as_array(),
            }
            .cloned()
            .unwrap_or_default();
            for item in &items {
                issues.push(read_issue(item, &list.read, ""));
            }
            match next_page(list.paginate.as_ref(), &response.body, items.len(), page) {
                Some(Next::Page(next)) => page = next,
                Some(Next::Cursor(next)) => cursor = Some(next),
                None => break,
            }
        }
        Ok(issues)
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
        patch: &Patch,
        effective: &IssueFields,
        state: Option<&str>,
    ) -> Result<()> {
        let update = self.operation("update", self.spec.issue.update.as_ref())?;
        // The *effective* values travel with the call, not the patch's: on a platform
        // that carries the priority or the due date inside a label, writing the labels
        // without it drops it - and the patch has nothing to say about a field it did
        // not move.
        let values = self.context(
            update,
            &Call::new(scope)
                .id(id)
                .patch(patch)
                .priority(Some(effective.priority))
                .due_date(effective.due_date.as_deref())
                .state(state),
        )?;
        self.execute(update, &values)?;

        // Labels are their own operation on most platforms (a forge replaces the
        // whole set; Linear takes ids in the same mutation and has rendered
        // `$label_ids` above) - and it runs only when this patch touches them.
        if let Some(labels) = &self.spec.issue.labels {
            if !patch.labels.is_leave() {
                let values = self.context(
                    labels,
                    &Call::new(scope)
                        .id(id)
                        .patch(patch)
                        .priority(Some(effective.priority))
                        .due_date(effective.due_date.as_deref()),
                )?;
                self.execute(labels, &values)?;
            }
        }
        Ok(())
    }

    fn comment(&self, scope: &str, id: &str, body: &str) -> Result<RemoteRef> {
        let create = self
            .spec
            .issue
            .comment
            .as_ref()
            .map(|comment| &comment.create);
        let comment = self.operation("comment", create)?;
        // `$id` is the issue here: a new comment is addressed through its parent.
        let values = self.context(comment, &Call::new(scope).id(id).comment(body))?;
        let reference = self.execute(comment, &values)?;
        Ok(reference)
    }

    fn update_comment(&self, scope: &str, id: &str, body: &str) -> Result<()> {
        let update = self
            .spec
            .issue
            .comment
            .as_ref()
            .and_then(|comment| comment.update.as_ref());
        let comment = self.operation("comment.update", update)?;
        let values = self.context(comment, &Call::new(scope).id(id).comment(body))?;
        self.execute(comment, &values)?;
        Ok(())
    }

    fn delete_comment(&self, scope: &str, id: &str) -> Result<()> {
        let delete = self
            .spec
            .issue
            .comment
            .as_ref()
            .and_then(|comment| comment.delete.as_ref());
        let comment = self.operation("comment.delete", delete)?;
        let values = self.context(comment, &Call::new(scope).id(id))?;
        self.execute_delete(comment, &values)
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
        self.execute_delete(delete, &values)
    }

    fn attach(&self, scope: &str, id: &str, url: &str, title: &str) -> Result<()> {
        let attach = self.operation("attach", self.spec.issue.attach.as_ref())?;
        let values = self.context(attach, &Call::new(scope).id(id).attachment(url, title))?;
        self.execute(attach, &values)?;
        Ok(())
    }

    // --- projects ----------------------------------------------------------

    fn fetch_project(&self, scope: &str, id: &str) -> Result<Option<RemoteIssue>> {
        let project = self.declared("project", self.spec.project.as_ref())?;
        let fetch = self.operation("project.fetch", project.fetch.as_ref())?;
        let values = self.context(fetch, &Call::new(scope).id(id))?;
        let request = self.spec.request(fetch, &values, self.secret.as_ref())?;
        let response = self.client.send(&request)?;
        if response.status == 404 {
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
        let read = project.read.clone().unwrap_or_default();
        Ok(Some(read_issue(&response.body, &read, id)))
    }

    fn list_projects(&self, scope: &str) -> Result<Vec<RemoteIssue>> {
        let project = self.declared("project", self.spec.project.as_ref())?;
        let list = self
            .declared("project.list", project.list.as_ref())?
            .clone();
        let mut projects = Vec::new();
        let mut page = 1usize;
        let mut cursor: Option<String> = None;

        let budget = list.paginate.as_ref().map_or(1, PaginateSpec::max_pages);
        for _ in 0..budget {
            let values = self.context(
                &list.request,
                &Call::new(scope).page(page).cursor(cursor.as_deref()),
            )?;
            let request = self
                .spec
                .request(&list.request, &values, self.secret.as_ref())?;
            let response = self.send(&request)?;
            let items: Vec<Value> = match &list.request.items {
                Some(pointer) => resolve(&response.body, pointer).and_then(Value::as_array),
                None => response.body.as_array(),
            }
            .cloned()
            .unwrap_or_default();
            for item in &items {
                projects.push(read_issue(item, &list.read, ""));
            }
            match next_page(list.paginate.as_ref(), &response.body, items.len(), page) {
                Some(Next::Page(next)) => page = next,
                Some(Next::Cursor(next)) => cursor = Some(next),
                None => break,
            }
        }
        Ok(projects)
    }

    fn create_project(
        &self,
        scope: &str,
        fields: &IssueFields,
        state: Option<&str>,
    ) -> Result<RemoteRef> {
        let project = self.declared("project", self.spec.project.as_ref())?;
        let create = self.operation("project.create", project.create.as_ref())?;
        let values = self.context(create, &Call::new(scope).fields(fields).state(state))?;
        let reference = self.execute(create, &values)?;
        if reference.id.is_empty() {
            return Err(Error::Upstream(format!(
                "{} created a project but the response had no id at `{}`",
                self.id,
                create.id.as_deref().unwrap_or("-")
            )));
        }
        Ok(reference)
    }

    fn update_project(
        &self,
        scope: &str,
        id: &str,
        patch: &Patch,
        _effective: &IssueFields,
    ) -> Result<()> {
        let project = self.declared("project", self.spec.project.as_ref())?;
        let update = self.operation("project.update", project.update.as_ref())?;
        let values = self.context(update, &Call::new(scope).id(id).patch(patch))?;
        self.execute(update, &values)?;
        Ok(())
    }

    fn place_issue(
        &self,
        scope: &str,
        issue: &str,
        project: &str,
        column: Option<&str>,
    ) -> Result<()> {
        self.membership("project.assign", scope, issue, project, column)
    }

    fn remove_issue(&self, scope: &str, issue: &str, project: &str) -> Result<()> {
        self.membership("project.unassign", scope, issue, project, None)
    }

    /// Which column a card sits in, read off the board the preset describes.
    ///
    /// The card is matched by its number, as text: one platform numbers its issues and
    /// another names them, and a board read should not care which. Three answers, not two -
    /// no `[sink.board]` means this sink cannot see placement at all (`Unknown`), a board it
    /// read with the card on none of its columns means the card is genuinely adrift
    /// (`NotOnBoard`), and only the last of those is something a sweep may act on.
    fn board_cards(&self, scope: &str, project: &str) -> Result<Option<Vec<BoardCard>>> {
        let Some(board) = &self.spec.board else {
            return Ok(None);
        };
        let values = json!({ "scope": scope, "id": project });
        let request = self
            .spec
            .request(&board.list, &values, self.secret.as_ref())?;
        let response = self.send(&request)?;
        let columns = match &board.columns {
            Some(pointer) => resolve(&response.body, pointer),
            None => Some(&response.body),
        }
        .and_then(Value::as_array);

        // Borrow the response; the old per-issue `card_column` cloned the whole
        // board (columns, then every column's cards) on every call.
        let mut cards = Vec::new();
        if let Some(columns) = columns {
            for column in columns {
                let title = resolve_string(column, &board.title);
                let Some(issues) = resolve(column, &board.cards).and_then(Value::as_array) else {
                    continue;
                };
                for card in issues {
                    let id = match card {
                        Value::Number(number) => number.to_string(),
                        Value::String(text) => text.clone(),
                        _ => continue,
                    };
                    cards.push((id, title.clone()));
                }
            }
        }
        Ok(Some(cards))
    }
}

/// What one call knows. A struct rather than a list of `Option`s: the call sites
/// read as the operation they are performing, and adding a directive later does
/// not lengthen every one of them.
#[derive(Clone, Copy, Debug, Default)]
struct Call<'a> {
    scope: &'a str,
    id: Option<&'a str>,
    page: Option<usize>,
    cursor: Option<&'a str>,
    fields: Option<&'a IssueFields>,
    patch: Option<&'a Patch>,
    /// The priority the issue will end up with, whether or not this call sets it.
    priority: Option<u8>,
    /// The due date the issue will end up with, on the same terms.
    due_date: Option<&'a str>,
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

    fn patch(mut self, patch: &'a Patch) -> Self {
        self.patch = Some(patch);
        self
    }

    fn priority(mut self, priority: Option<u8>) -> Self {
        self.priority = priority;
        self
    }

    fn due_date(mut self, due_date: Option<&'a str>) -> Self {
        self.due_date = due_date;
        self
    }

    fn page(mut self, page: usize) -> Self {
        self.page = Some(page);
        self
    }

    fn cursor(mut self, cursor: Option<&'a str>) -> Self {
        self.cursor = cursor;
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

/// A due date in the neutral model (`YYYY-MM-DD`) as the RFC 3339 timestamp a forge stores.
///
/// A forge's due-date field is a `time.Time`, and it rejects the bare date (`422 parsing time
/// "2026-10-20" as "2006-01-02T15:04:05Z07:00"`). `null` and anything that is not a `YYYY-MM-DD`
/// string pass through unchanged, so "no due date" stays "no due date" and a value the source
/// already sent as a timestamp is not damaged.
fn due_date_timestamp(value: &Value) -> Value {
    match value {
        Value::String(date) if date.len() == 10 => json!(format!("{date}T00:00:00Z")),
        other => other.clone(),
    }
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
mod tests;
