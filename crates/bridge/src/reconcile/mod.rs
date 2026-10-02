//! The reconciler: deciding *what* to write, given an event and the two sides'
//! authoritative state.
//!
//! Split deliberately into two halves:
//!
//! - this module is the **decision** - pure, no I/O, no platform, no clock. Every
//!   rule that decides between "mirror this", "this is our own echo" and "these
//!   two already agree" lives here, and is tested without a network;
//! - [`handler`] is the **execution** - read both sides, carry out the step, record
//!   what was written. It has no opinion beyond "the plan said so".
//!
//! The reason to keep them apart is not tidiness: the loop guard is the part of a
//! mirror that is hardest to reason about and easiest to get subtly wrong, and it
//! is only testable if asking the question costs nothing.
//!
//! ## Why the state is not compared by name
//!
//! Two platforms do not share a vocabulary. Linear has `Todo`, `In Progress`,
//! `Done`; a forge has `open` and `closed`. Comparing names across them is
//! meaningless, and comparing them *within* a platform does not answer the
//! question a mirror asks. What both sides can answer is "is this finished?", so
//! that - [`Openness`] - is what is compared and what the content key carries.

pub mod handler;

use crate::domain::{markers, Actor, ConnectorId, EntityKind, Event, EventDetail, IssueFields};
use crate::store::Link;

/// Which end of a mapping something happened on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Source,
    Sink,
}

impl Side {
    pub fn other(self) -> Self {
        match self {
            Side::Source => Side::Sink,
            Side::Sink => Side::Source,
        }
    }
}

/// A value per side, so a policy cannot be quietly read from the wrong one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sides<T> {
    pub source: T,
    pub sink: T,
}

impl<T> Sides<T> {
    pub fn new(source: T, sink: T) -> Self {
        Self { source, sink }
    }

    pub fn get(&self, side: Side) -> &T {
        match side {
            Side::Source => &self.source,
            Side::Sink => &self.sink,
        }
    }

    pub fn of(&self, side: Side) -> &T {
        self.get(side)
    }
}

/// Which way a mapping mirrors.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Direction {
    #[default]
    Both,
    /// Changes made on the source platform are mirrored to the sink; the sink is
    /// read but never written back from.
    SourceToSink,
    SinkToSource,
}

impl Direction {
    /// May a change observed on `side` be written to the other side?
    pub fn allows(&self, side: Side) -> bool {
        match self {
            Direction::Both => true,
            Direction::SourceToSink => side == Side::Source,
            Direction::SinkToSource => side == Side::Sink,
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "both" | "two-way" | "twoway" => Some(Direction::Both),
            "source-to-sink" | "oneway" | "one-way" => Some(Direction::SourceToSink),
            "sink-to-source" => Some(Direction::SinkToSource),
            _ => None,
        }
    }
}

/// The platform's states, as the deployment names them.
///
/// Deployment-specific rather than platform-specific: which Linear state means
/// "finished" is a fact about a workspace, not about Linear. Names are matched
/// case-insensitively, because a human writing `Done` and the API returning `Done`
/// differ only in a way nobody means.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StateNames {
    /// Every name that means "finished". A list, because a workflow usually has
    /// more than one (`Done` and `Canceled`) and a mirror that knew only one of
    /// them would keep re-opening issues that were deliberately closed.
    pub closed: Vec<String>,
    /// Where a *newly created* mirrored issue lands. Linear's own default is
    /// `Backlog`; a mirrored issue that nobody planned lands in `Todo`.
    pub initial: Option<String>,
    /// The state to move an issue into when it is re-opened here.
    pub open: Option<String>,
}

impl StateNames {
    /// A closedness reading that is always defined: a state that is not configured
    /// as finished counts as open.
    ///
    /// Total on purpose. Leaving an unrecognised name undefined would make the two
    /// sides' content keys skip the state altogether, so a `Todo` issue and a
    /// `closed` one could look like the same revision - and the mirror would stop
    /// syncing state changes it could not name. Erring towards "open" is the safe
    /// direction: the worst case is an issue reopened that someone had closed with
    /// a status nobody configured.
    pub fn openness(&self, state: Option<&str>) -> Openness {
        match state {
            Some(state) if self.is_closed(state) => Openness::Closed,
            _ => Openness::Open,
        }
    }

    fn is_closed(&self, state: &str) -> bool {
        let state = state.trim();
        self.closed
            .iter()
            .any(|name| name.eq_ignore_ascii_case(state))
    }

    /// The name to write to make this side have the given openness.
    pub fn name_for(&self, openness: Openness) -> Option<&str> {
        match openness {
            Openness::Closed => self.closed.first().map(String::as_str),
            // A reopen has to land somewhere specific; the initial state is the
            // honest fallback when the deployment named no open state.
            Openness::Open => self.open.as_deref().or(self.initial.as_deref()),
        }
    }
}

