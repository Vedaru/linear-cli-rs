//! The planners: one function per kind of thing that can be mirrored.
//!
//! Split out of `reconcile/mod.rs` (VED-288). The helpers only the planners use
//! moved with them, so the parent keeps no function nobody calls.

use super::*;

pub(super) fn plan_issue(context: &Context<'_>) -> Step {
    let policy = context.policy;
    if !policy.issues() {
        return Step::Nothing(Nothing::SwitchedOff);
    }
    if !policy.direction.allows(context.side) {
        return Step::Nothing(Nothing::Direction);
    }
    if !context.observed.exists() {
        // A delete we can see: the platform that sent the event no longer has the
        // entity. A forge cannot observe a deletion at all, so this is also how a
        // deletion that arrived as an edit is recognised.
        return plan_delete(context);
    }

    match context.event.action {
        crate::domain::Action::Deleted => plan_delete(context),
        crate::domain::Action::Created => {
            if context.link.is_some() {
                // Already paired, so this is a replay or a re-delivery - not a new
                // entity to copy. Creating again would be the classic duplicate.
                return Step::Nothing(Nothing::Echo);
            }
            // Not paired - but that is not the same as "new". See `create_step`.
            create_step(&Pairwise {
                side: context.side,
                policy: context.policy,
                observed: context.observed,
                counterpart: context.counterpart,
                expected: context.expected,
                target: context.target,
            })
        }
        crate::domain::Action::Other(_) => Step::Nothing(Nothing::NotOurKind),
        _ => plan_change(context),
    }
}

fn plan_change(context: &Context<'_>) -> Step {
    let Some(link) = context.link else {
        return Step::Nothing(Nothing::Unpaired);
    };
    if !context.counterpart.exists() {
        // The pair is broken: the other side lost the entity (deleted by hand, or
        // before this bridge existed). Re-creating it from an *edit* is how a
        // mirror resurrects things, so it does not happen here - a sweep may, since
        // making the two sides agree is the whole of what it was asked to do.
        return Step::Nothing(Nothing::Unpaired);
    }
    change_step(
        &Pairwise {
            side: context.side,
            policy: context.policy,
            observed: context.observed,
            counterpart: context.counterpart,
            expected: context.expected,
            target: context.target,
        },
        link.recorded_revision(),
    )
}

fn plan_delete(context: &Context<'_>) -> Step {
    if !context.policy.delete_sync {
        return Step::Nothing(Nothing::SwitchedOff);
    }
    if context.link.is_none() {
        return Step::Nothing(Nothing::Unpaired);
    }
    if !context.counterpart.exists() {
        // Both sides are already gone; there is nothing to delete and the link is
        // the caller's to drop.
        return Step::Nothing(Nothing::AlreadyEqual);
    }
    Step::Delete
}

/// The decision for a *project*: title and description only.
///
/// A project is not an issue with fewer fields - it has no labels, priority, due
/// date, assignee or workflow state that both platforms share - so it gets its own
/// decision rather than a special case threaded through the issue one. What it does
/// share is the shape: a create is mirrored from either side, an edit converges onto
/// the other, and a pair is proven by the marker in the copy's description.
pub(super) fn plan_project(context: &Context<'_>) -> Step {
    let policy = context.policy;
    if !policy.projects() {
        return Step::Nothing(Nothing::SwitchedOff);
    }
    if !policy.direction.allows(context.side) {
        return Step::Nothing(Nothing::Direction);
    }
    if !context.observed.exists() {
        // A project the platform no longer has. Deletion is not mirrored for
        // projects: neither preset declares a delete operation, and inventing one
        // from an empty read would be a destructive guess.
        return Step::Nothing(Nothing::Unpaired);
    }

    match context.event.action {
        crate::domain::Action::Created => {
            if context.link.is_some() {
                // Already paired: a replay, not a second project.
                return Step::Nothing(Nothing::Echo);
            }
            Step::Create {
                fields: context.expected.fields.clone(),
                // Projects have no workflow state to land in.
                state: None,
                // Nor a board column: a project *is* the board.
                column: None,
                skipped: context.expected.skipped.clone(),
            }
        }
        crate::domain::Action::Deleted => Step::Nothing(Nothing::Unsupported),
        crate::domain::Action::Other(_) => Step::Nothing(Nothing::NotOurKind),
        _ => plan_project_change(context),
    }
}

