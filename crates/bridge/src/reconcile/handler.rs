//! Executing a plan: read both sides, write the other one, record what was written.
//!
//! The handler owns no opinion. Everything it does is `plan`'s answer plus the I/O
//! to carry it out, and the one thing it *must* get right is the bookkeeping: the
//! link row's content key is what makes the echo of this very write recognisable
//! when it comes back as a webhook a second later.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::connector::Source;
use crate::domain::{parse_connector_ref, ConnectorId, EntityKind, EntityRef, Event, UserMap};
use crate::error::{Error, Result};
use crate::queue::Handler;
use crate::reconcile::projection::{Projected, Projection, Skipped};
use crate::reconcile::{
    content_key, plan, Context, Direction, Nothing, Openness, Policy, Side, Sides, Snapshot,
    StateNames, Step,
};
use crate::sink::{RemoteIssue, Sink};
use crate::store::{Delivery, Link, Store};

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

        let observed = self.snapshot(&here.connector, &here.scope, &event.subject.native_id)?;
        let counterpart = match &counterpart_ref {
            Some(reference) => {
                self.snapshot(&there.connector, &there.scope, &reference.native_id)?
            }
            None => Snapshot::gone(),
        };

        // What the target can hold, and where an identity has no counterpart. Read
        // from the target's own capabilities rather than assumed, and computed before
        // the decision because it *is* the decision's input: the mirror compares what
        // it can bring into agreement, not what the source happens to say.
        let target = self.sink(&there.connector)?.capabilities();
        let projection = Projection::new(&target, &mapping.users);
        let expected = match &observed.fields {
            Some(fields) => projection.of(fields, &here.connector, &there.connector),
            None => Projected::default(),
        };

        let step = plan(&Context {
            event,
            side,
            policy: &mapping.policy,
            link: link.as_ref(),
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
                self.report_skipped(pair.mapping, &skipped);
                let sink = self.sink(&pair.there.connector)?;
                let created = sink.create_issue(&pair.there.scope, &fields, state.as_deref())?;
                let created_ref = EntityRef {
                    connector: pair.there.connector.clone(),
                    kind: EntityKind::Issue,
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
                self.store
                    .upsert_link(&Link::new(pair.subject.clone(), created_ref).with_hash(hash))?;
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
                let Some(reference) = pair.counterpart.cloned() else {
                    return Ok(());
                };
                self.report_skipped(pair.mapping, &skipped);
                let touched = patch.touched().join(", ");
                let sink = self.sink(&pair.there.connector)?;
                sink.update_issue(
                    &pair.there.scope,
                    &reference.native_id,
                    &patch,
                    state.as_deref(),
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
            Step::Attach { url, title } => {
                let Some(reference) = pair.counterpart.cloned() else {
                    return Ok(());
                };
                let sink = self.sink(&pair.there.connector)?;
                sink.attach(&pair.there.scope, &reference.native_id, &url, &title)?;
                log::info!(
                    "attached {} to {} {}",
                    url,
                    pair.there.connector,
                    reference.native_id
                );
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

    /// Say what did not travel, once per delivery, at a level an operator sees.
    ///
    /// Not an error - the mapping is still doing what it can - but never silent
    /// either: "the assignee did not come across" has to be findable in the log.
    fn report_skipped(&self, mapping: &str, skipped: &[Skipped]) {
        for skipped in skipped {
            log::warn!("`{mapping}`: {skipped}");
        }
    }

    /// One end's current state, as the platform reports it.
    fn snapshot(&self, connector: &ConnectorId, scope: &str, id: &str) -> Result<Snapshot> {
        let sink = self.sink(connector)?;
        Ok(match sink.fetch_issue(scope, id)? {
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
        assert_eq!(mapping.side_of(&event("github", "VED")), None);
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
