//! The generic connector: one engine, one configuration file per platform.
//!
//! There is deliberately no `linear.rs` or `forgejo.rs`. A platform is described
//! by [`SourceSpec`] - where its signature lives, where the event name lives, and
//! how to address the entity inside the payload - and the presets in
//! `crates/bridge/presets/` are exactly that description written down. Adding a
//! platform is adding a file, and a deployment can describe an internal service
//! the same way without touching this crate.
//!
//! What the engine cannot express, it refuses to guess: an unmatched event name is
//! acknowledged with nothing to do (a `ping`), but an event whose *matched* rule
//! cannot find the fields it declares is a rejection naming the pointer, because a
//! preset that silently matches nothing is a preset that silently stops syncing.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::connector::{Algorithm, HeaderMap, Reject, SignatureScheme, Source};
use crate::domain::references::CLOSING_KEYWORDS as DEFAULT_CLOSING_KEYWORDS;
use crate::domain::{
    Action, Actor, Capabilities, ConnectorId, DeliveryId, EntityKind, EntityRef, Event,
    EventDetail, Secret, StateModel,
};
use crate::pointer::{resolve, resolve_string};
use crate::verify::body_digest;

/// A full platform description.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceSpec {
    #[serde(default)]
    pub signature: SignatureSpec,
    #[serde(default)]
    pub delivery: SelectorSpec,
    #[serde(default)]
    pub event: EventSpec,
    /// Optional replay bound. Only platforms that sign a timestamp can have one.
    #[serde(default)]
    pub freshness: Option<FreshnessSpec>,
    #[serde(default)]
    pub capabilities: CapabilitySpec,
    /// The write half, for a platform that can be written to as well as read from.
    ///
    /// It lives in this file on purpose. One platform is one description: the
    /// capabilities above are the *shared* contract, and a sink built from this
    /// spec is held to the same declaration, so a platform cannot claim in one
    /// direction what it denies in the other. The source engine ignores this
    /// field entirely.
    #[serde(default)]
    pub sink: Option<crate::sink::spec::SinkSpec>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignatureSpec {
    /// Header names, tried in order - providers rename headers across versions.
    pub headers: Vec<String>,
    #[serde(default)]
    pub algorithm: Algorithm,
    /// Stripped before decoding, for providers that send `sha256=<hex>`.
    #[serde(default)]
    pub prefix: Option<String>,
}

