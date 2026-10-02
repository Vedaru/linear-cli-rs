//! Executing a plan: read both sides, write the other one, record what was written.
//!
//! The handler owns no opinion. Everything it does is `plan`'s answer plus the I/O
//! to carry it out, and the one thing it *must* get right is the bookkeeping: the
//! link row's content key is what makes the echo of this very write recognisable
//! when it comes back as a webhook a second later.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::connector::Source;
use crate::domain::{
    markers, parse_connector_ref, references, Capabilities, Change, ConnectorId, EntityKind,
    EntityRef, Event, EventDetail, IssueFields, Patch, UserMap,
};
use crate::error::{Error, Result};
use crate::queue::Handler;
use crate::reconcile::projection::{Projected, Projection, Skipped};
use crate::reconcile::survey::{Action, Entry, Survey};
use crate::reconcile::sweep::{self, Found};
use crate::reconcile::{
    content_key, converge, plan, Context, Direction, Nothing, Openness, Pairwise, Policy, Side,
    Sides, Snapshot, StateNames, Step,
};
use crate::sink::{RemoteIssue, Sink};
use crate::store::{Delivery, Link, ReferenceLink, Store};

/// One end of a mapping: a platform, and the container inside it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub connector: ConnectorId,
    /// A team key, a `owner/name`, whatever the platform calls the container.
    pub scope: String,
}

impl Endpoint {
    /// `connector:scope`.
    pub fn parse(reference: &str) -> Result<Self> {
        let (connector, scope) = parse_connector_ref(reference)
            .map_err(|error| Error::Config(format!("mapping endpoint `{reference}`: {error}")))?;
        Ok(Self {
            connector,
            scope: scope.to_string(),
        })
    }

    pub fn describe(&self) -> String {
        format!("{}:{}", self.connector, self.scope)
    }
}

/// A pairing the reconciler acts on.
#[derive(Clone, Debug)]
pub struct Mapping {
    pub name: String,
    pub source: Endpoint,
    pub sink: Endpoint,
    pub policy: Policy,
    /// How a person is known on each platform. Empty is meaningful: it means the
    /// deployment has not said, so assignee sync is off rather than guessed.
    pub users: UserMap,
}

impl Mapping {
    /// The end an event arrived on, if it arrived on one of them.
    ///
    /// Scope matters as much as the connector: one Linear workspace and one forge
    /// can be paired several times over (per team, per repository), and a mapping
    /// that ignored the scope would mirror the wrong repository's issues.
    pub fn side_of(&self, event: &Event) -> Option<Side> {
        // A scope the platform did not report is not a mismatch: a Linear comment
        // payload names the issue but not the team, and the link - not the scope -
        // is what disambiguates when several mappings share a connector.
        let scope_matches = |endpoint: &Endpoint| {
            event
                .subject
                .scope
                .as_deref()
                .is_none_or(|scope| scope.eq_ignore_ascii_case(&endpoint.scope))
        };
        if event.connector == self.source.connector && scope_matches(&self.source) {
            Some(Side::Source)
        } else if event.connector == self.sink.connector && scope_matches(&self.sink) {
            Some(Side::Sink)
        } else {
            None
        }
    }

    fn endpoint(&self, side: Side) -> &Endpoint {
        match side {
            Side::Source => &self.source,
            Side::Sink => &self.sink,
        }
    }
}

/// The reconciler as the queue sees it: one delivery in, one outcome out.
pub struct ReconcileHandler {
    sources: BTreeMap<ConnectorId, Arc<dyn Source>>,
    sinks: BTreeMap<ConnectorId, Arc<dyn Sink>>,
    mappings: Vec<Mapping>,
    store: Box<dyn Store>,
}

/// The pair a step is about, in the terms carrying it out needs.
struct Pair<'a> {
    /// The mapping's name, for the log an operator reads.
    mapping: &'a str,
    /// The end being written to.
    there: &'a Endpoint,
    /// That end's state vocabulary.
    names_there: &'a StateNames,
    /// The entity the step was decided from, with its scope filled in.
    subject: &'a EntityRef,
    /// The entity on the other end, when there is one yet.
    counterpart: Option<&'a EntityRef>,
    /// A comment's own pairing, when the step is about a comment.
    comment: Option<&'a EntityRef>,
    comment_link: Option<&'a Link>,
    /// The other end's state as the platform reports it, for the recorded revision.
    counterpart_state: Option<&'a str>,
    /// Whether the end being written to is the mapping's *sink*, which is the only
    /// end that holds an issue's container. False for everything else, so a project a
    /// human set on the source platform is never touched by a step writing back.
    project_mirroring: bool,
}

impl ReconcileHandler {
    /// Build the handler, refusing a mapping it could never carry out.
    ///
    /// A mapping whose platform has no write half is a configuration mistake, and
    /// discovering it on the first webhook means discovering it in production.
    pub fn new(
        sources: Vec<Arc<dyn Source>>,
        sinks: Vec<Arc<dyn Sink>>,
        mappings: Vec<Mapping>,
        store: Box<dyn Store>,
    ) -> Result<Self> {
        let sources: BTreeMap<ConnectorId, Arc<dyn Source>> = sources
            .into_iter()
            .map(|source| (source.id().clone(), source))
            .collect();
        let sinks: BTreeMap<ConnectorId, Arc<dyn Sink>> = sinks
            .into_iter()
            .map(|sink| (sink.id().clone(), sink))
            .collect();

        for mapping in &mappings {
            for endpoint in [&mapping.source, &mapping.sink] {
                if !sources.contains_key(&endpoint.connector) {
                    return Err(Error::Config(format!(
                        "mapping `{}` names platform `{}`, which is not configured",
                        mapping.name, endpoint.connector
                    )));
                }
                let sink = sinks.get(&endpoint.connector).ok_or_else(|| {
                    Error::Config(format!(
                        "mapping `{}` needs to read `{}` (to see what is already there), but that platform has no write half: add a `[sink]` to its preset",
                        mapping.name, endpoint.connector
                    ))
                })?;
                if sink.capabilities().deletion && mapping.policy.delete_sync {
                    // Not a mistake, but worth saying out loud: the platform claims
                    // it can observe deletions, so the switch will act on them.
                    log::debug!(
                        "mapping `{}` mirrors deletions into `{}`",
                        mapping.name,
                        endpoint.connector
                    );
                }
            }
            if mapping.policy.direction != Direction::Both {
                log::info!(
                    "mapping `{}` mirrors {}",
                    mapping.name,
                    match mapping.policy.direction {
                        Direction::SourceToSink => "one way (source -> sink)",
                        Direction::SinkToSource => "one way (sink -> source)",
                        Direction::Both => unreachable!("checked above"),
                    }
                );
            }
        }

        Ok(Self {
            sources,
            sinks,
            mappings,
            store,
        })
    }

