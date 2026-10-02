//! `linear sync link`: the one thing the engine cannot work out for itself.

#![cfg(feature = "service")]

mod common;

use std::path::{Path, PathBuf};

use common::run_cli;
use linear_bridge::domain::{ConnectorId, EntityKind, EntityRef};
use linear_bridge::store::sqlite::SqliteStore;
use linear_bridge::store::Store;

fn config(dir: &Path) -> PathBuf {
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
            // TOML reads `\` in a basic string as an escape, so a Windows path has
            // to be written with `/` - which Windows accepts - or it is a parse
            // error, not a path.
            dir.join("bridge.db").to_string_lossy().replace('\\', "/")
        ),
    )
    .unwrap();
    path
}

fn entity(connector: &str, scope: &str, id: &str) -> EntityRef {
    EntityRef::new(ConnectorId::new(connector), EntityKind::Issue, id).with_scope(scope)
}

#[test]
fn linking_two_entities_records_the_pairing_without_claiming_a_revision() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = config(dir.path());
    let store_path = dir.path().join("bridge.db");

    let out = run_cli(
        &[
            "sync",
            "link",
            "linear:VED#a-uuid",
            "forgejo:Vedaru/linear-cli-rs#7",
            "--config",
            config_path.to_str().unwrap(),
        ],
        &[],
    );

    assert!(
        out.success(),
        "stdout: {} stderr: {}",
        out.stdout,
        out.stderr
    );
    assert!(
        out.stdout
            .contains("linked linear:VED#a-uuid to forgejo:Vedaru/linear-cli-rs#7"),
        "{}",
        out.stdout
    );
    // The operator is told which side the sweep will keep, because that is the decision this
    // command is quietly making.
    assert!(
        out.stdout.contains("keeps linear:VED#a-uuid"),
        "the mapping's source side wins: {}",
        out.stdout
    );

    // And in the store: the link is there, with **no** recorded revision - which is what makes
    // the sweep treat the pair as adopted rather than as one it wrote across itself.
    let mut store = SqliteStore::open(&store_path).expect("the store the command wrote");
    store.migrate().expect("migrated");
    let left = entity("linear", "VED", "a-uuid");
    let link = store
        .find_link(&left, &ConnectorId::new("forgejo"))
        .expect("readable")
        .expect("the pairing the command recorded");
    assert_eq!(
        link.last_synced_hash, None,
        "nothing was written across this link, and the link does not claim otherwise"
    );
    assert_eq!(
        link.counterpart(&left).expect("the other end"),
        &entity("forgejo", "Vedaru/linear-cli-rs", "7")
    );
}

#[test]
fn an_address_that_is_not_one_is_refused_with_the_syntax() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = config(dir.path());

    let out = run_cli(
        &[
            "sync",
            "link",
            "linear-VED-123",
            "forgejo:Vedaru/linear-cli-rs#7",
            "--config",
            config_path.to_str().unwrap(),
        ],
        &[],
    );

    assert!(!out.success());
    assert!(
        out.stderr.contains("connector:scope#id"),
        "the syntax is named, not described: {}",
        out.stderr
    );
}

#[test]
fn a_pair_no_mapping_connects_is_refused_naming_the_mappings() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = config(dir.path());

    // Both entities real, both configured platforms - but no mapping runs between these two
    // connectors, so the sweep would never look at the link.
    let out = run_cli(
        &[
            "sync",
            "link",
            "linear:VED#a-uuid",
            "linear:VED#b-uuid",
            "--config",
            config_path.to_str().unwrap(),
        ],
        &[],
    );

    assert!(!out.success());
    assert!(out.stderr.contains("no mapping connects"), "{}", out.stderr);
    assert!(
        out.stderr.contains("sync (linear -> forgejo)"),
        "and it says what it does have: {}",
        out.stderr
    );
}
