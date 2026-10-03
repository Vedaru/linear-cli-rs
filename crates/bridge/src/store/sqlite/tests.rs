use super::*;
use crate::domain::EntityRef;
use crate::store::{Link, ReferenceLink, Store};

fn new_delivery(delivery_id: &str) -> NewDelivery {
    NewDelivery {
        connector: ConnectorId::new("forgejo"),
        delivery_id: delivery_id.to_string(),
        event: "issues".into(),
        kind: EntityKind::Issue,
        action: Action::Created,
        scope: Some("Vedaru/linear-cli-rs".into()),
        native_id: "7".into(),
        body: "{\"action\":\"opened\"}".into(),
    }
}

#[test]
fn a_reference_is_recognised_by_what_it_pairs_not_by_the_delivery() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    store.migrate().unwrap();

    let commit = EntityRef {
        connector: ConnectorId::new("forgejo"),
        kind: EntityKind::Reference,
        scope: Some("a/b".into()),
        native_id: "abc123".into(),
        url: Some("http://forge/commit/abc123".into()),
    };
    let issue = EntityRef {
        connector: ConnectorId::new("linear"),
        kind: EntityKind::Issue,
        scope: Some("VED".into()),
        native_id: "issue-uuid".into(),
        url: None,
    };
    let link = ReferenceLink {
        source: commit.clone(),
        target: issue.clone(),
        url: Some("http://forge/commit/abc123".into()),
    };

    assert!(
        !store.reference_exists(&commit, &issue).unwrap(),
        "nothing carried out yet"
    );
    store.record_reference(&link).unwrap();
    assert!(
        store.reference_exists(&commit, &issue).unwrap(),
        "the pairing of reference and issue is what is recognised, not the delivery"
    );
    // The same commit under a new delivery id: the same reference. Recording it again is a
    // no-op, which is what the caller relies on when it sees one arrive twice.
    store.record_reference(&link).unwrap();
    assert_eq!(
        store.references_for(&issue).unwrap().len(),
        1,
        "one row, not two"
    );
    // A different issue is a different reference, however similar the commit.
    let elsewhere = EntityRef {
        native_id: "another-issue".into(),
        ..issue.clone()
    };
    assert!(!store.reference_exists(&commit, &elsewhere).unwrap());
}

#[test]
fn a_delivery_is_found_by_the_id_a_log_line_names() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    store.migrate().unwrap();

    let inserted = store.insert_delivery(&new_delivery("d-9")).unwrap();
    assert_eq!(inserted, InsertOutcome::Inserted);
    let claimed = store.claim_due(1, Duration::from_secs(30)).unwrap();
    let id = claimed[0].id;

    let found = store.find_delivery(id).unwrap().expect("the row is there");
    assert_eq!(found.delivery_id, "d-9");
    // The body comes back too: a re-run has to work on what the provider sent, not on a
    // summary of it.
    assert_eq!(found.body, "{\"action\":\"opened\"}");
    assert!(
        store.find_delivery(id + 1000).unwrap().is_none(),
        "and an id nobody has is nobody's"
    );
}

#[test]
fn a_dead_delivery_is_listed_with_the_reason_it_stopped() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    store.migrate().unwrap();

    store.insert_delivery(&new_delivery("d-1")).unwrap();
    let claimed = store.claim_due(1, Duration::from_secs(30)).unwrap();
    assert_eq!(claimed.len(), 1);
    // No retry left: the queue's decision, recorded the way a worker records it.
    store
        .fail(claimed[0].id, "HTTP 500 from linear", None)
        .unwrap();

    let dead = store.dead_deliveries(10).unwrap();
    assert_eq!(dead.len(), 1, "the one that stopped");
    assert_eq!(dead[0].last_error.as_deref(), Some("HTTP 500 from linear"));
    assert_eq!(dead[0].delivery_id, "d-1", "and which delivery it was");
    assert_eq!(dead[0].attempts, 1, "and that it was tried");
    assert!(
        store.dead_deliveries(0).unwrap().is_empty(),
        "the limit is a limit"
    );
}

#[test]
fn migrations_are_idempotent_and_recorded() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    store.migrate().unwrap();
    store.migrate().unwrap();
    let revisions: i64 = store
        .connection()
        .query_row("SELECT COUNT(*) FROM schema_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(revisions, 2, "one row per migration, applied once");
}

#[test]
fn intake_is_idempotent_on_the_delivery_id() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    assert_eq!(
        store.insert_delivery(&new_delivery("d-1")).unwrap(),
        InsertOutcome::Inserted
    );
    assert_eq!(
        store.insert_delivery(&new_delivery("d-1")).unwrap(),
        InsertOutcome::Duplicate
    );
    assert_eq!(store.counts().unwrap().pending, 1);
}

