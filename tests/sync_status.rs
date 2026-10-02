//! `linear sync status`: the command that says whether a quiet service is idle or stuck.
//!
//! A service that is up but backed up and one that is doing nothing answer a webhook the same
//! way and neither writes anything, so the difference has to be visible somewhere. This is it.

#![cfg(feature = "service")]

mod common;

use std::path::{Path, PathBuf};

use common::run_cli;

/// A config pointing at `store`. The file is deliberately not created: that is what a status on
/// a fresh deployment looks like, and the answer should be "nothing has happened".
fn config_with_store(dir: &Path, store: &Path) -> PathBuf {
    let path = dir.join("linear.toml");
    std::fs::write(
        &path,
        format!(
            r#"[bridge]
bind = "127.0.0.1:0"
store = "{}"

[platform.linear]
type = "linear"
token = "lin_api_a-token-long-enough-to-be-one"

[platform.forgejo]
type = "forgejo"
token = "test-token-long-enough-to-be-a-credential"
api_url = "http://127.0.0.1:1/api/v1"

[[mapping]]
name = "sync"
source = "linear:VED"
sink = "forgejo:Vedaru/linear-cli-rs"
"#,
            store.display()
        ),
    )
    .unwrap();
    path
}

fn config(dir: &Path) -> PathBuf {
    config_with_store(dir, &dir.join("bridge.db"))
}

#[test]
fn a_quiet_store_is_reported_as_quiet() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());

    let out = run_cli(
        &["sync", "status", "--config", config.to_str().unwrap()],
        &[],
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout
            .contains("queue:    0 pending, 0 active, 0 done, 0 dead"),
        "the queue it reports: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("mappings: sync"),
        "and the mappings it would run: {}",
        out.stdout
    );
    assert!(
        !out.stdout.contains("dead ("),
        "a store with nothing dead says nothing about death: {}",
        out.stdout
    );
}

#[test]
fn the_json_form_carries_the_same_numbers() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());

    let out = run_cli(
        &[
            "sync",
            "status",
            "--config",
            config.to_str().unwrap(),
            "--json",
        ],
        &[],
    );

    assert!(out.success(), "stderr: {}", out.stderr);
    let parsed: serde_json::Value = serde_json::from_str(&out.stdout).expect("valid json");
    assert_eq!(parsed["queue"]["pending"], 0);
    assert_eq!(parsed["queue"]["dead"], 0);
    assert_eq!(parsed["mappings"][0], "sync");
    assert!(
        parsed["store"].as_str().unwrap().ends_with("bridge.db"),
        "the store it looked at: {}",
        parsed["store"]
    );
}

#[test]
fn a_store_that_cannot_be_read_fails_instead_of_reporting_zero() {
    // The failure this command exists to prevent: a status that cannot see the store answering
    // "0 pending, 0 dead" and reading like a healthy system.
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("a-directory-where-a-database-should-be");
    std::fs::create_dir(&store).unwrap();
    let config = config_with_store(dir.path(), &store);

    let out = run_cli(
        &["sync", "status", "--config", config.to_str().unwrap()],
        &[],
    );

    assert!(
        !out.success(),
        "a store that cannot be read is not a quiet store: {}",
        out.stdout
    );
    assert!(
        !out.stdout.contains("0 pending"),
        "and it must not report counts it never read: {}",
        out.stdout
    );
}