/// The part of a state both sides can agree on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Openness {
    Open,
    Closed,
}

impl Openness {
    fn tag(self) -> &'static str {
        match self {
            Openness::Open => "open",
            Openness::Closed => "closed",
        }
    }
}

/// One side's authoritative state, as it was read from the platform.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    /// `None` when the platform says the entity is not there.
    pub fields: Option<IssueFields>,
    pub state: Option<String>,
}

impl Snapshot {
    pub fn present(fields: IssueFields, state: Option<String>) -> Self {
        Self {
            fields: Some(fields),
            state,
        }
    }

    /// The platform answered "gone" (a 404, or an empty result).
    pub fn gone() -> Self {
        Self::default()
    }

    pub fn exists(&self) -> bool {
        self.fields.is_some()
    }

    /// The identity of this revision: the fields plus the part of the state that
    /// the other side can recognise. Two sides that produce the same key already
    /// agree, and a side that produces the key a link recorded has not changed
    /// since the bridge last wrote across that link.
    pub fn key(&self, names: &StateNames) -> Option<String> {
        let fields = self.fields.as_ref()?;
        Some(content_key(fields, self.state.as_deref(), names))
    }
}

/// See [`Snapshot::key`].
pub fn content_key(fields: &IssueFields, state: Option<&str>, names: &StateNames) -> String {
    format!("{}|{}", fields.signature(), names.openness(state).tag())
}

/// What a mapping allows, in the terms the decision needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    pub direction: Direction,
    pub sync_issues: bool,
    pub git_automation: bool,
    pub delete_sync: bool,
    pub names: Sides<StateNames>,
}

impl Policy {
    /// Issue-level syncing is on, both for issues and for their comments.
    fn issues(&self) -> bool {
        self.sync_issues
    }
}

/// Why nothing happened.
///
/// Named rather than a bare `()`: "the delivery did nothing" is the outcome an
/// operator asks about, and a log line that says *why* is the difference between
/// a five-minute answer and a debugging session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Nothing {
    /// A kind this build does not mirror (a milestone, a project).
    NotOurKind,
    /// The entity is not part of a mirrored pair, and this event does not create
    /// one. Mirrors grow from a creation, never from an edit: creating on an edit
    /// would resurrect an entity someone deliberately unpaired.
    Unpaired,
    /// The change we are told about is the echo of our own write.
    Echo,
    /// The two sides already hold the same content.
    AlreadyEqual,
    /// The mapping does not mirror in this direction.
    Direction,
    /// The mapping has this class of syncing switched off.
    SwitchedOff,
    /// Nothing to carry: an empty comment, a reference with no URL.
    Empty,
    /// The platform cannot do this (a forge cannot attach an issue link).
    Unsupported,
}

/// What to do about one event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    Nothing(Nothing),
    Create {
        fields: IssueFields,
        state: Option<String>,
    },
    Update {
        fields: IssueFields,
        state: Option<String>,
    },
    Comment {
        body: String,
    },
    /// Edit the mirrored copy of a comment rather than posting a second one.
    UpdateComment {
        body: String,
    },
    /// Remove the mirrored copy of a comment.
    DeleteComment,
    Delete,
    Attach {
        url: String,
        title: String,
    },
}

impl Step {
    pub fn is_nothing(&self) -> bool {
        matches!(self, Step::Nothing(_))
    }
}

/// Everything the decision needs. A struct rather than a positional list: the
/// rules read as sentences about a situation, and a rule that needs one more fact
/// does not lengthen every call site.
#[derive(Clone, Copy, Debug)]
pub struct Context<'a> {
    pub event: &'a Event,
    /// Which end of the mapping this event arrived on.
    pub side: Side,
    pub policy: &'a Policy,
    /// The pair this entity is part of, if it has one.
    pub link: Option<&'a Link>,
    /// The pair a *comment* is part of, when the event is about a comment.
    ///
    /// A comment needs its own pairing: the issue's says where the copy lives, and
    /// this one says which comment it is - which is what an edit or a deletion has
    /// to address. Without it the only safe answer to an edit is to do nothing, and
    /// re-posting the text would duplicate it.
    pub comment_link: Option<&'a Link>,
    /// The entity as the platform that sent the event has it *now*. The payload is
    /// a snapshot from whenever the provider queued it; this is the truth.
    pub observed: &'a Snapshot,
    /// The same entity on the other platform, or `Snapshot::gone()`.
    pub counterpart: &'a Snapshot,
    /// The connector the other side of the mapping names.
    pub counterpart_connector: &'a ConnectorId,
}

/// The decision, and the whole of it.
pub fn plan(context: &Context<'_>) -> Step {
    match context.event.kind {
        EntityKind::Issue => plan_issue(context),
        EntityKind::Comment => plan_comment(context),
        EntityKind::Reference => plan_reference(context),
        EntityKind::Other(_) => Step::Nothing(Nothing::NotOurKind),
    }
}

