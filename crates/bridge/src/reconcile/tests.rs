use super::*;
use crate::domain::{Action, DeliveryId, EntityRef};

fn connector(name: &str) -> ConnectorId {
    ConnectorId::new(name)
}

/// The mapping's own table is the only thing that names a column, and a state it
/// does not name is not "the default column" - it is no opinion at all.
#[test]
fn a_column_is_named_by_the_state_the_mapping_lists() {
    let mut mapping = policy();
    mapping.columns = [
        ("In Progress".to_string(), "In Progress".to_string()),
        ("Done".to_string(), "Done".to_string()),
    ]
    .into_iter()
    .collect();

    assert_eq!(mapping.column_for(Some("In Progress")), Some("In Progress"));
    // Names are matched the way every other state name here is.
    assert_eq!(mapping.column_for(Some("in progress")), Some("In Progress"));
    assert_eq!(mapping.column_for(Some("  Done  ")), Some("Done"));
    assert_eq!(mapping.column_for(Some("Backlog")), None);
    assert_eq!(mapping.column_for(None), None);
}

fn reference(connector: &str, id: &str) -> EntityRef {
    EntityRef {
        connector: crate::domain::ConnectorId::new(connector),
        kind: EntityKind::Issue,
        scope: Some("scope".into()),
        native_id: id.into(),
        url: Some(format!("http://{connector}/{id}")),
    }
}

fn fields(title: &str, labels: &[&str], priority: u8) -> IssueFields {
    IssueFields {
        title: title.into(),
        body: "why".into(),
        labels: labels.iter().map(|label| (*label).to_string()).collect(),
        priority,
        due_date: None,
        assignee: None,
        project: None,
        slug: None,
        identifier: None,
        links: Vec::new(),
    }
}

fn policy() -> Policy {
    Policy {
        direction: Direction::Both,
        sync_issues: true,
        sync_projects: false,
        git_automation: true,
        delete_sync: true,
        names: Sides::new(
            StateNames {
                closed: vec!["Done".into(), "Canceled".into()],
                initial: Some("Todo".into()),
                open: Some("In Progress".into()),
            },
            StateNames {
                closed: vec!["closed".into()],
                initial: None,
                open: Some("open".into()),
            },
        ),
        columns: Default::default(),
    }
}

fn event(kind: EntityKind, action: Action) -> Event {
    Event {
        connector: connector("linear"),
        delivery: DeliveryId::new("d-1"),
        event: "Issue".into(),
        kind,
        action,
        subject: reference("linear", "issue-1"),
        actor: Some(Actor {
            id: "u-1".into(),
            name: Some("vedaru".into()),
        }),
        detail: EventDetail::None,
    }
}

struct Fixture {
    event: Event,
    policy: Policy,
    link: Option<Link>,
    /// The pairing a *comment* has, when the event is about one.
    comment_link: Option<Link>,
    /// The issue a reference event names, as the handler would have resolved it.
    reference_target: Option<EntityRef>,
    observed: Snapshot,
    counterpart: Snapshot,
    counterpart_connector: ConnectorId,
    /// What the other platform can hold. The default is a forge: no priority
    /// field of its own, so the priority travels in the label set.
    target: Capabilities,
    /// What the source's fields look like once projected onto the target. Tests
    /// set this only when they are about the projection; otherwise the source's
    /// own fields stand in, which is what the projection yields undeformed.
    expected: Option<Projected>,
}

impl Default for Fixture {
    fn default() -> Self {
        Self {
            event: event(EntityKind::Issue, Action::Updated),
            policy: policy(),
            link: None,
            reference_target: None,
            comment_link: None,
            observed: Snapshot::present(fields("One", &["bug"], 0), Some("In Progress".into())),
            counterpart: Snapshot::present(fields("One", &["bug"], 0), Some("open".into())),
            counterpart_connector: connector("forgejo"),
            target: Capabilities {
                states: crate::domain::StateModel::OpenClosed,
                list: true,
                labels: true,
                due_dates: true,
                priorities: false,
                multiple_assignees: false,
                native_pull_requests: true,
                deletion: false,
            },
            expected: None,
        }
    }
}

