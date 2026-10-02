//! `linear config service`: the scaffold has to be a config the bridge can run.
//!
//! A template that only looks like a config is worse than no template: it fails at the first
//! command that tries it, which is after the operator has already committed to it. So the test
//! parses what the command printed - with the same parser the service uses - and then asks it to
//! reconcile, which is the step that proves the sections are not only well-formed but runnable.

mod common;

use common::run_cli;

#[test]
fn the_scaffold_printed_is_a_config_the_bridge_can_read() {
    // The config resolves `token_env` as it loads, so the variables have to exist for this to be
    // the real check. A dummy value is enough: nothing here talks to a platform, and what the
    // scaffold is *about* is the variable's name, never its value.
    std::env::set_var("LINEAR_API_KEY", "a-dummy-key-long-enough-to-be-one");
    std::env::set_var("FORGEJO_TOKEN", "a-dummy-token-long-enough-to-be-one");

    let out = run_cli(
        &[
            "config",
            "service",
            "--team",
            "VED",
            "--repo",
            "Vedaru/linear-cli-rs",
            "--forge",
            "https://git.example.com",
        ],
        &[],
    );
    assert!(out.success(), "stderr: {}", out.stderr);

    let config =
        linear_bridge::config::BridgeConfig::from_toml(&out.stdout).unwrap_or_else(|error| {
            panic!(
                "the scaffold should be a config the bridge reads: {error}\n{}",
                out.stdout
            )
        });
    let mappings = config
        .reconcile_mappings()
        .expect("and one it can run, not only one it can parse");
    assert_eq!(mappings.len(), 1);
    assert_eq!(mappings[0].source.connector.as_str(), "linear");
    assert_eq!(mappings[0].source.scope, "VED");
    assert_eq!(mappings[0].sink.scope, "Vedaru/linear-cli-rs");

    // The store path is expanded on the way in, so a config saying `~/…` does not make the
    // service create a directory literally called `~` beside wherever it started.
    assert!(
        config.store_path.starts_with('/'),
        "the store path should be absolute once the config has read it: {}",
        config.store_path
    );
    assert!(config.store_path.ends_with("bridge.db"));
}

#[test]
fn what_it_cannot_know_is_a_placeholder_rather_than_a_guess() {
    let out = run_cli(&["config", "service"], &[]);

    assert!(out.success(), "stderr: {}", out.stderr);
    // The team, because finding it needs the network and a generator that fails in a fresh
    // checkout is not a generator. The forge url, because there is nothing to read it from.
    assert!(out.stdout.contains("<TEAM>"), "{}", out.stdout);
    assert!(out.stdout.contains("<your forge>"), "{}", out.stdout);
    // The repository, deliberately *not* asserted: this test runs inside a git checkout, so the
    // command finds the remote and fills it in - which is the feature, not an accident.
    assert!(
        !out.stdout.contains("<owner>/<repo>") || out.stdout.contains("sink = \"forgejo:"),
        "a repository is either filled in or left obvious: {}",
        out.stdout
    );
    // And never a secret: the file names the variable, the environment holds the value.
    assert!(out.stdout.contains("token_env"), "{}", out.stdout);
    assert!(
        !out.stdout.contains("lin_api"),
        "nothing that looks like a key: {}",
        out.stdout
    );
}
