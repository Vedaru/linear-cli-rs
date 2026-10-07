//! Surveying both sides: what is there, what scope it is in, and what to do about it.
//!
//! Split out of `handler.rs` (VED-288); see `deliver` for the visibility rules this split uses.

use std::collections::HashSet;

use super::*;
use crate::reconcile::survey::Survey;

impl ReconcileHandler {
    /// Read both ends of a mapping and work out what a sweep would do.
    ///
    /// Reads only. Every decision belongs to `sweep` and the reconciler's own planner,
    /// which are pure - so the plan an operator reads is the plan that would run, and
    /// carrying it out is not a second decision. That is what makes a dry run worth
    /// trusting.
    pub fn survey(&mut self, index: usize) -> Result<Survey> {
        let mapping = self.mappings[index].clone();
        // A board can change between sweeps, so the cache never outlives one.
        self.boards.clear();
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
                    },
                )
            })
            .collect();
        let mut issue_groups: BTreeMap<String, Vec<Found>> = BTreeMap::new();
        for found in &issues_source {
            match self.found_scope(&mapping, found, &mut project_facts)? {
                Some(scope) => issue_groups.entry(scope).or_default().push(found.clone()),
                None => log::debug!(
                    "mapping `{}`: {} declares no repository link and is not paired, so it is not mirrored",
                    mapping.name,
                    found.reference.describe()
                ),
            }
        }
        let mut project_groups: BTreeMap<String, Vec<Found>> = BTreeMap::new();
        for found in &projects_source {
            match self.found_scope(&mapping, found, &mut project_facts)? {
                Some(scope) => project_groups.entry(scope).or_default().push(found.clone()),
                None => log::debug!(
                    "mapping `{}`: {} declares no repository link and is not paired, so it is not mirrored",
                    mapping.name,
                    found.reference.describe()
                ),
            }
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
        // Both ends, not just the source: a link whose *source* end is the one a lagging
        // list omitted still has to be seen, or the sink entity looks like a stranger.
        for issues in sink_issues.values() {
            found.extend(issues.iter().cloned());
        }
        for projects in sink_projects.values() {
            found.extend(projects.iter().cloned());
        }
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
            for pairing in sweep::pair_up(
                group,
                &sinks,
                &mapping.source.connector,
                &mapping.sink.connector,
                &links,
            ) {
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
            for pairing in sweep::pair_up(
                group,
                &sinks,
                &mapping.source.connector,
                &mapping.sink.connector,
                &links,
            ) {
                survey.entries.push(self.judge(&mapping, &pairing, ends)?);
            }
        }
        Ok(survey)
    }

    /// The sink scope a source entity's mirror belongs in, for grouping a sweep, or
    /// `None` when neither a pairing nor a `[[mapping.project]]` entry names one.
    ///
    /// `found_facts` caches each project's slug and name, so a sweep reads a project
    /// once however many of its issues it judges.
    pub(super) fn found_scope(
        &mut self,
        mapping: &Mapping,
        found: &Found,
        project_facts: &mut BTreeMap<String, ContainerFacts>,
    ) -> Result<Option<String>> {
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
        // whole point - but it has to be *said*, so an operator who runs a sweep to find
        // out what a link change did does not get `Nothing to do` and conclude it did
        // nothing. The delivery path reports this per event; a sweep judges a whole
        // project at once, so it reports the container whose link moved - one line per
        // re-pointed project per sweep, not one per issue on its board.
        if let Some(placement) = &placement {
            if found.reference.kind == EntityKind::Project
                && placement.would_move()
                && placement.origin == crate::reconcile::Origin::Pair
            {
                log::warn!(
                    "mapping `{}`: {} is mirrored in `{}`, but its entry now names `{}`; keeping the mirror in `{}` rather than moving it (a move would delete the copy on the other side and lose its history)",
                    mapping.name,
                    found.reference.describe(),
                    placement.scope,
                    placement.configured.as_deref().unwrap_or("-"),
                    placement.scope
                );
            }
        }
        Ok(placement.map(|placement| placement.scope))
    }

    /// The routing facts of a source entity's container (a project's own, or its
    /// project's), with the sweep's per-project cache.
    pub(super) fn found_facts(
        &mut self,
        mapping: &Mapping,
        found: &Found,
        project_facts: &mut BTreeMap<String, ContainerFacts>,
    ) -> Result<ContainerFacts> {
        match found.reference.kind {
            EntityKind::Project => Ok(ContainerFacts {
                slug: found.fields.slug.clone(),
                name: Some(found.fields.title.clone()),
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
    pub(super) fn resolve_container(
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
    pub(super) fn list_end(&self, end: &Endpoint, kind: &EntityKind) -> Result<Vec<Found>> {
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
    ///
    /// The same link comes back when either end is queried, so dedup by its
    /// identity in one set. The previous scan (`links.iter().any(..)` inside the
    /// loop) was quadratic in entities, on a path a sweep walks once per side.
    pub(super) fn links_among(&mut self, found: &[Found]) -> Result<Vec<Link>> {
        let mut links: Vec<Link> = Vec::new();
        let mut seen: HashSet<(EntityRef, ConnectorId)> = HashSet::new();
        for found in found {
            for link in self.store.find_links(&found.reference)? {
                if seen.insert((link.left.clone(), link.right.connector.clone())) {
                    links.push(link);
                }
            }
        }
        Ok(links)
    }
}
