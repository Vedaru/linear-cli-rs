//! Converging a whole scope, instead of reacting to one delivery.
//!
//! A delivery says "this changed"; a sweep says "make these two agree" and has to work
//! out for itself what changed. Two questions, both answered without I/O so they can be
//! tested on their own:
//!
//! - **which two entities are the same thing?** Only two things prove it: a link the
//!   bridge recorded when it wrote one side from the other, or an origin marker the
//!   bridge left in the copy's body. Matching titles, or an identifier that happens to
//!   appear in a description, is a *guess* - and a guess about identity is how a mirror
//!   duplicates everything it was supposed to be looking after.
//! - **which side moved?** The content key a link recorded is what was last written
//!   across it, so the side that no longer matches that key is the side that changed.
//!   Both sides failing to match is a conflict, reported rather than resolved: a sweep
//!   cannot know which edit was meant, and choosing one throws the other away.

use crate::domain::{markers, ConnectorId, EntityRef, IssueFields};
use crate::store::Link;

use super::{content_key_with, Side as WhichSide, StateNames};

/// An entity as a sweep found it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub reference: EntityRef,
    pub fields: IssueFields,
    pub state: Option<String>,
}

impl Found {
    pub fn new(reference: EntityRef, fields: IssueFields, state: Option<String>) -> Self {
        Self {
            reference,
            fields,
            state,
        }
    }
}

/// One side of a pair, as it takes part in the comparison.
///
/// Named a *view* rather than a side because a side is which end of the mapping this is,
/// and this is what that end looks like to the comparison: projected fields and a
/// vocabulary, not the entity itself.
///
/// The fields are deliberately separate from `found`: the side being *written* is
/// compared in the form a write leaves behind - projected onto the other platform,
/// which is the form the record holds - while the other side is compared as it is,
/// because the record describes it and not a projection of it.
pub struct View<'a> {
    pub found: &'a Found,
    pub fields: &'a IssueFields,
    pub names: &'a StateNames,
}

impl View<'_> {
    fn key(&self) -> String {
        content_key_with(
            self.fields,
            self.names.openness(self.found.state.as_deref()),
        )
    }
}

/// What a sweep concluded about one pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Both sides read the same, through the vocabulary they share.
    InStep,
    /// The side named here moved, so the other one gets its revision.
    Moved(WhichSide),
    /// Both sides changed since the bridge last wrote. Reported, never guessed at.
    Conflict,
    /// No record: a pair adopted by its marker that this store never wrote across. The
    /// source's revision is the one to keep, because that is what "source" means.
    Adopted,
}

/// Who moved since the bridge last wrote across this link.
pub fn verdict(link: &Link, source: View<'_>, sink: View<'_>) -> Verdict {
    let source_key = source.key();
    let sink_key = sink.key();
    if source_key == sink_key {
        return Verdict::InStep;
    }
    match link.recorded_revision() {
        // The source still reads as the record did, so whatever moved, moved elsewhere.
        Some(recorded) if recorded == source_key => Verdict::Moved(WhichSide::Sink),
        Some(recorded) if recorded == sink_key => Verdict::Moved(WhichSide::Source),
        Some(_) => Verdict::Conflict,
        None => Verdict::Adopted,
    }
}

/// How two entities were shown to be the same.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Match {
    /// A link the bridge recorded when it wrote one from the other.
    Link,
    /// An origin marker in one of the two bodies.
    Marker,
    /// Nothing on the other side (yet).
    Nothing,
}

/// One pair, or one unpaired entity.
#[derive(Clone, Debug)]
pub struct Pairing {
    pub source: Option<Found>,
    pub sink: Option<Found>,
    /// The stored pairing, when the store already knew it.
    pub link: Option<Link>,
    pub matched: Match,
}

impl Pairing {
    /// True when both sides are present, so there is something to compare.
    pub fn is_pair(&self) -> bool {
        self.source.is_some() && self.sink.is_some()
    }
}

