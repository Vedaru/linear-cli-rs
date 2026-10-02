//! Events: what a delivery means, once its platform's shape has been dropped.

use crate::domain::{Actor, EntityKind, EntityRef};

/// Provider-supplied delivery id, or a content hash when the provider omits
/// one. Together with the connector name this is the idempotency key.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DeliveryId(String);

impl DeliveryId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What happened, normalized across platforms.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Created,
    Updated,
    Closed,
    Reopened,
    Deleted,
    /// An action this build does not model; kept so the delivery is acknowledged
    /// rather than retried forever by the provider.
    Other(String),
}

impl Action {
    pub fn as_str(&self) -> &str {
        match self {
            Action::Created => "created",
            Action::Updated => "updated",
            Action::Closed => "closed",
            Action::Reopened => "reopened",
            Action::Deleted => "deleted",
            Action::Other(name) => name,
        }
    }
}

/// Extra meaning the reconciler needs, without handing it a platform payload.
///
/// Keep this small: a variant earns its place by being consumed by the reconciler.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EventDetail {
    /// Nothing beyond the subject.
    None,
    Comment {
        /// The comment's own id on the platform that sent it.
        ///
        /// A comment is *about* an issue - that is what the subject carries, and
        /// what a link pairs - so the comment's identity has to live somewhere, and
        /// the marker needs it to tell a copy of ours from a user's new comment.
        id: Option<String>,
        body: Option<String>,
    },
    /// Text to mine for issue references - a commit message, a pull-request
    /// title/body. The platform's reference *syntax* is the connector's business,
    /// so the patterns are attached by the connector that owns them.
    ///
    /// The keywords are owned rather than `'static`: a connector built from
    /// configuration cannot hand out a `'static` slice, and leaking one per event
    /// to fake it would trade a real memory leak for a cosmetic type.
    Reference {
        text: String,
        closing_keywords: Vec<String>,
        /// Whether the review request this reference is about has been merged.
        ///
        /// `None` is a reference that is not a review request at all - a commit message is a
        /// mention, not a workflow step. `Some(false)` is an open request, `Some(true)` a
        /// merged one, and the difference is the whole reason a merge is not a close.
        merged: Option<bool>,
    },
}

/// One normalized event, produced by a [`crate::connector::Source`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub connector: crate::domain::ConnectorId,
    pub delivery: DeliveryId,
    /// The platform's own event name (`issues`, `Issue`, ...), kept for logs and
    /// metrics so an operator can correlate with the provider's UI.
    pub event: String,
    pub kind: EntityKind,
    pub action: Action,
    pub subject: EntityRef,
    pub actor: Option<Actor>,
    pub detail: EventDetail,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_names_are_stable() {
        assert_eq!(Action::Created.as_str(), "created");
        assert_eq!(Action::Other("labeled".into()).as_str(), "labeled");
    }

    #[test]
    fn entity_kinds_round_trip_through_their_names() {
        assert_eq!(EntityKind::Issue.as_str(), "issue");
        assert_eq!(EntityKind::Other("milestone".into()).as_str(), "milestone");
    }
}