    /// Apply one event. Every mapping that claims it gets a chance, in order.
    fn apply(&mut self, event: &Event) -> Result<()> {
        let mut claimed = false;
        for index in 0..self.mappings.len() {
            let Some(side) = self.mappings[index].side_of(event) else {
                continue;
            };
            claimed = true;
            self.reconcile(index, side, event)?;
        }
        if !claimed {
            // Normal and not an error: a webhook for a repository nobody mapped, or
            // an entity outside any mapping's scope. Logged because "why is nothing
            // syncing" is answered by this line.
            log::debug!(
                "no mapping claims {} {} in scope `{}`",
                event.connector,
                event.event,
                event.subject.scope.as_deref().unwrap_or("-")
            );
        }
        Ok(())
    }

    fn reconcile(&mut self, index: usize, side: Side, event: &Event) -> Result<()> {
        let mapping = self.mappings[index].clone();
        let here = mapping.endpoint(side).clone();
        let there = mapping.endpoint(side.other()).clone();

        // A platform may not say which container the event came from (a Linear
        // comment payload names the issue but not the team). The mapping does know,
        // and a link is looked up by identity - so the scope is filled in from the
        // mapping rather than left absent, which would make every pairing invisible.
        let subject = match event.subject.scope {
            Some(_) => event.subject.clone(),
            None => event.subject.clone().with_scope(here.scope.clone()),
        };

        // A comment needs its own pairing, and it is looked up from the comment's
        // id - which the delivery carries in the detail, because the subject is the
        // issue the comment is on.
        let comment_ref = comment_reference(event, &subject);
        let comment_link = match &comment_ref {
            Some(comment) => self.store.find_link(comment, &there.connector)?,
            None => None,
        };

        // A reference event's subject is the *pull request or commit*, not the issue its
        // text names - so the issue has to be resolved from the text. Without this, a
        // commit saying "fixes VED-1" is looked up as an entity nobody links, the planner
        // correctly decides there is nothing it can do, and the attachment this feature
        // exists for is never made.
        let reference_target = if event.kind == EntityKind::Reference {
            self.named_issue(event)?
        } else {
            None
        };

        // The pairing (if any), and the authoritative state of both ends. The
        // payload is a snapshot from whenever the provider queued it; these reads
        // are what the decision is actually made on.
        let link = self
            .store
            .find_link(&subject, &there.connector)?
            .filter(|link| link.pairs(&subject, &there.connector));
        let counterpart_ref = link
            .as_ref()
            .and_then(|link| link.counterpart(&subject))
            .cloned();

        let observed = self.snapshot(
            &subject.kind,
            &here.connector,
            &here.scope,
            &subject.native_id,
        )?;
        let mut counterpart = match &counterpart_ref {
            Some(reference) => self.snapshot(
                &reference.kind,
                &there.connector,
                &there.scope,
                &reference.native_id,
            )?,
            None => Snapshot::gone(),
        };

        // What the target can hold, and where an identity has no counterpart. Read
        // from the target's own capabilities rather than assumed, and computed before
        // the decision because it *is* the decision's input: the mirror compares what
        // it can bring into agreement, not what the source happens to say.
        let target = self.sink(&there.connector)?.capabilities();
        let projection = Projection::new(&target, &mapping.users);
        let mut expected = match &observed.fields {
            Some(fields) => projection.of(fields, &here.connector, &there.connector),
            None => Projected::default(),
        };

        // A container is a field of the issue mirror, and only the mapping's *sink*
        // is asked to hold it: the source names a project in its own ids, and the
        // pairing says which project on the sink that is. Any other direction leaves
        // the field out of the comparison, so a project a human set on the source
        // platform is never mistaken for a difference the mirror has to clear.
        let mirror_project =
            subject.kind == EntityKind::Issue && there.connector == mapping.sink.connector;
        if mirror_project {
            let named = observed
                .fields
                .as_ref()
                .and_then(|fields| fields.project.clone());
            expected.fields.project = self.project_on(&here, &there.connector, named.as_deref())?;
            // A forge reports an issue's project nowhere, so the target's current
            // value for this one field is what the pairing recorded when it last
            // placed the issue. Without it the diff could neither stop re-placing on
            // every edit nor see a project cleared.
            if let Some(project) = link.as_ref().and_then(|link| link.project.clone()) {
                if let Some(fields) = counterpart.fields.as_mut() {
                    if fields.project.is_none() {
                        fields.project = Some(project);
                    }
                }
            }
        } else {
            // The other direction does not hold the container, so its project is not
            // a difference either: cleared on both sides, the diff leaves it alone.
            expected.fields.project = None;
            if let Some(fields) = counterpart.fields.as_mut() {
                fields.project = None;
            }
        }

        let step = plan(&Context {
            event,
            side,
            policy: &mapping.policy,
            link: link.as_ref(),
            reference_target: reference_target.as_ref(),
            comment_link: comment_link.as_ref(),
            observed: &observed,
            counterpart: &counterpart,
            counterpart_connector: &there.connector,
            expected: &expected,
            target: &target,
        });

        if let Step::Nothing(reason) = &step {
            log::debug!(
                "{} {} {}/{}: {}",
                event.connector,
                event.action.as_str(),
                event.subject.scope.as_deref().unwrap_or("-"),
                event.subject.native_id,
                describe_nothing(*reason)
            );
            return Ok(());
        }

        let names_there = mapping.policy.names.of(side.other());
        if let Some(reference) = &counterpart_ref {
            log::debug!(
                "{} {} {} <-> {}",
                mapping.name,
                event.action.as_str(),
                event.subject.native_id,
                reference.native_id
            );
        }

        self.carry_out(
            &Pair {
                mapping: &mapping.name,
                there: &there,
                names_there,
                subject: &subject,
                counterpart: counterpart_ref.as_ref(),
                comment: comment_ref.as_ref(),
                comment_link: comment_link.as_ref(),
                counterpart_state: counterpart.state.as_deref(),
                project_mirroring: mirror_project,
            },
            step,
        )
    }

