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
use crate::reconcile::route::{Location, Route, Routes};
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
    /// `[[mapping.route]]`: which sink scope an entity's mirror lives in.
    ///
    /// ```toml
    /// [[mapping.route]]
    /// project = "project-uuid"      # a project by id, slug or name - and its issues
    /// scope = "Vedaru/kuro"
    ///
    /// [[mapping.route]]
    /// issue = "VED-119"             # one issue, by the identifier a person sees
    /// scope = "Vedaru/kuro"
    ///
    /// [[mapping.route]]
    /// label = "urgent"              # any issue carrying this label
    /// scope = "Vedaru/hotfix"
    /// ```
    ///
    /// At least one of `project`, `issue` or `label` is required, and `scope` names a
    /// container on the mapping's *sink* connector - a scope on another connector is
    /// refused at load time.
    ///
    /// The order an entity's scope is decided in, first match wins:
    ///
    /// 1. an explicit rule - `project` for a project (and, by inheritance, its
    ///    issues), then `issue`, then `label` for an issue;
    /// 2. a location the entity's project *declares*, when it points at the sink
    ///    platform (a link to the repository, recognised by the sink preset's URL
    ///    shape);
    /// 3. the mapping's own sink scope.
    ///
    /// Empty means every entity falls through to step 2 or 3.
    pub route: Vec<Route>,
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
                    routes: Routes::new(mapping.route.clone()),
                    sink_location: sink.sink_spec().and_then(|spec| {
                        let location = spec.location?;
                        // The connector's *effective* address: the deployment's `api_url`
                        // when it declares one, the preset's own default otherwise. Reading
                        // only the override would leave a deployment that never set it with
                        // no URL shape at all, and the link rule silently dead.
                        resolve_location(&location.url, Some(spec.base_url.as_str()))
                    }),
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

/// A preset's URL shape, made absolute against the platform it belongs to.
///
/// A preset must not name one deployment's host. `/{scope}` says "a repository of this platform
/// lives at this instance's root plus its scope", and the instance's address is where every
/// other call already goes: the connector's base URL. An absolute pattern is left alone, so a
/// preset can still spell out a host that is not the API's.
///
/// Only the origin is taken from the base URL - it carries the API prefix (`/api/v1`), which a
/// repository URL does not have.
fn resolve_location(pattern: &str, base_url: Option<&str>) -> Option<Location> {
    if pattern.contains("://") {
        return Location::parse(pattern);
    }
    let (scheme, rest) = base_url?.split_once("://")?;
    let host = rest.split('/').next()?;
    Location::parse(&format!("{scheme}://{host}{pattern}"))
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
            // A route may only name a scope on the mapping's *sink* connector: the sink
            // is what an entity is mirrored into, and the connector is already fixed.
            let (sink_connector, _) = parse_connector_ref(&section.sink)
                .map_err(|error| Error::Config(format!("mapping `{label}`: {error}")))?;
            let routes: Vec<Route> = section
                .route
                .iter()
                .enumerate()
                .map(|(index, route)| build_route(&label, index + 1, route, &sink_connector))
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
                route: routes,
                columns: section.columns,
            })
        })
        .collect()
}

