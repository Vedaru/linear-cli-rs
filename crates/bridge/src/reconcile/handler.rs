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
use crate::reconcile::route::{Identity, Location, Placement, Routes};
use crate::reconcile::survey::{Action, Entry, Survey};
use crate::reconcile::sweep::{self, Found};
use crate::reconcile::{
    content_key, converge, plan, Context, Direction, Nothing, Openness, Pairwise, Policy, Side,
    Sides, Snapshot, StateNames, Step,
};
use crate::sink::{CardColumn, RemoteIssue, Sink};
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
    /// Declarative routes: which sink scope an entity's project's mirror lives in.
    /// Empty means every entity stays in the mapping's own sink scope.
    pub routes: Routes,
    /// The sink platform's URL shape, when its preset declares one: a project that
    /// links to a location in this shape is routed to the scope the URL names. `None`
    /// means links are never consulted.
    pub sink_location: Option<Location>,
}

impl Mapping {
    /// The end an event arrived on, if it arrived on one of them.
    ///
    /// Scope matters as much as the connector: one Linear workspace and one forge
    /// can be paired several times over (per team, per repository), and a mapping
    /// that ignored the scope would mirror the wrong repository's issues.
    ///
    /// The sink side is matched against *every* scope this mapping writes through -
    /// its default and each route's - because a routed repository is still this
    /// mapping's, and an event from it must be claimed here rather than nowhere.
    pub fn side_of(&self, event: &Event) -> Option<Side> {
        // A scope the platform did not report is not a mismatch: a Linear comment
        // payload names the issue but not the team, and the link - not the scope -
        // is what disambiguates when several mappings share a connector.
        let scope_matches = |scope: &str| {
            event
                .subject
                .scope
                .as_deref()
                .is_none_or(|scope_on_event| scope_on_event.eq_ignore_ascii_case(scope))
        };
        if event.connector == self.source.connector && scope_matches(&self.source.scope) {
            Some(Side::Source)
        } else if event.connector == self.sink.connector
            && self.sink_scopes().iter().any(|scope| scope_matches(scope))
        {
            Some(Side::Sink)
        } else {
            None
        }
    }

    /// Every scope on the sink this mapping reads and writes through: its default
    /// container, then each route's, distinct and in declaration order.
    pub fn sink_scopes(&self) -> Vec<&str> {
        let mut scopes = vec![self.sink.scope.as_str()];
        for scope in self.routes.scopes() {
            if !scopes.contains(&scope) {
                scopes.push(scope);
            }
        }
        scopes
    }

    fn endpoint(&self, side: Side) -> &Endpoint {
        match side {
            Side::Source => &self.source,
            Side::Sink => &self.sink,
        }
    }
}

/// A sweep's two ends: what each can hold, and the sink scope the entry is written
/// in. One struct rather than separate references, so the judging functions do not
/// grow a parameter every time an end is consulted.
#[derive(Clone, Copy)]
struct Ends<'a> {
    source: &'a Capabilities,
    sink: &'a Capabilities,
    sink_scope: &'a str,
}

