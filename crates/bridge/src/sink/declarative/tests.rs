use super::*;
use crate::domain::StateModel;

#[test]
fn uses_finds_a_directive_anywhere_in_a_template() {
    let template = json!({
        "query": "mutation { x }",
        "variables": { "input": { "teamId": "$scope_id", "labels": ["$label_ids"] } }
    });
    assert!(uses(&Some(template.clone()), "$scope_id"));
    assert!(uses(&Some(template.clone()), "$label_ids"));
    assert!(!uses(&Some(template.clone()), "$state_id"));
    assert!(!uses(&None, "$scope_id"));
    // The explicit-null form counts as the same directive.
    assert!(uses(&Some(json!({ "due": "$due_date!" })), "$due_date"));
}

#[test]
fn names_resolve_case_insensitively_and_only_inside_their_scope() {
    let mut lookups = Lookups::default();
    lookups.remember("label", "a/b", None, "Bug", &json!(3));

    assert_eq!(
        lookups.resolved("label", "a/b", None, "bug"),
        Some(json!(3))
    );
    assert_eq!(
        lookups.resolved("label", "a/b", None, "BUG"),
        Some(json!(3))
    );
    // A label id means nothing in another repository, so it must not leak.
    assert_eq!(lookups.resolved("label", "c/d", None, "bug"), None);
    assert_eq!(lookups.resolved("state", "a/b", None, "bug"), None);
}

/// The container is part of the key, and is the reason it exists: two boards in one
/// repository may both have an "In Progress", and an answer filed under the
/// repository alone would put a card in the wrong one - the same bug this engine
/// change exists to fix, one level down.
#[test]
fn a_column_resolves_inside_its_project_and_not_across_them() {
    let mut lookups = Lookups::default();
    lookups.remember("column", "a/b", Some("7"), "In Progress", &json!(31));
    lookups.remember("column", "a/b", Some("8"), "In Progress", &json!(41));

    assert_eq!(
        lookups.resolved("column", "a/b", Some("7"), "in progress"),
        Some(json!(31))
    );
    assert_eq!(
        lookups.resolved("column", "a/b", Some("8"), "in progress"),
        Some(json!(41)),
        "the second board's own column, not the first board's"
    );
    // And an answer with a container is not the answer without one.
    assert_eq!(lookups.resolved("column", "a/b", None, "in progress"), None);
}

#[test]
fn a_platform_with_no_due_date_field_carries_the_date_in_a_label() {
    // The same degradation the priority already had, for the same reason: the
    // platform has no field for it, so the value travels in the one it does have -
    // and the read half takes it back out, so the two ends still agree about the
    // date instead of one of them quietly losing it.
    let read = ReadSpec {
        milestone: None,
        labels: Some(ReadField::Detailed(crate::sink::spec::ReadFieldSpec {
            path: Some("/labels".into()),
            pick: Some("/name".into()),
            from_labels: false,
        })),
        // Where the preset says a platform keeps its due dates.
        due_date: Some(ReadField::Detailed(crate::sink::spec::ReadFieldSpec {
            from_labels: true,
            ..Default::default()
        })),
        ..Default::default()
    };

    // What the bridge wrote: the label, because there is no field to write.
    let build = |capabilities: &Capabilities| {
        DeclarativeSink::new(
            "forgejo",
            SinkSpec {
                base_url: "http://127.0.0.1:1".into(),
                location: None,
                auth: None,
                headers: Default::default(),
                error_pointer: None,
                issue: Default::default(),
                project: None,
                board: None,
            },
            None,
            capabilities.clone(),
        )
    };
    let mut capabilities = Capabilities {
        states: StateModel::OpenClosed,
        labels: true,
        due_dates: false,
        milestones: false,
        priorities: false,
        multiple_assignees: false,
        native_pull_requests: true,
        deletion: false,
        list: false,
    };
    let fields = IssueFields {
        due_date: Some("2026-10-09".to_string()),
        ..Default::default()
    };
    let sink = build(&capabilities);
    let labels = sink.outbound_labels(&fields);
    assert!(
        labels.contains(&"due:2026-10-09".to_string()),
        "the date has to travel: {labels:?}"
    );

    // And it comes back out on the way in.
    let body = json!({ "labels": [{ "name": "due:2026-10-09" }] });
    assert_eq!(
        read_fields(&body, &read).due_date.as_deref(),
        Some("2026-10-09")
    );

    // A label that merely looks like one is not a date: a user may label an issue
    // `due:someday`, and reading that as one would invent a due date.
    let body = json!({ "labels": [{ "name": "due:someday" }] });
    assert_eq!(read_fields(&body, &read).due_date, None);

    // On a platform that *does* have the field, nothing is added: the field is the
    // date, and the label would be a second copy of it.
    capabilities.due_dates = true;
    let sink = build(&capabilities);
    assert!(sink.outbound_labels(&fields).is_empty());
}

