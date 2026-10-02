//! A reference, end to end.
//!
//! The decision layer is covered next to the planner. What this covers is the thing that was
//! missing: a real delivery, parsed by the real source, reconciled by the real handler, and the
//! write that the whole feature exists for arriving at the platform the commit *named*.
//!
//! The assertion is on what the target's fake received - the mutation and the id in it - because
//! that is the difference between "we decided to attach" and "the attachment happened".
//!
//! Replay is deliberately not tested here. Refusing a delivery id twice is the *intake's*
//! guarantee, not the handler's - the handler is handed a delivery that has already been accepted
//! and recorded - and `intake_server.rs` covers it against a real socket. Asserting it here would
//! have been this file testing a layer that is not the one it is about.

use std::sync::Arc;

use serde_json::json;

use linear_bridge::connector::Source;
use linear_bridge::domain::{Secret, UserMap};
use linear_bridge::queue::Handler;
use linear_bridge::reconcile::handler::{default_policy, Endpoint, Mapping, ReconcileHandler};
use linear_bridge::reconcile::{Policy, Sides, StateNames};
use linear_bridge::sink::Sink;
use linear_bridge::sources::declarative::DeclarativeSource;
use linear_bridge::sources::presets;
use linear_bridge::store::sqlite::SqliteStore;
use linear_bridge::store::{Delivery, Store};

mod support;
use support::Fake;

const SECRET: &str = "0123456789abcdef";
/// The scope the forge fixture's reference delivery comes from.
const SCOPE: &str = "a/b";

fn policy() -> Policy {
    // The mapping here is forge -> Linear, because that is the direction the automation runs
    // in: the forge's events arrive, and the issue they name is Linear's. Each side's states
    // are named as that platform holds them.
    let mut policy = default_policy(Sides::new(
        StateNames {
            closed: vec!["closed".into()],
            open: Some("open".into()),
            initial: None,
        },
        StateNames {
            closed: vec!["Done".into(), "Canceled".into()],
            open: Some("In Progress".into()),
            initial: Some("Todo".into()),
        },
    ));
    // Without this the planner switches the whole reference axis off, which is the
    // other thing this test would be hiding.
    policy.git_automation = true;
    policy
}

/// The delivery the forge fixture declares as a reference: a pull request whose text names a
/// Linear issue. Taken from the fixture rather than spelled out, so the payload the platform
/// really sends is the one on the wire.
fn reference_delivery() -> Delivery {
    delivery_where("an open review request", |delivery| {
        delivery.reference_text.is_some() && delivery.merged != Some(true)
    })
}

/// The merge: the same reference, reported as closed **and** merged.
fn merged_reference_delivery() -> Delivery {
    delivery_where("a merge", |delivery| delivery.merged == Some(true))
}

fn delivery_where(what: &str, matches: impl Fn(&support::DeliveryExpectation) -> bool) -> Delivery {
    let fixture = support::fixture_for("forgejo");
    let expected = fixture
        .deliveries
        .iter()
        .find(|delivery| matches(delivery))
        .unwrap_or_else(|| panic!("the fixture declares {what}"));

    Delivery {
        id: 1,
        connector: "forgejo".into(),
        delivery_id: expected
            .delivery_id
            .clone()
            .unwrap_or_else(|| "1001".into()),
        event: expected.event.clone().unwrap_or_default(),
        kind: linear_bridge::domain::EntityKind::Reference,
        action: linear_bridge::domain::Action::Created,
        scope: expected.scope.clone(),
        native_id: expected.id.clone().unwrap_or_default(),
        body: expected.body.clone(),
        attempts: 1,
        last_error: None,
    }
}

struct Fixture {
    handler: ReconcileHandler,
    target: Fake,
    /// Held, not dropped: a `Fake` shuts its server down when it goes out of scope, and the
    /// handler snaps the reference's own side on every delivery.
    _forge: Fake,
}

