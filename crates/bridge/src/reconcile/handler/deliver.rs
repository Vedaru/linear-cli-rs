//! Carrying a plan out: the delivery path, its facts, and the writes it makes.
//!
//! Split out of `handler.rs` (VED-288). A child module may read its parent's private fields, so
//! none of `ReconcileHandler`'s internals had to be opened up. The methods are `pub(super)`
//! because the parent and its siblings call them.

use super::*;

impl ReconcileHandler {
    pub(super) fn reconcile(&mut self, index: usize, side: Side, event: &Event) -> Result<()> {
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
    pub(super) fn placement(
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
    pub(super) fn entity_facts(
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
    pub(super) fn project_facts(
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
    pub(super) fn carry_out(&mut self, pair: &Pair<'_>, step: Step) -> Result<()> {
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
    pub(super) fn mirrored_comment(
        &self,
        link: Option<&Link>,
        comment: Option<&EntityRef>,
    ) -> Option<EntityRef> {
        link?.counterpart(comment?).cloned()
    }
}