    /// Carry out one pair's step.
    ///
    /// A delivery and a sweep arrive at their steps differently - one from an event,
    /// the other from comparing both sides against what was last written across the
    /// link - and everything after that point is the same, so it happens here once.
    fn carry_out(&mut self, pair: &Pair<'_>, step: Step) -> Result<()> {
        match step {
            Step::Nothing(_) => unreachable!("returned above"),
            Step::Create {
                fields,
                state,
                skipped,
            } => {
                let fields = stamped(fields, pair.subject);
                self.report_skipped(pair.mapping, &skipped);
                let sink = self.sink(&pair.there.connector)?;
                let kind = pair.subject.kind.clone();
                let created = match kind {
                    EntityKind::Project => {
                        sink.create_project(&pair.there.scope, &fields, state.as_deref())?
                    }
                    _ => sink.create_issue(&pair.there.scope, &fields, state.as_deref())?,
                };
                let created_ref = EntityRef {
                    connector: pair.there.connector.clone(),
                    kind,
                    scope: Some(pair.there.scope.clone()),
                    native_id: created.id.clone(),
                    url: created.url.clone(),
                };
                // What the far side will hold once this settles: the fields we sent
                // and the state it ended up in - which is the state we asked for, or
                // the one a fresh issue starts in when we asked for none.
                let effective = state.clone().or_else(|| {
                    pair.names_there
                        .name_for(Openness::Open)
                        .map(str::to_string)
                });
                let hash = content_key(&fields, effective.as_deref(), pair.names_there);
                self.store.upsert_link(
                    &Link::new(pair.subject.clone(), created_ref.clone()).with_hash(hash),
                )?;
                // A container is a field of the issue, so a newly created issue whose
                // source named a paired project also lands on that project's board.
                self.place_on_project(pair, &created_ref, fields.project.as_deref())?;
                log::info!(
                    "created {} {} for {} {}",
                    pair.there.connector,
                    created.id,
                    pair.subject.connector,
                    pair.subject.native_id
                );
            }
            Step::Update {
                patch,
                fields,
                state,
                skipped,
            } => {
                let patch = stamped_patch(patch, pair.subject);
                let Some(reference) = pair.counterpart.cloned() else {
                    return Ok(());
                };
                self.report_skipped(pair.mapping, &skipped);
                let touched = patch.touched().join(", ");
                let sink = self.sink(&pair.there.connector)?;
                match pair.subject.kind {
                    EntityKind::Project => sink.update_project(
                        &pair.there.scope,
                        &reference.native_id,
                        &patch,
                        &fields,
                    )?,
                    _ => sink.update_issue(
                        &pair.there.scope,
                        &reference.native_id,
                        &patch,
                        &fields,
                        state.as_deref(),
                    )?,
                }
                // The issue's container travels with the same patch: a project that
                // changed puts the issue on the new board (which moves it, as an
                // issue sits on one project), and one that was cleared takes it off.
                self.settle_project(pair, &reference, &patch, &fields)?;
                // The link records the revision the target now holds - the projected
                // fields, not the raw ones. Recording the source's own truth is how a
                // field the target cannot hold turns into a difference forever.
                let effective = state
                    .clone()
                    .or_else(|| pair.counterpart_state.map(str::to_string));
                let hash = content_key(&fields, effective.as_deref(), pair.names_there);
                self.store.upsert_link(
                    &Link::new(pair.subject.clone(), reference.clone()).with_hash(hash),
                )?;
                log::info!(
                    "updated {} {} from {} {} ({})",
                    pair.there.connector,
                    reference.native_id,
                    pair.subject.connector,
                    pair.subject.native_id,
                    touched
                );
            }
            Step::Comment { body } => {
                let Some(reference) = pair.counterpart.cloned() else {
                    return Ok(());
                };
                let sink = self.sink(&pair.there.connector)?;
                let created = sink.comment(&pair.there.scope, &reference.native_id, &body)?;
                // The comment gets its own pairing, and deliberately no content key:
                // a comment is not part of the issue's revision, so recording it as
                // one would make the next issue edit look like a change. What the
                // pairing buys is the next event about *this comment*: its edit has
                // somewhere to go, and its deletion something to remove.
                if let Some(comment) = pair.comment {
                    let mirrored = EntityRef {
                        connector: pair.there.connector.clone(),
                        kind: EntityKind::Comment,
                        scope: Some(pair.there.scope.clone()),
                        native_id: created.id.clone(),
                        url: created.url.clone(),
                    };
                    self.store
                        .upsert_link(&Link::new(comment.clone(), mirrored))?;
                }
                log::info!(
                    "mirrored comment {} -> {} {}",
                    pair.subject.native_id,
                    pair.there.connector,
                    created.id
                );
            }
            Step::UpdateComment { body } => {
                let Some(mirrored) = self.mirrored_comment(pair.comment_link, pair.comment) else {
                    return Ok(());
                };
                let sink = self.sink(&pair.there.connector)?;
                sink.update_comment(&pair.there.scope, &mirrored.native_id, &body)?;
                log::info!(
                    "mirrored comment edit {} -> {} {}",
                    pair.subject.native_id,
                    pair.there.connector,
                    mirrored.native_id
                );
            }
            Step::DeleteComment => {
                if let Some(mirrored) = self.mirrored_comment(pair.comment_link, pair.comment) {
                    let sink = self.sink(&pair.there.connector)?;
                    sink.delete_comment(&pair.there.scope, &mirrored.native_id)?;
                    log::info!(
                        "deleted {} comment {} (mirroring {} {})",
                        pair.there.connector,
                        mirrored.native_id,
                        pair.subject.connector,
                        pair.subject.native_id
                    );
                }
                if let Some(comment) = pair.comment {
                    // The copy is gone, so the pairing is: keeping it would make a
                    // later comment with the same id edit something that is not pair.there.
                    self.store.delete_links(comment)?;
                }
            }
            Step::Delete => {
                if let Some(reference) = pair.counterpart {
                    let sink = self.sink(&pair.there.connector)?;
                    sink.delete_issue(&pair.there.scope, &reference.native_id)?;
                    log::info!(
                        "deleted {} {} (mirroring {} {})",
                        pair.there.connector,
                        reference.native_id,
                        pair.subject.connector,
                        pair.subject.native_id
                    );
                }
                // The pairing is gone with the entity: keeping it would make a later
                // re-creation resume a stale pairing instead of starting clean.
                self.store.delete_links(pair.subject)?;
            }
            Step::Attach {
                url,
                title,
                target,
                transition,
            } => {
                // On the target's own side, by the target's own id: the attachment goes on
                // the issue the reference named.
                let scope = target.scope.clone().unwrap_or_default();

                // Carried out once per reference, not once per delivery. A force-push re-sends
                // the same commit message under a new delivery id, so the intake's replay log -
                // keyed by delivery - cannot see it; this is the record that can. The insert is
                // the guard: two workers on the same reference cannot both be told it is new.
                let reference = ReferenceLink {
                    source: pair.subject.clone(),
                    target: target.clone(),
                    url: Some(url.clone()),
                };
                // Whether this reference is new is a fact about the store, taken before a sink
                // is borrowed around it.
                let carried_out = self
                    .store
                    .reference_exists(&reference.source, &reference.target)?;
                let sink = self.sink(&target.connector)?;
                if carried_out {
                    log::debug!(
                        "{} is already attached to {} {}",
                        url,
                        target.connector,
                        target.native_id
                    );
                } else {
                    sink.attach(&scope, &target.native_id, &url, &title)?;
                    log::info!(
                        "attached {} to {} {}",
                        url,
                        target.connector,
                        target.native_id
                    );
                }
                // And, when the reference is a review request, the move that goes with it.
                // Through `transition` rather than a full update with one field in it: it is
                // the operation a state-only move has, and a preset that can move an issue but
                // not be updated wholesale still gets this step.
                if let Some(state) = transition {
                    sink.transition(&scope, &target.native_id, &state)?;
                    log::info!(
                        "moved {} {} to {}",
                        target.connector,
                        target.native_id,
                        state
                    );
                }

                // Recorded last, and only once the platform has accepted the write: recording
                // first would make a failure in between look like an attachment that exists,
                // and nothing would ever repair it. This way the worst case is a second
                // attachment - visible, and deletable by hand. The record deliberately does
                // not guard the move above: a merge is the same reference as the open that
                // attached it, so "already carried out" must not stop it.
                if !carried_out {
                    self.store.record_reference(&reference)?;
                }
            }
        }
        Ok(())
    }