impl Fixture {
    fn plan(&self, side: Side) -> Step {
        let expected = self.expected.clone().unwrap_or_else(|| Projected {
            fields: self.observed.fields.clone().unwrap_or_default(),
            ..Projected::default()
        });
        plan(&Context {
            event: &self.event,
            side,
            policy: &self.policy,
            link: self.link.as_ref(),
            reference_target: self.reference_target.as_ref(),
            comment_link: self.comment_link.as_ref(),
            observed: &self.observed,
            counterpart: &self.counterpart,
            counterpart_connector: &self.counterpart_connector,
            expected: &expected,
            target: &self.target,
        })
    }
}

/// A link whose recorded content is what `snapshot` holds.
fn paired_with(fixture: &mut Fixture, side: Side) {
    let names = fixture.policy.names.of(side);
    let hash = match side {
        Side::Source => fixture.observed.key(names),
        Side::Sink => fixture.counterpart.key(names),
    };
    fixture.link = Some(
        Link::new(reference("linear", "issue-1"), reference("forgejo", "12"))
            .with_hash(hash.expect("a present snapshot has a key")),
    );
}

#[test]
fn a_new_issue_is_created_in_the_others_vocabulary() {
    let mut fixture = Fixture::default();
    fixture.event.action = Action::Created;
    fixture.counterpart = Snapshot::gone();

    match fixture.plan(Side::Source) {
        Step::Create { fields, state, .. } => {
            assert_eq!(fields.title, "One");
            // Forgejo's own state, not Linear's: the create is on the sink.
            assert_eq!(state, None, "a forge has no configured initial state");
        }
        other => panic!("expected a create, got {other:?}"),
    }
}

#[test]
fn a_create_on_the_sink_side_lands_in_the_sources_initial_state() {
    let mut fixture = Fixture::default();
    fixture.event.action = Action::Created;
    fixture.event.connector = connector("forgejo");
    fixture.observed = Snapshot::present(fields("One", &[], 0), Some("open".into()));
    fixture.counterpart = Snapshot::gone();

    match fixture.plan(Side::Sink) {
        // Linear's `Todo`: a mirrored issue that nobody planned is not backlog.
        Step::Create { state, .. } => assert_eq!(state.as_deref(), Some("Todo")),
        other => panic!("expected a create, got {other:?}"),
    }
}

#[test]
fn a_create_event_for_an_already_paired_entity_does_not_create_a_second_copy() {
    let mut fixture = Fixture::default();
    fixture.event.action = Action::Created;
    paired_with(&mut fixture, Side::Source);

    assert_eq!(fixture.plan(Side::Source), Step::Nothing(Nothing::Echo));
}

#[test]
fn an_unpaired_entity_whose_text_carries_our_marker_is_not_created_from() {
    // The runaway, in one delivery: a pair adopted by its marker has no link row,
    // so the link check cannot see it and this used to reach `Step::Create`. The
    // copy the create produced carried the marker too, so its own delivery created
    // another - 134 issues in one afternoon, sub-second apart.
    let mut fixture = Fixture::default();
    fixture.event.action = Action::Created;
    fixture.observed = Snapshot::present(fields("One", &[], 0), Some("open".into()));
    if let Some(observed) = fixture.observed.fields.as_mut() {
        observed.body = markers::with_marker(
            &observed.body,
            &markers::OriginMarker::new("forgejo", "issue-7"),
        );
    }
    fixture.counterpart = Snapshot::gone();

    assert_eq!(fixture.plan(Side::Source), Step::Nothing(Nothing::Echo));
}

#[test]
fn a_snapshot_whose_body_carries_our_marker_reads_as_our_own_write() {
    // The predicate `create_step` consults. Both doors - a delivery and a sweep -
    // decide their create through that one function, so this is what both of them
    // ask; a sweep builds a `Pairwise` from the projection machinery, which is why
    // the door is covered structurally rather than by a second fixture here.
    let mut snapshot = Snapshot::present(fields("One", &[], 0), Some("open".into()));
    assert!(!snapshot.is_own_write(), "an ordinary body is not ours");
    if let Some(observed) = snapshot.fields.as_mut() {
        observed.body = markers::with_marker(
            &observed.body,
            &markers::OriginMarker::new("linear", "issue-1"),
        );
    }
    assert!(snapshot.is_own_write());
    assert!(
        !Snapshot::gone().is_own_write(),
        "an entity the platform says is gone is nobody's write"
    );
}