/// A project change converges on the other side, title and description only.
fn plan_project_change(context: &Context<'_>) -> Step {
    let policy = context.policy;
    let Some(link) = context.link else {
        return Step::Nothing(Nothing::Unpaired);
    };
    if !context.counterpart.exists() {
        // The pair is broken. Re-creating from an *edit* is how a mirror resurrects
        // things, so it does not happen here; a sweep may, because making the two
        // agree is what it was asked.
        return Step::Nothing(Nothing::Unpaired);
    }

    // The same echo guard an issue change has: a recorded revision that matches what
    // the source now holds means this delivery is our own write coming back.
    let ours = policy.names.of(context.side);
    let expected_key = content_key_with(
        &context.expected.fields,
        ours.openness(context.observed.state.as_deref()),
    );
    if link.recorded_revision() == Some(expected_key.as_str()) {
        return Step::Nothing(Nothing::Echo);
    }
    let counterpart_key = context
        .counterpart
        .key(policy.names.of(context.side.other()));
    if counterpart_key.as_deref() == Some(expected_key.as_str()) {
        return Step::Nothing(Nothing::AlreadyEqual);
    }

    let counterpart_fields = context
        .counterpart
        .fields
        .clone()
        .expect("checked that the counterpart exists");
    let mut patch = context.expected.fields.diff(&counterpart_fields);
    // A project carries only its title and description. Any other neutral field is
    // something neither platform models here, so it must not reach the request - a
    // label or an assignee on a project would be a field the platform cannot hold.
    patch.labels = Change::Leave;
    patch.priority = Change::Leave;
    patch.due_date = Change::Leave;
    patch.assignee = Change::Leave;
    // A project is not on a project: the container's own `project` field is empty
    // on both sides, and comparing it would be comparing a field neither holds.
    patch.project = Change::Leave;
    if patch.is_empty() {
        return Step::Nothing(Nothing::AlreadyEqual);
    }

    Step::Update {
        patch,
        fields: context.expected.fields.clone(),
        // No workflow state travels with a project.
        state: None,
        // Nor a board column: a project is the board.
        column: None,
        skipped: context.expected.skipped.clone(),
    }
}

pub(super) fn plan_comment(context: &Context<'_>) -> Step {
    let policy = context.policy;
    if !policy.issues() {
        return Step::Nothing(Nothing::SwitchedOff);
    }
    if !policy.direction.allows(context.side) {
        return Step::Nothing(Nothing::Direction);
    }
    if context.link.is_none() {
        // A comment on an entity that is not mirrored has nowhere to go. Mirroring
        // it would mean creating the issue as a side effect of a comment.
        return Step::Nothing(Nothing::Unpaired);
    }
    if !context.counterpart.exists() {
        return Step::Nothing(Nothing::Unpaired);
    }
    let Some(EventDetail::Comment { id, body }) = Some(&context.event.detail) else {
        return Step::Nothing(Nothing::NotOurKind);
    };
    // A body that carries our marker is text this service wrote on the other
    // platform, arriving back as that platform's own event - whatever the action.
    if body.as_deref().is_some_and(markers::has_marker) {
        return Step::Nothing(Nothing::Echo);
    }

    match context.event.action {
        crate::domain::Action::Deleted => {
            // Deleting the copy needs to know which comment it is; without a
            // pairing, there is nothing to delete that we could identify.
            match context.comment_link {
                Some(_) => Step::DeleteComment,
                None => Step::Nothing(Nothing::Unpaired),
            }
        }
        crate::domain::Action::Updated => {
            if context.comment_link.is_none() {
                // The edit is of a comment this bridge never mirrored (the pairing
                // is recorded when the copy is posted), so re-posting the edited
                // text would be a duplicate rather than an edit.
                return Step::Nothing(Nothing::Unpaired);
            }
            let Some(clean) = clean_comment(body.as_deref()) else {
                return Step::Nothing(Nothing::Empty);
            };
            Step::UpdateComment {
                body: render_comment(
                    &clean,
                    context.event.actor.as_ref(),
                    context.event.connector.as_str(),
                    comment_marker_id(id.as_deref(), context),
                ),
            }
        }
        crate::domain::Action::Created => {
            if context.comment_link.is_some() {
                // Already mirrored: this is a replay or a re-delivery.
                return Step::Nothing(Nothing::Echo);
            }
            let Some(clean) = clean_comment(body.as_deref()) else {
                return Step::Nothing(Nothing::Empty);
            };
            Step::Comment {
                body: render_comment(
                    &clean,
                    context.event.actor.as_ref(),
                    context.event.connector.as_str(),
                    comment_marker_id(id.as_deref(), context),
                ),
            }
        }
        // A comment has no workflow state, so an action that moves one is not
        // something this bridge models - and saying so is better than guessing.
        crate::domain::Action::Closed
        | crate::domain::Action::Reopened
        | crate::domain::Action::Other(_) => Step::Nothing(Nothing::NotOurKind),
    }
}

