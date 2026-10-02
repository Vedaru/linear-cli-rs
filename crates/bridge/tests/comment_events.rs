//! Parsing the comment deliveries a mirror actually receives.
//!
//! Pinned here rather than only exercised through the reconciler: a comment
//! delivery carries two ids and an action, and an action that reads as "created"
//! when it says "update" turns an edit into a second comment - a duplicate a reader
//! sees and no test at the HTTP layer would explain.

use serde_json::json;

use linear_bridge::connector::{HeaderMap, Source};
use linear_bridge::domain::{Action, EntityKind, EventDetail, Secret};
use linear_bridge::sources::declarative::DeclarativeSource;
use linear_bridge::sources::presets;

fn source() -> DeclarativeSource {
    DeclarativeSource::new(
        "linear",
        Secret::new("0123456789abcdef"),
        presets::preset("linear").expect("the linear preset loads"),
    )
}

fn linear_body(action: &str, kind: &str) -> Vec<u8> {
    json!({
        "action": action,
        "type": kind,
        "webhookTimestamp": linear_bridge::clock::now_millis(),
        "actor": { "id": "u-1", "name": "vedaru" },
        "data": {
            "id": "comment-9",
            "body": "looks good",
            "issue": { "id": "issue-1" },
            "team": { "key": "VED" }
        }
    })
    .to_string()
    .into_bytes()
}

fn parse(body: &[u8]) -> linear_bridge::domain::Event {
    let events = source()
        .parse(&HeaderMap::default(), body)
        .expect("the delivery parses");
    assert_eq!(events.len(), 1, "one event per comment delivery");
    events[0].clone()
}

#[test]
fn a_comment_edit_reads_as_an_edit() {
    let event = parse(&linear_body("update", "Comment"));

    assert_eq!(event.kind, EntityKind::Comment);
    assert_eq!(event.action, Action::Updated);
    // The subject is the issue (what a link pairs), the detail carries the comment.
    assert_eq!(event.subject.native_id, "issue-1");
    // A comment delivery names the issue but not the team: the scope is left empty
    // here and filled in from the mapping, which is the only place that knows it.
    assert_eq!(event.subject.scope, None);
    assert_eq!(
        event.detail,
        EventDetail::Comment {
            id: Some("comment-9".into()),
            body: Some("looks good".into())
        }
    );
}

#[test]
fn every_comment_action_linear_sends_is_recognised() {
    for (raw, expected) in [
        ("create", Action::Created),
        ("update", Action::Updated),
        ("remove", Action::Deleted),
    ] {
        let event = parse(&linear_body(raw, "Comment"));
        assert_eq!(event.action, expected, "action `{raw}`");
    }
}

#[test]
fn an_issue_event_still_reads_as_an_issue() {
    let event = parse(&linear_body("update", "Issue"));

    assert_eq!(event.kind, EntityKind::Issue);
    assert_eq!(event.action, Action::Updated);
    // For an issue the subject is the issue itself, and the id is its own.
    assert_eq!(event.subject.native_id, "comment-9");
}