#[test]
fn an_edit_that_matches_what_we_last_wrote_is_our_own_echo() {
    let mut fixture = Fixture::default();
    paired_with(&mut fixture, Side::Source);

    assert_eq!(fixture.plan(Side::Source), Step::Nothing(Nothing::Echo));
}

/// A comment event on the paired issue, with the comment itself paired too
/// (`paired: false` models a comment this bridge has never mirrored).
fn comment_event(action: Action, paired: bool) -> Fixture {
    let mut fixture = Fixture {
        event: event(EntityKind::Comment, action),
        ..Fixture::default()
    };
    paired_with(&mut fixture, Side::Source);
    fixture.event.subject = EntityRef {
        native_id: "comment-9".into(),
        ..reference("linear", "issue-1")
    };
    fixture.event.detail = EventDetail::Comment {
        id: Some("comment-9".into()),
        body: Some("looks good to me".into()),
    };
    fixture.comment_link = paired.then(|| {
        Link::new(
            EntityRef {
                connector: connector("linear"),
                kind: EntityKind::Comment,
                scope: Some("scope".into()),
                native_id: "comment-9".into(),
                url: None,
            },
            EntityRef {
                connector: connector("forgejo"),
                kind: EntityKind::Comment,
                scope: Some("scope".into()),
                native_id: "77".into(),
                url: None,
            },
        )
    });
    fixture
}

#[test]
fn a_comment_edit_edits_the_copy_instead_of_posting_a_second_one() {
    let fixture = comment_event(Action::Updated, true);

    match fixture.plan(Side::Source) {
        Step::UpdateComment { body } => {
            assert!(body.contains("**vedaru** wrote on linear"), "{body}");
            assert!(body.contains("looks good to me"), "{body}");
            // The marker is still the origin comment's, which is what makes the
            // copy recognisable as ours when the other platform reports the edit.
            let marker = markers::parse(&body).expect("a marker");
            assert_eq!(marker.connector, "linear");
            assert_eq!(marker.id, "comment-9");
        }
        other => panic!("expected an update, got {other:?}"),
    }
}

#[test]
fn a_comment_edit_this_bridge_never_mirrored_is_left_alone() {
    // Without a pairing there is no copy to edit, and posting the text would be
    // a duplicate rather than an edit.
    let fixture = comment_event(Action::Updated, false);
    assert_eq!(fixture.plan(Side::Source), Step::Nothing(Nothing::Unpaired));
}

#[test]
fn a_comment_deletion_removes_the_copy() {
    let fixture = comment_event(Action::Deleted, true);
    assert_eq!(fixture.plan(Side::Source), Step::DeleteComment);
}

#[test]
fn a_comment_deletion_without_a_pairing_does_nothing() {
    let fixture = comment_event(Action::Deleted, false);
    assert_eq!(fixture.plan(Side::Source), Step::Nothing(Nothing::Unpaired));
}

#[test]
fn a_comment_create_that_is_already_paired_does_not_post_again() {
    // A provider re-delivery, or our own copy coming back: the pairing is what
    // says this comment has already been mirrored.
    let fixture = comment_event(Action::Created, true);
    assert_eq!(fixture.plan(Side::Source), Step::Nothing(Nothing::Echo));
}

/// A fixture whose link records an older revision: the situation a real edit
/// arrives in (the recorded key is what "the bridge last wrote" means).
fn edited(observed: Snapshot) -> Fixture {
    Fixture {
        link: Some(
            Link::new(reference("linear", "issue-1"), reference("forgejo", "12"))
                .with_hash("an older revision"),
        ),
        observed,
        ..Fixture::default()
    }
}

#[test]
fn an_edit_the_other_side_already_has_is_a_no_op() {
    // A real edit - the link recorded an older revision - but the far side
    // already reads the same, so there is nothing to write.
    let fixture = edited(Snapshot::present(
        fields("One", &["bug"], 0),
        Some("In Progress".into()),
    ));

    assert_eq!(
        fixture.plan(Side::Source),
        Step::Nothing(Nothing::AlreadyEqual)
    );
}

