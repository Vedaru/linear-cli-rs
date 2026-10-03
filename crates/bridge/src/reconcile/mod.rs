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
pub mod projection;
pub mod route;
pub mod skipped_log;
pub mod survey;
pub mod sweep;

pub use route::{Entity, Identity, Location, Origin, Placement, Route, Routes};
pub use survey::{Action, Entry, Survey};

use std::collections::BTreeMap;

use crate::domain::{
    markers, Actor, Capabilities, Change, ConnectorId, EntityKind, EntityRef, Event, EventDetail,
    IssueFields, Patch,
};
use crate::store::Link;

use self::projection::{Projected, Skipped};

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

    /// The text here is text *this service* wrote, arriving back as the other
    /// platform's own event.
    ///
    /// The marker is the only identity a copy carries that survives the trip on its
    /// own: a pair adopted by its marker rather than written by us has no link row
    /// (see `sweep::find_by_marker`), and the comment path has always relied on this
    /// rather than on a link. Asking it here, once, where every create is decided, is
    /// what stops a mirror from copying its own copies - each copy is a fresh entity,
    /// so the next delivery would create another, and the loop has no fixed point.
    pub fn is_own_write(&self) -> bool {
        self.fields
            .as_ref()
            .is_some_and(|fields| markers::has_marker(&fields.body))
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
    content_key_with(fields, names.openness(state))
}

/// The same key, from an openness that has already been read out of one platform's
/// vocabulary - which is how a projection is compared against the platform it is
/// projected onto.
pub fn content_key_with(fields: &IssueFields, openness: Openness) -> String {
    format!("{}|{}", fields.signature(), openness.tag())
}

/// What a mapping allows, in the terms the decision needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    pub direction: Direction,
    pub sync_issues: bool,
    /// Whether *projects* - the container of issues - are mirrored. Off unless a
    /// deployment asks for it, the same way issue syncing is a switch: a mirror that
    /// suddenly started copying containers when it was configured for their contents
    /// is a surprise nobody wants.
    pub sync_projects: bool,
    pub git_automation: bool,
    pub delete_sync: bool,
    pub names: Sides<StateNames>,
    /// What a mirrored *board* calls the states the source names: a card's column, keyed
    /// by the source's own state name.
    ///
    /// Empty is the normal case and means the mapping has no opinion about columns - a
    /// card keeps whichever column it already has. Keyed by name because a board is
    /// *finer* than the openness the two sides share: `To Do` and `In Progress` are both
    /// open, so the vocabulary that exists for states cannot express a column.
    pub columns: BTreeMap<String, String>,
}

impl Policy {
    /// The column a card belongs in when the source names this state, if the mapping says.
    ///
    /// Case-insensitive, like every other state name comparison here.
    pub fn column_for(&self, state: Option<&str>) -> Option<&str> {
        let state = state?.trim();
        for (name, column) in &self.columns {
            if name.eq_ignore_ascii_case(state) {
                return Some(column.as_str());
            }
        }
        None
    }
}

impl Policy {
    /// Issue-level syncing is on, both for issues and for their comments.
    fn issues(&self) -> bool {
        self.sync_issues
    }

