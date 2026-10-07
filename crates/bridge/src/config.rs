//! Service configuration.
//!
//! Read from the *same* `linear.toml` the CLI already uses, so an operator has
//! one file and one precedence chain (`env` -> project -> global) instead of a
//! second config format for the service half of the same binary.
//!
//! ```toml
//! [bridge]
//! bind = "127.0.0.1:8787"
//!
//! [platform.forgejo]              # a built-in preset
//! type = "forgejo"
//! secret_env = "FORGEJO_WEBHOOK_SECRET"   # what it signs deliveries with
//! token_env = "FORGEJO_TOKEN"             # what this bridge writes with
//! closed_state = ["closed"]               # how this platform says "finished"
//! open_state = "open"
//!
//! [platform.linear]
//! type = "linear"
//! secret_env = "LINEAR_WEBHOOK_SECRET"   # only needed to *receive* from this platform
//! token_env = "LINEAR_API_KEY"
//! closed_state = ["Done", "Canceled"]
//! open_state = "In Progress"
//! initial_state = "Todo"          # where a newly mirrored issue lands
//!
//! [platform.internal]             # ... or a platform described right here
//! type = "generic"
//! secret_env = "INTERNAL_WEBHOOK_SECRET"
//! [platform.internal.spec.signature]
//! headers = ["x-signature"]
//! algorithm = "hmac-sha256"
//! [platform.internal.spec.event]
//! headers = ["x-event"]
//! [[platform.internal.spec.event.rule]]
//! match = "ticket"
//! kind = "issue"
//! [platform.internal.spec.event.rule.fields]
//! id = "/ticket/id"
//!
//! [[mapping]]
//! source = "linear:VED"
//! sink = "forgejo:Vedaru/linear-cli-rs"
//! direction = "both"              # or `oneway` for source -> sink only
//! sync_projects = true            # mirror projects as well as their issues
//! ```
//!
//! `type = "generic"` plus a spec is not a fallback for the platforms this crate
//! ships presets for - it *is* how they work. A preset is the same spec with
//! `include_str!` on it.
//!
//! Secrets are referenced by environment variable, not spelled in the file: a
//! config that is safe to commit is the point, and `secret_env` makes the
//! deployment's secret store the only place a secret exists.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;

use crate::connector::Source;
use crate::domain::{parse_connector_ref, ConnectorId, Identity, Secret, UserMap};
use crate::error::{Error, Result};
use crate::queue::WorkerConfig;
use crate::reconcile::handler::{Endpoint, Mapping};
use crate::reconcile::placement::{ProjectScope, ProjectScopes};
use crate::reconcile::{Direction, Sides, StateNames};
use crate::sink::spec::SinkSpec;
use crate::sink::Sink;
use crate::sources::declarative::{DeclarativeSource, SourceSpec};
use crate::sources::presets;
use crate::{DEFAULT_BODY_LIMIT, MIN_SECRET_LEN};

/// Default bind address. Loopback, because the service is expected to sit behind
/// the existing vhost/reverse proxy rather than on the public interface.
pub const DEFAULT_BIND: &str = "127.0.0.1:8787";

/// The configured type that means "the spec is in this file".
pub const GENERIC_TYPE: &str = "generic";

#[derive(Clone, Debug)]
pub struct PlatformConfig {
    pub name: ConnectorId,
    /// The configured `type`, kept for logs and `--check` output.
    pub declared_type: String,
    /// What the platform signs its deliveries with (inbound).
    ///
    /// Optional, because receiving is not the only thing this bridge does: a
    /// deployment that only ever *pushes* with `sync` needs no webhook secret at
    /// all, and requiring one would mean carrying service configuration to use a
    /// command that has no service. The requirement lives where it belongs - the
    /// moment a webhook source is built - so a config that is valid for `sync` is
    /// valid, and `webhook serve` says exactly which platform it is missing.
    pub secret: Option<Secret>,
    /// What this bridge authenticates with when it reads or writes the platform's
    /// API. Absent means the platform can only be a *source* of events: a
    /// deployment that only receives webhooks has no reason to hold a token.
    pub token: Option<Secret>,
    /// The platform's own state vocabulary, so the reconciler can compare two
    /// platforms that do not share one (see `reconcile::StateNames`).
    pub states: StateNames,
    /// Overrides the preset's API address (a self-hosted instance elsewhere).
    pub api_url: Option<String>,
    pub spec: SourceSpec,
}