    /// The comment this event is about, as an entity - `None` for anything else.
    ///
    /// A comment delivery is *about* its issue (that is the subject a link pairs)
    /// and carries the comment's own id in the detail, so the comment's identity has
    /// to be assembled from both.
    fn mirrored_comment(
        &self,
        link: Option<&Link>,
        comment: Option<&EntityRef>,
    ) -> Option<EntityRef> {
        link?.counterpart(comment?).cloned()
    }

    /// Read both ends of a mapping and work out what a sweep would do.
    ///
    /// Reads only. Every decision belongs to `sweep` and the reconciler's own planner,
    /// which are pure - so the plan an operator reads is the plan that would run, and
    /// carrying it out is not a second decision. That is what makes a dry run worth
    /// trusting.
    pub fn survey(&mut self, index: usize) -> Result<Survey> {
        let mapping = self.mappings[index].clone();
        let issues_source = self.list_end(&mapping.source, &EntityKind::Issue)?;
        let issues_sink = self.list_end(&mapping.sink, &EntityKind::Issue)?;

        // Projects are a second collection behind their own switch. A deployment that
        // never asked for them must not so much as list them, or a sweep would start
        // proposing project writes it was never configured for.
        let (projects_source, projects_sink) = if mapping.policy.sync_projects {
            (
                self.list_end(&mapping.source, &EntityKind::Project)?,
                self.list_end(&mapping.sink, &EntityKind::Project)?,
            )
        } else {
            (Vec::new(), Vec::new())
        };

        // Links are gathered from every entity either sweep will judge: an issue link
        // and a project link live in the same table, keyed by identity, so the one
        // lookup serves both.
        let mut found: Vec<Found> = Vec::new();
        found.extend(issues_source.iter().cloned());
        found.extend(issues_sink.iter().cloned());
        found.extend(projects_source.iter().cloned());
        found.extend(projects_sink.iter().cloned());
        let links = self.links_among(&found)?;

        let source_caps = self.sink(&mapping.source.connector)?.capabilities();
        let sink_caps = self.sink(&mapping.sink.connector)?.capabilities();

        let mut survey = Survey {
            mapping: mapping.name.clone(),
            source: describe_end(&mapping.source),
            sink: describe_end(&mapping.sink),
            entries: Vec::new(),
        };
        for pairing in sweep::pair_up(
            &issues_source,
            &issues_sink,
            &mapping.sink.connector,
            &links,
        ) {
            survey
                .entries
                .push(self.judge(&mapping, &pairing, &source_caps, &sink_caps)?);
        }
        for pairing in sweep::pair_up(
            &projects_source,
            &projects_sink,
            &mapping.sink.connector,
            &links,
        ) {
            survey
                .entries
                .push(self.judge(&mapping, &pairing, &source_caps, &sink_caps)?);
        }
        Ok(survey)
    }

