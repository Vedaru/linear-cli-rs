//! Preset conformance.
//!
//! Each preset is a *description* of a platform's payloads, so the thing worth
//! testing is that the description matches real deliveries. These are the payload
//! shapes the platforms actually send, checked field by field: if a preset drifts
//! from reality, intake starts rejecting or misreading live deliveries, and this
//! is where that shows up first.

use std::sync::Arc;

use linear_bridge::connector::{HeaderMap, Reject, Source};
use linear_bridge::domain::{Action, EntityKind, EventDetail, Secret};
use linear_bridge::sources::declarative::DeclarativeSource;
use linear_bridge::sources::presets;

const SECRET: &str = "0123456789abcdef";

fn source(name: &str) -> DeclarativeSource {
    DeclarativeSource::new(
        name,
        Secret::new(SECRET),
        presets::preset(name).expect("the preset loads"),
    )
}

fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
    HeaderMap::from_pairs(pairs.iter().map(|(name, value)| (*name, *value)))
}

// --- Linear -----------------------------------------------------------------

// --- Forgejo ----------------------------------------------------------------

#[test]
fn forgejo_issue_deliveries_map_onto_the_domain() {
    let source = source("forgejo");
    let body = br#"{
        "action": "opened",
        "repository": { "full_name": "Vedaru/linear-cli-rs" },
        "sender": { "login": "vedaru" },
        "issue": { "number": 7, "html_url": "http://127.0.0.1:3000/Vedaru/linear-cli-rs/issues/7" }
    }"#;
    let events = source
        .parse(
            &headers(&[("X-Forgejo-Event", "issues"), ("X-Forgejo-Delivery", "d-1")]),
            body,
        )
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, EntityKind::Issue);
    assert_eq!(events[0].action, Action::Created);
    assert_eq!(events[0].subject.native_id, "7");
    assert_eq!(
        events[0].subject.scope.as_deref(),
        Some("Vedaru/linear-cli-rs")
    );
    assert_eq!(events[0].delivery.as_str(), "d-1");
    assert_eq!(
        events[0].actor.as_ref().map(|a| a.id.as_str()),
        Some("vedaru")
    );
}

#[test]
fn forgejo_issue_actions_map_and_unknown_ones_are_surfaced() {
    let source = source("forgejo");
    for (action, expected) in [
        ("closed", Action::Closed),
        ("reopened", Action::Reopened),
        ("edited", Action::Updated),
        ("deleted", Action::Deleted),
        ("milestoned", Action::Other("milestoned".into())),
    ] {
        let body = format!(
            r#"{{"action":"{action}","repository":{{"full_name":"a/b"}},"issue":{{"number":3}}}}"#
        );
        let events = source
            .parse(&headers(&[("X-Forgejo-Event", "issues")]), body.as_bytes())
            .unwrap();
        assert_eq!(events[0].action, expected, "action `{action}`");
    }
}

#[test]
fn forgejo_comments_and_pull_requests_carry_their_text() {
    let source = source("forgejo");
    let comment = br#"{
        "action": "created",
        "repository": { "full_name": "a/b" },
        "sender": { "login": "vedaru" },
        "issue": { "number": 12 },
        "comment": { "id": 12, "body": "looks good", "html_url": "http://x/c/12" }
    }"#;
    let events = source
        .parse(&headers(&[("X-Forgejo-Event", "issue_comment")]), comment)
        .unwrap();
    assert_eq!(events[0].kind, EntityKind::Comment);
    assert_eq!(events[0].subject.native_id, "12");
    assert_eq!(
        events[0].detail,
        EventDetail::Comment {
            id: Some("12".into()),
            body: Some("looks good".into())
        }
    );

    let pull_request = br#"{
        "action": "opened",
        "repository": { "full_name": "a/b" },
        "pull_request": { "number": 4, "title": "Fixes VED-2", "body": "why" }
    }"#;
    let events = source
        .parse(
            &headers(&[("X-Forgejo-Event", "pull_request")]),
            pull_request,
        )
        .unwrap();
    assert_eq!(events[0].kind, EntityKind::Reference);
    match &events[0].detail {
        EventDetail::Reference {
            text,
            closing_keywords,
        } => {
            assert_eq!(text, "Fixes VED-2\n\nwhy");
            assert!(closing_keywords.contains(&"fixes".to_string()));
        }
        other => panic!("unexpected detail: {other:?}"),
    }
}