/// Match both sides' entities up.
///
/// `links` is whatever the caller could fetch for these entities; it can only ever *add*
/// pairings, so passing none means "pair by marker alone" - which is what a first run
/// against two existing sets relies on.
pub fn pair_up(
    source: &[Found],
    sink: &[Found],
    source_connector: &ConnectorId,
    sink_connector: &ConnectorId,
    links: &[Link],
) -> Vec<Pairing> {
    let mut taken = vec![false; sink.len()];
    let mut pairings = Vec::new();

    for found in source {
        // Proof, strongest first: a stored link, then an origin marker - in the copy's
        // body naming this entity, or in this body naming the copy.
        match find_by_link(links, sink, &taken, &found.reference, sink_connector) {
            Some(ByLink::Seen(position, link)) => {
                taken[position] = true;
                pairings.push(Pairing {
                    source: Some(found.clone()),
                    sink: Some(sink[position].clone()),
                    link: Some(link),
                    matched: Match::Link,
                });
                continue;
            }
            Some(ByLink::Unseen(link)) => {
                // The store recorded a pair this pass could not read on the other side -
                // a list lagging its own write. Carry the link (and no sink) so the judge
                // can tell "not seen yet" from "never paired" and skip, instead of
                // re-creating the copy made moments ago.
                pairings.push(Pairing {
                    source: Some(found.clone()),
                    sink: None,
                    link: Some(link),
                    matched: Match::Link,
                });
                continue;
            }
            None => {}
        }

        match find_by_marker(found, sink, &taken) {
            Some((position, link)) => {
                taken[position] = true;
                pairings.push(Pairing {
                    source: Some(found.clone()),
                    sink: Some(sink[position].clone()),
                    link,
                    matched: Match::Marker,
                });
            }
            None => pairings.push(Pairing {
                source: Some(found.clone()),
                sink: None,
                link: None,
                matched: Match::Nothing,
            }),
        }
    }

    // Whatever is left on the sink side has no counterpart on the source. A link still
    // proves the pair - the source end is just not in this pass's read - and the same lag
    // rule applies in that direction.
    for (index, found) in sink.iter().enumerate() {
        if taken[index] {
            continue;
        }
        let link = links
            .iter()
            .find(|link| link.pairs(&found.reference, source_connector))
            .cloned();
        let matched = if link.is_some() {
            Match::Link
        } else {
            Match::Nothing
        };
        pairings.push(Pairing {
            source: None,
            sink: Some(found.clone()),
            link,
            matched,
        });
    }

    pairings
}

/// What a stored link says about one entity.
enum ByLink {
    /// The counterpart was read in this pass; this is where it sits in the other list.
    Seen(usize, Link),
    /// The link names a counterpart this pass did not read.
    Unseen(Link),
}

fn find_by_link(
    links: &[Link],
    other: &[Found],
    taken: &[bool],
    reference: &EntityRef,
    other_connector: &ConnectorId,
) -> Option<ByLink> {
    let link = links
        .iter()
        .find(|link| link.pairs(reference, other_connector))?;
    let counterpart = link.counterpart(reference)?;
    match other
        .iter()
        .position(|candidate| candidate.reference.same_entity(counterpart))
    {
        Some(position) if !taken[position] => Some(ByLink::Seen(position, link.clone())),
        // Read but already claimed by an earlier entity, or not read at all: either way a
        // link exists, so this is not a stranger to create from.
        _ => Some(ByLink::Unseen(link.clone())),
    }
}