    /// Carry out a survey. The only place a sweep writes anything.
    pub fn apply_survey(&mut self, index: usize, survey: &Survey) -> Result<usize> {
        let mapping = self.mappings[index].clone();
        let mut written = 0;
        for entry in &survey.entries {
            match &entry.step {
                Some(step) => {
                    let there = match entry.side {
                        Side::Source => &mapping.sink,
                        Side::Sink => &mapping.source,
                    };
                    self.carry_out(
                        &Pair {
                            mapping: &mapping.name,
                            there,
                            names_there: mapping.policy.names.of(entry.side.other()),
                            subject: &entry.subject,
                            counterpart: entry.counterpart.as_ref(),
                            comment: None,
                            comment_link: None,
                            counterpart_state: entry.counterpart_state.as_deref(),
                            project_mirroring: there.connector == mapping.sink.connector,
                        },
                        step.clone(),
                    )?;
                    written += 1;
                }
                // Nothing to write - but a pair adopted by its marker still gets a
                // record, so the next sweep has a baseline instead of asking "who
                // moved?" about two sides it cannot date.
                None => {
                    if let (Some(counterpart), Some(record)) = (&entry.counterpart, &entry.record) {
                        self.store.upsert_link(
                            &Link::new(entry.subject.clone(), counterpart.clone())
                                .with_hash(record.clone()),
                        )?;
                    }
                }
            }
        }
        Ok(written)
    }

    /// Everything one end of a mapping holds, of one kind.
    fn list_end(&self, end: &Endpoint, kind: &EntityKind) -> Result<Vec<Found>> {
        let sink = self.sink(&end.connector)?;
        let remote = match kind {
            EntityKind::Project => sink.list_projects(&end.scope)?,
            _ => sink.list_issues(&end.scope)?,
        };
        Ok(remote
            .into_iter()
            .map(|issue| {
                Found::new(
                    EntityRef {
                        connector: end.connector.clone(),
                        kind: kind.clone(),
                        scope: Some(end.scope.clone()),
                        native_id: issue.reference.id.clone(),
                        url: issue.reference.url.clone(),
                    },
                    issue.fields,
                    issue.state,
                )
            })
            .collect())
    }

    /// Every pairing the store knows about among these entities.
    fn links_among(&mut self, found: &[Found]) -> Result<Vec<Link>> {
        let mut links: Vec<Link> = Vec::new();
        for found in found {
            for link in self.store.find_links(&found.reference)? {
                let known = links
                    .iter()
                    .any(|seen| seen.pairs(&link.left, &link.right.connector));
                if !known {
                    links.push(link);
                }
            }
        }
        Ok(links)
    }

    /// What a sweep should do about one pairing.
    fn judge(
        &mut self,
        mapping: &Mapping,
        pairing: &sweep::Pairing,
        source_caps: &Capabilities,
        sink_caps: &Capabilities,
    ) -> Result<Entry> {
        match (pairing.source.as_ref(), pairing.sink.as_ref()) {
            (Some(source), Some(sink)) => {
                self.judge_pair(mapping, pairing, source, sink, source_caps, sink_caps)
            }
            (Some(source), None) => {
                self.judge_single(mapping, source, Side::Source, source_caps, sink_caps)
            }
            (None, Some(sink)) => {
                self.judge_single(mapping, sink, Side::Sink, source_caps, sink_caps)
            }
            (None, None) => unreachable!("a pairing names at least one entity"),
        }
    }