#[test]
fn read_fields_map_a_flat_response_and_a_rich_one() {
    let read = ReadSpec {
        milestone: None,
        title: Some(ReadField::Pointer("/title".into())),
        body: Some(ReadField::Pointer("/body".into())),
        labels: Some(ReadField::Detailed(crate::sink::spec::ReadFieldSpec {
            path: Some("/labels".into()),
            pick: Some("/name".into()),
            from_labels: false,
        })),
        priority: Some(ReadField::Detailed(crate::sink::spec::ReadFieldSpec {
            from_labels: true,
            ..Default::default()
        })),
        due_date: Some(ReadField::Pointer("/due_date".into())),
        assignee: Some(ReadField::Pointer("/assignees/0/login".into())),
        project: None,
        slug: None,
        identifier: None,
        links: None,
        state: Some(ReadField::Pointer("/state".into())),
        id: Some("/number".into()),
        url: Some("/html_url".into()),
    };

    // A forge-shaped response: labels are objects, priority is absent and has
    // to come out of the labels a bridge wrote there.
    let body = json!({
        "number": 7,
        "title": "T",
        "body": "B",
        "labels": [{ "id": 1, "name": "bug" }, { "id": 2, "name": "priority:high" }],
        "due_date": "2026-10-02T00:00:00Z",
        "assignees": [{ "login": "vedaru" }],
        "state": "open"
    });
    let fields = read_fields(&body, &read);
    assert_eq!(fields.title, "T");
    // The neutral field set excludes the priority labels: there the priority
    // *is* the label, here it is the priority field, and carrying both would
    // make one issue look like two.
    assert_eq!(fields.labels, vec!["bug"]);
    assert_eq!(fields.priority, 2, "priority comes from the label");
    assert_eq!(fields.assignee.as_deref(), Some("vedaru"));
    // The timestamp a forge sends for a real due date is normalised to the
    // `YYYY-MM-DD` the neutral model holds.
    assert_eq!(fields.due_date.as_deref(), Some("2026-10-02"));
}

#[test]
fn a_linear_shaped_response_reads_the_same_fields() {
    let read = ReadSpec {
        milestone: None,
        title: Some(ReadField::Pointer("/title".into())),
        body: Some(ReadField::Pointer("/description".into())),
        labels: Some(ReadField::Detailed(crate::sink::spec::ReadFieldSpec {
            path: Some("/labels/nodes".into()),
            pick: Some("/name".into()),
            from_labels: false,
        })),
        priority: Some(ReadField::Pointer("/priority".into())),
        due_date: Some(ReadField::Pointer("/dueDate".into())),
        assignee: Some(ReadField::Pointer("/assignee/email".into())),
        state: Some(ReadField::Pointer("/state/name".into())),
        ..ReadSpec::default()
    };
    let body = json!({
        "title": "T",
        "description": "B",
        "labels": { "nodes": [{ "name": "Bug" }] },
        "priority": 3,
        "dueDate": "2026-10-02",
        "assignee": { "email": "loner@example.com" },
        "state": { "name": "Todo" }
    });
    let fields = read_fields(&body, &read);
    assert_eq!(fields.priority, 3);
    assert_eq!(fields.canonical_labels(), vec!["bug"]);
    assert_eq!(fields.assignee.as_deref(), Some("loner@example.com"));
}