/// The routing facts about an entity's *container* (its project): the declared
/// locations that may name a sink repository, and the slug and name that let a
/// `project` route match the container by more than its id.
///
/// An issue inherits its project's scope, so it needs its container's whole
/// identity - not just the id the issue names it by - or `project = "<slug>"` and
/// `project = "<name>"` rules would apply to the project and not to its issues. A
/// container whose slug and name could not be resolved carries neither and degrades
/// to id-only, exactly as before.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ContainerFacts {
    slug: Option<String>,
    name: Option<String>,
    links: Vec<String>,
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
        let endpoint = mapping.endpoint(side).clone();
        // The container the event names, when it names one. On the sink that is the
        // repository the entity already lives in - which a route never changes - and on
        // the source it is the team key. A payload that names none (a Linear comment
        // names its issue, not the team) falls back to the mapping's own endpoint.
        let here = Endpoint {
            connector: endpoint.connector,
            scope: event
                .subject
                .scope
                .clone()
                .filter(|scope| !scope.is_empty())
                .unwrap_or(endpoint.scope),
        };
        let there_connector = mapping.endpoint(side.other()).connector.clone();

        // A platform may not say which container the event came from (a Linear
        // comment payload names the issue but not the team). The mapping does know,
        // and a link is looked up by identity - so the scope is filled in from the
        // mapping rather than left absent, which would make every pairing invisible.
        let subject = event.subject.clone().with_scope(here.scope.clone());

        // A comment needs its own pairing, and it is looked up from the comment's
        // id - which the delivery carries in the detail, because the subject is the
        // issue the comment is on.
        let comment_ref = comment_reference(event, &subject);
        let comment_link = match &comment_ref {
            Some(comment) => self.store.find_link(comment, &there_connector)?,
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
            .find_link(&subject, &there_connector)?
            .filter(|link| link.pairs(&subject, &there_connector));
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

        // The endpoint the *other* side is read and written through. On the source
        // that is the repository this entity's mirror belongs in, resolved from its
        // project; on the sink it is the mapping's source, which routing never varies.
        let mut placement: Option<Placement> = None;
        let there = match side {
            Side::Sink => mapping.endpoint(Side::Source).clone(),
            Side::Source => {
                let fields = observed.fields.clone().unwrap_or_default();
                let facts = self.entity_facts(&mapping, &subject.kind, &fields)?;
                let placed = self.placement(&mapping, &subject, &fields, link.as_ref(), &facts)?;
                let endpoint = Endpoint {
                    connector: mapping.sink.connector.clone(),
                    scope: placed.scope.clone(),
                };
                placement = Some(placed);
                endpoint
            }
        };

        let mut counterpart = match &counterpart_ref {
            Some(reference) => self.snapshot(
                &reference.kind,
                &there.connector,
                &there.scope,
                &reference.native_id,
            )?,
            None => Snapshot::gone(),
        };

        // A delivery on the sink is still about the same source entity, and it is that
        // entity's project that decides the repository - so the placement is read from
        // the counterpart when the event arrived on the sink.
        if placement.is_none() {
            if let (Some(reference), Some(fields)) =
                (counterpart_ref.as_ref(), counterpart.fields.as_ref())
            {
                let facts = self.entity_facts(&mapping, &reference.kind, fields)?;
                placement =
                    Some(self.placement(&mapping, reference, fields, link.as_ref(), &facts)?);
            }
        }
        // A pair a route would have moved is reported, never relocated: moving a paired
        // copy means deleting the one on the other side, and its history with it.
        if let Some(placement) = &placement {
            if placement.would_move() {
                let pair = counterpart_ref
                    .as_ref()
                    .map(|other| match side {
                        Side::Source => format!("{} <-> {}", subject.describe(), other.describe()),
                        Side::Sink => format!("{} <-> {}", other.describe(), subject.describe()),
                    })
                    .unwrap_or_else(|| subject.describe());
                log::warn!(
                    "mapping `{}`: {} is mirrored in `{}`, but its project now routes to `{}`; keeping the mirror in `{}` rather than moving it (a move would delete the copy on the other side and lose its history)",
                    mapping.name,
                    pair,
                    placement.scope,
                    placement.routed.as_deref().unwrap_or("-"),
                    placement.scope
                );
            }
        }

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

        // An assignee the target has no counterpart for is held at the target's own
        // value: the delivery reports the skip (it travels in `expected.skipped`) and
        // writes nothing about the field, rather than sending "no assignee" and
        // deleting the one the target holds.
        expected.hold_untranslated(counterpart.fields.as_ref());

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

    /// Where an entity's mirror belongs on the sink, and what says so.
    ///
    /// `source` is the entity as the *source* platform holds it and `fields` its
    /// fields - whichever end of the mapping the event arrived on, because a delivery
    /// on the sink still says something about the same source entity. Closest to pure:
    /// the only I/O is reading the container's pairing, which the rules need to know
    /// whether a mirrored project already lives somewhere.
    fn placement(
        &mut self,
        mapping: &Mapping,
        source: &EntityRef,
        fields: &IssueFields,
        pair: Option<&Link>,
        container: &ContainerFacts,
    ) -> Result<Placement> {
        // The sink side of this entity's own pairing, when it has one: a pair never
        // moves repos, so this pins the entity wherever the mirror already is.
        let paired = pair
            .and_then(|link| link.counterpart(source))
            .filter(|other| other.connector == mapping.sink.connector)
            .and_then(|other| other.scope.clone());

        // The container an issue names, resolved through the container's *pairing*
        // first: a mirrored project knows the repository its board lives in. Its
        // identity - the id the issue names it by, plus the slug and name resolved
        // for the container - is what a `project` route matches, so an issue
        // inherits a route that names its project by slug or name, not only by id.
        let container_id = (source.kind == EntityKind::Issue)
            .then(|| fields.project.clone())
            .flatten();
        let (container_paired, container_identity) = match &container_id {
            Some(id) => {
                let project = EntityRef {
                    connector: mapping.source.connector.clone(),
                    kind: EntityKind::Project,
                    scope: Some(mapping.source.scope.clone()),
                    native_id: id.clone(),
                    url: None,
                };
                let paired_scope = self
                    .store
                    .find_link(&project, &mapping.sink.connector)?
                    .and_then(|link| link.counterpart(&project).cloned())
                    .filter(|other| other.connector == mapping.sink.connector)
                    .and_then(|other| other.scope);
                (
                    paired_scope,
                    Some(Identity {
                        id: id.as_str(),
                        slug: container.slug.as_deref(),
                        name: container.name.as_deref(),
                    }),
                )
            }
            None => (None, None),
        };

        // A container is routed by its own identity; a contained entity by its
        // container's project, then its own identifier, then its labels.
        let own = (source.kind == EntityKind::Project).then_some(Identity {
            id: source.native_id.as_str(),
            slug: fields.slug.as_deref(),
            name: Some(fields.title.as_str()),
        });
        // The key an `issue` rule names the entity by: the identifier a person sees,
        // falling back to the platform id when the platform exposes no other.
        let issue_key = (source.kind == EntityKind::Issue).then(|| {
            fields
                .identifier
                .as_deref()
                .unwrap_or(source.native_id.as_str())
        });

        Ok(crate::reconcile::route::place(
            &mapping.routes,
            mapping.sink_location.as_ref(),
            &mapping.sink.scope,
            crate::reconcile::route::Entity {
                paired: paired.as_deref(),
                container_paired: container_paired.as_deref(),
                container: container_identity,
                own,
                issue: issue_key,
                labels: &fields.labels,
                links: &container.links,
            },
        ))
    }

    /// The routing facts about an entity's container, for the `project` rule and the
    /// sink-platform link step: a contained entity uses its project's, a container its
    /// own. A project is read once and its slug and name are carried out alongside its
    /// links, so a `project` rule can match it by any of the three - not by id alone.
    fn entity_facts(
        &mut self,
        mapping: &Mapping,
        kind: &EntityKind,
        fields: &IssueFields,
    ) -> Result<ContainerFacts> {
        match kind {
            EntityKind::Project => Ok(ContainerFacts {
                slug: fields.slug.clone(),
                name: Some(fields.title.clone()),
                links: fields.links.clone(),
            }),
            EntityKind::Issue => self.project_facts(mapping, fields.project.as_deref()),
            _ => Ok(ContainerFacts::default()),
        }
    }

    /// A source project's routing facts, fetched once.
    ///
    /// A no-op unless the sink declares a URL shape to match (otherwise a fetch would
    /// be wasted) and the entity names a project. A project the source no longer has,
    /// or one that declares nothing, yields no facts - never an error. A project whose
    /// slug and name cannot be read this way degrades to an id-only identity.
    fn project_facts(
        &mut self,
        mapping: &Mapping,
        project: Option<&str>,
    ) -> Result<ContainerFacts> {
        if mapping.sink_location.is_none() {
            return Ok(ContainerFacts::default());
        }
        let Some(project) = project else {
            return Ok(ContainerFacts::default());
        };
        let snapshot = self.snapshot(
            &EntityKind::Project,
            &mapping.source.connector,
            &mapping.source.scope,
            project,
        )?;
        Ok(snapshot
            .fields
            .map(|fields| ContainerFacts {
                slug: fields.slug,
                name: Some(fields.title),
                links: fields.links,
            })
            .unwrap_or_default())
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
                column,
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
                self.place_on_project(
                    pair,
                    &created_ref,
                    fields.project.as_deref(),
                    column.as_deref(),
                )?;
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
                column,
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
                // A *state* moving travels the same way on a board, because there the
                // state is the column.
                self.settle_project(
                    pair,
                    &reference,
                    &patch,
                    &fields,
                    column.as_deref(),
                    state.is_some(),
                )?;
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
            Step::Place { project, column } => {
                // The card that moves is the *sink's* issue, which is what the pairing
                // names at its other end.
                let Some(reference) = pair.counterpart.cloned() else {
                    return Ok(());
                };
                self.sink(&pair.there.connector)?.place_issue(
                    &pair.there.scope,
                    &reference.native_id,
                    &project,
                    Some(&column),
                )?;
                log::info!(
                    "moved {} {} to column `{}` of project {}",
                    pair.there.connector,
                    reference.native_id,
                    column,
                    project
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

        // Projects are a second collection behind their own switch. A deployment that
        // never asked for them must not so much as list them, or a sweep would start
        // proposing project writes it was never configured for.
        let projects_source = if mapping.policy.sync_projects {
            self.list_end(&mapping.source, &EntityKind::Project)?
        } else {
            Vec::new()
        };

        // Group the source entities by the sink scope their mirror belongs in. Each
        // group is compared against *that* scope's contents and no other repository's.
        // A project's declared locations are needed to resolve an issue's repo from a
        // link, and its slug and name to route it by name rather than only by id; the
        // projects already listed seed the cache, and one that was not listed is
        // fetched once and remembered.
        let mut project_facts: BTreeMap<String, ContainerFacts> = projects_source
            .iter()
            .map(|found| {
                (
                    found.reference.native_id.clone(),
                    ContainerFacts {
                        slug: found.fields.slug.clone(),
                        name: Some(found.fields.title.clone()),
                        links: found.fields.links.clone(),
                    },
                )
            })
            .collect();
        let mut issue_groups: BTreeMap<String, Vec<Found>> = BTreeMap::new();
        for found in &issues_source {
            issue_groups
                .entry(self.found_scope(&mapping, found, &mut project_facts)?)
                .or_default()
                .push(found.clone());
        }
        let mut project_groups: BTreeMap<String, Vec<Found>> = BTreeMap::new();
        for found in &projects_source {
            project_groups
                .entry(self.found_scope(&mapping, found, &mut project_facts)?)
                .or_default()
                .push(found.clone());
        }

        // The sink scopes to read: the mapping's default, every route's, and every
        // scope a group resolved to (a pairing may have pinned an entity to a scope no
        // route names any more).
        let mut scopes: Vec<String> = mapping
            .sink_scopes()
            .iter()
            .map(|scope| scope.to_string())
            .collect();
        for group in issue_groups.keys().chain(project_groups.keys()) {
            if !scopes.iter().any(|scope| scope.eq_ignore_ascii_case(group)) {
                scopes.push(group.clone());
            }
        }

        // Read every scope once, of each kind. The source is listed once, above; the
        // sink is listed per scope so each entity is compared against its own.
        let mut sink_issues: BTreeMap<String, Vec<Found>> = BTreeMap::new();
        let mut sink_projects: BTreeMap<String, Vec<Found>> = BTreeMap::new();
        for scope in &scopes {
            let end = Endpoint {
                connector: mapping.sink.connector.clone(),
                scope: scope.clone(),
            };
            sink_issues.insert(scope.clone(), self.list_end(&end, &EntityKind::Issue)?);
            if mapping.policy.sync_projects {
                sink_projects.insert(scope.clone(), self.list_end(&end, &EntityKind::Project)?);
            }
        }

        // Links are gathered from every source entity a sweep will judge: an issue link
        // and a project link live in the same table, keyed by identity, so one lookup
        // serves both. Pairing by link is what lets a sweep recognise a copy whose repo
        // the config has since changed, instead of duplicating it.
        let mut found: Vec<Found> = issues_source.clone();
        found.extend(projects_source.iter().cloned());
        let links = self.links_among(&found)?;

        let source_caps = self.sink(&mapping.source.connector)?.capabilities();
        let sink_caps = self.sink(&mapping.sink.connector)?.capabilities();

        let mut survey = Survey {
            mapping: mapping.name.clone(),
            source: describe_end(&mapping.source),
            sink: describe_end(&mapping.sink),
            entries: Vec::new(),
        };

        let empty: Vec<Found> = Vec::new();
        for scope in &scopes {
            let ends = Ends {
                source: &source_caps,
                sink: &sink_caps,
                sink_scope: scope,
            };
            let group = issue_groups.get(scope).unwrap_or(&empty);
            let sinks = sink_issues.get(scope).cloned().unwrap_or_default();
            for pairing in sweep::pair_up(group, &sinks, &mapping.sink.connector, &links) {
                survey.entries.push(self.judge(&mapping, &pairing, ends)?);
            }
        }
        for scope in &scopes {
            if !mapping.policy.sync_projects {
                continue;
            }
            let ends = Ends {
                source: &source_caps,
                sink: &sink_caps,
                sink_scope: scope,
            };
            let group = project_groups.get(scope).unwrap_or(&empty);
            let sinks = sink_projects.get(scope).cloned().unwrap_or_default();
            for pairing in sweep::pair_up(group, &sinks, &mapping.sink.connector, &links) {
                survey.entries.push(self.judge(&mapping, &pairing, ends)?);
            }
        }
        Ok(survey)
    }

    /// The sink scope a source entity's mirror belongs in, for grouping a sweep.
    ///
    /// `found_facts` caches each project's routing facts, so a sweep reads a project
    /// once however many of its issues it judges.
    fn found_scope(
        &mut self,
        mapping: &Mapping,
        found: &Found,
        project_facts: &mut BTreeMap<String, ContainerFacts>,
    ) -> Result<String> {
        let link = self
            .store
            .find_link(&found.reference, &mapping.sink.connector)?;
        let facts = self.found_facts(mapping, found, project_facts)?;
        let placement = self.placement(
            mapping,
            &found.reference,
            &found.fields,
            link.as_ref(),
            &facts,
        )?;

        // A re-pointed project keeps its mirror where the pairing put it, which is the
        // whole point - but it has to be *said*, or the operator who runs a sweep to
        // find out what the config change did gets `Nothing to do` and concludes it did
        // nothing. The delivery path reports this per event; a sweep judges a whole
        // project at once, so it reports the *container* whose route moved - one line
        // per re-pointed project per sweep, not one per issue on its board, which would
        // be the same event counted a hundred times and repeated every sweep until
        // somebody acts. An issue an `issue` rule re-points on its own is the one case
        // this leaves to the delivery path.
        if found.reference.kind == EntityKind::Project
            && placement.would_move()
            && placement.origin == crate::reconcile::Origin::Pair
        {
            log::warn!(
                "mapping `{}`: {} is mirrored in `{}`, but its route now says `{}`; keeping the mirror in `{}` rather than moving it (a move would delete the copy on the other side and lose its history)",
                mapping.name,
                found.reference.describe(),
                placement.scope,
                placement.routed.as_deref().unwrap_or("-"),
                placement.scope
            );
        }
        Ok(placement.scope)
    }

    /// The routing facts of a source entity's container (a project's own, or its
    /// project's), with the sweep's per-project cache.
    fn found_facts(
        &mut self,
        mapping: &Mapping,
        found: &Found,
        project_facts: &mut BTreeMap<String, ContainerFacts>,
    ) -> Result<ContainerFacts> {
        match found.reference.kind {
            EntityKind::Project => Ok(ContainerFacts {
                slug: found.fields.slug.clone(),
                name: Some(found.fields.title.clone()),
                links: found.fields.links.clone(),
            }),
            EntityKind::Issue => {
                let Some(project) = found.fields.project.clone() else {
                    return Ok(ContainerFacts::default());
                };
                if let Some(facts) = project_facts.get(&project) {
                    return Ok(facts.clone());
                }
                let facts = self.project_facts(mapping, Some(&project))?;
                project_facts.insert(project, facts.clone());
                Ok(facts)
            }
            _ => Ok(ContainerFacts::default()),
        }
    }

    /// Carry out a survey. The only place a sweep writes anything.
    pub fn apply_survey(&mut self, index: usize, survey: &Survey) -> Result<usize> {
        let mapping = self.mappings[index].clone();
        let mut written = 0;
        // Containers before their contents: a project this sweep creates is paired by
        // the time its issues are written, so an issue whose project is being created
        // alongside it lands on the new board in this same pass rather than waiting for
        // the next. The relative order within each group is kept.
        let mut order: Vec<usize> = (0..survey.entries.len()).collect();
        order.sort_by_key(|&position| {
            let entry = &survey.entries[position];
            let creates_project = matches!(entry.step.as_ref(), Some(Step::Create { .. }))
                && entry.subject.kind == EntityKind::Project;
            !creates_project
        });
        for position in order {
            let entry = &survey.entries[position];
            match &entry.step {
                Some(step) => {
                    // The entry's own scope, not the mapping's default: a routed issue
                    // is written to the repository its project resolves to.
                    let there = match entry.side {
                        Side::Source => Endpoint {
                            connector: mapping.sink.connector.clone(),
                            scope: entry.sink_scope.clone(),
                        },
                        Side::Sink => mapping.source.clone(),
                    };
                    let step = self.resolve_container(&mapping, entry, step.clone())?;
                    self.carry_out(
                        &Pair {
                            mapping: &mapping.name,
                            there: &there,
                            names_there: mapping.policy.names.of(entry.side.other()),
                            subject: &entry.subject,
                            counterpart: entry.counterpart.as_ref(),
                            comment: None,
                            comment_link: None,
                            counterpart_state: entry.counterpart_state.as_deref(),
                            project_mirroring: there.connector == mapping.sink.connector,
                        },
                        step,
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

    /// The step with an issue's container resolved to the sink's own project id.
    ///
    /// A sweep plans an issue before the project it names may be paired - a project
    /// the same sweep creates has no sink id when the plan is made - so the project
    /// id is resolved here, at the write, once containers have been carried out. A
    /// project still unknown resolves to nothing and the issue is written where it
    /// would have been, exactly as before.
    fn resolve_container(
        &mut self,
        mapping: &Mapping,
        entry: &Entry,
        mut step: Step,
    ) -> Result<Step> {
        if entry.subject.kind != EntityKind::Issue {
            return Ok(step);
        }
        let Some(source_project) = entry.source_project.as_deref() else {
            return Ok(step);
        };
        let project = self.project_on(
            &mapping.source,
            &mapping.sink.connector,
            Some(source_project),
        )?;
        if let Some(project) = project {
            match &mut step {
                Step::Create { fields, .. } | Step::Update { fields, .. } => {
                    fields.project = Some(project);
                }
                _ => {}
            }
        }
        Ok(step)
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
        ends: Ends<'_>,
    ) -> Result<Entry> {
        match (pairing.source.as_ref(), pairing.sink.as_ref()) {
            (Some(source), Some(sink)) => self.judge_pair(mapping, pairing, source, sink, ends),
            (Some(source), None) => self.judge_single(mapping, source, Side::Source, ends),
            (None, Some(sink)) => self.judge_single(mapping, sink, Side::Sink, ends),
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
        ends: Ends<'_>,
    ) -> Result<Entry> {
        let (source_caps, sink_caps, sink_scope) = (ends.source, ends.sink, ends.sink_scope);
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
        // Same rule as the delivery path: an assignee the sink cannot name is held at
        // the sink's own, so a sweep neither reports it as a difference nor proposes
        // clearing it. A sweep that judged this differently from a delivery would be
        // the bug it exists to catch.
        source_as_sink.hold_untranslated(Some(&sink.fields));
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
        sink_as_source.hold_untranslated(Some(&source.fields));
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
                    // Fields agreeing is not the whole story: a card somebody dragged is a
                    // difference the field diff cannot see, so the placement check rides on
                    // exactly this path - the one that would otherwise say "nothing to do".
                    let entry =
                        in_step(source, sink, &source_as_sink.fields, sink_names, sink_scope);
                    return self.judge_placement(mapping, pairing, source, sink, entry, sink_scope);
                }
                sweep::Verdict::Conflict => return Ok(conflict(source, sink, sink_scope)),
                sweep::Verdict::Moved(side) => side,
                sweep::Verdict::Adopted => Side::Source,
            },
            _ => Side::Source,
        };

        let (observed, counterpart, expected) = match winner {
            Side::Source => (source, sink, &source_as_sink),
            Side::Sink => {
                // Writing back to the source: the container belongs to the sink, so
                // it is not a difference the source is asked to resolve. Taking the
                // source's own value makes the diff leave it alone.
                sink_as_source.fields.project = source.fields.project.clone();
                (sink, source, &sink_as_source)
            }
        };
        let entry = decide(
            mapping,
            winner,
            observed,
            Some(counterpart),
            expected,
            ends,
            recorded.as_deref(),
        );
        self.judge_placement(mapping, pairing, source, sink, entry, ends.sink_scope)
    }

    /// A board is a field of the issue like the rest, so a sweep has to see it.
    ///
    /// Checked only when the pair has nothing else to do: a write that moves the state
    /// re-places the card already (`settle_project`), and reporting a second time would
    /// say the same thing twice. What this catches is the card nobody's edit will move -
    /// the one a human dragged to the wrong column, or one placed before the mapping had
    /// a `[mapping.columns]` table.
    ///
    /// Reported, never made in passing: the entry carries the move as its *step*, so a dry
    /// run says "would move" and `--apply` is what moves it. A sweep that moved cards
    /// silently would be the same class of bug as an assignee cleared in passing.
    fn judge_placement(
        &mut self,
        mapping: &Mapping,
        pairing: &sweep::Pairing,
        source: &Found,
        sink: &Found,
        entry: Entry,
        sink_scope: &str,
    ) -> Result<Entry> {
        if sink.reference.kind != EntityKind::Issue || mapping.policy.columns.is_empty() {
            return Ok(entry);
        }
        if entry.step.as_ref().is_some_and(|step| !step.is_nothing()) {
            // Something else is already being written, and it will place the card.
            return Ok(entry);
        }
        let Some(column) = mapping.policy.column_for(source.state.as_deref()) else {
            return Ok(entry);
        };
        // The board to look at is the one the pairing recorded: a forge reports an issue's
        // project nowhere, and a card is only readable through the board it is on.
        let Some(project) = pairing.link.as_ref().and_then(|link| link.project.clone()) else {
            return Ok(entry);
        };
        let here = self.sink(&mapping.sink.connector)?.card_column(
            sink_scope,
            &project,
            &sink.reference.native_id,
        )?;
        match here {
            // Where the mapping says it belongs: nothing to report.
            CardColumn::In(column_name) if column_name == column => Ok(Entry {
                action: Action::InStep,
                ..entry
            }),
            // This sink cannot see placement at all. Nothing to compare, so nothing to say:
            // an absence of knowledge is not evidence that somebody's card is misplaced.
            CardColumn::Unknown => Ok(entry),
            // Somewhere else, or on no column at all - both are the difference this rule
            // exists for, and both are reported (and applied, if asked) as a placement.
            CardColumn::In(_) | CardColumn::NotOnBoard => Ok(Entry {
                action: Action::Write {
                    touched: vec!["column".to_string()],
                },
                step: Some(Step::Place {
                    project,
                    column: column.to_string(),
                }),
                ..entry
            }),
        }
    }

    /// One end present: mirror it, or say why the mapping does not.
    fn judge_single(
        &mut self,
        mapping: &Mapping,
        found: &Found,
        side: Side,
        ends: Ends<'_>,
    ) -> Result<Entry> {
        let (source_caps, sink_caps, sink_scope) = (ends.source, ends.sink, ends.sink_scope);
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
            sink_scope: sink_scope.to_string(),
            source_project: None,
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
        Ok(decide(mapping, side, found, None, &expected, ends, None))
    }

    /// Say what did not travel, once per reason rather than once per issue.
    ///
    /// Not an error - the mapping is still doing what it can - but never silent either: "the
    /// assignee did not come across" has to be findable in the log.
    ///
    /// The *first* skip of a given `(field, reason)` in this process is a `warn`, with the config
    /// that would stop it when there is one; the repeats are `debug`, because a sweep delivers one
    /// issue at a time and an unconfigured `[[mapping.user]]` therefore used to print one identical
    /// warning per issue, burying the `unmapped` and `emulated` skips that differ per issue. See
    /// [`super::skipped_log`] - nothing that differs is ever suppressed.
    fn report_skipped(&self, mapping: &str, skipped: &[Skipped]) {
        for skipped in skipped {
            if super::skipped_log::SKIPPED_THIS_PROCESS.record(skipped) {
                log::warn!("{}", super::skipped_log::line(mapping, skipped));
            } else {
                log::debug!("`{mapping}`: {skipped} (already reported this run)");
            }
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
        column: Option<&str>,
    ) -> Result<()> {
        if pair.subject.kind != EntityKind::Issue || !pair.project_mirroring {
            return Ok(());
        }
        if let Some(project) = project {
            self.sink(&pair.there.connector)?.place_issue(
                &pair.there.scope,
                &issue.native_id,
                project,
                column,
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
    ///
    /// A board is also where a *state* lives as a column, so a state that moved places
    /// the card again in the column the mapping names for it. Only a board that was told
    /// what its columns mean moves anything: with no column named, the card stays exactly
    /// where a human put it.
    fn settle_project(
        &mut self,
        pair: &Pair<'_>,
        issue: &EntityRef,
        patch: &Patch,
        fields: &IssueFields,
        column: Option<&str>,
        state_moving: bool,
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
                        column,
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
            Change::Leave => {
                // The container did not change, but the *state* may have - and on a board
                // the state is the column, so the card has to move with it. Onto the
                // project the pairing recorded, because a forge does not report which one
                // an issue is on and the patch says nothing about it.
                if state_moving {
                    if let Some(column) = column {
                        if let Some(project) = self.store.link_project(pair.subject)? {
                            self.sink(&pair.there.connector)?.place_issue(
                                &pair.there.scope,
                                &issue.native_id,
                                &project,
                                Some(column),
                            )?;
                            log::info!(
                                "moved {} {} to column `{}` of project {}",
                                pair.there.connector,
                                issue.native_id,
                                column,
                                project
                            );
                        }
                    }
                }
            }
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
        columns: Default::default(),
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
fn in_step(
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
fn conflict(source: &Found, sink: &Found, sink_scope: &str) -> Entry {
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
fn decide(
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
            routes: Routes::default(),
            sink_location: None,
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
    fn a_mapping_claims_an_event_from_a_routed_scope() {
        // A routed repository is still this mapping's: an event from it must be
        // claimed here rather than nowhere.
        let mut mapping = mapping();
        mapping.routes = Routes::new(vec![crate::reconcile::route::Route {
            project: Some("project-kuro".into()),
            issue: None,
            label: None,
            scope: "Vedaru/kuro".into(),
        }]);
        assert_eq!(
            mapping.sink_scopes(),
            vec!["Vedaru/linear-cli-rs", "Vedaru/kuro"]
        );
        assert_eq!(
            mapping.side_of(&event("forgejo", "Vedaru/kuro")),
            Some(Side::Sink)
        );
        // A repository nobody routed is still nobody's.
        assert_eq!(mapping.side_of(&event("forgejo", "Vedaru/other")), None);
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