    /// Both ends present: who moved, and what the other end gets.
    fn judge_pair(
        &mut self,
        mapping: &Mapping,
        pairing: &sweep::Pairing,
        source: &Found,
        sink: &Found,
        source_caps: &Capabilities,
        sink_caps: &Capabilities,
    ) -> Result<Entry> {
        let source_names = mapping.policy.names.of(Side::Source);
        let sink_names = mapping.policy.names.of(Side::Sink);

        // A forge reports an issue's project nowhere, so a sweep's view of the
        // *sink's* project is what the pairing recorded when it placed the issue.
        // Only an issue has a container, and only the mapping's sink holds it.
        let mut sink_owned = sink.clone();
        if source.reference.kind == EntityKind::Issue {
            if let Some(project) = pairing.link.as_ref().and_then(|link| link.project.clone()) {
                if sink_owned.fields.project.is_none() {
                    sink_owned.fields.project = Some(project);
                }
            }
        }
        let sink = &sink_owned;

        let onto_sink = Projection::new(sink_caps, &mapping.users);
        let onto_source = Projection::new(source_caps, &mapping.users);
        // The source names its project in its own ids, while the comparison - and the
        // hash a write records - are in the sink's: the pairing translates between
        // them. An unpaired (or absent) project resolves to `None`, which places
        // nothing and is deliberately not an error.
        let mut source_as_sink = onto_sink.of(
            &source.fields,
            &mapping.source.connector,
            &mapping.sink.connector,
        );
        if source.reference.kind == EntityKind::Issue {
            source_as_sink.fields.project = self.project_on(
                &mapping.source,
                &mapping.sink.connector,
                source.fields.project.as_deref(),
            )?;
        } else {
            source_as_sink.fields.project = None;
        }
        let mut sink_as_source = onto_source.of(
            &sink.fields,
            &mapping.sink.connector,
            &mapping.source.connector,
        );
        let recorded = pairing
            .link
            .as_ref()
            .and_then(|link| link.recorded_revision())
            .map(str::to_owned);

        // A pair with no record - adopted by its marker, or held without one - keeps the
        // source's revision, because that is what "source" means in the mapping.
        let winner = match (&pairing.link, recorded.as_deref()) {
            (Some(link), Some(_)) => match sweep::verdict(
                link,
                sweep::View {
                    found: source,
                    fields: &source_as_sink.fields,
                    names: source_names,
                },
                sweep::View {
                    found: sink,
                    fields: &sink.fields,
                    names: sink_names,
                },
            ) {
                sweep::Verdict::InStep => {
                    return Ok(in_step(source, sink, &source_as_sink.fields, sink_names))
                }
                sweep::Verdict::Conflict => return Ok(conflict(source, sink)),
                sweep::Verdict::Moved(side) => side,
                sweep::Verdict::Adopted => Side::Source,
            },
            _ => Side::Source,
        };

        let (observed, counterpart, expected, target) = match winner {
            Side::Source => (source, sink, &source_as_sink, sink_caps),
            Side::Sink => {
                // Writing back to the source: the container belongs to the sink, so
                // it is not a difference the source is asked to resolve. Taking the
                // source's own value makes the diff leave it alone.
                sink_as_source.fields.project = source.fields.project.clone();
                (sink, source, &sink_as_source, source_caps)
            }
        };
        Ok(decide(
            mapping,
            winner,
            observed,
            Some(counterpart),
            expected,
            target,
            recorded.as_deref(),
        ))
    }

    /// One end present: mirror it, or say why the mapping does not.
    fn judge_single(
        &mut self,
        mapping: &Mapping,
        found: &Found,
        side: Side,
        source_caps: &Capabilities,
        sink_caps: &Capabilities,
    ) -> Result<Entry> {
        let not_mirrored = |why: &str| Entry {
            subject: found.reference.clone(),
            side,
            counterpart: None,
            counterpart_state: None,
            action: Action::NotMirrored {
                why: why.to_string(),
            },
            step: None,
            record: None,
        };
        if !mapping.policy.direction.allows(side) {
            return Ok(not_mirrored("the mapping only mirrors the other way"));
        }

        let (target_caps, target_connector) = match side {
            Side::Source => (sink_caps, &mapping.sink.connector),
            Side::Sink => (source_caps, &mapping.source.connector),
        };
        let projection = Projection::new(target_caps, &mapping.users);
        let mut expected =
            projection.of(&found.fields, &found.reference.connector, target_connector);
        // Only the mapping's sink holds the container: an issue that names a paired
        // project lands on that project's board when it is created there. Any other
        // direction leaves the field out.
        if found.reference.kind == EntityKind::Issue && target_connector == &mapping.sink.connector
        {
            expected.fields.project = self.project_on(
                mapping.endpoint(side),
                target_connector,
                found.fields.project.as_deref(),
            )?;
        } else {
            expected.fields.project = None;
        }
        Ok(decide(
            mapping,
            side,
            found,
            None,
            &expected,
            target_caps,
            None,
        ))
    }

    /// Say what did not travel, once per delivery, at a level an operator sees.
    ///
    /// Not an error - the mapping is still doing what it can - but never silent
    /// either: "the assignee did not come across" has to be findable in the log.
    fn report_skipped(&self, mapping: &str, skipped: &[Skipped]) {
        for skipped in skipped {
            log::warn!("`{mapping}`: {skipped}");
        }
    }

    /// The project the other end names, resolved through the project pairing.
    ///
    /// The source names a project by its own id; the sink knows it by a different
    /// one. The pairing recorded when the projects were mirrored is what translates
    /// between them. A project the issue names that is not paired (or no project at
    /// all) resolves to `None` - which places nothing and is deliberately not an
    /// error, because a forge without that project has nothing to do rather than a
    /// reason to fail the whole issue.
    fn project_on(
        &mut self,
        from: &Endpoint,
        onto: &ConnectorId,
        named: Option<&str>,
    ) -> Result<Option<String>> {
        let Some(named) = named else {
            return Ok(None);
        };
        let reference = EntityRef {
            connector: from.connector.clone(),
            kind: EntityKind::Project,
            scope: Some(from.scope.clone()),
            native_id: named.to_string(),
            url: None,
        };
        let Some(link) = self.store.find_link(&reference, onto)? else {
            return Ok(None);
        };
        Ok(link
            .counterpart(&reference)
            .map(|found| found.native_id.clone()))
    }

    /// Put a newly mirrored issue on the board of the project it names, and record
    /// where it landed. A no-op for anything but an issue, and for an issue whose
    /// source named no paired project.
    fn place_on_project(
        &mut self,
        pair: &Pair<'_>,
        issue: &EntityRef,
        project: Option<&str>,
    ) -> Result<()> {
        if pair.subject.kind != EntityKind::Issue || !pair.project_mirroring {
            return Ok(());
        }
        if let Some(project) = project {
            self.sink(&pair.there.connector)?.place_issue(
                &pair.there.scope,
                &issue.native_id,
                project,
            )?;
            log::info!(
                "placed {} {} on project {}",
                pair.there.connector,
                issue.native_id,
                project
            );
        }
        // Recorded whether or not there was a project, so a pairing that never had
        // one is not later mistaken for one that did.
        self.store.set_link_project(pair.subject, project)?;
        Ok(())
    }

