//! The survey side of the handler: turning what was read into an `Entry`.
//!
//! Split out of `handler.rs` (VED-288). Every function here was private to that file's module,
//! and `pub(super)` is the least visibility that lets the parent keep calling them unchanged.

use super::*;

/// The comment an event is about, if it is about one.
pub(super) fn comment_reference(event: &Event, subject: &EntityRef) -> Option<EntityRef> {
    if event.kind != EntityKind::Comment {
        return None;
    }
    let crate::domain::EventDetail::Comment { id: Some(id), .. } = &event.detail else {
        return None;
    };
    Some(EntityRef {
        connector: subject.connector.clone(),
        kind: EntityKind::Comment,
        scope: subject.scope.clone(),
        native_id: id.clone(),
        url: None,
    })
}

/// The policy a mapping gets when the deployment does not say otherwise.
pub fn default_policy(names: Sides<StateNames>) -> Policy {
    Policy {
        direction: Direction::Both,
        sync_issues: true,
        sync_projects: false,
        git_automation: true,
        delete_sync: false,
        names,
        columns: Default::default(),
    }
}

pub(super) fn describe_nothing(reason: Nothing) -> &'static str {
    match reason {
        Nothing::NotOurKind => "nothing to do: not a kind this bridge mirrors",
        Nothing::Unpaired => "nothing to do: not part of a mirrored pair",
        Nothing::Echo => "nothing to do: this is the echo of our own write",
        Nothing::AlreadyEqual => "nothing to do: both sides already agree",
        Nothing::Direction => "nothing to do: the mapping does not mirror this direction",
        Nothing::SwitchedOff => "nothing to do: this class of syncing is switched off",
        Nothing::Empty => "nothing to do: nothing to carry",
        Nothing::Unsupported => "nothing to do: the far platform cannot do this",
    }
}

/// The entry for a pair that needs no write.
///
/// It still records a revision, so the next sweep has a baseline rather than asking
/// "who moved?" about two sides it cannot date.
pub(super) fn in_step(
    source: &Found,
    sink: &Found,
    expected: &IssueFields,
    sink_names: &StateNames,
    sink_scope: &str,
) -> Entry {
    Entry {
        subject: source.reference.clone(),
        side: Side::Source,
        counterpart: Some(sink.reference.clone()),
        counterpart_state: sink.state.clone(),
        action: Action::InStep,
        step: None,
        record: Some(content_key(expected, sink.state.as_deref(), sink_names)),
        sink_scope: sink_scope.to_string(),
        source_project: None,
    }
}

/// Both ends changed since the bridge last wrote: reported, never resolved.
pub(super) fn conflict(source: &Found, sink: &Found, sink_scope: &str) -> Entry {
    Entry {
        subject: source.reference.clone(),
        side: Side::Source,
        counterpart: Some(sink.reference.clone()),
        counterpart_state: sink.state.clone(),
        action: Action::Conflict,
        step: None,
        record: None,
        sink_scope: sink_scope.to_string(),
        source_project: None,
    }
}

/// The step for a pair whose winner is known, and the entry that describes it.
pub(super) fn decide(
    mapping: &Mapping,
    winner: Side,
    observed: &Found,
    counterpart: Option<&Found>,
    expected: &Projected,
    ends: Ends<'_>,
    recorded: Option<&str>,
) -> Entry {
    // The target is the winner's *other* end: the platform the step writes to.
    let target = match winner {
        Side::Source => ends.sink,
        Side::Sink => ends.source,
    };
    let counterpart_state = counterpart.and_then(|found| found.state.clone());
    let counterpart_snapshot = match counterpart {
        Some(found) => Snapshot::present(found.fields.clone(), found.state.clone()),
        None => Snapshot::gone(),
    };
    let step = converge(
        &Pairwise {
            side: winner,
            policy: &mapping.policy,
            observed: &Snapshot::present(observed.fields.clone(), observed.state.clone()),
            counterpart: &counterpart_snapshot,
            expected,
            target,
        },
        recorded,
    );

    let (action, record) = match &step {
        Step::Create { .. } => (Action::Create, None),
        Step::Update { patch, state, .. } => {
            let mut touched: Vec<String> = patch
                .touched()
                .iter()
                .map(|name| (*name).to_string())
                .collect();
            if state.is_some() {
                touched.push("state".into());
            }
            (Action::Write { touched }, None)
        }
        Step::Nothing(reason) => match reason {
            Nothing::Echo | Nothing::AlreadyEqual | Nothing::Empty => (
                Action::InStep,
                Some(content_key(
                    &expected.fields,
                    counterpart_state.as_deref(),
                    mapping.policy.names.of(winner.other()),
                )),
            ),
            other => (
                Action::NotMirrored {
                    why: describe_nothing(*other).to_string(),
                },
                None,
            ),
        },
        // Unreachable in practice: a sweep never reaches for a comment, an attachment
        // or a deletion - those come from deliveries, where an event says what happened.
        other => (
            Action::Write {
                touched: vec![format!("{other:?}")],
            },
            None,
        ),
    };

    Entry {
        subject: observed.reference.clone(),
        side: winner,
        counterpart: counterpart.map(|found| found.reference.clone()),
        counterpart_state,
        action,
        step: (!matches!(step, Step::Nothing(_))).then_some(step),
        record,
        sink_scope: ends.sink_scope.to_string(),
        // Only an issue the source side names a project for, being written to the
        // sink: the id is resolved to the sink's own when the write is carried out.
        source_project: (winner == Side::Source && observed.reference.kind == EntityKind::Issue)
            .then(|| observed.fields.project.clone())
            .flatten(),
    }
}

/// A body that carries our marker.
///
/// The marker is how a copy is recognised *without* the store - a scope can be swept,
/// and a lost link table does not mean losing every pairing. Both the content signature
/// and the field diff ignore it, so stamping changes nothing about what the two sides
/// compare, and a body is never re-sent because of it.
pub(super) fn stamped(mut fields: IssueFields, subject: &EntityRef) -> IssueFields {
    fields.body = with_marker(&fields.body, subject);
    fields
}

/// The same for the body a patch carries, when it carries one.
pub(super) fn stamped_patch(mut patch: Patch, subject: &EntityRef) -> Patch {
    if let Change::Set(body) = patch.body {
        patch.body = Change::Set(with_marker(&body, subject));
    }
    patch
}

pub(super) fn with_marker(body: &str, subject: &EntityRef) -> String {
    markers::with_marker(
        body,
        &markers::OriginMarker::new(subject.connector.as_str(), subject.native_id.clone()),
    )
}

/// `connector:scope`, as the report and the config both write an endpoint.
pub(super) fn describe_end(end: &Endpoint) -> String {
    format!("{}:{}", end.connector, end.scope)
}