impl PlatformConfig {
    /// The write half as this deployment configured it: the preset's spec, with
    /// the deployment's API address substituted when it named one.
    pub fn sink_spec(&self) -> Option<SinkSpec> {
        let mut spec = self.spec.sink.clone()?;
        if let Some(url) = &self.api_url {
            spec.base_url = url.clone();
        }
        Some(spec)
    }

    /// Whether the reconciler could read or write this platform: it needs both a
    /// write half in its spec and a credential to use it.
    pub fn can_be_written(&self) -> bool {
        self.sink_spec().is_some() && self.token.is_some()
    }

    /// The connector this platform can be written through, when it can be.
    pub fn sink(&self) -> Option<crate::sink::declarative::DeclarativeSink> {
        let spec = self.sink_spec()?;
        let token = self.token.clone()?;
        Some(crate::sink::declarative::DeclarativeSink::new(
            self.name.clone(),
            spec,
            Some(token),
            self.spec.capabilities.resolve(self.spec.sink.as_ref()),
        ))
    }
}

/// A mapping between two connectors. Consumed by the reconciler from M3; parsed
/// and validated now so a bad mapping fails at startup rather than mid-sync.
#[derive(Clone, Debug)]
pub struct MappingConfig {
    pub name: Option<String>,
    /// `connector:scope`, e.g. `linear:VED`.
    pub source: String,
    /// `connector:scope`, e.g. `forgejo:Vedaru/linear-cli-rs`.
    pub sink: String,
    pub direction: Direction,
    pub sync_issues: bool,
    /// Mirror *projects* as well as their issues. Off unless a deployment asks:
    /// copying the containers when it was configured for their contents is a
    /// surprise, so this is opt-in exactly as it is in the policy.
    pub sync_projects: bool,
    pub git_automation: bool,
    pub delete_sync: bool,
    /// `[[mapping.identity]]`: how one person is known on each platform, e.g.
    ///
    /// ```toml
    /// [[mapping.identity]]
    /// linear = "loner@example.com"
    /// forgejo = "vedaru"
    /// ```
    ///
    /// Absent means the deployment has not said, and then assignee sync is off
    /// rather than guessed at - a login from one platform sent to another is worse
    /// than a field that stays behind, and the log says which it was.
    pub identity: Vec<BTreeMap<String, String>>,
    /// `[[mapping.project]]`: which sink scope a project's mirror lives in.
    ///
    /// ```toml
    /// [[mapping.project]]
    /// project = "kuro"          # a project by id, slug or name
    /// scope = "Vedaru/kuro"     # the repository its mirror lives in
    /// ```
    ///
    /// This is the only source: a project's *links* are written for people (a reference
    /// implementation, a design doc) and change for human reasons, so they are not a sync
    /// contract. A project no entry names is not mirrored; several entries may name the
    /// same repository.
    pub project: Vec<ProjectScope>,
    /// `[mapping.columns]`: what the sink's *board* calls each of the source's states.
    ///
    /// ```toml
    /// [mapping.columns]
    /// "Todo" = "To Do"
    /// "In Progress" = "In Progress"
    /// ```
    ///
    /// A board's columns are the finest thing a forge has that a set of states can be
    /// projected onto, and they are the sink's, so the *key* is a state name on the
    /// source. A state the table does not name leaves the card where it is: a mapping
    /// that has not thought about a state must not move somebody's card to the default
    /// column. Empty - the default - means the mapping never places a card in a named
    /// column at all.
    pub columns: BTreeMap<String, String>,
}

impl MappingConfig {
    /// The identity map, as the reconciler reads it.
    ///
    /// Each entry is a group of `platform = key` pairs. One entry naming a single
    /// platform is a configuration error, not a half-known person: it would match
    /// nothing and skip every assignee in silence.
    pub fn users(&self) -> Result<UserMap> {
        let mut groups = Vec::with_capacity(self.identity.len());
        for (index, entry) in self.identity.iter().enumerate() {
            if entry.len() < 2 {
                return Err(Error::Config(format!(
                    "mapping `{}`: identity {} names {} platform(s), and an identity needs at least two",
                    self.label(),
                    index + 1,
                    entry.len()
                )));
            }
            groups.push(
                entry
                    .iter()
                    .map(|(connector, key)| Identity::new(connector.clone(), key.clone()))
                    .collect(),
            );
        }
        Ok(UserMap::from_groups(groups))
    }