    /// Carry the container part of an issue update: a changed project moves the
    /// issue (a forge issue sits on one project, so assigning the new board takes it
    /// off the old), and a cleared one takes it off the board the record names.
    fn settle_project(
        &mut self,
        pair: &Pair<'_>,
        issue: &EntityRef,
        patch: &Patch,
        fields: &IssueFields,
    ) -> Result<()> {
        if pair.subject.kind != EntityKind::Issue || !pair.project_mirroring {
            return Ok(());
        }
        let desired = fields.project.as_deref();
        match &patch.project {
            Change::Set(_) => {
                if let Some(project) = desired {
                    self.sink(&pair.there.connector)?.place_issue(
                        &pair.there.scope,
                        &issue.native_id,
                        project,
                    )?;
                    log::info!(
                        "moved {} {} to project {}",
                        pair.there.connector,
                        issue.native_id,
                        project
                    );
                }
            }
            Change::Clear => {
                // The id to remove it from is the one the pairing recorded: the
                // platform reports it nowhere, and the patch only says "cleared".
                let previous = self.store.link_project(pair.subject)?;
                if let Some(project) = previous.as_deref() {
                    self.sink(&pair.there.connector)?.remove_issue(
                        &pair.there.scope,
                        &issue.native_id,
                        project,
                    )?;
                    log::info!(
                        "removed {} {} from project {}",
                        pair.there.connector,
                        issue.native_id,
                        project
                    );
                }
            }
            Change::Leave => {}
        }
        self.store.set_link_project(pair.subject, desired)?;
        Ok(())
    }

    /// One end's current state, as the platform reports it.
    ///
    /// The kind decides which read reaches the platform: a project is not an issue
    /// and has its own `fetch`, so a Project delivery must not be fetched as an
    /// issue (which would look, wrongly, like a deleted issue).
    fn snapshot(
        &self,
        kind: &EntityKind,
        connector: &ConnectorId,
        scope: &str,
        id: &str,
    ) -> Result<Snapshot> {
        let sink = self.sink(connector)?;
        let fetched = match kind {
            EntityKind::Project => sink.fetch_project(scope, id)?,
            _ => sink.fetch_issue(scope, id)?,
        };
        Ok(match fetched {
            Some(RemoteIssue { fields, state, .. }) => Snapshot::present(fields, state),
            // The platform says it is gone. That is an answer, not a failure, and
            // the decision layer treats it as the deletion it is.
            None => Snapshot::gone(),
        })
    }

    fn sink(&self, connector: &ConnectorId) -> Result<&Arc<dyn Sink>> {
        self.sinks.get(connector).ok_or_else(|| {
            Error::Config(format!(
                "no write half is configured for platform `{connector}`"
            ))
        })
    }
}

impl std::fmt::Debug for ReconcileHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReconcileHandler")
            .field("sources", &self.sources.keys().collect::<Vec<_>>())
            .field("sinks", &self.sinks.keys().collect::<Vec<_>>())
            .field("mappings", &self.mappings)
            .finish()
    }
}

impl Handler for ReconcileHandler {
    fn handle(&mut self, delivery: &Delivery) -> Result<()> {
        let source = self.sources.get(&delivery.connector).ok_or_else(|| {
            Error::Config(format!(
                "delivery {} names platform `{}`, which is no longer configured",
                delivery.id, delivery.connector
            ))
        })?;

        // The stored body is re-parsed rather than trusted from the row: one
        // delivery can carry several events (a push fans out per commit), and the
        // row keeps only the first one's summary. The event name comes from the row
        // because the header that carried it was not stored.
        let headers = source.replay_headers(&delivery.event);
        let events = source
            .parse(&headers, delivery.body.as_bytes())
            .map_err(|reject| Error::Handler(format!("stored body no longer parses: {reject}")))?;

        for event in &events {
            self.apply(event)?;
        }
        if events.is_empty() {
            log::debug!("delivery {} carried no events", delivery.id);
        }
        Ok(())
    }
}

/// The issue a reference event names.
///
/// The identifier in the text (`VED-1`) belongs to one of the mapping's two ends - the one
/// whose scope is that team key - and the link a pairing would use is keyed by the id that
/// platform issued, not by the identifier a human wrote. So this asks that platform, which
/// is also the only way an identifier and an id can disagree without anyone finding out.
impl ReconcileHandler {
    fn named_issue(&self, event: &Event) -> Result<Option<EntityRef>> {
        let EventDetail::Reference { text, .. } = &event.detail else {
            return Ok(None);
        };
        let named = references::extract(text);
        if named.is_empty() {
            return Ok(None);
        }

        for mapping in &self.mappings {
            for side in [Side::Source, Side::Sink] {
                let endpoint = mapping.endpoint(side);
                // Only identifiers this deployment owns: a quoted unrelated ticket in a
                // commit message is not a reason to touch anything.
                let Some(found) = references::filter_by_team_keys(
                    named.clone(),
                    std::slice::from_ref(&endpoint.scope),
                )
                .into_iter()
                .next() else {
                    continue;
                };
                let Some(issue) = self
                    .sink(&endpoint.connector)?
                    .fetch_issue(&endpoint.scope, &found.identifier)?
                else {
                    continue;
                };
                return Ok(Some(EntityRef {
                    connector: endpoint.connector.clone(),
                    kind: EntityKind::Issue,
                    scope: Some(endpoint.scope.clone()),
                    native_id: issue.reference.id,
                    url: issue.reference.url,
                }));
            }
        }
        Ok(None)
    }
}

/// The comment an event is about, if it is about one.
fn comment_reference(event: &Event, subject: &EntityRef) -> Option<EntityRef> {
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
    }
}

