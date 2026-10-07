//! One end of a mapping, and the mapping itself: reference parsing and scope lookup.
//!
//! Split out of `handler.rs` (VED-288). The parent re-exports both types, because `config.rs`
//! builds them from configuration and names them from outside this module.

use super::*;
use crate::domain::parse_connector_ref;
use crate::reconcile::placement::ProjectScopes;

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
    /// `[[mapping.project]]`: which sink scope each project's mirror lives in. Placement
    /// is configuration, not something read off a project's links, which are written for
    /// people and change for human reasons.
    pub project_scopes: ProjectScopes,
}

impl Mapping {
    /// The end an event arrived on, if it arrived on one of them.
    ///
    /// Scope matters as much as the connector: one Linear workspace and one forge
    /// can be paired several times over (per team, per repository), and a mapping
    /// that ignored the scope would mirror the wrong repository's issues.
    ///
    /// The sink side is matched against every scope this mapping declares. A repository a
    /// project is configured into is not in that set - the handler asks the link store
    /// instead ([`ReconcileHandler::linked_side`]), which is what keeps an event from a
    /// repository nobody mapped out of this mapping.
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

    /// Every scope on the sink this mapping declares, which is its own container.
    ///
    /// A mapping also writes the scopes its entities are configured into; a sweep reads
    /// those from the placement of each entity, and the link store answers for an event
    /// from a configured repository.
    pub fn sink_scopes(&self) -> Vec<&str> {
        vec![self.sink.scope.as_str()]
    }

    pub(super) fn endpoint(&self, side: Side) -> &Endpoint {
        match side {
            Side::Source => &self.source,
            Side::Sink => &self.sink,
        }
    }
}