/// The comment body that travels, markers stripped, or nothing when there is
/// nothing left to carry.
fn clean_comment(body: Option<&str>) -> Option<String> {
    let stripped = markers::strip(body?);
    let trimmed = stripped.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// The id a mirrored comment's marker names: the comment's own when the delivery
/// named one, else the parent's (the marker only has to be unique within it).
fn comment_marker_id<'a>(id: Option<&'a str>, context: &'a Context<'_>) -> &'a str {
    id.unwrap_or(&context.event.subject.native_id)
}

/// A mirrored comment, attributed and marked.
///
/// The marker is what stops the copy coming back: it is the one piece of identity
/// a comment carries that survives the trip, since a comment has no link row.
pub fn render_comment(body: &str, actor: Option<&Actor>, platform: &str, id: &str) -> String {
    let who = actor
        .map(|actor| actor.name.clone().unwrap_or_else(|| actor.id.clone()))
        .unwrap_or_else(|| "someone".to_string());
    let attributed = format!("**{who}** wrote on {platform}:\n\n{body}");
    markers::with_marker(
        &attributed,
        &markers::OriginMarker::new(platform, id.to_string()),
    )
}

pub(super) fn plan_reference(context: &Context<'_>) -> Step {
    let policy = context.policy;
    if !policy.git_automation {
        return Step::Nothing(Nothing::SwitchedOff);
    }
    if !policy.direction.allows(context.side) {
        return Step::Nothing(Nothing::Direction);
    }
    // A reference to an issue this deployment does not have is nothing to do: a commit is
    // not a reason to create an issue.
    let Some(target) = context.reference_target.cloned() else {
        return Step::Nothing(Nothing::Unpaired);
    };
    let Some(url) = context.event.subject.url.clone() else {
        return Step::Nothing(Nothing::Empty);
    };
    let text = match &context.event.detail {
        EventDetail::Reference { text, .. } => text.trim(),
        _ => "",
    };
    let title = if text.is_empty() {
        format!(
            "{} {}",
            context.event.event, context.event.subject.native_id
        )
    } else {
        text.lines().next().unwrap_or_default().trim().to_string()
    };
    if title.is_empty() {
        return Step::Nothing(Nothing::Empty);
    }
    Step::Attach {
        url,
        title,
        target: target.clone(),
        transition: reference_transition(context, &target),
    }
}

/// The move a review request makes to the issue it names, if any.
///
/// Opening one says the work has started; merging it says the work is done. Closing it
/// *without* merging says neither, so it leaves the state alone - an abandoned request is not
/// a finished one, and guessing would leave someone undoing it by hand. A commit has no merge
/// state at all, so it never moves anything: a mention is not a workflow step.
fn reference_transition(context: &Context<'_>, target: &EntityRef) -> Option<String> {
    let EventDetail::Reference { merged, .. } = &context.event.detail else {
        return None;
    };
    let merged = (*merged)?;
    // The state vocabulary is the one belonging to the end the issue is on, which is not
    // necessarily the end the event arrived from.
    let side = if target.connector == *context.counterpart_connector {
        context.side.other()
    } else {
        context.side
    };
    let names = context.policy.names.of(side);
    if merged {
        // Merged means finished: the first name this policy accepts as closed - `Done` before
        // `Canceled`, which is why the list is ordered.
        names.closed.first().cloned()
    } else if context.event.action == crate::domain::Action::Created {
        names.open.clone()
    } else {
        None
    }
}
