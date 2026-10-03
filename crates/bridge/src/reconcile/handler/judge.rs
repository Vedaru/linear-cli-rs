//! Judging a pair that exists on both sides: which side wins, and what that means for the other.
//!
//! Split out of `handler.rs` (VED-288); see `deliver` for the visibility rules this split uses.

use super::*;

impl ReconcileHandler {
    /// What a sweep should do about one pairing.
    pub(super) fn judge(
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
    pub(super) fn judge_pair(
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
    pub(super) fn judge_placement(
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
    pub(super) fn judge_single(
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
}