#[test]
fn a_real_edit_is_written_with_the_state_the_other_side_understands() {
    let fixture = edited(Snapshot::present(
        fields("Two", &["bug"], 0),
        Some("Done".into()),
    ));

    match fixture.plan(Side::Source) {
        Step::Update { fields, state, .. } => {
            assert_eq!(fields.title, "Two");
            // Linear's `Done` is the forge's `closed`.
            assert_eq!(state.as_deref(), Some("closed"));
        }
        other => panic!("expected an update, got {other:?}"),
    }
}

#[test]
fn a_state_that_is_already_equivalent_is_left_alone() {
    // Linear `In Progress` and a forge `open` are the same openness, so the
    // update must carry the text and leave the workflow alone.
    let fixture = edited(Snapshot::present(
        fields("Two", &["bug"], 0),
        Some("In Progress".into()),
    ));

    match fixture.plan(Side::Source) {
        Step::Update { state, .. } => assert_eq!(state, None),
        other => panic!("expected an update, got {other:?}"),
    }
}

#[test]
fn an_unpaired_entity_is_not_mirrored_by_an_edit() {
    let fixture = Fixture::default();
    assert_eq!(fixture.plan(Side::Source), Step::Nothing(Nothing::Unpaired));
}

#[test]
fn a_close_with_no_text_change_still_travels() {
    let mut fixture = Fixture::default();
    // The link recorded the open revision; only the state moved.
    paired_with(&mut fixture, Side::Source);
    fixture.observed = Snapshot::present(fields("One", &["bug"], 0), Some("Done".into()));

    match fixture.plan(Side::Source) {
        Step::Update { state, .. } => assert_eq!(state.as_deref(), Some("closed")),
        other => panic!("expected an update, got {other:?}"),
    }
}

#[test]
fn a_deletion_only_travels_when_it_is_switched_on() {
    let mut fixture = Fixture::default();
    paired_with(&mut fixture, Side::Source);
    fixture.event.action = Action::Deleted;
    fixture.observed = Snapshot::gone();

    assert_eq!(fixture.plan(Side::Source), Step::Delete);

    fixture.policy.delete_sync = false;
    assert_eq!(
        fixture.plan(Side::Source),
        Step::Nothing(Nothing::SwitchedOff)
    );
}

#[test]
fn a_deletion_that_arrived_as_an_edit_is_still_a_deletion() {
    // A forge emits no webhook when an issue is deleted, so the only trace is
    // that the issue is no longer there.
    let mut fixture = Fixture::default();
    paired_with(&mut fixture, Side::Source);
    fixture.event.action = Action::Updated;
    fixture.observed = Snapshot::gone();

    assert_eq!(fixture.plan(Side::Source), Step::Delete);
}

#[test]
fn a_deletion_of_something_unpaired_does_nothing() {
    let mut fixture = Fixture::default();
    fixture.event.action = Action::Deleted;
    fixture.observed = Snapshot::gone();

    assert_eq!(fixture.plan(Side::Source), Step::Nothing(Nothing::Unpaired));
}

#[test]
fn a_one_way_mapping_ignores_the_side_it_does_not_mirror() {
    let mut fixture = Fixture::default();
    fixture.policy.direction = Direction::SourceToSink;
    paired_with(&mut fixture, Side::Sink);
    fixture.observed = Snapshot::present(fields("Changed", &[], 0), Some("open".into()));

    assert_eq!(fixture.plan(Side::Sink), Step::Nothing(Nothing::Direction));
    // The source side is still mirrored (it is the direction's source), so this
    // is not the direction blocking it.
    assert_ne!(
        fixture.plan(Side::Source),
        Step::Nothing(Nothing::Direction)
    );
}

#[test]
fn switching_issue_syncing_off_stops_everything_issue_shaped() {
    let mut fixture = Fixture::default();
    fixture.policy.sync_issues = false;
    assert_eq!(
        fixture.plan(Side::Source),
        Step::Nothing(Nothing::SwitchedOff)
    );

    fixture.event.kind = EntityKind::Comment;
    fixture.event.detail = EventDetail::Comment {
        id: Some("comment-9".into()),
        body: Some("hello".into()),
    };
    assert_eq!(
        fixture.plan(Side::Source),
        Step::Nothing(Nothing::SwitchedOff)
    );
}