/// A list of alternative header names.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectorSpec {
    pub headers: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventSpec {
    /// Where the event name lives, if not in the body.
    #[serde(default)]
    pub headers: Vec<String>,
    /// Where the event name lives, if not in a header.
    #[serde(default)]
    pub body_field: Option<String>,
    #[serde(default, rename = "rule")]
    pub rules: Vec<RuleSpec>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleSpec {
    /// Event name to match, or `*` for anything else.
    #[serde(rename = "match")]
    pub event: String,
    /// Fallback kind: `issue`, `comment`, `reference`, `skip`, or `event-name`
    /// (an entity of kind `Other`, named by the event).
    pub kind: String,
    /// Per-event-kind overrides, so one rule can cover a family of events.
    #[serde(default)]
    pub kinds: BTreeMap<String, String>,
    #[serde(default)]
    pub fields: Fields,
    #[serde(default)]
    pub actions: BTreeMap<String, String>,
    /// Action for deliveries that carry none (e.g. a push, where a commit is a
    /// creation). Defaults to `created`: the reconciler creates-if-missing and
    /// treats an already-linked entity as an update, so `created` is the safe
    /// default and `updated` is the one that can lose an entity.
    #[serde(default)]
    pub action_default: Option<String>,
    /// Closing keywords for reference text; the forge defaults when omitted.
    #[serde(default)]
    pub closing_keywords: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fields {
    pub action: Option<String>,
    pub id: Option<String>,
    pub scope: Option<String>,
    pub url: Option<String>,
    pub actor: Option<String>,
    pub actor_name: Option<String>,
    /// Comment text.
    pub body: Option<String>,
    /// A comment's own id, when the event is *about* its parent.
    ///
    /// A comment delivery carries two ids: the comment's and the issue's. The
    /// subject is the issue (a link pairs issues, so that is what the reconciler
    /// looks up), which leaves the comment's own id to be named here.
    pub comment_id: Option<String>,
    /// The kind of the entity the subject points at, when it differs from the kind
    /// of the event itself (`comment` deliveries point at an `issue`).
    pub subject_kind: Option<String>,
    /// Reference text: all non-empty parts, joined (a title and a body).
    #[serde(default)]
    pub text: Vec<String>,
    /// Where the platform says whether a review request was merged.
    ///
    /// Declaring this is what makes a reference a *review request* rather than a commit: a
    /// commit mentions an issue and that is all it does, while a review request opens and
    /// (sometimes) merges, and the engine can tell them apart only because the preset says
    /// where to look. Absent, a reference never moves anything.
    pub merged: Option<String>,
    /// Address of an array in the payload; one event is produced per element,
    /// with these fields resolved inside the element first and the document
    /// second. A push is the case: the repository is on the delivery, the commit
    /// is in the array.
    pub fan_out: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FreshnessSpec {
    pub field: String,
    #[serde(default)]
    pub unit: TimeUnit,
    pub tolerance_secs: i64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TimeUnit {
    #[default]
    Millis,
    Seconds,
}

/// Declared capabilities, so a mapping that asks for something a platform cannot
/// carry fails at startup instead of dropping values at sync time.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilitySpec {
    #[serde(default)]
    pub states: StateKind,
    #[serde(default)]
    pub labels: bool,
    #[serde(default)]
    pub due_dates: bool,
    pub milestones: bool,
    #[serde(default)]
    pub priorities: bool,
    #[serde(default)]
    pub multiple_assignees: bool,
    #[serde(default)]
    pub native_pull_requests: bool,
    #[serde(default)]
    pub deletion: bool,
}

impl Default for CapabilitySpec {
    fn default() -> Self {
        // Conservatively: a platform that declares nothing claims nothing.
        Self {
            states: StateKind::OpenClosed,
            labels: false,
            due_dates: false,
            milestones: false,
            priorities: false,
            multiple_assignees: false,
            native_pull_requests: false,
            deletion: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum StateKind {
    Named,
    #[default]
    OpenClosed,
}

impl CapabilitySpec {
    /// The capabilities as the engine reads them, with enumeration taken from the
    /// sink.
    ///
    /// Whether a platform can be swept is a property of its `[sink.issue.list]`, so
    /// asking a preset author to also set `capabilities.list` would be asking them to
    /// keep two facts in agreement.
    pub fn resolve(&self, sink: Option<&crate::sink::spec::SinkSpec>) -> Capabilities {
        let mut capabilities: Capabilities = (*self).into();
        capabilities.list = sink.is_some_and(|sink| sink.issue.list.is_some());
        capabilities
    }
}

impl From<CapabilitySpec> for Capabilities {
    fn from(spec: CapabilitySpec) -> Self {
        Capabilities {
            states: match spec.states {
                StateKind::Named => StateModel::Named,
                StateKind::OpenClosed => StateModel::OpenClosed,
            },
            // Filled in by `resolve`, which can see the sink half.
            list: false,
            labels: spec.labels,
            due_dates: spec.due_dates,
            milestones: spec.milestones,
            priorities: spec.priorities,
            multiple_assignees: spec.multiple_assignees,
            native_pull_requests: spec.native_pull_requests,
            deletion: spec.deletion,
        }
    }
}

/// The kinds a rule can produce.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Issue,
    Comment,
    Reference,
    /// A container of issues: Linear's project, a forge's project.
    Project,
    /// Recognised and deliberately ignored.
    Skip,
    /// An entity kind named by the event itself: models a platform's new events
    /// without a code change, and makes them visible instead of dropping them.
    EventName,
}

impl Kind {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "issue" => Some(Kind::Issue),
            "comment" => Some(Kind::Comment),
            "reference" => Some(Kind::Reference),
            "project" => Some(Kind::Project),
            "skip" => Some(Kind::Skip),
            "event-name" => Some(Kind::EventName),
            _ => None,
        }
    }
}

impl SourceSpec {
    pub fn from_toml(text: &str) -> Result<Self, String> {
        let spec: Self = toml::from_str(text).map_err(|error| error.to_string())?;
        spec.validate()?;
        Ok(spec)
    }

    /// Everything that can be checked without a payload. Called at load time so a
    /// typo in a preset or an inline spec fails `linear webhook serve --check`
    /// rather than a webhook, hours later.
    pub fn validate(&self) -> Result<(), String> {
        if self.signature.headers.is_empty() {
            return Err("`signature.headers` must name at least one header".into());
        }
        if self.event.headers.is_empty() && self.event.body_field.is_none() {
            return Err(
                "`event.headers` or `event.body_field` must say where the event name lives".into(),
            );
        }
        if self.event.rules.is_empty() {
            return Err("`[[event.rule]]` must have at least one rule".into());
        }
        if let Some(freshness) = &self.freshness {
            if freshness.tolerance_secs <= 0 {
                return Err("`freshness.tolerance_secs` must be positive".into());
            }
            if freshness.field.is_empty() {
                return Err("`freshness.field` must be a JSON pointer".into());
            }
        }
        let mut has_catch_all = false;
        for rule in &self.event.rules {
            if let Some(kind) = &rule.fields.subject_kind {
                if Kind::parse(kind).is_none() {
                    return Err(format!(
                        "rule `{}` declares an unknown subject_kind `{kind}`",
                        rule.event
                    ));
                }
            }
            if rule.event.is_empty() {
                return Err("a rule needs `match` (an event name, or `*`)".into());
            }
            if rule.event == "*" {
                has_catch_all = true;
            }
            if Kind::parse(&rule.kind).is_none() {
                return Err(format!(
                    "rule for `{}` has unknown kind `{}` (known: issue, comment, reference, project, skip, event-name)",
                    rule.event, rule.kind
                ));
            }
            for (event, kind) in &rule.kinds {
                if Kind::parse(kind).is_none() {
                    return Err(format!(
                        "rule for `{}` maps `{event}` to unknown kind `{kind}`",
                        rule.event
                    ));
                }
            }
            if rule.fields.id.is_none() && Kind::parse(&rule.kind) != Some(Kind::Skip) {
                return Err(format!(
                    "rule for `{}` has no `fields.id`; without an id there is nothing to reconcile",
                    rule.event
                ));
            }
            if let Some(fan_out) = &rule.fields.fan_out {
                if !fan_out.starts_with('/') {
                    return Err(format!(
                        "rule for `{}` has fan_out `{fan_out}`, which is not a JSON pointer",
                        rule.event
                    ));
                }
            }
        }
        if !has_catch_all && self.event.headers.is_empty() && self.event.body_field.is_some() {
            // A body-named event can be anything (Linear adds types); without a
            // catch-all, every new type would be silently ignored.
            return Err(
                "an event name read from the payload needs a catch-all rule (`match = \"*\"`)"
                    .into(),
            );
        }
        Ok(())
    }

    /// Whether a payload can be validated end to end without a signature, for
    /// `--check`.
    pub fn describe(&self) -> String {
        let rules: Vec<&str> = self
            .event
            .rules
            .iter()
            .map(|rule| rule.event.as_str())
            .collect();
        format!(
            "{} signature header(s), event from {} , rules: {}",
            self.signature.headers.len(),
            self.event
                .body_field
                .as_deref()
                .map(|pointer| format!("body{pointer}"))
                .unwrap_or_else(|| self.event.headers.join("/")),
            rules.join(", ")
        )
    }
}

/// A connector built from a [`SourceSpec`].
pub struct DeclarativeSource {
    id: ConnectorId,
    /// Absent when this deployment has no webhook secret for this platform.
    secret: Option<Secret>,
    scheme: SignatureScheme,
    spec: SourceSpec,
}

impl DeclarativeSource {
    pub fn new(
        id: impl Into<ConnectorId>,
        secret: impl Into<Option<Secret>>,
        spec: SourceSpec,
    ) -> Self {
        let scheme = SignatureScheme {
            headers: spec.signature.headers.clone(),
            algorithm: spec.signature.algorithm,
            prefix: spec.signature.prefix.clone(),
        };
        Self {
            id: id.into(),
            secret: secret.into(),
            scheme,
            spec,
        }
    }

    pub fn spec(&self) -> &SourceSpec {
        &self.spec
    }

    /// The event name: from a header when the platform names events there, else
    /// from the payload.
    fn event_name(&self, headers: &HeaderMap, document: &serde_json::Value) -> Option<String> {
        if let Some(name) = headers.get_any(
            &self
                .spec
                .event
                .headers
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
        ) {
            return Some(name.to_owned());
        }
        let pointer = self.spec.event.body_field.as_ref()?;
        resolve_string(document, pointer)
    }

    /// Exact matches win over the catch-all; the first match in file order wins
    /// among equals.
    fn rule_for(&self, event_name: &str) -> Option<&RuleSpec> {
        self.spec
            .event
            .rules
            .iter()
            .find(|rule| rule.event == event_name)
            .or_else(|| self.spec.event.rules.iter().find(|rule| rule.event == "*"))
    }

    fn build_event(
        &self,
        rule: &RuleSpec,
        event_name: &str,
        kind: EntityKind,
        delivery: &DeliveryId,
        item: &serde_json::Value,
        root: &serde_json::Value,
    ) -> Result<Event, Reject> {
        let id = rule
            .fields
            .id
            .as_ref()
            .and_then(|pointer| pick(item, root, pointer))
            .ok_or_else(|| {
                Reject::Malformed(format!(
                    "`{event_name}` delivery has no id at `{}`",
                    rule.fields.id.as_deref().unwrap_or("-")
                ))
            })?;

        // What the event is *about* is not always what kind of event it is. A
        // comment delivery is a comment, but the entity it points at is the issue
        // the comment is on - and that issue is what a link pairs, so the subject
        // has to be typed as the parent. Getting this wrong makes every pairing
        // invisible to the reconciler, which then treats a comment as an entity of
        // its own and finds nothing to attach it to.
        let subject_kind = match rule.fields.subject_kind.as_deref().and_then(Kind::parse) {
            Some(Kind::Issue) => EntityKind::Issue,
            Some(Kind::Comment) => EntityKind::Comment,
            Some(Kind::Reference) => EntityKind::Reference,
            Some(Kind::Project) => EntityKind::Project,
            // `skip` and `event-name` point at no entity of their own, and neither
            // does an undeclared subject kind: the event's own kind stands.
            _ => kind.clone(),
        };

        let mut subject = EntityRef::new(self.id.clone(), subject_kind, id);
        if let Some(scope) = as_field(item, root, rule.fields.scope.as_deref()) {
            subject = subject.with_scope(scope);
        }
        if let Some(url) = as_field(item, root, rule.fields.url.as_deref()) {
            subject = subject.with_url(url);
        }

        let actor = as_field(item, root, rule.fields.actor.as_deref()).map(|id| Actor {
            id,
            name: as_field(item, root, rule.fields.actor_name.as_deref()),
        });

        let action = self.action_for(rule, item, root);
        let detail = match kind {
            EntityKind::Comment => EventDetail::Comment {
                id: as_field(item, root, rule.fields.comment_id.as_deref()),
                body: as_field(item, root, rule.fields.body.as_deref()),
            },
            EntityKind::Reference => EventDetail::Reference {
                text: join_text(item, root, &rule.fields.text).unwrap_or_default(),
                merged: rule
                    .fields
                    .merged
                    .as_deref()
                    .and_then(|pointer| flag(item, root, pointer)),
                closing_keywords: match &rule.closing_keywords {
                    Some(configured) => configured.clone(),
                    None => DEFAULT_CLOSING_KEYWORDS
                        .iter()
                        .map(|keyword| (*keyword).to_string())
                        .collect(),
                },
            },
            _ => EventDetail::None,
        };

        Ok(Event {
            connector: self.id.clone(),
            delivery: delivery.clone(),
            event: event_name.to_owned(),
            kind,
            action,
            subject,
            actor,
            detail,
        })
    }

    fn action_for(
        &self,
        rule: &RuleSpec,
        item: &serde_json::Value,
        root: &serde_json::Value,
    ) -> Action {
        let raw = as_field(item, root, rule.fields.action.as_deref());
        match raw {
            Some(raw) => match rule.actions.get(&raw) {
                Some(mapped) => action_from_name(mapped),
                // An action the preset does not know: surfaced, not dropped.
                None => Action::Other(raw),
            },
            None => rule
                .action_default
                .as_deref()
                .map(action_from_name)
                .unwrap_or(Action::Created),
        }
    }
}

impl Source for DeclarativeSource {
    fn id(&self) -> &ConnectorId {
        &self.id
    }

    fn signature(&self) -> SignatureScheme {
        self.scheme.clone()
    }

    fn secret(&self) -> Option<&Secret> {
        self.secret.as_ref()
    }

    fn parse(&self, headers: &HeaderMap, body: &[u8]) -> Result<Vec<Event>, Reject> {
        self.translate(headers, body, true)
    }

    /// Replay a stored body: intake's freshness bound was already applied when
    /// the delivery arrived, and this body has been in the durable queue since.
    fn reparse(&self, headers: &HeaderMap, body: &[u8]) -> Result<Vec<Event>, Reject> {
        self.translate(headers, body, false)
    }

    fn capabilities(&self) -> Capabilities {
        self.spec.capabilities.resolve(self.spec.sink.as_ref())
    }

    /// The event name arrived in a header (a forge) or in the body (Linear). Only
    /// the header case needs rebuilding, and the delivery row kept the name.
    fn replay_headers(&self, event: &str) -> HeaderMap {
        if self.spec.event.headers.is_empty() {
            return HeaderMap::default();
        }
        HeaderMap::from_pairs(
            self.spec
                .event
                .headers
                .iter()
                .map(|header| (header.clone(), event.to_string())),
        )
    }
}

/// Resolve a pointer against the element, then against the whole document. This
/// is what lets one rule describe a payload whose scope is on the delivery and
/// whose id is inside an array element.
fn pick(item: &serde_json::Value, root: &serde_json::Value, pointer: &str) -> Option<String> {
    resolve_string(item, pointer).or_else(|| resolve_string(root, pointer))
}

/// Resolve a flag the way [`pick`] resolves a string: the element first, the document second.
///
/// Only a real boolean counts. A platform that sends `"true"` as text is saying something
/// else, and reading it as a merge would be a guess about the most consequential field here.
fn flag(item: &serde_json::Value, root: &serde_json::Value, pointer: &str) -> Option<bool> {
    let at = |document: &serde_json::Value| {
        document
            .pointer(pointer)
            .and_then(serde_json::Value::as_bool)
    };
    at(item).or_else(|| at(root))
}

fn as_field(
    item: &serde_json::Value,
    root: &serde_json::Value,
    pointer: Option<&str>,
) -> Option<String> {
    pointer.and_then(|pointer| pick(item, root, pointer))
}

fn join_text(
    item: &serde_json::Value,
    root: &serde_json::Value,
    pointers: &[String],
) -> Option<String> {
    let parts: Vec<String> = pointers
        .iter()
        .filter_map(|pointer| pick(item, root, pointer))
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

fn action_from_name(name: &str) -> Action {
    match name {
        "created" => Action::Created,
        "updated" => Action::Updated,
        "closed" => Action::Closed,
        "reopened" => Action::Reopened,
        "deleted" => Action::Deleted,
        other => Action::Other(other.to_owned()),
    }
}

fn check_freshness(document: &serde_json::Value, freshness: &FreshnessSpec) -> Result<(), Reject> {
    let raw = resolve(document, &freshness.field).ok_or(Reject::Stale)?;
    let value = match (raw, freshness.unit) {
        (serde_json::Value::Number(number), TimeUnit::Millis) => number.as_i64(),
        (serde_json::Value::Number(number), TimeUnit::Seconds) => {
            number.as_i64().map(|seconds| seconds * 1_000)
        }
        // Some providers quote their timestamps; accept a numeric string rather
        // than rejecting a delivery over JSON type pedantry.
        (serde_json::Value::String(text), unit) => {
            text.trim().parse::<i64>().ok().map(|value| match unit {
                TimeUnit::Millis => value,
                TimeUnit::Seconds => value * 1_000,
            })
        }
        _ => None,
    };
    let value = value.ok_or(Reject::Stale)?;
    let now = crate::clock::now_millis();
    if (now - value).abs() > freshness.tolerance_secs * 1_000 {
        return Err(Reject::Stale);
    }
    Ok(())
}

#[cfg(test)]
mod tests;

impl DeclarativeSource {
    fn translate(
        &self,
        headers: &HeaderMap,
        body: &[u8],
        enforce_freshness: bool,
    ) -> Result<Vec<Event>, Reject> {
        let document: serde_json::Value =
            serde_json::from_slice(body).map_err(|error| Reject::Malformed(error.to_string()))?;

        if enforce_freshness {
            if let Some(freshness) = &self.spec.freshness {
                check_freshness(&document, freshness)?;
            }
        }

        let event_name = self.event_name(headers, &document).ok_or_else(|| {
            Reject::MissingHeader(
                self.spec
                    .event
                    .headers
                    .first()
                    .cloned()
                    .unwrap_or_else(|| self.spec.event.body_field.clone().unwrap_or_default()),
            )
        })?;

        let delivery = DeliveryId::new(
            headers
                .get_any(
                    &self
                        .spec
                        .delivery
                        .headers
                        .iter()
                        .map(String::as_str)
                        .collect::<Vec<_>>(),
                )
                .map(str::to_owned)
                .unwrap_or_else(|| body_digest(body)),
        );

        // No rule: an event this deployment does not model. Acknowledged with
        // nothing to do, so the provider never retries it.
        let Some(rule) = self.rule_for(&event_name) else {
            return Ok(Vec::new());
        };

        let kind_name = rule
            .kinds
            .get(&event_name)
            .map(String::as_str)
            .unwrap_or(rule.kind.as_str());
        let Some(kind) = Kind::parse(kind_name) else {
            // Unreachable for a validated spec; reported rather than panicked.
            return Err(Reject::Malformed(format!(
                "rule for `{event_name}` declares unknown kind `{kind_name}`"
            )));
        };
        if kind == Kind::Skip {
            return Ok(Vec::new());
        }
        let entity_kind = match kind {
            Kind::Issue => EntityKind::Issue,
            Kind::Comment => EntityKind::Comment,
            Kind::Reference => EntityKind::Reference,
            Kind::Project => EntityKind::Project,
            Kind::EventName => EntityKind::Other(event_name.clone()),
            Kind::Skip => unreachable!("handled above"),
        };

        match &rule.fields.fan_out {
            None => Ok(vec![self.build_event(
                rule,
                &event_name,
                entity_kind,
                &delivery,
                &document,
                &document,
            )?]),
            Some(pointer) => {
                let items = resolve(&document, pointer)
                    .and_then(|value| value.as_array())
                    .ok_or_else(|| {
                        Reject::Malformed(format!(
                            "`{event_name}` delivery has no array at `{pointer}`"
                        ))
                    })?;
                items
                    .iter()
                    .map(|item| {
                        self.build_event(
                            rule,
                            &event_name,
                            entity_kind.clone(),
                            &delivery,
                            item,
                            &document,
                        )
                    })
                    .collect()
            }
        }
    }
}