#[test]
fn the_same_delivery_id_on_two_connectors_is_not_a_duplicate() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    store.insert_delivery(&new_delivery("d-1")).unwrap();
    let mut other = new_delivery("d-1");
    other.connector = ConnectorId::new("linear");
    assert_eq!(
        store.insert_delivery(&other).unwrap(),
        InsertOutcome::Inserted
    );
    assert_eq!(store.counts().unwrap().pending, 2);
}

#[test]
fn a_claim_marks_the_row_active_and_records_the_attempt() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    store.insert_delivery(&new_delivery("d-1")).unwrap();
    let claimed = store.claim_due(4, Duration::from_secs(60)).unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].attempts, 1);
    assert_eq!(claimed[0].native_id, "7");
    assert_eq!(claimed[0].scope.as_deref(), Some("Vedaru/linear-cli-rs"));
    // Claimed, so not due again while the lease holds.
    assert!(store
        .claim_due(4, Duration::from_secs(60))
        .unwrap()
        .is_empty());
    assert_eq!(store.counts().unwrap().active, 1);
}

#[test]
fn an_expired_lease_is_reclaimed_so_a_crash_cannot_lose_work() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    store.insert_delivery(&new_delivery("d-1")).unwrap();
    // A zero-length lease is already expired on the next tick.
    assert_eq!(store.claim_due(1, Duration::ZERO).unwrap().len(), 1);
    let reclaimed = store.claim_due(1, Duration::from_secs(60)).unwrap();
    assert_eq!(reclaimed.len(), 1);
    assert_eq!(reclaimed[0].attempts, 2);
}

#[test]
fn completion_and_failure_move_the_row_as_intended() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    store.insert_delivery(&new_delivery("d-1")).unwrap();
    store.insert_delivery(&new_delivery("d-2")).unwrap();
    let claimed = store.claim_due(10, Duration::from_secs(60)).unwrap();

    store.complete(claimed[0].id).unwrap();
    store
        .fail(claimed[1].id, "boom", Some(Duration::ZERO))
        .unwrap();
    let counts = store.counts().unwrap();
    assert_eq!(counts.done, 1);
    assert_eq!(counts.pending, 1);

    // The retry is immediately due and keeps its error message.
    let retried = store.claim_due(1, Duration::from_secs(60)).unwrap();
    assert_eq!(retried.len(), 1);
    assert_eq!(retried[0].last_error.as_deref(), Some("boom"));

    store.fail(retried[0].id, "again", None).unwrap();
    assert_eq!(store.counts().unwrap().dead, 1);
    assert!(store
        .claim_due(10, Duration::from_secs(60))
        .unwrap()
        .is_empty());
}

#[test]
fn health_reports_a_live_connection() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    assert!(store.health().is_ok());
}

// --- links ---------------------------------------------------------------

fn entity(connector: &str, scope: Option<&str>, id: &str) -> EntityRef {
    let mut reference = EntityRef::new(ConnectorId::new(connector), EntityKind::Issue, id);
    if let Some(scope) = scope {
        reference = reference.with_scope(scope);
    }
    reference
}

#[test]
fn a_link_is_found_from_either_end_and_never_twice() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let linear = entity("linear", Some("VED"), "issue-uuid");
    let forgejo = entity("forgejo", Some("Vedaru/linear-cli-rs"), "7");

    store
        .upsert_link(&Link::new(linear.clone(), forgejo.clone()).with_hash("abc"))
        .unwrap();
    // A second upsert of the same pair is the same row, not a second link.
    store
        .upsert_link(&Link::new(linear.clone(), forgejo.clone()).with_hash("def"))
        .unwrap();

    let from_linear = store.find_links(&linear).unwrap();
    let from_forgejo = store.find_links(&forgejo).unwrap();
    assert_eq!(from_linear.len(), 1);
    assert_eq!(from_linear, from_forgejo, "one row, seen from both ends");
    assert_eq!(from_linear[0].recorded_revision(), Some("def"));

    let found = store
        .find_link(&linear, &ConnectorId::new("forgejo"))
        .unwrap()
        .expect("the link resolves");
    assert_eq!(found.counterpart(&linear), Some(&forgejo));
    assert_eq!(
        store
            .find_link(&linear, &ConnectorId::new("other"))
            .unwrap(),
        None,
        "a counterpart on an unmapped connector is not this link"
    );
}