#[test]
fn a_comment_is_attributed_and_marked() {
    let mut fixture = Fixture::default();
    paired_with(&mut fixture, Side::Source);
    fixture.event.kind = EntityKind::Comment;
    fixture.event.action = Action::Created;
    fixture.event.subject = EntityRef {
        native_id: "comment-9".into(),
        ..reference("linear", "issue-1")
    };
    fixture.event.detail = EventDetail::Comment {
        id: Some("comment-9".into()),
        body: Some("looks good to me".into()),
    };

    match fixture.plan(Side::Source) {
        Step::Comment { body } => {
            assert!(body.contains("**vedaru** wrote on linear"), "{body}");
            assert!(body.contains("looks good to me"), "{body}");
            // The marker is what stops this text coming back as a new comment.
            assert!(markers::has_marker(&body));
            let marker = markers::parse(&body).unwrap();
            assert_eq!(marker.connector, "linear");
            assert_eq!(marker.id, "comment-9");
        }
        other => panic!("expected a comment, got {other:?}"),
    }
}

#[test]
fn a_mirrored_comment_coming_back_is_ignored() {
    let mut fixture = Fixture::default();
    paired_with(&mut fixture, Side::Source);
    fixture.event.kind = EntityKind::Comment;
    fixture.event.detail = EventDetail::Comment {
        id: Some("comment-9".into()),
        body: Some(format!(
            "copy\n\n{}",
            markers::render(&markers::OriginMarker::new("forgejo", "77"))
        )),
    };

    assert_eq!(fixture.plan(Side::Source), Step::Nothing(Nothing::Echo));
}

#[test]
fn an_empty_comment_is_not_mirrored() {
    let mut fixture = Fixture::default();
    paired_with(&mut fixture, Side::Source);
    fixture.event.kind = EntityKind::Comment;
    fixture.event.action = Action::Created;
    fixture.event.detail = EventDetail::Comment {
        id: Some("comment-9".into()),
        body: Some("   \n".into()),
    };

    assert_eq!(fixture.plan(Side::Source), Step::Nothing(Nothing::Empty));
}

#[test]
fn a_comment_on_an_unmirrored_issue_has_nowhere_to_go() {
    let mut fixture = Fixture::default();
    fixture.event.kind = EntityKind::Comment;
    fixture.event.detail = EventDetail::Comment {
        id: Some("comment-9".into()),
        body: Some("hi".into()),
    };

    assert_eq!(fixture.plan(Side::Source), Step::Nothing(Nothing::Unpaired));
}

#[test]
fn a_reference_becomes_an_attachment_on_the_mirrored_issue() {
    let mut fixture = Fixture::default();
    paired_with(&mut fixture, Side::Source);
    fixture.event.kind = EntityKind::Reference;
    fixture.event.subject = EntityRef {
        kind: EntityKind::Reference,
        native_id: "4".into(),
        url: Some("http://forge/pulls/4".into()),
        ..reference("forgejo", "4")
    };
    fixture.event.detail = EventDetail::Reference {
        text: "Fix the thing\n\nlonger body".into(),
        closing_keywords: vec!["fixes".into()],
        // A commit: a reference with no merge state moves nothing.
        merged: None,
    };

    // The issue the text names, as the handler resolves it: the attachment belongs on
    // *that* issue, not on a mirror of it.
    fixture.reference_target = Some(reference("linear", "issue-uuid"));

    match fixture.plan(Side::Source) {
        Step::Attach {
            url,
            title,
            target,
            transition,
        } => {
            assert_eq!(url, "http://forge/pulls/4");
            assert_eq!(title, "Fix the thing");
            assert_eq!(target.connector.as_str(), "linear");
            assert_eq!(target.native_id, "issue-uuid");
            assert_eq!(transition, None, "a commit does not move the issue");
        }
        other => panic!("expected an attachment, got {other:?}"),
    }
}

