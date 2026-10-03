use super::*;

const INLINE: &str = r#"
[signature]
headers = ["x-signature"]
algorithm = "hmac-sha256"

[delivery]
headers = ["x-delivery"]

[event]
headers = ["x-event"]

[[event.rule]]
match = "note"
kind = "comment"
[event.rule.fields]
action = "/kind"
id = "/note/id"
scope = "/repo"
body = "/note/text"
[event.rule.actions]
add = "created"

[[event.rule]]
match = "batch"
kind = "issue"
[event.rule.fields]
fan_out = "/items"
id = "/id"
scope = "/repo"
"#;

fn source() -> DeclarativeSource {
    DeclarativeSource::new(
        "custom",
        Secret::new("0123456789abcdef"),
        SourceSpec::from_toml(INLINE).expect("the fixture is valid"),
    )
}

fn headers(event: &str) -> HeaderMap {
    HeaderMap::from_pairs([
        ("X-Event".to_string(), event.to_string()),
        ("X-Delivery".to_string(), "d-1".to_string()),
    ])
}

#[test]
fn a_configured_rule_produces_a_comment_event() {
    let body = br#"{"kind":"add","repo":"a/b","note":{"id":7,"text":"hello"}}"#;
    let events = source().parse(&headers("note"), body).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, EntityKind::Comment);
    assert_eq!(events[0].action, Action::Created);
    assert_eq!(events[0].subject.native_id, "7");
    assert_eq!(events[0].subject.scope.as_deref(), Some("a/b"));
    assert_eq!(
        events[0].detail,
        EventDetail::Comment {
            id: None,
            body: Some("hello".into())
        }
    );
}

#[test]
fn an_unmapped_action_is_surfaced_rather_than_dropped() {
    let body = br#"{"kind":"pin","repo":"a/b","note":{"id":7}}"#;
    let events = source().parse(&headers("note"), body).unwrap();
    assert_eq!(events[0].action, Action::Other("pin".into()));
}

#[test]
fn fan_out_produces_one_event_per_element_and_falls_back_to_the_document() {
    let body = br#"{"repo":"a/b","items":[{"id":"c1"},{"id":"c2"}]}"#;
    let events = source().parse(&headers("batch"), body).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].subject.native_id, "c1");
    assert_eq!(events[1].subject.native_id, "c2");
    // Scope is only on the document, and resolves for both.
    assert_eq!(events[0].subject.scope.as_deref(), Some("a/b"));
    assert_eq!(events[1].subject.scope.as_deref(), Some("a/b"));
    // Both share the delivery id: they are one delivery.
    assert_eq!(events[0].delivery.as_str(), events[1].delivery.as_str());
}

#[test]
fn an_event_with_no_rule_is_acknowledged_with_nothing_to_do() {
    let body = br#"{"anything":true}"#;
    assert_eq!(source().parse(&headers("ping"), body).unwrap(), vec![]);
}

#[test]
fn a_matched_rule_that_cannot_find_its_id_is_rejected_not_skipped() {
    let body = br#"{"kind":"add","repo":"a/b","note":{}}"#;
    let outcome = source().parse(&headers("note"), body);
    match outcome {
        Err(Reject::Malformed(message)) => assert!(message.contains("/note/id"), "{message}"),
        other => panic!("expected a malformed rejection, got {other:?}"),
    }
}

#[test]
fn a_missing_fan_out_array_is_a_rejection_that_names_the_pointer() {
    let body = br#"{"repo":"a/b"}"#;
    match source().parse(&headers("batch"), body) {
        Err(Reject::Malformed(message)) => assert!(message.contains("/items"), "{message}"),
        other => panic!("expected a malformed rejection, got {other:?}"),
    }
}

#[test]
fn validation_refuses_a_spec_that_cannot_work() {
    let no_headers = INLINE.replace("headers = [\"x-signature\"]", "headers = []");
    assert!(SourceSpec::from_toml(&no_headers)
        .unwrap_err()
        .contains("signature.headers"));

    let bad_kind = INLINE.replace("kind = \"comment\"", "kind = \"comments\"");
    assert!(SourceSpec::from_toml(&bad_kind)
        .unwrap_err()
        .contains("unknown kind"));

    let no_id = INLINE.replace("id = \"/note/id\"", "url = \"/note/url\"");
    assert!(SourceSpec::from_toml(&no_id)
        .unwrap_err()
        .contains("fields.id"));

    let unknown_key = format!("{INLINE}\n[capabilities]\nlabels = true\nphotos = true\n");
    assert!(SourceSpec::from_toml(&unknown_key).is_err());
}

#[test]
fn validated_kind_names_map_onto_the_domain() {
    assert_eq!(Kind::parse("event-name"), Some(Kind::EventName));
    assert_eq!(Kind::parse("nope"), None);
    assert_eq!(action_from_name("reopened"), Action::Reopened);
    assert_eq!(action_from_name("weird"), Action::Other("weird".into()));
}

#[test]
fn freshness_units_and_missing_fields() {
    let spec = FreshnessSpec {
        field: "/ts".into(),
        unit: TimeUnit::Seconds,
        tolerance_secs: 60,
    };
    let now_seconds = crate::clock::now_millis() / 1_000;
    let fresh = serde_json::json!({ "ts": now_seconds });
    assert!(check_freshness(&fresh, &spec).is_ok());
    let stale = serde_json::json!({ "ts": now_seconds - 600 });
    assert_eq!(check_freshness(&stale, &spec), Err(Reject::Stale));
    let missing = serde_json::json!({});
    assert_eq!(check_freshness(&missing, &spec), Err(Reject::Stale));
    let quoted = serde_json::json!({ "ts": now_seconds.to_string() });
    assert!(check_freshness(&quoted, &spec).is_ok());
}