    /// Project-level syncing, which is its own switch.
    fn projects(&self) -> bool {
        self.sync_projects
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
        /// The source's fields as the target will hold them.
        fields: IssueFields,
        state: Option<String>,
        /// The board column this issue belongs in, when the mapping names one for the
        /// state the source holds. `None` means the mapping has no opinion, and a card
        /// keeps whichever column it has.
        column: Option<String>,
        /// Fields the target could not be given, for the log.
        skipped: Vec<Skipped>,
    },
    Update {
        /// Only the fields that differ - what is actually sent.
        patch: Patch,
        /// What the target should hold once the patch lands, which is what the link
        /// records as synced. The whole set, not the patch: the hash has to describe
        /// the resulting revision, or the next delivery reads as a difference.
        fields: IssueFields,
        state: Option<String>,
        /// The board column this issue belongs in; see [`Step::Create`].
        column: Option<String>,
        skipped: Vec<Skipped>,
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
    /// Move a card to another column of a board it is already on.
    ///
    /// Its own step rather than a field of an update, because placement is its own
    /// request: a board's column is not part of an issue's fields, and a sweep that finds
    /// a card where the mapping says it should not be has exactly one thing to say.
    Place {
        /// The project (the board) the card is on, as the sink names it.
        project: String,
        /// The column the mapping names for the issue's state.
        column: String,
    },
    Attach {
        url: String,
        title: String,
        /// The issue to attach it to. A reference names an issue in text, and the
        /// attachment belongs on *that* issue - not on its mirror, which may not even
        /// exist. Carrying the target is what makes the step independent of the link.
        target: EntityRef,
        /// The state the named issue moves to, when the reference is a review request
        /// opening or merging. Attaching and moving travel together because they are one
        /// event's consequence, and a reference always attaches - the url is what it is.
        transition: Option<String>,
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
    /// The issue a *reference* event names, resolved by the handler against the platform
    /// the identifier belongs to.
    ///
    /// A commit or pull request says "fixes VED-1": the issue is on the side the team key
    /// names, which may be either end of the mapping, and it need not be mirrored at all.
    /// Resolving it here - rather than inferring it from a link - is what lets a reference
    /// travel in either direction.
    /// Borrowed, like the links: the handler owns it for the length of the call.
    pub reference_target: Option<&'a EntityRef>,
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
    /// The source's fields as the *other* platform will hold them.
    ///
    /// Every field decision is made on this rather than on the raw source: a field
    /// the target cannot hold is not a difference, it is a permanent one.
    pub expected: &'a Projected,
    /// What the other platform can hold. Needed for one encoding rule that is not a
    /// fact about either platform: where the priority travels inside the label set,
    /// changing it means rewriting that set.
    pub target: &'a Capabilities,
}

/// The decision, and the whole of it.
pub fn plan(context: &Context<'_>) -> Step {
    match context.event.kind {
        EntityKind::Issue => plan_issue(context),
        EntityKind::Comment => plan_comment(context),
        EntityKind::Reference => plan_reference(context),
        EntityKind::Project => plan_project(context),
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

/// The pair a convergence question is asked about.
///
/// A delivery reaches this through an event; a sweep has no event at all, so this is
/// where the two meet: after the event, before any I/O.
pub struct Pairwise<'a> {
    /// The side whose revision wins.
    pub side: Side,
    pub policy: &'a Policy,
    pub observed: &'a Snapshot,
    pub counterpart: &'a Snapshot,
    /// The winner's fields as the *other* side will hold them.
    pub expected: &'a Projected,
    /// What the other side can hold.
    pub target: &'a Capabilities,
}

/// What converging one pair does - the sweep's entry point.
///
/// The difference from a delivery is the missing counterpart: a delivery refuses to
/// re-create what the other side deleted, and a sweep does it, because "these two
/// should agree" is exactly what it was asked.
pub fn converge(pair: &Pairwise<'_>, recorded: Option<&str>) -> Step {
    if !pair.observed.exists() {
        return Step::Nothing(Nothing::Unpaired);
    }
    if !pair.counterpart.exists() {
        return create_step(pair);
    }
    // `recorded` is `None` for a pair adopted by its marker: nothing was ever written
    // across it, so no side can be "the echo of our own write" and the two are compared
    // on their content alone.
    change_step(pair, recorded)
}

/// The one place an issue copy is created.
///
/// Both entry points - a delivery (via `plan_issue`) and a sweep (via `converge`) -
/// arrive at "there is no counterpart here, so make one", and they arrive the same
/// way, so the question that must be asked before *any* create is asked once, here.
///
/// The question is not "is this paired?" - both callers have already asked that, and
/// a pair adopted by its marker has no link row to find. It is "is the side we would
/// copy *from* our own writing?", which only the text can answer.
fn create_step(pair: &Pairwise<'_>) -> Step {
    if pair.observed.is_own_write() {
        // No link, but the text is ours: this is a copy of a copy, and creating from
        // it is how a mirror runs away - each generation is a fresh entity, so the
        // next delivery has nothing to match against either. Refusing is the only
        // stable answer, and saying so (`Echo`) is better than a silent skip.
        return Step::Nothing(Nothing::Echo);
    }
    Step::Create {
        fields: pair.expected.fields.clone(),
        // The *other* platform's vocabulary, not ours: this is the state the new
        // issue will have over there.
        state: pair.policy.names.of(pair.side.other()).initial.clone(),
        column: pair
            .policy
            .column_for(pair.observed.state.as_deref())
            .map(str::to_string),
        skipped: pair.expected.skipped.clone(),
    }
}

/// What to change on the other side, given a pair that exists on both.
fn change_step(pair: &Pairwise<'_>, recorded: Option<&str>) -> Step {
    let policy = pair.policy;
    // The key is taken through the projection, in the *target's* openness: what
    // matters is not what the source says but what the target can be brought to
    // say. Compared raw, a field the target cannot hold (an unmapped assignee, a
    // due date on a platform without them) differs on every single delivery - the
    // bridge rewriting the same content forever is what that looks like.
    let ours = policy.names.of(pair.side);
    let expected_key = content_key_with(
        &pair.expected.fields,
        ours.openness(pair.observed.state.as_deref()),
    );
    if recorded == Some(expected_key.as_str()) {
        // What the source holds is exactly what we last wrote across this link:
        // this event is our own write coming back, or an edit that changed nothing
        // we mirror.
        return Step::Nothing(Nothing::Echo);
    }

    let counterpart_key = pair.counterpart.key(policy.names.of(pair.side.other()));
    if counterpart_key.as_deref() == Some(expected_key.as_str()) {
        // Both sides already read the same, through the vocabulary they share.
        return Step::Nothing(Nothing::AlreadyEqual);
    }

    let counterpart_fields = pair
        .counterpart
        .fields
        .clone()
        .expect("checked that the counterpart exists");
    let mut patch = pair.expected.fields.diff(&counterpart_fields);
    if !pair.target.priorities && !patch.priority.is_leave() {
        // The priority lives in the label set on this platform, so a new priority is
        // a new label set. Sending the patches separately would leave the labels
        // alone and drop the priority label with them.
        patch.labels = Change::Set(pair.expected.fields.canonical_labels());
    }
    // The state is a separate question from the fields: a close with no text change
    // has nothing to patch and still has to travel.
    let state = state_to_write(pair);
    if patch.is_empty() && state.is_none() {
        // Nothing the target can hold differs, whatever the raw comparison said.
        return Step::Nothing(Nothing::AlreadyEqual);
    }

    Step::Update {
        patch,
        fields: pair.expected.fields.clone(),
        state,
        column: pair
            .policy
            .column_for(pair.observed.state.as_deref())
            .map(str::to_string),
        skipped: pair.expected.skipped.clone(),
    }
}

/// The state to write on the other side, or `None` to leave it alone.
fn state_to_write(pair: &Pairwise<'_>) -> Option<String> {
    let ours = pair.policy.names.of(pair.side);
    let theirs = pair.policy.names.of(pair.side.other());
    let target = ours.openness(pair.observed.state.as_deref());
    let current = theirs.openness(pair.counterpart.state.as_deref());
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

/// The decision for a *project*: title and description only.
///
/// A project is not an issue with fewer fields - it has no labels, priority, due
/// date, assignee or workflow state that both platforms share - so it gets its own
/// decision rather than a special case threaded through the issue one. What it does
/// share is the shape: a create is mirrored from either side, an edit converges onto
/// the other, and a pair is proven by the marker in the copy's description.
fn plan_project(context: &Context<'_>) -> Step {
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

#[cfg(test)]
mod tests;