/// One `[[mapping.route]]`, validated against the mapping's sink connector.
///
/// A route names at least one key to match on and a *scope* - nothing else. The
/// connector is the mapping's own, so an operator who writes `connector:scope` either
/// means a scope on the sink (the prefix is accepted and stripped) or a scope on a
/// platform this mapping does not write to - which is a mistake worth failing at
/// startup over, not a request to the wrong platform at runtime.
fn build_route(
    label: &str,
    position: usize,
    route: &RouteSection,
    sink: &ConnectorId,
) -> Result<Route> {
    // An empty string is the same as absent: a key that matches nothing is not a key.
    let key = |value: Option<&str>| {
        value
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let project = key(route.project.as_deref());
    let issue = key(route.issue.as_deref());
    let route_label = key(route.label.as_deref());
    if project.is_none() && issue.is_none() && route_label.is_none() {
        return Err(Error::Config(format!(
            "mapping `{label}`: route {position} names no `project`, `issue` or `label` to match on"
        )));
    }
    let named = project
        .as_ref()
        .map(|value| format!("project `{value}`"))
        .or_else(|| issue.as_ref().map(|value| format!("issue `{value}`")))
        .or_else(|| route_label.as_ref().map(|value| format!("label `{value}`")))
        .unwrap_or_else(|| format!("route {position}"));

    let scope = route.scope.trim();
    if scope.is_empty() {
        return Err(Error::Config(format!(
            "mapping `{label}`: the route for {named} names no scope"
        )));
    }
    let scope = match parse_connector_ref(scope) {
        Ok((connector, inner)) if connector == *sink => inner,
        Ok((connector, _)) => {
            return Err(Error::Config(format!(
                "mapping `{label}`: the route for {named} names a scope on connector `{connector}`, but this mapping writes to `{sink}`; a route may only name a scope on the mapping's sink connector"
            )))
        }
        Err(_) if scope.contains(':') => {
            return Err(Error::Config(format!(
                "mapping `{label}`: the route for {named} has scope `{scope}`, which is not a valid scope"
            )))
        }
        Err(_) => scope.to_string(),
    };
    Ok(Route {
        project,
        issue,
        label: route_label,
        scope,
    })
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
    /// `[[mapping.route]]`: a project, and the sink scope its entities live in.
    #[serde(default)]
    route: Vec<RouteSection>,
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

fn default_true() -> bool {
    true
}

/// One `[[mapping.route]]`: what it matches, and the sink scope its mirror lives in.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RouteSection {
    /// A project by its id, slug or name. Matches the project and its issues.
    #[serde(default)]
    project: Option<String>,
    /// One issue by its source identifier (e.g. `VED-119`).
    #[serde(default)]
    issue: Option<String>,
    /// Any issue carrying this label.
    #[serde(default)]
    label: Option<String>,
    /// The sink scope - a repository, a board - this route's entities live in.
    scope: String,
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
mod tests {
    use super::*;

    #[test]
    fn a_tilde_is_expanded_from_whichever_home_this_platform_names() {
        assert_eq!(
            expand_tilde_with("~/x/bridge.db", Some("/home/dev")),
            "/home/dev/x/bridge.db"
        );
        // Windows names the same thing USERPROFILE, and a joined path is absolute there
        // whatever separator the join used.
        assert_eq!(
            expand_tilde_with("~/x/bridge.db", Some(r"C:\Users\dev")),
            r"C:\Users\dev/x/bridge.db"
        );
        // Nothing to expand against: the operator's path, left exactly as it is.
        assert_eq!(expand_tilde_with("~/x/bridge.db", None), "~/x/bridge.db");
        assert_eq!(
            expand_tilde_with("/already/absolute.db", Some("/home/dev")),
            "/already/absolute.db"
        );
    }

    fn document(extra: &str) -> String {
        format!(
            r#"
api_key = "lin_api_ignored_by_the_service"
team_id = "VED"

[bridge]
bind = "127.0.0.1:9999"
store = "/tmp/bridge.db"

[platform.forgejo]
type = "forgejo"
secret_env = "BRIDGE_TEST_SECRET"
token_env = "BRIDGE_TEST_TOKEN"
closed_state = ["closed"]
open_state = "open"

[platform.linear]
type = "linear"
secret = "0123456789abcdef"
token_env = "BRIDGE_TEST_TOKEN"
closed_state = ["Done", "Canceled"]
open_state = "In Progress"
initial_state = "Todo"

[[mapping]]
name = "linear-cli-rs"
source = "linear:VED"
sink = "forgejo:Vedaru/linear-cli-rs"
{extra}
"#
        )
    }

    /// The document without the mapping, for the cases that publish their own.
    #[test]
    fn an_identity_map_reaches_the_reconciler() {
        let config = parse(&document(
            "\n[[mapping.identity]]\nlinear = \"loner@example.com\"\nforgejo = \"vedaru\"\n",
        ))
        .expect("the config loads");

        let mappings = config.reconcile_mappings().expect("the mapping resolves");
        let users = &mappings[0].users;
        assert_eq!(users.len(), 1);
        let on_forge = users
            .counterpart_for(
                &Identity::new("linear", "loner@example.com"),
                &ConnectorId::new("forgejo"),
            )
            .expect("known on the forge");
        assert_eq!(on_forge.key, "vedaru");
        assert_eq!(users.describe().len(), 1);
    }

    #[test]
    fn an_identity_for_one_platform_only_is_refused() {
        // It would match nothing and skip every assignee in silence, so it is a load
        // error rather than a half-known person.
        // The document loads; it is resolving the mapping that refuses, which is
        // where every other mapping-shaped error is caught.
        let config = parse(&document(
            "\n[[mapping.identity]]\nlinear = \"loner@example.com\"\n",
        ))
        .expect("the document itself is valid");
        let error = config
            .reconcile_mappings()
            .expect_err("a one-platform identity is not an identity");
        let message = error.to_string();
        assert!(message.contains("at least two"), "{message}");
    }

    #[test]
    fn an_identity_naming_platforms_this_mapping_does_not_connect_is_refused() {
        let config = parse(&document(
            "\n[[mapping.identity]]\ncodeberg = \"vedaru\"\ngitea = \"vedaru\"\n",
        ))
        .expect("the document itself is valid");
        let error = config
            .reconcile_mappings()
            .expect_err("no identity names either end");
        let message = error.to_string();
        assert!(message.contains("no identity names"), "{message}");
        assert!(message.contains("linear-cli-rs"), "{message}");
    }

    fn document_without_mapping() -> String {
        let text = document("");
        let start = text.find("[[mapping]]").expect("the fixture has a mapping");
        text[..start].to_string()
    }

    /// Parses with a deterministic environment.
    ///
    /// Deliberately *not* the process environment: tests run in parallel inside
    /// one process, so a case that sets a variable for its own benefit is a case
    /// that breaks every other test running at the same moment.
    fn parse(text: &str) -> Result<BridgeConfig> {
        BridgeConfig::from_toml_with_env(text, &env)
    }

    fn env(name: &str) -> Option<String> {
        match name {
            "BRIDGE_TEST_SECRET" => Some("0123456789abcdef".to_string()),
            "BRIDGE_TEST_TOKEN" => Some("gto_0123456789abcdef".to_string()),
            _ => None,
        }
    }

    #[test]
    fn parses_the_service_sections_and_ignores_the_cli_ones() {
        let config = parse(&document("")).expect("valid document");
        assert_eq!(config.bind.to_string(), "127.0.0.1:9999");
        assert_eq!(config.store_path, "/tmp/bridge.db");
        assert_eq!(config.platforms.len(), 2);
        assert_eq!(config.mappings.len(), 1);
        assert_eq!(config.mappings[0].source, "linear:VED");
        assert!(config.mappings[0].sync_issues);
        assert!(!config.mappings[0].delete_sync);
        assert_eq!(config.sources().len(), 2);
    }

    #[test]
    fn a_preset_type_loads_the_preset_spec() {
        let config = parse(&document("")).unwrap();
        let forgejo = config
            .platforms
            .iter()
            .find(|platform| platform.name.as_str() == "forgejo")
            .unwrap();
        assert_eq!(forgejo.declared_type, "forgejo");
        // The forgejo preset's signature headers are the ones the forge sends.
        assert_eq!(
            forgejo.spec.signature.headers,
            vec!["x-forgejo-signature", "x-gitea-signature"]
        );
    }

    #[test]
    fn the_state_vocabulary_and_the_direction_come_from_the_config() {
        let config = parse(&document("direction = \"oneway\"")).unwrap();

        let linear = config
            .platforms
            .iter()
            .find(|platform| platform.name.as_str() == "linear")
            .unwrap();
        assert_eq!(linear.states.closed, vec!["Done", "Canceled"]);
        assert_eq!(linear.states.initial.as_deref(), Some("Todo"));
        assert_eq!(linear.states.open.as_deref(), Some("In Progress"));

        let forgejo = config
            .platforms
            .iter()
            .find(|platform| platform.name.as_str() == "forgejo")
            .unwrap();
        assert_eq!(forgejo.states.closed, vec!["closed"]);
        assert_eq!(forgejo.states.initial, None);

        assert_eq!(config.mappings[0].direction, Direction::SourceToSink);
        assert_eq!(
            config.mappings[0].policy(linear, forgejo).direction,
            Direction::SourceToSink
        );
    }

    #[test]
    fn a_direction_nobody_implements_is_refused_by_name() {
        let error = parse(&document("direction = \"sideways\""))
            .unwrap_err()
            .to_string();
        assert!(error.contains("sideways"), "{error}");
        assert!(error.contains("oneway"), "{error}");
    }

    #[test]
    fn both_ends_of_a_mapping_need_a_credential() {
        // The forgejo platform loses its token, and only its own line is touched:
        // the mapping cannot read it, so it cannot be carried out.
        let text = document("").replace(
            "secret_env = \"BRIDGE_TEST_SECRET\"\ntoken_env = \"BRIDGE_TEST_TOKEN\"",
            "secret_env = \"BRIDGE_TEST_SECRET\"",
        );
        let error = parse(&text).unwrap_err().to_string();
        assert!(error.contains("forgejo"), "{error}");
        assert!(error.contains("token_env"), "{error}");
        assert!(
            error.contains("webhook secret"),
            "the message distinguishes the two credentials: {error}"
        );
    }

    #[test]
    fn a_platform_without_a_token_is_a_source_only() {
        // Intake needs no credential, so a read-only platform is a valid deployment;
        // it simply cannot take part in a mapping.
        let config = parse(&document_without_mapping()).unwrap();
        assert_eq!(config.sources().len(), 2, "both still accept webhooks");
        assert_eq!(config.sinks().len(), 2, "and both have a token here");

        let text = document_without_mapping().replace(
            "[platform.linear]\ntype = \"linear\"\nsecret = \"0123456789abcdef\"\ntoken_env = \"BRIDGE_TEST_TOKEN\"",
            "[platform.linear]\ntype = \"linear\"\nsecret = \"0123456789abcdef\"",
        );
        let config = parse(&text).unwrap();
        assert_eq!(config.sinks().len(), 1, "only the forge has a credential");
    }

    #[test]
    fn an_unset_token_variable_is_a_startup_error_naming_it() {
        let error = BridgeConfig::from_toml_with_env(&document(""), &|name| {
            (name == "BRIDGE_TEST_SECRET").then(|| "0123456789abcdef".to_string())
        })
        .unwrap_err()
        .to_string();
        assert!(error.contains("BRIDGE_TEST_TOKEN"), "{error}");
        assert!(error.contains("not set"), "{error}");
    }

    #[test]
    fn a_platform_may_point_at_another_api_address() {
        let text = document("").replace(
            "[platform.forgejo]",
            "[platform.forgejo]\napi_url = \"http://127.0.0.1:4000/api/v1\"",
        );
        let config = parse(&text).unwrap();
        let forgejo = config
            .platforms
            .iter()
            .find(|platform| platform.name.as_str() == "forgejo")
            .unwrap();
        // The preset's own default is the real address; the deployment's override
        // replaces it, and the rest of the write half is untouched.
        assert_eq!(
            forgejo.sink_spec().unwrap().base_url,
            "http://127.0.0.1:4000/api/v1"
        );
        assert!(forgejo.sink_spec().unwrap().issue.create.is_some());
        assert!(forgejo.can_be_written());
    }

    #[test]
    fn the_mappings_resolve_into_the_reconcilers_terms() {
        let config = parse(&document("")).unwrap();
        let mappings = config.reconcile_mappings().unwrap();

        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].source.describe(), "linear:VED");
        assert_eq!(mappings[0].sink.describe(), "forgejo:Vedaru/linear-cli-rs");
        // The policy carries the two vocabularies, which is what lets the reconciler
        // compare `Done` with `closed` without either platform knowing the other.
        assert_eq!(
            mappings[0].policy.names.source.closed,
            vec!["Done", "Canceled"]
        );
        assert_eq!(mappings[0].policy.names.sink.closed, vec!["closed"]);
        assert_eq!(mappings[0].policy.names.sink.initial, None);
    }

    #[test]
    fn a_generic_platform_can_describe_itself_in_the_config() {
        let text = document(
            r#"
[platform.internal]
type = "generic"
secret_env = "BRIDGE_TEST_SECRET"
[platform.internal.spec.signature]
headers = ["x-signature"]
algorithm = "token"
[platform.internal.spec.event]
headers = ["x-event"]
[[platform.internal.spec.event.rule]]
match = "ticket"
kind = "issue"
[platform.internal.spec.event.rule.fields]
id = "/ticket/id"
"#,
        );
        let config = parse(&text).unwrap();
        let internal = config
            .platforms
            .iter()
            .find(|platform| platform.name.as_str() == "internal")
            .unwrap();
        assert_eq!(
            internal.spec.signature.algorithm,
            crate::connector::Algorithm::Token
        );
        assert_eq!(internal.spec.event.rules.len(), 1);
        // And it is a usable source, built by the same engine as the presets.
        assert_eq!(config.sources().len(), 3);
    }

    #[test]
    fn defaults_are_applied_when_the_bridge_section_is_minimal() {
        let text = document("").replace("bind = \"127.0.0.1:9999\"\n", "");
        let config = parse(&text).unwrap();
        assert_eq!(config.bind.to_string(), DEFAULT_BIND);
        assert_eq!(config.body_limit, DEFAULT_BODY_LIMIT);
        assert_eq!(
            config.worker.max_attempts,
            WorkerConfig::default().max_attempts
        );
    }

    #[test]
    fn an_unknown_platform_type_names_the_alternatives() {
        let text = document("").replace("type = \"forgejo\"", "type = \"bitbucket\"");
        let error = parse(&text).unwrap_err().to_string();
        assert!(error.contains("unknown platform type"), "{error}");
        assert!(error.contains("linear"), "{error}");
    }

    #[test]
    fn a_missing_secret_is_a_startup_error_not_a_runtime_one() {
        // An empty environment: the point of the case is that a referenced but
        // unset variable fails at startup, naming the variable.
        let error = BridgeConfig::from_toml_with_env(&document(""), &|_| None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("BRIDGE_TEST_SECRET"), "{error}");
        assert!(error.contains("not set"), "{error}");
    }

    #[test]
    fn a_short_secret_is_refused() {
        let text = document("").replace("secret = \"0123456789abcdef\"", "secret = \"short\"");
        let error = parse(&text).unwrap_err().to_string();
        assert!(error.contains("at least 16"), "{error}");
    }

    /// Why a config was refused as a webhook receiver, or a panic saying it was not.
    fn source_refusal(config: &BridgeConfig) -> String {
        match config.receiving_sources() {
            Ok(sources) => panic!("a config with no secret built {} sources", sources.len()),
            Err(error) => error.to_string(),
        }
    }

    /// Strip the secret lines from the fixture, leaving its credentials.
    fn cli_only(document: &str) -> String {
        document
            .lines()
            .filter(|line| !line.starts_with("secret"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_config_with_no_secrets_is_valid_for_a_sweep() {
        // A deployment that only pushes changes has no use for a webhook secret: no
        // service, nothing to verify a delivery against, nothing to configure. The
        // file has to load, and its mappings have to resolve, with none.
        let config = parse(&cli_only(&document(""))).expect("a CLI-only config loads");

        assert_eq!(
            config.sinks().len(),
            2,
            "credentials are everything the write path needs"
        );
        assert_eq!(
            config
                .reconcile_mappings()
                .expect("the mapping resolves")
                .len(),
            1
        );
    }

    #[test]
    fn asking_a_cli_only_config_to_receive_names_the_platform() {
        // The requirement lives where it belongs - building a webhook source, which is
        // what `webhook serve` does and `linear sync` does not.
        let both = parse(&cli_only(&document(""))).expect("the config loads");
        let error = source_refusal(&both);
        assert!(error.contains("[platform.forgejo]"), "{error}");
        assert!(error.contains("no webhook secret"), "{error}");
        assert!(error.contains("secret_env"), "the remedy: {error}");
        assert!(
            error.contains("linear sync"),
            "and the way that needs no secret: {error}"
        );

        // Per platform, not per file: the one that is missing a secret is the one named.
        let one = parse(&document("").replace("secret = \"0123456789abcdef\"\n", ""))
            .expect("the config loads");
        let error = source_refusal(&one);
        assert!(error.contains("[platform.linear]"), "{error}");
        assert!(!error.contains("[platform.forgejo]"), "{error}");
    }

    #[test]
    fn a_mapping_naming_an_undeclared_platform_is_refused() {
        let text = document("").replace(
            "sink = \"forgejo:Vedaru/linear-cli-rs\"",
            "sink = \"codeberg:o/r\"",
        );
        let error = parse(&text).unwrap_err().to_string();
        assert!(error.contains("not declared"), "{error}");
    }

    #[test]
    fn a_mapping_without_a_scope_is_refused() {
        let text = document("").replace("source = \"linear:VED\"", "source = \"linear\"");
        let error = parse(&text).unwrap_err().to_string();
        assert!(error.contains("connector:scope"), "{error}");
    }

    #[test]
    fn a_config_with_no_platform_is_refused() {
        let text = r#"
[bridge]
bind = "127.0.0.1:1"
"#;
        let error = parse(text).unwrap_err().to_string();
        assert!(error.contains("nothing to accept"), "{error}");
    }

    #[test]
    fn an_invalid_bind_names_the_value() {
        let text = document("").replace("127.0.0.1:9999", "not-an-address");
        let error = parse(&text).unwrap_err().to_string();
        assert!(error.contains("not-an-address"), "{error}");
    }

    #[test]
    fn an_unknown_key_inside_a_section_is_refused_rather_than_ignored() {
        let text = document("").replace(
            "[platform.forgejo]",
            "[platform.forgejo]\nunexpected = true",
        );
        let error = parse(&text).unwrap_err().to_string();
        assert!(error.contains("unexpected"), "{error}");
    }

    #[test]
    fn a_generic_platform_without_a_spec_is_refused() {
        let text = document(
            r#"
[platform.thing]
type = "generic"
secret_env = "BRIDGE_TEST_SECRET"
"#,
        );
        let error = parse(&text).unwrap_err().to_string();
        assert!(error.contains("no spec"), "{error}");
    }

    #[test]
    fn a_preset_with_an_inline_spec_is_refused_as_ambiguous() {
        let text = document(
            r#"
[platform.linear.spec.signature]
headers = ["x"]
"#,
        );
        let error = parse(&text).unwrap_err().to_string();
        assert!(error.contains("also declares a spec"), "{error}");
    }

    #[test]
    fn a_broken_inline_spec_fails_at_startup_with_its_reason() {
        let text = document(
            r#"
[platform.internal]
type = "generic"
secret_env = "BRIDGE_TEST_SECRET"
[platform.internal.spec.signature]
headers = ["x"]
[platform.internal.spec.event]
headers = ["x-event"]
[[platform.internal.spec.event.rule]]
match = "ticket"
kind = "ticketing"
[platform.internal.spec.event.rule.fields]
id = "/id"
"#,
        );
        let error = parse(&text).unwrap_err().to_string();
        assert!(error.contains("unknown kind"), "{error}");
        assert!(error.contains("internal"), "{error}");
    }

    #[test]
    fn a_zero_thread_count_is_clamped_rather_than_fatal() {
        let text =
            document("").replace("[bridge]", "[bridge]\nhttp_threads = 0\nworker_threads = 0");
        let config = parse(&text).unwrap();
        assert_eq!(config.http_threads, 1);
        assert_eq!(config.worker_threads, 1);
    }

    #[test]
    fn the_lease_never_undercuts_the_backoff_window() {
        let text = document("").replace(
            "[platform.forgejo]",
            "[bridge.worker]\nbackoff_max_ms = 600000\n\n[platform.forgejo]",
        );
        let config = parse(&text).unwrap();
        assert!(config.worker.lease > config.worker.backoff_max);
    }

    #[test]
    fn project_mirroring_is_off_unless_a_mapping_asks_for_it() {
        // The switch is opt-in: an existing config mirrors issues only, and a Project
        // event stays inert exactly as it did before projects were modelled.
        let config = parse(&document("")).expect("the document loads");
        assert!(
            !config.mappings[0].sync_projects,
            "projects must be off by default"
        );

        let config = parse(&document("sync_projects = true")).expect("the document loads");
        assert!(config.mappings[0].sync_projects);
        let linear = config
            .platforms
            .iter()
            .find(|platform| platform.name.as_str() == "linear")
            .unwrap();
        let forgejo = config
            .platforms
            .iter()
            .find(|platform| platform.name.as_str() == "forgejo")
            .unwrap();
        // And it reaches the policy the reconciler runs with, the same way the issue
        // switch does.
        assert!(config.mappings[0].policy(linear, forgejo).sync_projects);
    }

    #[test]
    fn routes_reach_the_reconciler() {
        let config = parse(&document(
            "\n[[mapping.route]]\nproject = \"project-uuid\"\nscope = \"Vedaru/kuro\"\n\
             \n[[mapping.route]]\nissue = \"VED-119\"\nscope = \"Vedaru/one-off\"\n\
             \n[[mapping.route]]\nlabel = \"urgent\"\nscope = \"Vedaru/hotfix\"\n",
        ))
        .expect("the config loads");

        let routes = &config.mappings[0].route;
        assert_eq!(routes.len(), 3);
        assert_eq!(routes[0].project.as_deref(), Some("project-uuid"));
        assert_eq!(routes[0].scope, "Vedaru/kuro");
        assert_eq!(routes[1].issue.as_deref(), Some("VED-119"));
        assert_eq!(routes[2].label.as_deref(), Some("urgent"));

        // And it reaches the mapping the reconciler runs with.
        let mappings = config.reconcile_mappings().expect("the mapping resolves");
        let routes = &mappings[0].routes;
        assert_eq!(routes.len(), 3);
        assert_eq!(
            routes.scopes(),
            vec!["Vedaru/kuro", "Vedaru/one-off", "Vedaru/hotfix"]
        );
    }

    #[test]
    fn a_route_naming_a_scope_on_another_connector_is_refused() {
        let error = parse(&document(
            "\n[[mapping.route]]\nproject = \"project-uuid\"\nscope = \"linear:VED\"\n",
        ))
        .expect_err("a route may only name a scope on the mapping's sink")
        .to_string();
        assert!(error.contains("connector `linear`"), "{error}");
        assert!(error.contains("writes to `forgejo`"), "{error}");
    }

    #[test]
    fn a_route_with_nothing_to_match_on_is_refused() {
        let error = parse(&document("\n[[mapping.route]]\nscope = \"Vedaru/kuro\"\n"))
            .expect_err("a route needs a key")
            .to_string();
        assert!(
            error.contains("no `project`, `issue` or `label`"),
            "{error}"
        );
    }

    #[test]
    fn a_route_may_repeat_the_mappings_own_sink_connector() {
        let config = parse(&document(
            "\n[[mapping.route]]\nproject = \"project-uuid\"\nscope = \"forgejo:Vedaru/kuro\"\n",
        ))
        .expect("the document loads");
        // The prefix is the mapping's own sink; it is accepted and stripped.
        assert_eq!(config.mappings[0].route[0].scope, "Vedaru/kuro");
    }

    #[test]
    fn a_route_may_name_every_key_at_once() {
        let config = parse(&document(
            "\n[[mapping.route]]\nproject = \"p\"\nissue = \"VED-1\"\nlabel = \"urgent\"\nscope = \"Vedaru/kuro\"\n",
        ))
        .expect("the config loads");
        let route = &config.mappings[0].route[0];
        assert_eq!(route.project.as_deref(), Some("p"));
        assert_eq!(route.issue.as_deref(), Some("VED-1"));
        assert_eq!(route.label.as_deref(), Some("urgent"));
    }

    #[test]
    fn a_relative_url_shape_follows_the_deployment_rather_than_the_preset() {
        // A shipped preset must not name one deployment's host. `/{scope}` is resolved
        // against the connector's own address, so the same preset serves any instance -
        // and a link to whatever host the preset's default happens to name is no longer
        // this deployment's repository.
        let text = document("").replace(
            "[platform.forgejo]",
            "[platform.forgejo]\napi_url = \"http://127.0.0.1:4000/api/v1\"",
        );
        let config = parse(&text).expect("the config loads");
        let mappings = config.reconcile_mappings().expect("the mapping resolves");
        let location = mappings[0]
            .sink_location
            .as_ref()
            .expect("forgejo declares its URL shape");
        assert_eq!(
            location.scope("http://127.0.0.1:4000/Vedaru/kuro"),
            Some("Vedaru/kuro")
        );
        assert_eq!(location.scope("https://git.vedaru.cn/Vedaru/kuro"), None);
    }

    #[test]
    fn a_presets_url_shape_reaches_the_mapping() {
        // The forgejo preset declares how its own web URLs name a repository, so a
        // project linking to one is routed there without an explicit route. The shape is
        // relative, so it resolves against the address this deployment talks to - here the
        // preset's own default.
        let config = parse(&document("")).expect("the config loads");
        let mappings = config.reconcile_mappings().expect("the mapping resolves");
        let location = mappings[0]
            .sink_location
            .as_ref()
            .expect("forgejo declares its URL shape");
        assert_eq!(
            location.scope("http://127.0.0.1:3000/Vedaru/kuro"),
            Some("Vedaru/kuro")
        );
        assert_eq!(
            location.scope("https://github.com/h-paetzold/linforge"),
            None
        );
    }
}
