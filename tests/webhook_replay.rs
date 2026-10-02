//! `linear webhook replay`: the command that uses the bodies the store kept for it.
//!
//! The schema says why it keeps them - "so a handler can be re-run against exactly what the
//! provider sent, which is the only way to diagnose a sync bug after deploying a fix" - and
//! until now nothing ran one.

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::run_cli;
use linear_bridge::domain::{Action, ConnectorId, EntityKind};
use linear_bridge::store::sqlite::SqliteStore;
use linear_bridge::store::{NewDelivery, Store};

/// A forgejo delivery for a repository this deployment does not mirror. It parses, no mapping
/// claims it, and the handler therefore finishes - which is what makes this a delivery a replay
/// can *succeed* on without standing two fake platforms up.
const UNSYNCED_BODY: &str = r#"{
  "action": "opened",
  "number": 7,
  "issue": { "id": 7, "number": 7, "title": "T", "body": "B" },
  "repository": { "full_name": "nobody/else" }
}"#;

fn config(dir: &Path) -> (PathBuf, PathBuf) {
    let store = dir.join("bridge.db");
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
    (path, store)
}

/// A delivery the queue has already given up on, and its id.
fn a_dead_delivery(store_path: &Path) -> i64 {
    let mut store = SqliteStore::open(store_path).expect("a store");
    store.migrate().expect("migrated");
    store
        .insert_delivery(&NewDelivery {
            connector: ConnectorId::new("forgejo"),
            delivery_id: "replay-me".into(),
            event: "issues".into(),
            kind: EntityKind::Issue,
            action: Action::Created,
            scope: Some("nobody/else".into()),
            native_id: "7".into(),
            body: UNSYNCED_BODY.into(),
        })
        .expect("stored");
    let claimed = store.claim_due(1, Duration::from_secs(1)).expect("claimed");
    let id = claimed[0].id;
    // No retry left: the queue's decision, recorded the way a worker records it.
    store
        .fail(id, "HTTP 500 from the forge", None)
        .expect("parked");
    id
}

#[test]
fn replaying_a_dead_delivery_runs_it_and_takes_it_off_the_dead_list() {
    let dir = tempfile::tempdir().unwrap();
    let (config_path, store_path) = config(dir.path());
    let id = a_dead_delivery(&store_path);
    let config_arg = config_path.to_str().unwrap().to_string();

    // Before: the queue has given up, and the status says so - with the reason, which is the
    // whole point of printing them rather than counting them.
    let before = run_cli(&["sync", "status", "--config", &config_arg], &[]);
    assert!(before.success(), "stderr: {}", before.stderr);
    assert!(before.stdout.contains("1 dead"), "{}", before.stdout);
    assert!(
        before.stdout.contains("HTTP 500 from the forge"),
        "the reason it stopped: {}",
        before.stdout
    );

    // The replay runs it against the body the provider sent.
    let replayed = run_cli(
        &[
            "webhook",
            "replay",
            &id.to_string(),
            "--config",
            &config_arg,
        ],
        &[],
    );
    assert!(
        replayed.success(),
        "stdout: {} / stderr: {}",
        replayed.stdout,
        replayed.stderr
    );
    assert!(
        replayed.stdout.contains(&format!("replayed #{id}")),
        "{}",
        replayed.stdout
    );

    // After: it is not something the queue gave up on any more, and nothing else moved.
    let after = run_cli(&["sync", "status", "--config", &config_arg], &[]);
    assert!(after.success(), "stderr: {}", after.stderr);
    assert!(
        after.stdout.contains("0 dead"),
        "the delivery is filed, not still dead: {}",
        after.stdout
    );
    assert!(
        after.stdout.contains("1 done"),
        "and it is filed as done, which is what the worker would have recorded: {}",
        after.stdout
    );
}

#[test]
fn an_id_the_store_does_not_hold_is_refused_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let (config_path, _store) = config(dir.path());

    let out = run_cli(
        &[
            "webhook",
            "replay",
            "4242",
            "--config",
            config_path.to_str().unwrap(),
        ],
        &[],
    );

    assert!(!out.success(), "nothing to replay is a failure to replay");
    assert!(
        out.stderr.contains("holds no delivery #4242"),
        "and it names the store that was asked: {}",
        out.stderr
    );
}