#[test]
fn a_forgejo_push_fans_out_per_commit() {
    let source = source("forgejo");
    let body = br#"{
        "repository": { "full_name": "a/b" },
        "sender": { "login": "vedaru" },
        "commits": [
            { "id": "abc123", "message": "fixes VED-1", "url": "http://x/commit/abc123",
              "author": { "name": "Vedaru", "email": "v@example.com" } },
            { "id": "def456", "message": "chore: tidy", "author": { "name": "Vedaru" } }
        ]
    }"#;
    let events = source
        .parse(&headers(&[("X-Forgejo-Event", "push")]), body)
        .unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].subject.native_id, "abc123");
    assert_eq!(events[0].subject.scope.as_deref(), Some("a/b"));
    assert_eq!(events[0].action, Action::Created);
    match &events[0].detail {
        EventDetail::Reference { text, .. } => assert_eq!(text, "fixes VED-1"),
        other => panic!("unexpected detail: {other:?}"),
    }
    assert_eq!(events[1].subject.native_id, "def456");
}

#[test]
fn forgejo_accepts_the_legacy_gitea_headers_and_ignores_pings() {
    let source = source("forgejo");
    let body = br#"{"action":"closed","repository":{"full_name":"a/b"},"issue":{"number":3}}"#;
    let events = source
        .parse(&headers(&[("X-Gitea-Event", "issues")]), body)
        .unwrap();
    assert_eq!(events[0].action, Action::Closed);
    // No delivery header on the old releases: the id falls back to a body digest.
    assert_eq!(events[0].delivery.as_str().len(), 16);

    let ping = br#"{"repository":{"full_name":"a/b"},"zen":"Keep it logically awesome."}"#;
    assert_eq!(
        source
            .parse(&headers(&[("X-Forgejo-Event", "ping")]), ping)
            .unwrap(),
        vec![]
    );
    assert_eq!(
        source
            .parse(&headers(&[("X-Forgejo-Event", "wiki")]), ping)
            .unwrap(),
        vec![],
        "an event this deployment does not model is acknowledged, not retried"
    );
    assert_eq!(
        source.parse(&HeaderMap::default(), ping),
        Err(Reject::MissingHeader("x-forgejo-event".into()))
    );
}

// --- GitHub and GitLab ------------------------------------------------------

#[test]
fn enumeration_is_derived_from_the_sink_rather_than_declared_twice() {
    // The capability follows the operation, so the two cannot disagree - and a sweep
    // asks "can I look?" rather than discovering it on the first run.
    for name in ["linear", "forgejo"] {
        let preset = presets::preset(name).expect("the preset loads");
        let capabilities = preset.capabilities.resolve(preset.sink.as_ref());
        assert!(capabilities.list, "{name} declares a list operation");
        assert!(capabilities.describe().contains(&"list"), "{name}");
    }

    // The intake-only presets cannot be enumerated, and they say so: their API half
    // is a separate piece of work, and a sweep running against one must refuse
    // rather than report an empty scope.
    for name in ["github", "gitlab"] {
        let preset = presets::preset(name).expect("the preset loads");
        assert!(preset.sink.is_none(), "{name} is intake-only today");
        let capabilities = preset.capabilities.resolve(preset.sink.as_ref());
        assert!(!capabilities.list, "{name} cannot be swept");
    }
}

#[test]
fn every_preset_is_reachable_as_a_configured_platform() {
    // The point of the presets is that a deployment selects one by name; if a
    // name in the list did not resolve, `type = "<name>"` would fail at startup.
    for name in presets::preset_names() {
        let preset = presets::preset(name).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert!(!preset.event.rules.is_empty(), "{name} has no rules");
    }
    // And the same engine treats a hand-written spec and a preset identically.
    let inline = DeclarativeSource::new(
        "custom",
        Secret::new(SECRET),
        presets::preset("forgejo").expect("preset"),
    );
    let built_in = source("forgejo");
    assert_eq!(
        inline.capabilities(),
        built_in.capabilities(),
        "a preset is just a spec"
    );
    assert_eq!(
        Arc::strong_count(&Arc::new(inline)),
        1,
        "the source is shareable across threads"
    );
}
