//! `linear webhook serve`: the configuration half of the service.
//!
//! The reconciler itself is tested in `crates/bridge/tests/reconcile.rs`, against
//! two fake platforms and a real store. What can only be tested here is the join:
//! that a `linear.toml` turns into a service that would actually run - the sinks
//! built from the platforms' credentials, and the mappings resolved with the state
//! vocabularies the two platforms declared.

#![cfg(feature = "service")]

mod common;

use std::io::Write;

use common::{run_cli_full, CliOutput};

/// A config file in a temporary directory, removed when the guard drops.
struct Config(std::path::PathBuf);

impl Config {
    fn new(tag: &str, body: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "linear-serve-{}-{}-{}.toml",
            std::process::id(),
            tag,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("a sane clock")
                .as_nanos()
        ));
        let mut file = std::fs::File::create(&path).expect("create the config");
        file.write_all(body.as_bytes()).expect("write the config");
        Self(path)
    }

    fn path(&self) -> String {
        self.0.to_string_lossy().into_owned()
    }
}

impl Drop for Config {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// The environment a service run needs, with the ambient config kept out of it:
/// these assertions are about the file under test, not about the developer's own
/// `~/.config/linear/`.
fn service_env() -> (Vec<(String, String)>, Vec<&'static str>) {
    (
        vec![
            ("LINEAR_IGNORE_ENV_FILE".to_string(), "1".to_string()),
            (
                "FORGEJO_WEBHOOK_SECRET".to_string(),
                "0123456789abcdef".to_string(),
            ),
            (
                "LINEAR_WEBHOOK_SECRET".to_string(),
                "fedcba9876543210".to_string(),
            ),
            (
                "FORGEJO_TOKEN".to_string(),
                "gto_0123456789abcdef".to_string(),
            ),
            (
                "LINEAR_API_KEY".to_string(),
                "lin_api_0123456789abcdef".to_string(),
            ),
        ],
        vec!["XDG_CONFIG_HOME", "LINEAR_TEAM_ID"],
    )
}

fn check(config: &Config, env: &[(String, String)], remove: &[&str]) -> CliOutput {
    run_cli_full(
        &["webhook", "serve", "--check", "--config", &config.path()],
        env,
        remove,
        None,
    )
}

const DOCUMENT: &str = r#"
[bridge]
bind = "127.0.0.1:8791"
store = "/tmp/linear-serve-test.db"

[platform.forgejo]
type = "forgejo"
secret_env = "FORGEJO_WEBHOOK_SECRET"
token_env = "FORGEJO_TOKEN"
closed_state = ["closed"]
open_state = "open"

[platform.linear]
type = "linear"
secret_env = "LINEAR_WEBHOOK_SECRET"
token_env = "LINEAR_API_KEY"
closed_state = ["Done", "Canceled"]
open_state = "In Progress"
initial_state = "Todo"

[[mapping]]
name = "linear-cli-rs"
source = "linear:VED"
sink = "forgejo:Vedaru/linear-cli-rs"
"#;

#[test]
fn check_resolves_a_config_into_a_runnable_mirror() {
    let config = Config::new("resolved", DOCUMENT);
    let (env, remove) = service_env();
    let output = check(&config, &env, &remove);

    assert_eq!(output.code, Some(0), "stderr: {}", output.stderr);
    let json: serde_json::Value = serde_json::from_str(&output.stdout).expect("the output is JSON");

    // Both platforms can be read and written, which is what a mapping requires.
    assert_eq!(json["writable"], 2);
    // And each mapping is shown resolved: the endpoints it pairs, the direction,
    // and the vocabularies that let the reconciler compare `Done` with `closed`.
    let mapping = &json["resolved_mappings"][0];
    assert_eq!(mapping["source"], "linear:VED");
    assert_eq!(mapping["sink"], "forgejo:Vedaru/linear-cli-rs");
    assert_eq!(mapping["direction"], "both ways");
    assert_eq!(
        mapping["states"]["source_closed"],
        serde_json::json!(["Done", "Canceled"])
    );
    assert_eq!(
        mapping["states"]["sink_closed"],
        serde_json::json!(["closed"])
    );
    assert_eq!(
        mapping["states"]["initial_on_sink"],
        serde_json::Value::Null
    );

    // The endpoints the service will listen on are still reported.
    assert_eq!(
        json["endpoints"],
        serde_json::json!([
            "POST http://127.0.0.1:8791/webhooks/forgejo",
            "POST http://127.0.0.1:8791/webhooks/linear"
        ])
    );
}

#[test]
fn check_shows_the_platforms_write_credential_without_showing_it() {
    let config = Config::new("redaction", DOCUMENT);
    let (env, remove) = service_env();
    let output = check(&config, &env, &remove);

    let json: serde_json::Value = serde_json::from_str(&output.stdout).expect("JSON");
    assert_eq!(json["platforms"][0]["token"], "set");
    // The values never reach stdout, in either direction: the webhook secret and the
    // API token are both `Secret` values, and their Debug impl is what omits them.
    assert!(
        !output.stdout.contains("gto_0123456789abcdef"),
        "token leaked"
    );
    assert!(!output.stdout.contains("fedcba9876543210"), "secret leaked");
    assert!(output.stdout.contains("redacted"), "{}", output.stdout);
}

#[test]
fn a_mapping_whose_platform_has_no_credential_is_refused_at_startup() {
    let config = Config::new(
        "no-token",
        &DOCUMENT.replace(
            "secret_env = \"FORGEJO_WEBHOOK_SECRET\"\ntoken_env = \"FORGEJO_TOKEN\"",
            "secret_env = \"FORGEJO_WEBHOOK_SECRET\"",
        ),
    );
    let (env, remove) = service_env();
    let output = check(&config, &env, &remove);

    assert_ne!(output.code, Some(0), "it should fail: {}", output.stdout);
    let complaint = format!("{}{}", output.stdout, output.stderr);
    assert!(complaint.contains("forgejo"), "{complaint}");
    assert!(complaint.contains("token_env"), "{complaint}");
    assert!(
        complaint.contains("webhook secret"),
        "and it says which credential is which: {complaint}"
    );
}

#[test]
fn a_cli_only_config_is_refused_by_serve_and_names_the_platform() {
    // No webhook secrets at all: a deployment that only ever pushes with `sync`. The
    // credentials for the write path are all it has, and all it should need - but the
    // moment this file is handed to the service, the service says what is missing
    // rather than serving an endpoint that could not verify anything.
    let without_secrets: String = DOCUMENT
        .lines()
        .filter(|line| !line.starts_with("secret_env"))
        .collect::<Vec<_>>()
        .join("\n");
    let config = Config::new("cli-only", &without_secrets);
    let (env, remove) = service_env();
    let output = check(&config, &env, &remove);

    assert_ne!(
        output.code,
        Some(0),
        "the service must refuse a config it cannot receive with: {}",
        output.stdout
    );
    let complaint = format!("{}{}", output.stdout, output.stderr);
    // The first platform in the file is the one named; fixing it surfaces the next.
    assert!(complaint.contains("[platform.forgejo]"), "{complaint}");
    assert!(complaint.contains("no webhook secret"), "{complaint}");
    assert!(complaint.contains("secret_env"), "{complaint}");
    assert!(
        complaint.contains("linear sync"),
        "and it offers the way that needs no secret: {complaint}"
    );
}

#[test]
fn a_newly_created_issue_lands_in_the_state_the_platform_declares() {
    // The initial state is a fact about the *destination* platform, so it comes from
    // that platform's own section - and `--check` proves the reconciler received it.
    let config = Config::new(
        "initial",
        &DOCUMENT
            .replace("[[mapping]]", "[[mapping]]\ndirection = \"oneway\"")
            .replace(
                "[platform.forgejo]\ntype = \"forgejo\"",
                "[platform.forgejo]\ntype = \"forgejo\"\ninitial_state = \"open\"",
            ),
    );
    let (env, remove) = service_env();
    let output = check(&config, &env, &remove);

    let json: serde_json::Value = serde_json::from_str(&output.stdout).expect("JSON");
    let mapping = &json["resolved_mappings"][0];
    assert_eq!(mapping["direction"], "one way, source to sink");
    assert_eq!(mapping["states"]["initial_on_sink"], "open");
}

#[test]
fn a_direction_nobody_implements_is_refused_by_name() {
    let config = Config::new(
        "direction",
        &DOCUMENT.replace("[[mapping]]", "[[mapping]]\ndirection = \"sideways\""),
    );
    let (env, remove) = service_env();
    let output = check(&config, &env, &remove);

    assert_ne!(output.code, Some(0));
    let complaint = format!("{}{}", output.stdout, output.stderr);
    assert!(complaint.contains("sideways"), "{complaint}");
}

#[test]
fn a_config_without_mappings_is_an_intake_service() {
    let text = DOCUMENT[..DOCUMENT.find("[[mapping]]").expect("the fixture has one")].to_string();
    let config = Config::new("intake-only", &text);
    let (env, remove) = service_env();
    let output = check(&config, &env, &remove);

    assert_eq!(output.code, Some(0), "stderr: {}", output.stderr);
    let json: serde_json::Value = serde_json::from_str(&output.stdout).expect("JSON");
    assert_eq!(json["mappings"], serde_json::json!([]));
    assert_eq!(json["resolved_mappings"], serde_json::json!([]));
    // The webhook endpoints still exist: receiving events needs no mapping.
    assert_eq!(json["endpoints"].as_array().expect("endpoints").len(), 2);
}