fn describe_nothing(reason: Nothing) -> &'static str {
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
fn in_step(source: &Found, sink: &Found, expected: &IssueFields, sink_names: &StateNames) -> Entry {
    Entry {
        subject: source.reference.clone(),
        side: Side::Source,
        counterpart: Some(sink.reference.clone()),
        counterpart_state: sink.state.clone(),
        action: Action::InStep,
        step: None,
        record: Some(content_key(expected, sink.state.as_deref(), sink_names)),
    }
}

/// Both ends changed since the bridge last wrote: reported, never resolved.
fn conflict(source: &Found, sink: &Found) -> Entry {
    Entry {
        subject: source.reference.clone(),
        side: Side::Source,
        counterpart: Some(sink.reference.clone()),
        counterpart_state: sink.state.clone(),
        action: Action::Conflict,
        step: None,
        record: None,
    }
}

/// The step for a pair whose winner is known, and the entry that describes it.
fn decide(
    mapping: &Mapping,
    winner: Side,
    observed: &Found,
    counterpart: Option<&Found>,
    expected: &Projected,
    target: &Capabilities,
    recorded: Option<&str>,
) -> Entry {
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
    }
}

/// A body that carries our marker.
///
/// The marker is how a copy is recognised *without* the store - a scope can be swept,
/// and a lost link table does not mean losing every pairing. Both the content signature
/// and the field diff ignore it, so stamping changes nothing about what the two sides
/// compare, and a body is never re-sent because of it.
fn stamped(mut fields: IssueFields, subject: &EntityRef) -> IssueFields {
    fields.body = with_marker(&fields.body, subject);
    fields
}

/// The same for the body a patch carries, when it carries one.
fn stamped_patch(mut patch: Patch, subject: &EntityRef) -> Patch {
    if let Change::Set(body) = patch.body {
        patch.body = Change::Set(with_marker(&body, subject));
    }
    patch
}

fn with_marker(body: &str, subject: &EntityRef) -> String {
    markers::with_marker(
        body,
        &markers::OriginMarker::new(subject.connector.as_str(), subject.native_id.clone()),
    )
}

/// `connector:scope`, as the report and the config both write an endpoint.
fn describe_end(end: &Endpoint) -> String {
    format!("{}:{}", end.connector, end.scope)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Action, DeliveryId};

    fn endpoint(reference: &str) -> Endpoint {
        Endpoint::parse(reference).expect("a valid endpoint")
    }

    fn mapping() -> Mapping {
        Mapping {
            name: "linear-cli-rs".into(),
            source: endpoint("linear:VED"),
            sink: endpoint("forgejo:Vedaru/linear-cli-rs"),
            users: UserMap::default(),
            policy: default_policy(Sides::new(
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
            )),
        }
    }

    fn event(connector: &str, scope: &str) -> Event {
        Event {
            connector: ConnectorId::new(connector),
            delivery: DeliveryId::new("d-1"),
            event: "issues".into(),
            kind: EntityKind::Issue,
            action: Action::Created,
            subject: EntityRef {
                connector: ConnectorId::new(connector),
                kind: EntityKind::Issue,
                scope: Some(scope.into()),
                native_id: "1".into(),
                url: None,
            },
            actor: None,
            detail: crate::domain::EventDetail::None,
        }
    }

    #[test]
    fn a_mapping_claims_an_event_on_either_end_and_only_in_its_scope() {
        let mapping = mapping();
        assert_eq!(mapping.side_of(&event("linear", "VED")), Some(Side::Source));
        assert_eq!(
            mapping.side_of(&event("forgejo", "Vedaru/linear-cli-rs")),
            Some(Side::Sink)
        );
        // Right connector, wrong scope: another team's issues are not this mapping's.
        assert_eq!(mapping.side_of(&event("linear", "OPS")), None);
        assert_eq!(mapping.side_of(&event("forgejo", "Vedaru/other")), None);
        // And an unrelated platform is nobody's.
        assert_eq!(mapping.side_of(&event("other", "VED")), None);
    }

    #[test]
    fn scope_matching_ignores_case() {
        // A forge reports `Vedaru/linear-cli-rs`; a human may have written
        // `vedaru/linear-cli-rs` in the config.
        let mapping = mapping();
        assert_eq!(
            mapping.side_of(&event("forgejo", "vedaru/linear-cli-rs")),
            Some(Side::Sink)
        );
    }

    #[test]
    fn an_endpoint_round_trips_through_its_reference() {
        let parsed = Endpoint::parse("forgejo:Vedaru/linear-cli-rs").unwrap();
        assert_eq!(parsed.connector.as_str(), "forgejo");
        // The scope keeps its slash: it is a path for a forge, not a fragment.
        assert_eq!(parsed.scope, "Vedaru/linear-cli-rs");
        assert_eq!(parsed.describe(), "forgejo:Vedaru/linear-cli-rs");
    }

    #[test]
    fn an_endpoint_without_a_scope_is_refused() {
        let error = Endpoint::parse("forgejo").unwrap_err().to_string();
        assert!(error.contains("connector:scope"), "{error}");
    }

    #[test]
    fn a_mapping_that_could_never_run_is_refused_at_startup() {
        let store: Box<dyn Store> =
            Box::new(crate::store::sqlite::SqliteStore::open_in_memory().unwrap());
        let error = ReconcileHandler::new(vec![], vec![], vec![mapping()], store)
            .expect_err("no platforms were configured at all")
            .to_string();
        assert!(error.contains("linear"), "{error}");
    }

    #[test]
    fn every_nothing_reason_has_an_explanation() {
        for reason in [
            Nothing::NotOurKind,
            Nothing::Unpaired,
            Nothing::Echo,
            Nothing::AlreadyEqual,
            Nothing::Direction,
            Nothing::SwitchedOff,
            Nothing::Empty,
            Nothing::Unsupported,
        ] {
            assert!(
                describe_nothing(reason).starts_with("nothing to do"),
                "{reason:?}"
            );
        }
    }
}
