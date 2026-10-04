//! Executing a plan: read both sides, write the other one, record what was written.
//!
//! The handler owns no opinion. Everything it does is `plan`'s answer plus the I/O
//! to carry it out, and the one thing it *must* get right is the bookkeeping: the
//! link row's content key is what makes the echo of this very write recognisable
//! when it comes back as a webhook a second later.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use crate::connector::Source;
use crate::domain::{
    markers, references, Capabilities, Change, ConnectorId, EntityKind, EntityRef, Event,
    EventDetail, IssueFields, Patch, UserMap,
};
use crate::error::{Error, Result};
use crate::queue::Handler;
use crate::reconcile::projection::{Projected, Projection, Skipped};
use crate::reconcile::route::{Identity, Placement, Routes};
use crate::reconcile::survey::{Action, Entry};
use crate::reconcile::sweep::{self, Found};
use crate::reconcile::{
    content_key, converge, plan, Context, Direction, Nothing, Openness, Pairwise, Policy, Side,
    Sides, Snapshot, StateNames, Step,
};
use crate::sink::{BoardCards, CardColumn, RemoteIssue, Sink};
use crate::store::{Delivery, Link, ReferenceLink, Store};

mod decide;
mod endpoint;

// The move must not change how the rest of the crate names these: `config.rs` imports
// `crate::reconcile::handler::{Endpoint, Mapping}` and continues to.
use decide::*;
pub use endpoint::{Endpoint, Mapping};
// The integration tests build a policy through this path; it stays public and stays here.
pub use decide::default_policy;

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
    /// One board read per `(connector, scope, project)` per survey. A board is the
    /// same for every card on it, so a sweep must not read it once per card; the
    /// value is `None` when the sink cannot report placement at all. Cleared at the
    /// start of each survey, so a long-lived service never serves a stale board.
    boards: HashMap<(ConnectorId, String, String), Option<BoardCards>>,
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
            boards: HashMap::new(),
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

mod deliver;
mod judge;
mod project;
mod survey;

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
        // because the header that carried it was not stored. `reparse`, not
        // `parse`: freshness was intake's to check, and this body has waited in a
        // durable queue since it passed.
        let headers = source.replay_headers(&delivery.event);
        let events = source
            .reparse(&headers, delivery.body.as_bytes())
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
