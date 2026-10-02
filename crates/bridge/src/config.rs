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
//! secret_env = "FORGEJO_WEBHOOK_SECRET"
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
use crate::domain::{parse_connector_ref, ConnectorId, Secret};
use crate::error::{Error, Result};
use crate::queue::WorkerConfig;
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
    pub secret: Secret,
    pub spec: SourceSpec,
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
    pub sync_issues: bool,
    pub git_automation: bool,
    pub delete_sync: bool,
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
            store_path: bridge
                .store
                .unwrap_or_else(|| "linear-bridge.db".to_string()),
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
    pub fn sources(&self) -> Result<Vec<Arc<dyn Source>>> {
        Ok(self
            .platforms
            .iter()
            .map(|platform| {
                Arc::new(DeclarativeSource::new(
                    platform.name.clone(),
                    platform.secret.clone(),
                    platform.spec.clone(),
                )) as Arc<dyn Source>
            })
            .collect())
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
            let spec = resolve_spec(&name, &section)?;
            Ok(PlatformConfig {
                name: ConnectorId::new(name),
                declared_type: section.kind,
                secret,
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
) -> Result<Secret> {
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
        (None, None) => {
            return Err(Error::Config(format!(
                "[platform.{name}] has no webhook secret; set `secret_env`"
            )))
        }
    };

    if secret.len() < MIN_SECRET_LEN {
        return Err(Error::Config(format!(
            "[platform.{name}] secret is {} characters; the webhook endpoint is the only unauthenticated surface, so at least {MIN_SECRET_LEN} are required",
            secret.len()
        )));
    }
    Ok(secret)
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
            for reference in [&section.source, &section.sink] {
                let (connector, _) = parse_connector_ref(reference)
                    .map_err(|error| Error::Config(format!("mapping `{label}`: {error}")))?;
                if !known.contains(&connector.as_str()) {
                    return Err(Error::Config(format!(
                        "mapping `{label}` names platform `{connector}`, which is not declared (declared: {})",
                        known.join(", ")
                    )));
                }
            }
            Ok(MappingConfig {
                name: section.name,
                source: section.source,
                sink: section.sink,
                sync_issues: section.sync_issues,
                git_automation: section.git_automation,
                delete_sync: section.delete_sync,
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
    #[serde(default = "default_true")]
    sync_issues: bool,
    #[serde(default = "default_true")]
    git_automation: bool,
    #[serde(default)]
    delete_sync: bool,
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

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

[platform.linear]
type = "linear"
secret = "0123456789abcdef"

[[mapping]]
name = "linear-cli-rs"
source = "linear:VED"
sink = "forgejo:Vedaru/linear-cli-rs"
{extra}
"#
        )
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
        (name == "BRIDGE_TEST_SECRET").then(|| "0123456789abcdef".to_string())
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
        assert_eq!(config.sources().unwrap().len(), 2);
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
        assert_eq!(config.sources().unwrap().len(), 3);
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

    #[test]
    fn a_mapping_naming_an_undeclared_platform_is_refused() {
        let text = document("").replace(
            "sink = \"forgejo:Vedaru/linear-cli-rs\"",
            "sink = \"github:o/r\"",
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
}