#[test]
fn re_upserting_without_a_hash_keeps_the_recorded_one() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let linear = entity("linear", Some("VED"), "i");
    let forgejo = entity("forgejo", Some("a/b"), "1");
    store
        .upsert_link(&Link::new(linear.clone(), forgejo.clone()).with_hash("hash-1"))
        .unwrap();
    // Knowing the pairing without knowing the content must not erase the
    // record of what was last written: that hash is the echo guard.
    store
        .upsert_link(&Link::new(linear.clone(), forgejo))
        .unwrap();
    let links = store.find_links(&linear).unwrap();
    assert_eq!(links[0].recorded_revision(), Some("hash-1"));
}

#[test]
fn the_same_id_in_two_scopes_is_two_links() {
    // A forge issue number is only unique per repository, so the scope has to
    // be part of the key - otherwise mirroring a second repo silently
    // overwrites the first repo's link.
    let mut store = SqliteStore::open_in_memory().unwrap();
    let one = entity("forgejo", Some("a/one"), "7");
    let two = entity("forgejo", Some("a/two"), "7");
    store
        .upsert_link(&Link::new(
            one.clone(),
            entity("linear", Some("VED"), "i-1"),
        ))
        .unwrap();
    store
        .upsert_link(&Link::new(
            two.clone(),
            entity("linear", Some("VED"), "i-2"),
        ))
        .unwrap();
    assert_eq!(store.find_links(&one).unwrap().len(), 1);
    assert_eq!(store.find_links(&two).unwrap().len(), 1);
    assert_eq!(
        store
            .find_link(&one, &ConnectorId::new("linear"))
            .unwrap()
            .unwrap()
            .right
            .native_id,
        "i-1"
    );
    assert_eq!(
        store
            .find_link(&two, &ConnectorId::new("linear"))
            .unwrap()
            .unwrap()
            .right
            .native_id,
        "i-2"
    );
}

#[test]
fn deleting_a_side_removes_the_link_seen_from_either_end() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let linear = entity("linear", Some("VED"), "i");
    let forgejo = entity("forgejo", Some("a/b"), "1");
    store
        .upsert_link(&Link::new(linear.clone(), forgejo.clone()).with_hash("h"))
        .unwrap();
    store.delete_links(&forgejo).unwrap();
    assert!(store.find_links(&linear).unwrap().is_empty());
    assert!(store.find_links(&forgejo).unwrap().is_empty());
    // Deleting again is a no-op, not an error.
    store.delete_links(&forgejo).unwrap();
}

#[test]
fn a_link_records_the_project_an_issue_is_on_and_can_forget_it() {
    // A forge reports an issue's project nowhere, so the pairing is the only
    // place that knows which board the issue was placed on - without it a
    // cleared project could never be taken off.
    let mut store = SqliteStore::open_in_memory().unwrap();
    let linear = entity("linear", Some("VED"), "i");
    let forgejo = entity("forgejo", Some("a/b"), "1");
    store
        .upsert_link(&Link::new(linear.clone(), forgejo.clone()).with_hash("h-1"))
        .unwrap();
    assert_eq!(
        store.link_project(&linear).unwrap(),
        None,
        "nothing placed yet"
    );

    store.set_link_project(&linear, Some("4")).unwrap();
    assert_eq!(store.link_project(&linear).unwrap().as_deref(), Some("4"));
    // Seen from either end of the link.
    assert_eq!(store.link_project(&forgejo).unwrap().as_deref(), Some("4"));
    // A later hash-only upsert - a sweep's baseline - must not forget the board.
    store
        .upsert_link(&Link::new(linear.clone(), forgejo).with_hash("h-2"))
        .unwrap();
    assert_eq!(
        store.link_project(&linear).unwrap().as_deref(),
        Some("4"),
        "the baseline erased which board the issue was on"
    );

    // Clearing is explicit: `None` means "off the board", not "leave it alone".
    store.set_link_project(&linear, None).unwrap();
    assert_eq!(store.link_project(&linear).unwrap(), None);
}

#[test]
fn a_reference_recorded_twice_is_one_row() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let commit = entity("forgejo", Some("a/b"), "abc123");
    let issue = entity("linear", Some("VED"), "i-1");
    store
        .record_reference(&ReferenceLink {
            source: commit.clone(),
            target: issue.clone(),
            url: Some("http://x/commit/abc123".into()),
        })
        .unwrap();
    // A later re-delivery of the same push carries no URL change, and must
    // keep the one we recorded.
    store
        .record_reference(&ReferenceLink {
            source: commit.clone(),
            target: issue.clone(),
            url: None,
        })
        .unwrap();

    let references = store.references_for(&issue).unwrap();
    assert_eq!(references.len(), 1);
    assert_eq!(references[0].source, commit);
    assert_eq!(references[0].url.as_deref(), Some("http://x/commit/abc123"));
    assert!(store
        .references_for(&entity("linear", Some("VED"), "other"))
        .unwrap()
        .is_empty());
}
