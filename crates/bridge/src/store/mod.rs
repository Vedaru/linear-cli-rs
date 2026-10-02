//! Durable state: the delivery queue and the link tables.
//!
//! The [`Store`] trait is the whole persistence contract. SQLite implements it
//! today; a Postgres driver would be an addition, not a rewrite, because the
//! trait speaks in the domain's terms rather than in SQL.

pub mod sqlite;

use std::time::Duration;

use crate::domain::{Action, ConnectorId, EntityKind, EntityRef};
use crate::error::Result;

/// A delivery row as it is about to be inserted.
#[derive(Clone, Debug)]
pub struct NewDelivery {
    pub connector: ConnectorId,
    pub delivery_id: String,
    /// The provider's event name, for logs and operator correlation.
    pub event: String,
    pub kind: EntityKind,
    pub action: Action,
    pub scope: Option<String>,
    pub native_id: String,
    /// Raw request body, stored verbatim so a delivery can be replayed.
    pub body: String,
}

/// A delivery row as it was stored.
#[derive(Clone, Debug)]
pub struct Delivery {
    pub id: i64,
    pub connector: ConnectorId,
    pub delivery_id: String,
    pub event: String,
    pub kind: EntityKind,
    pub action: Action,
    pub scope: Option<String>,
    pub native_id: String,
    pub body: String,
    /// Attempts *including* the one in progress.
    pub attempts: u32,
    pub last_error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryStatus {
    Pending,
    /// Claimed by a worker. A row left here by a crash is reclaimed once its
    /// lease expires, which is why the lease and the retry delay share a clock.
    Active,
    Done,
    /// Out of attempts. Kept for an operator: a dead delivery is the only
    /// evidence of what failed.
    Dead,
}

impl DeliveryStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            DeliveryStatus::Pending => "pending",
            DeliveryStatus::Active => "active",
            DeliveryStatus::Done => "done",
            DeliveryStatus::Dead => "dead",
        }
    }
}

/// What `insert_delivery` did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InsertOutcome {
    Inserted,
    /// The same (connector, delivery id) was already stored: a provider retry.
    Duplicate,
}

/// Row counts by status, for health output and `sync status`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub pending: i64,
    pub active: i64,
    pub done: i64,
    pub dead: i64,
}

/// A mirrored pair: the same entity, as two platforms know it.
///
/// `last_synced_hash` is the signature of the content this bridge last wrote
/// across the link. It is what makes an event a no-op when the change it reports
/// is the echo of our own write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub left: EntityRef,
    pub right: EntityRef,
    pub last_synced_hash: Option<String>,
}

impl Link {
    pub fn new(left: EntityRef, right: EntityRef) -> Self {
        Self {
            left,
            right,
            last_synced_hash: None,
        }
    }

    pub fn with_hash(mut self, hash: impl Into<String>) -> Self {
        self.last_synced_hash = Some(hash.into());
        self
    }

    /// The other end of this link, when `side` is one of them.
    pub fn counterpart(&self, side: &EntityRef) -> Option<&EntityRef> {
        if self.left.same_entity(side) {
            Some(&self.right)
        } else if self.right.same_entity(side) {
            Some(&self.left)
        } else {
            None
        }
    }

    /// True when this link pairs `side` with an entity on `counterpart`.
    pub fn pairs(&self, side: &EntityRef, counterpart: &ConnectorId) -> bool {
        match self.counterpart(side) {
            Some(other) => &other.connector == counterpart,
            None => false,
        }
    }

    pub fn entities(&self) -> [&EntityRef; 2] {
        [&self.left, &self.right]
    }
}

/// A reference from a commit or pull request to an entity it mentions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReferenceLink {
    pub source: EntityRef,
    pub target: EntityRef,
    pub url: Option<String>,
}

pub trait Store: Send {
    /// Apply any pending migrations. Idempotent, called once per connection.
    fn migrate(&mut self) -> Result<()>;

    /// Store a delivery, or report that this delivery id is already known.
    fn insert_delivery(&mut self, delivery: &NewDelivery) -> Result<InsertOutcome>;

    /// Claim up to `limit` due deliveries, marking them active for `lock_for`.
    ///
    /// "Due" includes rows a crashed worker left `active` whose lease has
    /// expired, so a delivery is never lost to a process that died mid-flight.
    fn claim_due(&mut self, limit: usize, lock_for: Duration) -> Result<Vec<Delivery>>;

    fn complete(&mut self, id: i64) -> Result<()>;

    /// Record a failure. `retry_after` reschedules; `None` parks the delivery as
    /// dead, which is the queue's decision once attempts are exhausted.
    fn fail(&mut self, id: i64, error: &str, retry_after: Option<Duration>) -> Result<()>;

    fn counts(&mut self) -> Result<Counts>;

    /// Cheapest possible liveness check on the store, used by `/healthz`.
    fn health(&mut self) -> Result<()>;

    /// Create or refresh a mirror link.
    ///
    /// A `None` hash preserves whatever hash the link already carried: a caller
    /// that knows the pairing but not the content must not erase the record of
    /// what was last written, or the next echo of our own write looks like a real
    /// change.
    fn upsert_link(&mut self, link: &Link) -> Result<()>;

    /// Every link that has `side` at either end.
    fn find_links(&mut self, side: &EntityRef) -> Result<Vec<Link>>;

    /// The link pairing `side` with `counterpart`, if one exists.
    fn find_link(&mut self, side: &EntityRef, counterpart: &ConnectorId) -> Result<Option<Link>>;

    /// Remove the link(s) involving `side` - used after a deletion on either
    /// side, so a later re-creation starts cleanly instead of resuming a stale
    /// pairing.
    fn delete_links(&mut self, side: &EntityRef) -> Result<()>;

    /// Record that `source` (a pull request, a commit) referenced `target`.
    /// Idempotent: the same pair recorded twice is one row.
    fn record_reference(&mut self, link: &ReferenceLink) -> Result<()>;

    /// Every reference pointing at `target`.
    fn references_for(&mut self, target: &EntityRef) -> Result<Vec<ReferenceLink>>;
}
