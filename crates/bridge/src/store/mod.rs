//! Durable state: the delivery queue and the link tables.
//!
//! The [`Store`] trait is the whole persistence contract. SQLite implements it
//! today; a Postgres driver would be an addition, not a rewrite, because the
//! trait speaks in the domain's terms rather than in SQL.

pub mod sqlite;

use std::time::Duration;

use crate::domain::{Action, ConnectorId, EntityKind};
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
}