/// A reference event that names an issue, as the handler resolves one: the merge state is
/// the one thing the case varies, because it is the one thing that decides the move.
fn review_request(merged: Option<bool>) -> Fixture {
    let mut fixture = Fixture::default();
    fixture.event.kind = EntityKind::Reference;
    fixture.event.subject = EntityRef {
        kind: EntityKind::Reference,
        native_id: "4".into(),
        url: Some("http://forge/pulls/4".into()),
        ..reference("forgejo", "4")
    };
    fixture.event.detail = EventDetail::Reference {
        text: "Fixes VED-2".into(),
        closing_keywords: vec!["fixes".into()],
        merged,
    };
    fixture.reference_target = Some(reference("linear", "issue-uuid"));
    fixture
}

fn transition_of(fixture: &Fixture) -> Option<String> {
    match fixture.plan(Side::Source) {
        Step::Attach { transition, .. } => transition,
        other => panic!("expected an attachment, got {other:?}"),
    }
}

#[test]
fn an_open_review_request_moves_the_issue_to_the_open_state() {
    let mut fixture = review_request(Some(false));
    fixture.event.action = Action::Created;

    assert_eq!(transition_of(&fixture).as_deref(), Some("In Progress"));
}

#[test]
fn a_merged_review_request_moves_the_issue_to_a_closed_state() {
    let mut fixture = review_request(Some(true));
    fixture.event.action = Action::Closed;

    // `Done` and not `Canceled`: the policy's list is ordered, and a merge means finished.
    assert_eq!(transition_of(&fixture).as_deref(), Some("Done"));
}

#[test]
fn a_review_request_closed_without_merging_moves_nothing() {
    // An abandoned request is not finished work, and marking it done would be a claim
    // somebody has to undo by hand. The attachment still happens: the reference is real
    // either way.
    let mut fixture = review_request(Some(false));
    fixture.event.action = Action::Closed;

    assert_eq!(transition_of(&fixture), None);
    assert!(matches!(fixture.plan(Side::Source), Step::Attach { .. }));
}

#[test]
fn a_commit_never_moves_an_issue() {
    // No merge state at all: a commit message is a mention, not a workflow step.
    let mut fixture = review_request(None);
    fixture.event.action = Action::Created;

    assert_eq!(transition_of(&fixture), None);
}

#[test]
fn a_reference_to_an_issue_this_deployment_cannot_resolve_creates_nothing() {
    let mut fixture = Fixture::default();
    fixture.event.kind = EntityKind::Reference;
    fixture.event.detail = EventDetail::Reference {
        text: "Fix the thing".into(),
        closing_keywords: vec![],
        merged: None,
    };

    assert_eq!(fixture.plan(Side::Source), Step::Nothing(Nothing::Unpaired));
}

#[test]
fn a_reference_without_a_url_is_nothing() {
    let mut fixture = Fixture::default();
    paired_with(&mut fixture, Side::Source);
    fixture.event.kind = EntityKind::Reference;
    fixture.event.subject = EntityRef {
        url: None,
        ..reference("forgejo", "4")
    };
    fixture.event.detail = EventDetail::Reference {
        text: "Fix the thing".into(),
        closing_keywords: vec![],
        merged: None,
    };
    // The issue resolves; it is the *url* that is missing, which is what this is about.
    fixture.reference_target = Some(reference("linear", "issue-uuid"));

    assert_eq!(fixture.plan(Side::Source), Step::Nothing(Nothing::Empty));
}

#[test]
fn git_automation_can_be_switched_off_on_its_own() {
    let mut fixture = Fixture::default();
    fixture.policy.git_automation = false;
    paired_with(&mut fixture, Side::Source);
    fixture.event.kind = EntityKind::Reference;
    fixture.event.detail = EventDetail::Reference {
        text: "Fix".into(),
        closing_keywords: vec![],
        merged: None,
    };

    assert_eq!(
        fixture.plan(Side::Source),
        Step::Nothing(Nothing::SwitchedOff)
    );
}

#[test]
fn a_kind_this_build_does_not_mirror_is_left_alone() {
    let mut fixture = Fixture::default();
    fixture.event.kind = EntityKind::Other("milestone".into());

    assert_eq!(
        fixture.plan(Side::Source),
        Step::Nothing(Nothing::NotOurKind)
    );
}