impl Fixture {
    fn start() -> Self {
        linear_bridge::logging::init_default();
        let forge = Fake::start_from("forgejo");
        let target = Fake::start_from("linear");

        // Both ends are sources as well as sinks, as a real deployment configures them: a
        // sweep has to be able to read either side, so a platform that can only be written
        // to cannot be named by a mapping.
        let sources: Vec<Arc<dyn Source>> = vec![
            Arc::new(DeclarativeSource::new(
                "forgejo",
                Secret::new(SECRET),
                presets::preset("forgejo").expect("the preset loads"),
            )),
            Arc::new(DeclarativeSource::new(
                "linear",
                Secret::new(SECRET),
                presets::preset("linear").expect("the preset loads"),
            )),
        ];
        let sinks: Vec<Arc<dyn Sink>> = vec![
            Arc::new(forge.sink("forgejo")),
            Arc::new(target.sink("linear")),
        ];

        // The forge is the source end, so a reference from it names the *other* end - which is
        // the direction that used to attach to the wrong platform, or to none.
        let mapping = Mapping {
            name: "references".into(),
            source: Endpoint::parse(&format!("forgejo:{SCOPE}")).expect("an endpoint"),
            sink: Endpoint::parse("linear:VED").expect("an endpoint"),
            users: UserMap::default(),
            policy: policy(),
        };

        let path = std::env::temp_dir().join(format!(
            "linear-bridge-references-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut store = SqliteStore::open(&path).expect("a store");
        store.migrate().expect("migrated");

        let handler = ReconcileHandler::new(sources, sinks, vec![mapping], Box::new(store))
            .expect("the handler builds");

        Self {
            handler,
            target,
            _forge: forge,
        }
    }

    fn deliver(&mut self, delivery: &Delivery) -> linear_bridge::Result<()> {
        self.handler.handle(delivery)
    }

    /// The state ids the target was asked to move an issue to.
    fn moved_to(&self) -> Vec<String> {
        self.target
            .graphql("issueUpdate")
            .into_iter()
            .filter_map(|record| {
                record.body["variables"]["input"]["stateId"]
                    .as_str()
                    .map(str::to_string)
            })
            .collect()
    }

    /// The ids the target was asked to attach things to.
    fn attached_to(&self) -> Vec<String> {
        self.target
            .graphql("attachmentCreate")
            .into_iter()
            .filter_map(|record| {
                record.body["variables"]["input"]["issueId"]
                    .as_str()
                    .map(str::to_string)
            })
            .collect()
    }
}

#[test]
fn a_pull_request_that_names_an_issue_is_attached_to_it() {
    let mut fixture = Fixture::start();

    fixture
        .deliver(&reference_delivery())
        .expect("the delivery is handled");

    // Which half is missing, when this breaks: the resolution or the write.
    let fetched = fixture.target.graphql("issue(id: $id)");
    let all: Vec<String> = fixture
        .target
        .seen()
        .iter()
        .map(|record| {
            format!(
                "{} {} {}",
                record.method,
                record
                    .body
                    .get("query")
                    .and_then(|q| q.as_str())
                    .unwrap_or(""),
                record.body.get("variables").cloned().unwrap_or_default()
            )
        })
        .collect();
    assert_eq!(
        fetched.len(),
        1,
        "the identifier was resolved once (saw: {all:?})"
    );
    assert_eq!(
        fetched[0].body["variables"]["id"],
        json!("VED-2"),
        "resolved by identifier, because that is all the text carries"
    );

    // The id is the one the *platform* issued, not the "VED-2" a human wrote - which is the
    // whole reason the identifier is resolved through the API rather than guessed at. A fetch
    // and a mutation share Linear's single url, so this also pins that the fixture answered
    // them apart: had it answered the fetch's shape to the mutation, nothing would attach.
    let attached = fixture.attached_to();
    assert_eq!(
        attached,
        vec!["1a2b3c4d-0000-4000-8000-000000000002".to_string()],
        "one attachment, on the issue the text named"
    );

    // And the half that makes it automation rather than a bookmark: an open review request
    // moves the issue it names to the state this policy calls open.
    assert_eq!(
        fixture.moved_to(),
        vec!["state-progress".to_string()],
        "an open review request moves the issue to the open state"
    );
}

#[test]
fn a_merged_pull_request_moves_the_named_issue_to_done() {
    let mut fixture = Fixture::start();

    fixture
        .deliver(&merged_reference_delivery())
        .expect("the merge is handled");

    // Merged is what says done. A forge reports a merge as a close, so without the flag this
    // delivery would be indistinguishable from an abandoned request - and the issue would sit
    // in progress forever, or be marked done while the work was cancelled.
    assert_eq!(
        fixture.moved_to(),
        vec!["state-done".to_string()],
        "a merge moves the issue to the first state this policy calls closed"
    );
    assert_eq!(
        fixture.attached_to().len(),
        1,
        "and it is still attached - the merge is the same reference"
    );
}

#[test]
fn a_reference_that_names_nothing_this_deployment_owns_attaches_nothing() {
    let mut fixture = Fixture::start();
    let mut delivery = reference_delivery();
    // A commit that quotes somebody else's tracker: not our team key, so not our issue.
    delivery.body = delivery
        .body
        .replace("Fixes VED-2", "Fixes OTHER-7")
        .to_string();

    fixture.deliver(&delivery).expect("handled");

    assert!(
        fixture.attached_to().is_empty(),
        "another tracker's identifier is not ours to touch"
    );
    assert!(
        fixture.target.graphql("issue(id: $id)").is_empty(),
        "and it is not even asked about"
    );
}

#[test]
fn the_side_the_reference_arrives_from_does_not_change_where_it_goes() {
    // The same delivery, with the mapping the other way round: the issue it names is still on
    // the Linear end, because the *team key* says so - which is what makes this independent of
    // the direction a mapping happens to run in.
    let forge = Fake::start_from("forgejo");
    let target = Fake::start_from("linear");
    let sources: Vec<Arc<dyn Source>> = vec![
        Arc::new(DeclarativeSource::new(
            "forgejo",
            Secret::new(SECRET),
            presets::preset("forgejo").expect("the preset loads"),
        )),
        Arc::new(DeclarativeSource::new(
            "linear",
            Secret::new(SECRET),
            presets::preset("linear").expect("the preset loads"),
        )),
    ];
    let sinks: Vec<Arc<dyn Sink>> = vec![
        Arc::new(forge.sink("forgejo")),
        Arc::new(target.sink("linear")),
    ];
    let mut policy = policy();
    // A mapping that points the other way still receives the forge's events - and the state
    // vocabularies swap with the mapping, because the ends did.
    policy.direction = linear_bridge::reconcile::Direction::Both;
    policy.names = Sides::new(policy.names.sink.clone(), policy.names.source.clone());

    let mapping = Mapping {
        name: "reversed".into(),
        source: Endpoint::parse("linear:VED").expect("an endpoint"),
        sink: Endpoint::parse(&format!("forgejo:{SCOPE}")).expect("an endpoint"),
        users: UserMap::default(),
        policy,
    };
    let path = std::env::temp_dir().join(format!(
        "linear-bridge-reversed-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut store = SqliteStore::open(&path).expect("a store");
    store.migrate().expect("migrated");
    let mut handler =
        ReconcileHandler::new(sources, sinks, vec![mapping], Box::new(store)).expect("handler");

    handler.handle(&reference_delivery()).expect("handled");

    let attached: Vec<String> = target
        .graphql("attachmentCreate")
        .into_iter()
        .filter_map(|record| {
            record.body["variables"]["input"]["issueId"]
                .as_str()
                .map(str::to_string)
        })
        .collect();
    assert_eq!(
        attached.len(),
        1,
        "the attachment still lands on the issue the text named"
    );
}