fn plan_issue(context: &Context<'_>) -> Step {
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
            let fields = context
                .observed
                .fields
                .clone()
                .expect("checked that the entity exists");
            Step::Create {
                fields,
                // The *other* platform's vocabulary, not ours: this is the state
                // the new issue will have over there.
                state: policy.names.of(context.side.other()).initial.clone(),
            }
        }
        crate::domain::Action::Other(_) => Step::Nothing(Nothing::NotOurKind),
        _ => plan_change(context),
    }
}

fn plan_change(context: &Context<'_>) -> Step {
    let policy = context.policy;
    let Some(link) = context.link else {
        return Step::Nothing(Nothing::Unpaired);
    };
    if !context.counterpart.exists() {
        // The pair is broken: the other side lost the entity (deleted by hand, or
        // before this bridge existed). Re-creating it from an *edit* is how a
        // mirror resurrects things, so it does not happen here.
        return Step::Nothing(Nothing::Unpaired);
    }

    let observed_key = match context.observed.key(policy.names.of(context.side)) {
        Some(key) => key,
        None => return Step::Nothing(Nothing::Empty),
    };
    if link.last_synced_hash.as_deref() == Some(observed_key.as_str()) {
        // What the source holds is exactly what we last wrote across this link:
        // this event is our own write coming back, or an edit that changed nothing
        // we mirror.
        return Step::Nothing(Nothing::Echo);
    }

    let counterpart_key = context
        .counterpart
        .key(policy.names.of(context.side.other()));
    if counterpart_key.as_deref() == Some(observed_key.as_str()) {
        // Both sides already read the same, through the vocabulary they share.
        return Step::Nothing(Nothing::AlreadyEqual);
    }

    let fields = context
        .observed
        .fields
        .clone()
        .expect("checked that the entity exists");
    let state = state_to_write(context);
    Step::Update { fields, state }
}

/// The state to write on the other side, or `None` to leave it alone.
fn state_to_write(context: &Context<'_>) -> Option<String> {
    let ours = context.policy.names.of(context.side);
    let theirs = context.policy.names.of(context.side.other());
    let target = ours.openness(context.observed.state.as_deref());
    let current = theirs.openness(context.counterpart.state.as_deref());
    if current == target {
        // Already in the right kind of state: naming a specific one would move an
        // issue through the other platform's workflow for no reason.
        return None;
    }
    theirs.name_for(target).map(str::to_string)
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

fn plan_comment(context: &Context<'_>) -> Step {
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

fn plan_reference(context: &Context<'_>) -> Step {
    let policy = context.policy;
    if !policy.git_automation {
        return Step::Nothing(Nothing::SwitchedOff);
    }
    if !policy.direction.allows(context.side) {
        return Step::Nothing(Nothing::Direction);
    }
    if context.link.is_none() || !context.counterpart.exists() {
        // A referenced issue that is not mirrored stays unmirrored: a commit is not
        // a reason to create an issue.
        return Step::Nothing(Nothing::Unpaired);
    }
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
    Step::Attach { url, title }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Action, DeliveryId, EntityRef};

    fn connector(name: &str) -> ConnectorId {
        ConnectorId::new(name)
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
        }
    }

    fn policy() -> Policy {
        Policy {
            direction: Direction::Both,
            sync_issues: true,
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
        observed: Snapshot,
        counterpart: Snapshot,
        counterpart_connector: ConnectorId,
    }

    impl Default for Fixture {
        fn default() -> Self {
            Self {
                event: event(EntityKind::Issue, Action::Updated),
                policy: policy(),
                link: None,
                comment_link: None,
                observed: Snapshot::present(fields("One", &["bug"], 0), Some("In Progress".into())),
                counterpart: Snapshot::present(fields("One", &["bug"], 0), Some("open".into())),
                counterpart_connector: connector("forgejo"),
            }
        }
    }

    impl Fixture {
        fn plan(&self, side: Side) -> Step {
            plan(&Context {
                event: &self.event,
                side,
                policy: &self.policy,
                link: self.link.as_ref(),
                comment_link: self.comment_link.as_ref(),
                observed: &self.observed,
                counterpart: &self.counterpart,
                counterpart_connector: &self.counterpart_connector,
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
            Step::Create { fields, state } => {
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
            Step::Update { fields, state } => {
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
        };

        match fixture.plan(Side::Source) {
            Step::Attach { url, title } => {
                assert_eq!(url, "http://forge/pulls/4");
                assert_eq!(title, "Fix the thing");
            }
            other => panic!("expected an attachment, got {other:?}"),
        }
    }

    #[test]
    fn a_reference_to_an_unmirrored_issue_creates_nothing() {
        let mut fixture = Fixture::default();
        fixture.event.kind = EntityKind::Reference;
        fixture.event.detail = EventDetail::Reference {
            text: "Fix the thing".into(),
            closing_keywords: vec![],
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
        };

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
}