fn find_by_marker(found: &Found, sink: &[Found], taken: &[bool]) -> Option<(usize, Option<Link>)> {
    // The copy names this entity...
    let names_source = sink.iter().enumerate().find_map(|(index, candidate)| {
        if taken[index] {
            return None;
        }
        let marker = markers::parse(&candidate.fields.body)?;
        (marker.connector == found.reference.connector.as_str()
            && marker.id == found.reference.native_id)
            .then_some(index)
    });
    if let Some(position) = names_source {
        return Some((position, None));
    }

    // ...or this body names the copy (which side the copy landed on depends on which
    // way the first mirror ran).
    let marker = markers::parse(&found.fields.body)?;
    sink.iter()
        .enumerate()
        .find(|(index, candidate)| {
            !taken[*index]
                && candidate.reference.connector.as_str() == marker.connector
                && candidate.reference.native_id == marker.id
        })
        .map(|(index, _)| (index, None))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::markers;
    use crate::reconcile::Openness;

    fn linear() -> ConnectorId {
        ConnectorId::new("linear")
    }

    fn forge() -> ConnectorId {
        ConnectorId::new("forgejo")
    }

    fn names(closed: &[&str], open: &str) -> StateNames {
        StateNames {
            closed: closed.iter().map(|name| name.to_string()).collect(),
            open: Some(open.to_string()),
            initial: None,
        }
    }

    fn found(connector: &ConnectorId, id: &str, scope: &str, title: &str, state: &str) -> Found {
        Found::new(
            EntityRef {
                connector: connector.clone(),
                kind: crate::domain::EntityKind::Issue,
                scope: Some(scope.to_string()),
                native_id: id.to_string(),
                url: None,
            },
            IssueFields {
                title: title.to_string(),
                body: "body".to_string(),
                ..IssueFields::default()
            },
            Some(state.to_string()),
        )
    }

    fn link() -> Link {
        Link::new(
            EntityRef {
                connector: linear(),
                kind: crate::domain::EntityKind::Issue,
                scope: Some("VED".into()),
                native_id: "issue-1".into(),
                url: None,
            },
            EntityRef {
                connector: forge(),
                kind: crate::domain::EntityKind::Issue,
                scope: Some("Vedaru/linear-cli-rs".into()),
                native_id: "12".into(),
                url: None,
            },
        )
    }

    #[test]
    fn a_recorded_link_pairs_the_two_sides() {
        let source = vec![found(&linear(), "issue-1", "VED", "One", "Todo")];
        let sink = vec![found(&forge(), "12", "Vedaru/linear-cli-rs", "One", "open")];

        let pairings = pair_up(&source, &sink, &linear(), &forge(), &[link()]);

        assert_eq!(pairings.len(), 1);
        assert!(pairings[0].is_pair());
        assert_eq!(pairings[0].matched, Match::Link);
        assert!(pairings[0].link.is_some());
    }

    #[test]
    fn a_marker_in_the_copy_pairs_them_without_a_link() {
        let mut copied = found(&forge(), "12", "Vedaru/linear-cli-rs", "One", "open");
        copied.fields.body =
            markers::with_marker("body", &markers::OriginMarker::new("linear", "issue-1"));
        let source = vec![found(&linear(), "issue-1", "VED", "One", "Todo")];

        let pairings = pair_up(&source, &[copied], &linear(), &forge(), &[]);

        assert_eq!(pairings.len(), 1);
        assert_eq!(pairings[0].matched, Match::Marker);
        assert_eq!(
            pairings[0]
                .sink
                .as_ref()
                .expect("a sink")
                .reference
                .native_id,
            "12"
        );
        assert!(pairings[0].link.is_none(), "nothing recorded it yet");
    }

    #[test]
    fn a_marker_on_the_other_side_pairs_them_too() {
        // The copy is on the *source* side: the first mirror ran the other way.
        let mut original = found(&linear(), "issue-1", "VED", "One", "Todo");
        original.fields.body =
            markers::with_marker("body", &markers::OriginMarker::new("forgejo", "12"));
        let sink = vec![found(&forge(), "12", "Vedaru/linear-cli-rs", "One", "open")];

        let pairings = pair_up(&[original], &sink, &linear(), &forge(), &[]);

        assert_eq!(pairings.len(), 1);
        assert_eq!(pairings[0].matched, Match::Marker);
    }

    #[test]
    fn a_project_pairs_by_the_marker_embedded_in_its_description() {
        // A forge project has no body field of its own, so the copy's *description*
        // carries the marker - which is the marker a forge project read spec reads
        // back into the neutral body. Pairing has to look there, or a swept project
        // is a stranger and the sweep duplicates it.
        let project = |connector: &ConnectorId, id: &str, title: &str| {
            Found::new(
                EntityRef {
                    connector: connector.clone(),
                    kind: crate::domain::EntityKind::Project,
                    scope: Some("scope".to_string()),
                    native_id: id.to_string(),
                    url: None,
                },
                IssueFields {
                    title: title.to_string(),
                    body: "the description".to_string(),
                    ..IssueFields::default()
                },
                None,
            )
        };
        let mut copied = project(&forge(), "4", "Mirror the widget");
        copied.fields.body = markers::with_marker(
            "the description",
            &markers::OriginMarker::new("linear", "project-uuid"),
        );
        let source = vec![project(&linear(), "project-uuid", "Mirror the widget")];

        let pairings = pair_up(&source, &[copied], &linear(), &forge(), &[]);

        assert_eq!(pairings.len(), 1);
        assert!(pairings[0].is_pair());
        assert_eq!(pairings[0].matched, Match::Marker);
    }

    #[test]
    fn two_issues_that_merely_look_alike_are_not_paired() {
        // The whole safety of a sweep: identity is proven by a link or a marker, and a
        // sweep that matched on titles would mirror every issue into a duplicate of
        // itself the first time it ran.
        let source = vec![found(
            &linear(),
            "issue-1",
            "VED",
            "Deploy the widget",
            "Todo",
        )];
        let sink = vec![found(
            &forge(),
            "12",
            "Vedaru/linear-cli-rs",
            "Deploy the widget",
            "open",
        )];

        let pairings = pair_up(&source, &sink, &linear(), &forge(), &[]);

        assert_eq!(pairings.len(), 2, "one unpaired on each side, not a pair");
        assert!(pairings.iter().all(|pairing| !pairing.is_pair()));
        assert!(pairings
            .iter()
            .all(|pairing| pairing.matched == Match::Nothing));
    }

    #[test]
    fn an_entity_with_nothing_on_the_other_side_comes_out_unpaired() {
        let source = vec![
            found(&linear(), "issue-1", "VED", "One", "Todo"),
            found(&linear(), "issue-2", "VED", "Two", "Todo"),
        ];
        let sink = vec![found(&forge(), "12", "Vedaru/linear-cli-rs", "One", "open")];

        let pairings = pair_up(&source, &sink, &linear(), &forge(), &[link()]);

        assert_eq!(pairings.len(), 2);
        assert!(pairings
            .iter()
            .any(|pairing| pairing.matched == Match::Link));
        let unpaired = pairings
            .iter()
            .find(|pairing| pairing.sink.is_none())
            .expect("the second source entity has no counterpart");
        assert_eq!(
            unpaired
                .source
                .as_ref()
                .expect("a source")
                .reference
                .native_id,
            "issue-2"
        );
    }

    #[test]
    fn a_link_whose_counterpart_is_absent_survives_the_missing_read() {
        // The list lagged the write: the source is here, the copy it was linked to is
        // not. The pairing must keep the link so the judge can tell this from a stranger
        // it should create from - which is the duplicate VED-291 describes.
        let source = vec![found(&linear(), "issue-1", "VED", "One", "Todo")];

        let pairings = pair_up(&source, &[], &linear(), &forge(), &[link()]);

        assert_eq!(pairings.len(), 1);
        assert!(!pairings[0].is_pair());
        assert_eq!(pairings[0].matched, Match::Link);
        assert!(
            pairings[0].link.is_some(),
            "the link must survive the read that missed its counterpart"
        );
    }

    #[test]
    fn a_link_whose_source_is_absent_survives_on_the_sink_side() {
        // The same lag on the other list: the sink entity is here and linked, its source
        // counterpart is not. A sweep must not create a second source from it.
        let sink = vec![found(&forge(), "12", "Vedaru/linear-cli-rs", "One", "open")];

        let pairings = pair_up(&[], &sink, &linear(), &forge(), &[link()]);

        assert_eq!(pairings.len(), 1);
        assert!(!pairings[0].is_pair());
        assert_eq!(pairings[0].matched, Match::Link);
        assert!(pairings[0].link.is_some());
    }

    fn view<'a>(found: &'a Found, fields: &'a IssueFields, names: &'a StateNames) -> View<'a> {
        View {
            found,
            fields,
            names,
        }
    }

    #[test]
    fn a_side_that_still_reads_as_the_record_leaves_the_other_one_to_write() {
        let source_names = names(&["Done"], "In Progress");
        let sink_names = names(&["closed"], "open");
        let source = found(&linear(), "issue-1", "VED", "One", "In Progress");
        let sink = found(
            &forge(),
            "12",
            "Vedaru/linear-cli-rs",
            "One (edited)",
            "open",
        );

        // The record holds the source's revision as the *sink* would hold it.
        let recorded = content_key_with(&source.fields, source_names.openness(Some("In Progress")));
        let link = link().with_hash(recorded);

        let verdict = verdict(
            &link,
            view(&source, &source.fields, &source_names),
            view(&sink, &sink.fields, &sink_names),
        );
        assert_eq!(verdict, Verdict::Moved(WhichSide::Sink));
    }

    #[test]
    fn a_sink_that_still_reads_as_the_record_leaves_the_source_to_write() {
        let source_names = names(&["Done"], "In Progress");
        let sink_names = names(&["closed"], "open");
        let source = found(&linear(), "issue-1", "VED", "One (edited)", "In Progress");
        let sink = found(&forge(), "12", "Vedaru/linear-cli-rs", "One", "open");

        let recorded = content_key_with(&sink.fields, sink_names.openness(Some("open")));
        let link = link().with_hash(recorded);

        let verdict = verdict(
            &link,
            view(&source, &source.fields, &source_names),
            view(&sink, &sink.fields, &sink_names),
        );
        assert_eq!(verdict, Verdict::Moved(WhichSide::Source));
    }

    #[test]
    fn both_sides_having_changed_is_a_conflict_rather_than_a_guess() {
        let source_names = names(&["Done"], "In Progress");
        let sink_names = names(&["closed"], "open");
        let source = found(&linear(), "issue-1", "VED", "Source edit", "In Progress");
        let sink = found(&forge(), "12", "Vedaru/linear-cli-rs", "Sink edit", "open");

        // A record neither side matches: both moved since the last write.
        let link = link().with_hash(content_key_with(&IssueFields::default(), Openness::Open));

        let verdict = verdict(
            &link,
            view(&source, &source.fields, &source_names),
            view(&sink, &sink.fields, &sink_names),
        );
        assert_eq!(verdict, Verdict::Conflict);
    }

    #[test]
    fn a_record_from_an_older_vocabulary_is_replaced_rather_than_argued_with() {
        // Adding a field to the signature changes every key at once, so a record written
        // before the addition matches neither side. Read as "both sides moved", every pair
        // in an upgraded store would sit in `Conflict` for ever - reported, never acted on,
        // which is a frozen mirror rather than a cautious one.
        let source_names = names(&["Done"], "In Progress");
        let sink_names = names(&["closed"], "open");
        let source = found(&linear(), "issue-1", "VED", "Source edit", "In Progress");
        let sink = found(&forge(), "12", "Vedaru/linear-cli-rs", "Sink edit", "open");

        // A record the way an older vocabulary left it: no tag this engine knows.
        let mut link = link();
        link.last_synced_hash = Some("Team|title|open".to_string());

        let verdict = verdict(
            &link,
            view(&source, &source.fields, &source_names),
            view(&sink, &sink.fields, &sink_names),
        );
        assert_eq!(verdict, Verdict::Adopted);
    }

    #[test]
    fn a_record_this_vocabulary_wrote_is_still_read() {
        // The other half: a tagged record keeps its meaning, so the fix re-baselines an
        // old store once instead of disabling the comparison for everything.
        let source_names = names(&["Done"], "In Progress");
        let sink_names = names(&["closed"], "open");
        let source = found(&linear(), "issue-1", "VED", "One", "In Progress");
        let sink = found(
            &forge(),
            "12",
            "Vedaru/linear-cli-rs",
            "One (edited)",
            "open",
        );

        let recorded = content_key_with(&source.fields, source_names.openness(Some("In Progress")));
        let link = link().with_hash(recorded);

        let verdict = verdict(
            &link,
            view(&source, &source.fields, &source_names),
            view(&sink, &sink.fields, &sink_names),
        );
        assert_eq!(verdict, Verdict::Moved(WhichSide::Sink));
    }

    #[test]
    fn sides_that_agree_need_no_record_at_all() {
        let source_names = names(&["Done"], "In Progress");
        let sink_names = names(&["closed"], "open");
        let source = found(&linear(), "issue-1", "VED", "One", "In Progress");
        let sink = found(&forge(), "12", "Vedaru/linear-cli-rs", "One", "open");

        let verdict = verdict(
            &link(),
            view(&source, &source.fields, &source_names),
            view(&sink, &sink.fields, &sink_names),
        );
        assert_eq!(verdict, Verdict::InStep);
    }

    #[test]
    fn a_pair_with_no_record_keeps_the_sources_revision() {
        let source_names = names(&["Done"], "In Progress");
        let sink_names = names(&["closed"], "open");
        let source = found(&linear(), "issue-1", "VED", "One", "In Progress");
        let sink = found(
            &forge(),
            "12",
            "Vedaru/linear-cli-rs",
            "Something else",
            "open",
        );

        let verdict = verdict(
            &link(),
            view(&source, &source.fields, &source_names),
            view(&sink, &sink.fields, &sink_names),
        );
        assert_eq!(verdict, Verdict::Adopted);
    }
}