/// A project pair: the entity is a project on both ends, with no state.
fn project_fixture() -> Fixture {
    let mut fixture = Fixture::default();
    fixture.event.kind = EntityKind::Project;
    fixture.event.subject = EntityRef {
        kind: EntityKind::Project,
        native_id: "project-1".into(),
        ..reference("linear", "project-1")
    };
    fixture.observed = Snapshot::present(fields("Mirror the widget", &[], 0), None);
    fixture.counterpart = Snapshot::present(fields("Mirror the widget", &[], 0), None);
    fixture
}

#[test]
fn project_mirroring_is_switched_off_until_a_deployment_asks_for_it() {
    // The acceptance clause for the default: a Project event is inert unless
    // `sync_projects` is on, exactly as it was before this build learned about
    // projects at all.
    let mut fixture = project_fixture();
    fixture.event.action = Action::Created;
    fixture.counterpart = Snapshot::gone();

    assert_eq!(
        fixture.plan(Side::Source),
        Step::Nothing(Nothing::SwitchedOff),
        "a project event must not mirror by default"
    );
}

#[test]
fn a_project_event_is_a_create_once_project_syncing_is_on() {
    // The other half of the default: with the switch on, the same event is no
    // longer the inert catch-all - it becomes a real project to mirror.
    let mut fixture = project_fixture();
    fixture.policy.sync_projects = true;
    fixture.event.action = Action::Created;
    fixture.counterpart = Snapshot::gone();

    match fixture.plan(Side::Source) {
        Step::Create { fields, state, .. } => {
            assert_eq!(fields.title, "Mirror the widget");
            assert_eq!(state, None, "a project has no workflow state");
        }
        other => panic!("expected a create, got {other:?}"),
    }
}

#[test]
fn a_project_rename_converges_on_the_other_side() {
    let mut fixture = project_fixture();
    fixture.policy.sync_projects = true;
    // The recorded revision is the original title; the source then moves.
    paired_with(&mut fixture, Side::Source);
    fixture.observed = Snapshot::present(fields("Mirror the widget, properly", &[], 0), None);

    match fixture.plan(Side::Source) {
        Step::Update { patch, state, .. } => {
            assert_eq!(
                patch.title,
                Change::Set("Mirror the widget, properly".to_string())
            );
            assert!(state.is_none(), "no workflow state travels with a project");
        }
        other => panic!("expected an update, got {other:?}"),
    }
}

#[test]
fn a_key_is_comparable_across_vocabularies_but_not_across_revisions() {
    let names = Sides::new(
        StateNames {
            closed: vec!["Done".into()],
            initial: None,
            open: Some("In Progress".into()),
        },
        StateNames {
            closed: vec!["closed".into()],
            initial: None,
            open: Some("open".into()),
        },
    );
    let linear = Snapshot::present(fields("One", &["bug"], 0), Some("In Progress".into()));
    let forge = Snapshot::present(fields("One", &["bug"], 0), Some("open".into()));
    assert_eq!(
        linear.key(names.of(Side::Source)),
        forge.key(names.of(Side::Sink)),
        "the same content in two vocabularies is the same revision"
    );

    let closed = Snapshot::present(fields("One", &["bug"], 0), Some("Done".into()));
    assert_ne!(
        closed.key(names.of(Side::Source)),
        forge.key(names.of(Side::Sink))
    );
}

#[test]
fn an_unconfigured_state_is_compared_on_content_alone() {
    let names = StateNames::default();
    let one = Snapshot::present(fields("One", &[], 0), Some("Whatever".into()));
    let two = Snapshot::present(fields("One", &[], 0), Some("Something Else".into()));
    assert_eq!(one.key(&names), two.key(&names));
}

#[test]
fn direction_names_round_trip() {
    assert_eq!(Direction::parse("both"), Some(Direction::Both));
    assert_eq!(Direction::parse("oneway"), Some(Direction::SourceToSink));
    assert_eq!(Direction::parse("sideways"), None);
    assert!(!Direction::SinkToSource.allows(Side::Source));
    assert!(Direction::Both.allows(Side::Sink));
}