    /// A name for logs and errors, when the deployment did not give one.
    pub fn label(&self) -> String {
        self.name
            .clone()
            .unwrap_or_else(|| format!("{} -> {}", self.source, self.sink))
    }

    /// The policy the reconciler runs this mapping with.
    pub fn policy(
        &self,
        source: &PlatformConfig,
        sink: &PlatformConfig,
    ) -> crate::reconcile::Policy {
        crate::reconcile::Policy {
            direction: self.direction,
            sync_issues: self.sync_issues,
            sync_projects: self.sync_projects,
            git_automation: self.git_automation,
            delete_sync: self.delete_sync,
            names: Sides::new(source.states.clone(), sink.states.clone()),
            columns: self.columns.clone(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct BridgeConfig {
    pub bind: SocketAddr,
    pub store_path: String,
    pub body_limit: usize,
    pub http_threads: usize,
    pub worker_threads: usize,
    pub worker: WorkerConfig,
    pub platforms: Vec<PlatformConfig>,
    pub mappings: Vec<MappingConfig>,
}

impl BridgeConfig {
    /// Parse the service sections out of a `linear.toml` document. Keys that
    /// belong to the CLI are ignored, so both halves can share one file.
    pub fn from_toml(text: &str) -> Result<Self> {
        Self::from_toml_with_env(text, &|name| std::env::var(name).ok())
    }

    /// As [`BridgeConfig::from_toml`], with the environment lookup injected.
    ///
    /// The environment is the one input a parser cannot take from its argument,
    /// and reading it directly would make "a missing `secret_env` is a startup
    /// error" untestable without mutating process-global state (which is
    /// racy under a parallel test runner).
    pub fn from_toml_with_env(text: &str, env: &dyn Fn(&str) -> Option<String>) -> Result<Self> {
        let document: Document = toml::from_str(text)
            .map_err(|error| Error::Config(format!("cannot parse the service config: {error}")))?;

        let bridge = document.bridge.unwrap_or_default();
        let bind_raw = bridge.bind.unwrap_or_else(|| DEFAULT_BIND.to_string());
        let bind: SocketAddr = bind_raw.parse().map_err(|error| {
            Error::Config(format!(
                "bridge.bind `{bind_raw}` is not host:port ({error})"
            ))
        })?;

        let platforms = build_platforms(document.platform, env)?;
        let mappings = build_mappings(document.mapping, &platforms)?;

        if platforms.is_empty() {
            return Err(Error::Config(
                "no `[platform.*]` is configured: the service has nothing to accept webhooks for"
                    .into(),
            ));
        }

        Ok(Self {
            bind,
            // A leading `~` is expanded here rather than left to the shell, because the file
            // is read by a service that has no shell: a config saying `~/…` would otherwise
            // create a directory literally called `~`, next to wherever it happened to start.
            store_path: expand_tilde(
                &bridge
                    .store
                    .unwrap_or_else(|| "linear-bridge.db".to_string()),
            ),
            body_limit: bridge.body_limit.unwrap_or(DEFAULT_BODY_LIMIT),
            http_threads: bridge.http_threads.unwrap_or(4).max(1),
            worker_threads: bridge.worker_threads.unwrap_or(2).max(1),
            worker: bridge.worker.into_worker_config(),
            platforms,
            mappings,
        })
    }

    /// The webhook sources these platforms produce. All of them are the same
    /// engine, configured differently.
    pub fn sources(&self) -> Vec<Arc<dyn Source>> {
        self.platforms
            .iter()
            .map(|platform| {
                Arc::new(DeclarativeSource::new(
                    platform.name.clone(),
                    platform.secret.clone(),
                    platform.spec.clone(),
                )) as Arc<dyn Source>
            })
            .collect()
    }

    /// The sources a webhook endpoint may be served from: these platforms are asked to
    /// *receive*, so each one needs a secret to verify deliveries with.
    ///
    /// This is the only place the requirement belongs, and it is `serve`'s question, not
    /// the file's: `linear sync` pushes changes through the same connectors, never
    /// verifies a delivery, and so is never asked for a secret.
    pub fn receiving_sources(&self) -> Result<Vec<Arc<dyn Source>>> {
        self.platforms
            .iter()
            .map(|platform| {
                let name = platform.name.as_str().to_string();
                if platform.secret.is_none() {
                    return Err(Error::Config(format!(
                        "[platform.{name}] would receive deliveries, but it has no webhook secret: set `secret_env`. If this deployment should only push changes, use `linear sync`, which needs no secret."
                    )));
                }
                Ok(Arc::new(DeclarativeSource::new(
                    platform.name.clone(),
                    platform.secret.clone(),
                    platform.spec.clone(),
                )) as Arc<dyn Source>)
            })
            .collect()
    }

    /// The write halves this deployment can use, one per platform that has an API
    /// token. A platform without one stays a source of events - receiving webhooks
    /// needs no credential, and a deployment that only wants to observe a platform
    /// should not have to hold one.
    pub fn sinks(&self) -> Vec<Arc<dyn Sink>> {
        self.platforms
            .iter()
            .filter_map(|platform| platform.sink())
            .map(|sink| Arc::new(sink) as Arc<dyn Sink>)
            .collect()
    }

    /// The mappings, resolved into the terms the reconciler works in.
    ///
    /// This is the join between configuration and behaviour: the endpoints come from
    /// the `connector:scope` references, and the state vocabulary from the two
    /// platforms - so a mapping cannot be run with a vocabulary nobody declared,
    /// which is the shape of bug that otherwise shows up as "state never synced".
    pub fn reconcile_mappings(&self) -> Result<Vec<Mapping>> {
        self.mappings
            .iter()
            .map(|mapping| {
                let label = mapping.label();
                let source = self.platform_for(&mapping.source, &label)?;
                let sink = self.platform_for(&mapping.sink, &label)?;
                let users = mapping.users()?;
                let named = |connector: &str| users.keys_for(&ConnectorId::new(connector)).len();
                if !mapping.identity.is_empty()
                    && named(source.name.as_str()) == 0
                    && named(sink.name.as_str()) == 0
                {
                    // Every identity in this mapping names platforms it does not
                    // connect. It is a typo, and the only symptom would be assignees
                    // quietly staying behind.
                    return Err(Error::Config(format!(
                        "mapping `{label}`: no identity names `{}` or `{}`, so none of them can apply",
                        source.name, sink.name
                    )));
                }
                Ok(Mapping {
                    name: label,
                    source: Endpoint::parse(&mapping.source)?,
                    sink: Endpoint::parse(&mapping.sink)?,
                    policy: mapping.policy(source, sink),
                    users,
                    project_scopes: ProjectScopes::new(mapping.project.clone()),
                })
            })
            .collect()
    }

    fn platform_for<'a>(&'a self, reference: &str, mapping: &str) -> Result<&'a PlatformConfig> {
        let (connector, _) = parse_connector_ref(reference)
            .map_err(|error| Error::Config(format!("mapping `{mapping}`: {error}")))?;
        self.platforms
            .iter()
            .find(|platform| platform.name == connector)
            .ok_or_else(|| {
                Error::Config(format!(
                    "mapping `{mapping}` names platform `{connector}`, which is not declared"
                ))
            })
    }

    /// Override the bind address (the CLI's `--bind`), so the same config can be
    /// run on a different port without editing it.
    pub fn with_bind(mut self, bind: SocketAddr) -> Self {
        self.bind = bind;
        self
    }
}

fn build_platforms(
    sections: BTreeMap<String, PlatformSection>,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<Vec<PlatformConfig>> {
    sections
        .into_iter()
        .map(|(name, section)| {
            let secret = resolve_secret(&name, &section, env)?;
            let token = resolve_token(&name, &section, env)?;
            let spec = resolve_spec(&name, &section)?;
            let states = StateNames {
                closed: section.closed_state.clone(),
                initial: section.initial_state.clone(),
                open: section.open_state.clone(),
            };
            Ok(PlatformConfig {
                name: ConnectorId::new(name),
                declared_type: section.kind,
                secret,
                token,
                states,
                api_url: section.api_url.clone(),
                spec,
            })
        })
        .collect()
}

/// The spec for a platform: a preset by name, or one written in the config.
fn resolve_spec(name: &str, section: &PlatformSection) -> Result<SourceSpec> {
    let inline = section.spec.is_some() || section.spec_file.is_some();
    if section.kind == GENERIC_TYPE {
        if inline {
            return read_inline_spec(name, section);
        }
        return Err(Error::Config(format!(
            "[platform.{name}] has `type = \"generic\"` but no spec: add `[platform.{name}.spec]` or `spec_file`"
        )));
    }

    if inline {
        return Err(Error::Config(format!(
            "[platform.{name}] uses preset `{}` and also declares a spec; keep one (`type = \"{GENERIC_TYPE}\"` for a custom spec)",
            section.kind
        )));
    }

    presets::preset(&section.kind).map_err(|error| match error {
        Error::Config(message) => Error::Config(format!("[platform.{name}]: {message}")),
        other => other,
    })
}

fn read_inline_spec(name: &str, section: &PlatformSection) -> Result<SourceSpec> {
    let value = match (&section.spec, &section.spec_file) {
        (Some(_), Some(_)) => {
            return Err(Error::Config(format!(
                "[platform.{name}] sets both `spec` and `spec_file`; keep one"
            )))
        }
        (Some(value), None) => value.clone(),
        (None, Some(path)) => {
            let text = std::fs::read_to_string(path).map_err(|error| {
                Error::Config(format!(
                    "[platform.{name}] cannot read spec_file `{path}`: {error}"
                ))
            })?;
            return SourceSpec::from_toml(&text).map_err(|error| {
                Error::Config(format!("[platform.{name}] spec is invalid: {error}"))
            });
        }
        (None, None) => unreachable!("callers check that a spec is present"),
    };

    // Deserialised from the value directly rather than via its text form: an
    // inline table serialises to `{ ... }`, which is not a valid TOML document.
    let spec: SourceSpec = value
        .try_into()
        .map_err(|error| Error::Config(format!("[platform.{name}] spec is invalid: {error}")))?;
    spec.validate()
        .map_err(|error| Error::Config(format!("[platform.{name}] spec is invalid: {error}")))?;
    Ok(spec)
}

fn resolve_secret(
    name: &str,
    section: &PlatformSection,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<Option<Secret>> {
    let secret = match (&section.secret_env, &section.secret) {
        (Some(variable), None) => {
            let value = env(variable).ok_or_else(|| {
                Error::Config(format!(
                    "[platform.{name}] needs the environment variable `{variable}`, which is not set"
                ))
            })?;
            Secret::new(value)
        }
        (None, Some(value)) => {
            log::warn!(
                "[platform.{name}] has an inline secret; prefer `secret_env` so the config stays commit-safe"
            );
            Secret::new(value.clone())
        }
        (Some(_), Some(_)) => {
            return Err(Error::Config(format!(
                "[platform.{name}] sets both `secret` and `secret_env`; keep one"
            )))
        }
        // No secret *declared* is a shape, not a mistake: it is what a config for
        // `linear sync` looks like. A declared variable that is not set is still an
        // error - the deployment said where to find the secret and it was not there.
        (None, None) => return Ok(None),
    };

    if secret.len() < MIN_SECRET_LEN {
        return Err(Error::Config(format!(
            "[platform.{name}] secret is {} characters; the webhook endpoint is the only unauthenticated surface, so at least {MIN_SECRET_LEN} are required",
            secret.len()
        )));
    }
    Ok(Some(secret))
}

/// The API credential for the write path, if this deployment configured one.
///
/// Optional on purpose: a platform this bridge only receives webhooks from does not
/// need a token, and demanding one would make the read-only case impossible.
fn resolve_token(
    name: &str,
    section: &PlatformSection,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<Option<Secret>> {
    let token = match (&section.token_env, &section.token) {
        (Some(variable), None) => {
            let value = env(variable).ok_or_else(|| {
                Error::Config(format!(
                    "[platform.{name}] needs the environment variable `{variable}` for its API token, which is not set (it is what this bridge reads and writes the platform's API with)"
                ))
            })?;
            Secret::new(value)
        }
        (None, Some(value)) => {
            log::warn!(
                "[platform.{name}] has an inline API token; prefer `token_env` so the config stays commit-safe"
            );
            Secret::new(value.clone())
        }
        (Some(_), Some(_)) => {
            return Err(Error::Config(format!(
                "[platform.{name}] sets both `token` and `token_env`; keep one"
            )))
        }
        (None, None) => return Ok(None),
    };

    if token.len() < MIN_SECRET_LEN {
        return Err(Error::Config(format!(
            "[platform.{name}] API token is {} characters; that is short enough to be a mistake rather than a credential",
            token.len()
        )));
    }
    Ok(Some(token))
}

fn build_mappings(
    sections: Vec<MappingSection>,
    platforms: &[PlatformConfig],
) -> Result<Vec<MappingConfig>> {
    let known: Vec<&str> = platforms.iter().map(|p| p.name.as_str()).collect();
    sections
        .into_iter()
        .map(|section| {
            let label = section
                .name
                .clone()
                .unwrap_or_else(|| format!("{} -> {}", section.source, section.sink));
            let direction = match section.direction.as_deref() {
                None => Direction::Both,
                Some(name) => Direction::parse(name).ok_or_else(|| {
                    Error::Config(format!(
                        "mapping `{label}` has direction `{name}` (expected `both`, `oneway` or `sink-to-source`)"
                    ))
                })?,
            };
            for reference in [&section.source, &section.sink] {
                let (connector, _) = parse_connector_ref(reference)
                    .map_err(|error| Error::Config(format!("mapping `{label}`: {error}")))?;
                let platform = platforms
                    .iter()
                    .find(|platform| platform.name == connector)
                    .ok_or_else(|| {
                        Error::Config(format!(
                            "mapping `{label}` names platform `{connector}`, which is not declared (declared: {})",
                            known.join(", ")
                        ))
                    })?;
                // A mapping reads *both* sides to decide anything, so both ends need
                // a credential. Saying so here means an operator hears about it at
                // startup rather than on the first delivery.
                if platform.sink_spec().is_none() {
                    return Err(Error::Config(format!(
                        "mapping `{label}` needs to read `{connector}`, but that platform has no API description to read it through: its preset has no `[sink]` section (or an inline spec needs one)"
                    )));
                }
                if platform.token.is_none() {
                    return Err(Error::Config(format!(
                        "mapping `{label}` needs to read `{connector}`, but [platform.{connector}] has no API token: add `token_env` (that is what the bridge authenticates with - not `secret_env`, which is the webhook secret it verifies deliveries with)"
                    )));
                }
            }
            // `[[mapping.project]]`: placement is configuration. Both keys are required;
            // an empty one is a mistake worth failing at startup over.
            let project: Vec<ProjectScope> = section
                .project
                .iter()
                .enumerate()
                .map(|(index, entry)| {
                    let project = entry.project.as_deref().unwrap_or("").trim();
                    let scope = entry.scope.as_deref().unwrap_or("").trim();
                    if project.is_empty() {
                        return Err(Error::Config(format!(
                            "mapping `{label}`: project entry {} names no project",
                            index + 1
                        )));
                    }
                    if scope.is_empty() {
                        return Err(Error::Config(format!(
                            "mapping `{label}`: the entry for project `{project}` names no scope"
                        )));
                    }
                    Ok(ProjectScope {
                        project: project.to_string(),
                        scope: scope.to_string(),
                    })
                })
                .collect::<Result<_>>()?;
            Ok(MappingConfig {
                name: section.name,
                source: section.source,
                sink: section.sink,
                direction,
                sync_issues: section.sync_issues,
                sync_projects: section.sync_projects,
                git_automation: section.git_automation,
                delete_sync: section.delete_sync,
                identity: section.identity,
                project,
                columns: section.columns,
            })
        })
        .collect()
}

// --- raw document shapes ----------------------------------------------------

#[derive(Debug, Deserialize)]
struct Document {
    #[serde(default)]
    bridge: Option<BridgeSection>,
    #[serde(default)]
    platform: BTreeMap<String, PlatformSection>,
    #[serde(default)]
    mapping: Vec<MappingSection>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct BridgeSection {
    bind: Option<String>,
    store: Option<String>,
    body_limit: Option<usize>,
    http_threads: Option<usize>,
    worker_threads: Option<usize>,
    #[serde(default)]
    worker: WorkerSection,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkerSection {
    max_attempts: Option<u32>,
    backoff_base_ms: Option<u64>,
    backoff_max_ms: Option<u64>,
    poll_interval_ms: Option<u64>,
    lease_secs: Option<u64>,
}

impl WorkerSection {
    fn into_worker_config(self) -> WorkerConfig {
        let defaults = WorkerConfig::default();
        let backoff_max = self
            .backoff_max_ms
            .map(Duration::from_millis)
            .unwrap_or(defaults.backoff_max);
        WorkerConfig {
            max_attempts: self.max_attempts.unwrap_or(defaults.max_attempts),
            backoff_base: self
                .backoff_base_ms
                .map(Duration::from_millis)
                .unwrap_or(defaults.backoff_base),
            backoff_max,
            poll_interval: self
                .poll_interval_ms
                .map(Duration::from_millis)
                .unwrap_or(defaults.poll_interval),
            // A lease shorter than the backoff window would let a slow retry be
            // claimed twice.
            lease: self
                .lease_secs
                .map(Duration::from_secs)
                .unwrap_or(backoff_max + Duration::from_secs(60)),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlatformSection {
    #[serde(rename = "type")]
    kind: String,
    secret: Option<String>,
    secret_env: Option<String>,
    /// The API credential for the write path, inline or by environment variable.
    token: Option<String>,
    token_env: Option<String>,
    /// Overrides the preset's API address.
    api_url: Option<String>,
    /// The names this platform uses. A list for `closed_state` because a workflow
    /// usually has more than one way of being finished.
    #[serde(default)]
    closed_state: Vec<String>,
    open_state: Option<String>,
    initial_state: Option<String>,
    /// An inline spec, as a nested table. Kept as a raw value so the spec's own
    /// schema is the only thing that validates it.
    spec: Option<toml::Value>,
    /// Path to a spec file, for a payload description too large to inline.
    spec_file: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MappingSection {
    name: Option<String>,
    source: String,
    sink: String,
    direction: Option<String>,
    /// `[[mapping.identity]]`: one person, one key per platform. Each table is a
    /// group, so a person known on three platforms is one entry rather than three
    /// pairs that disagree about who is the counterpart.
    #[serde(default)]
    identity: Vec<BTreeMap<String, String>>,
    /// `[[mapping.project]]`: a project, and the sink scope its mirror lives in.
    #[serde(default)]
    project: Vec<ProjectSection>,
    /// `[mapping.columns]`: what the sink's board calls each of the source's states.
    #[serde(default)]
    columns: BTreeMap<String, String>,
    #[serde(default = "default_true")]
    sync_issues: bool,
    /// Projects are opt-in: absent means off.
    #[serde(default)]
    sync_projects: bool,
    #[serde(default = "default_true")]
    git_automation: bool,
    #[serde(default)]
    delete_sync: bool,
}

/// One `[[mapping.project]]`: which project, and the sink scope its mirror lives in.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectSection {
    /// The project, by the id, slug or name a person knows it by.
    #[serde(default)]
    project: Option<String>,
    /// The sink scope - a repository - its mirror lives in.
    #[serde(default)]
    scope: Option<String>,
}

fn default_true() -> bool {
    true
}

/// `~/…` against the current user's home directory, when there is one. Anything else is left
/// exactly as it is: a path this does not understand is the operator's, not ours to rewrite.
fn expand_tilde(path: &str) -> String {
    expand_tilde_with(path, home_directory().as_deref())
}

/// The home directory this platform names.
///
/// Windows has no `HOME` - it calls the same thing `USERPROFILE`. Asking for `HOME` alone left
/// a store path of `~/.local/…` unexpanded there, and the service then created a directory
/// literally called `~` beside wherever it happened to start.
fn home_directory() -> Option<String> {
    ["HOME", "USERPROFILE"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|value| !value.is_empty())
}

/// The expansion itself, with the home directory named rather than looked up, so both branches
/// can be pinned on any platform instead of only on the one that fails.
fn expand_tilde_with(path: &str, home: Option<&str>) -> String {
    let Some(rest) = path.strip_prefix("~/") else {
        return path.to_string();
    };
    match home {
        Some(home) if !home.is_empty() => {
            format!("{}/{}", home.trim_end_matches(['/', '\\']), rest)
        }
        _ => path.to_string(),
    }
}

#[cfg(test)]
mod tests;
