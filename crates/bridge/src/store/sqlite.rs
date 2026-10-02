//! SQLite driver.
//!
//! One connection per thread and no shared `Mutex`: SQLite serialises writers
//! itself, so a connection per worker is both simpler and faster than a queue in
//! front of a shared handle. WAL keeps readers from blocking the intake path
//! while a worker is mid-transaction.

use std::path::Path;
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension};

use crate::clock::now_millis;
use crate::domain::{Action, ConnectorId, EntityKind};
use crate::error::{Error, Result};
use crate::store::{Counts, Delivery, InsertOutcome, NewDelivery, Store};

/// Schema revision, applied with `include_str!` so the SQL ships inside the
/// binary: a service deployed as a single file must not need the source tree to
/// migrate its own database.
const MIGRATIONS: &[&str] = &[include_str!("../../migrations/0001_init.sql")];

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

pub struct SqliteStore {
    conn: Connection,
}

impl SqliteStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        Self::prepare(conn, true)
    }

    /// A private in-memory database. Used by tests, where the schema still has
    /// to be created because nothing else will.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::prepare(conn, false)
    }

    fn prepare(conn: Connection, on_disk: bool) -> Result<Self> {
        conn.busy_timeout(BUSY_TIMEOUT)?;
        if on_disk {
            // WAL is persistent per database, but setting it is idempotent and
            // cheap, and the alternative - noticing a missing WAL in production -
            // is a lock-contention bug that only shows up under load.
            conn.pragma_update(None, "journal_mode", "WAL")?;
        }
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let mut store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    /// Test helper: the store currently has no other way to be inspected.
    #[cfg(test)]
    fn connection(&self) -> &Connection {
        &self.conn
    }
}

impl Store for SqliteStore {
    fn migrate(&mut self) -> Result<()> {
        // Runs inside a transaction so a failure part way through a migration
        // leaves the database exactly as it was.
        let tx = self.conn.transaction()?;
        tx.execute_batch("CREATE TABLE IF NOT EXISTS schema_version (revision INTEGER NOT NULL)")?;
        let applied: Option<i64> = tx
            .query_row("SELECT MAX(revision) FROM schema_version", [], |row| {
                row.get::<_, Option<i64>>(0)
            })
            .optional()?
            .flatten();
        let mut revision = applied.unwrap_or(0);
        for (index, sql) in MIGRATIONS.iter().enumerate() {
            let number = index as i64 + 1;
            if number <= revision {
                continue;
            }
            tx.execute_batch(sql)?;
            tx.execute("INSERT INTO schema_version (revision) VALUES (?)", [number])?;
            revision = number;
        }
        tx.commit()?;
        Ok(())
    }

    fn insert_delivery(&mut self, delivery: &NewDelivery) -> Result<InsertOutcome> {
        let now = now_millis();
        let changed = self.conn.execute(
            "INSERT INTO deliveries
                 (connector, delivery_id, event, kind, action, scope, native_id, body,
                  status, attempts, available_at, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'pending', 0, ?9, ?9)
             ON CONFLICT (connector, delivery_id) DO NOTHING",
            params![
                delivery.connector.as_str(),
                delivery.delivery_id,
                delivery.event,
                delivery.kind.as_str(),
                delivery.action.as_str(),
                delivery.scope,
                delivery.native_id,
                delivery.body,
                now,
            ],
        )?;
        Ok(if changed == 0 {
            InsertOutcome::Duplicate
        } else {
            InsertOutcome::Inserted
        })
    }

    fn claim_due(&mut self, limit: usize, lock_for: Duration) -> Result<Vec<Delivery>> {
        let now = now_millis();
        let lease_until = now + lock_for.as_millis() as i64;
        // `available_at` doubles as the lease deadline while a row is active, so
        // `<= now` means the lease has expired and the row is reclaimable. A
        // zero-length lease therefore hides nothing - which is what a test (and a
        // caller that wants synchronous processing) expects.
        let mut statement = self.conn.prepare(
            "UPDATE deliveries
                SET status = 'active', attempts = attempts + 1, available_at = ?2
              WHERE id IN (
                    SELECT id FROM deliveries
                     WHERE (status = 'pending' AND available_at <= ?1)
                        OR (status = 'active' AND available_at <= ?1)
                     ORDER BY available_at ASC
                     LIMIT ?3
              )
              RETURNING id, connector, delivery_id, event, kind, action, scope, native_id,
                        body, attempts, last_error",
        )?;
        let rows = statement.query_map(params![now, lease_until, limit as i64], |row| {
            Ok(Delivery {
                id: row.get(0)?,
                connector: ConnectorId::new(row.get::<_, String>(1)?),
                delivery_id: row.get(2)?,
                event: row.get(3)?,
                kind: kind_from_name(&row.get::<_, String>(4)?),
                action: action_from_name(&row.get::<_, String>(5)?),
                scope: row.get(6)?,
                native_id: row.get(7)?,
                body: row.get(8)?,
                attempts: row.get::<_, i64>(9)?.max(0) as u32,
                last_error: row.get(10)?,
            })
        })?;
        let mut claimed = Vec::new();
        for row in rows {
            claimed.push(row?);
        }
        Ok(claimed)
    }

    fn complete(&mut self, id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE deliveries SET status = 'done', last_error = NULL WHERE id = ?1",
            [id],
        )?;
        Ok(())
    }

    fn fail(&mut self, id: i64, error: &str, retry_after: Option<Duration>) -> Result<()> {
        match retry_after {
            Some(delay) => {
                let available_at = now_millis() + delay.as_millis() as i64;
                self.conn.execute(
                    "UPDATE deliveries
                        SET status = 'pending', available_at = ?2, last_error = ?3
                      WHERE id = ?1",
                    params![id, available_at, error],
                )?;
            }
            None => {
                self.conn.execute(
                    "UPDATE deliveries SET status = 'dead', last_error = ?2 WHERE id = ?1",
                    params![id, error],
                )?;
            }
        }
        Ok(())
    }

    fn counts(&mut self) -> Result<Counts> {
        let mut statement = self
            .conn
            .prepare("SELECT status, COUNT(*) FROM deliveries GROUP BY status")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        let mut counts = Counts::default();
        for row in rows {
            let (status, count) = row?;
            match status.as_str() {
                "pending" => counts.pending = count,
                "active" => counts.active = count,
                "done" => counts.done = count,
                "dead" => counts.dead = count,
                other => log::warn!("unknown delivery status `{other}` in store"),
            }
        }
        Ok(counts)
    }

    fn health(&mut self) -> Result<()> {
        self.conn
            .query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
            .map(|_| ())
            .map_err(Error::from)
    }
}

/// Unknown kinds and actions come from a newer provider; they are stored and
/// reported rather than dropped, and they never abort a parse.
fn kind_from_name(name: &str) -> EntityKind {
    match name {
        "issue" => EntityKind::Issue,
        "comment" => EntityKind::Comment,
        "reference" => EntityKind::Reference,
        other => EntityKind::Other(other.to_owned()),
    }
}

fn action_from_name(name: &str) -> Action {
    match name {
        "created" => Action::Created,
        "updated" => Action::Updated,
        "closed" => Action::Closed,
        "reopened" => Action::Reopened,
        "deleted" => Action::Deleted,
        other => Action::Other(other.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    fn new_delivery(delivery_id: &str) -> NewDelivery {
        NewDelivery {
            connector: ConnectorId::new("forgejo"),
            delivery_id: delivery_id.to_string(),
            event: "issues".into(),
            kind: EntityKind::Issue,
            action: Action::Created,
            scope: Some("Vedaru/linear-cli-rs".into()),
            native_id: "7".into(),
            body: "{\"action\":\"opened\"}".into(),
        }
    }

    #[test]
    fn migrations_are_idempotent_and_recorded() {
        let mut store = SqliteStore::open_in_memory().unwrap();
        store.migrate().unwrap();
        store.migrate().unwrap();
        let revisions: i64 = store
            .connection()
            .query_row("SELECT COUNT(*) FROM schema_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(revisions, 1);
    }

    #[test]
    fn intake_is_idempotent_on_the_delivery_id() {
        let mut store = SqliteStore::open_in_memory().unwrap();
        assert_eq!(
            store.insert_delivery(&new_delivery("d-1")).unwrap(),
            InsertOutcome::Inserted
        );
        assert_eq!(
            store.insert_delivery(&new_delivery("d-1")).unwrap(),
            InsertOutcome::Duplicate
        );
        assert_eq!(store.counts().unwrap().pending, 1);
    }

    #[test]
    fn the_same_delivery_id_on_two_connectors_is_not_a_duplicate() {
        let mut store = SqliteStore::open_in_memory().unwrap();
        store.insert_delivery(&new_delivery("d-1")).unwrap();
        let mut other = new_delivery("d-1");
        other.connector = ConnectorId::new("linear");
        assert_eq!(
            store.insert_delivery(&other).unwrap(),
            InsertOutcome::Inserted
        );
        assert_eq!(store.counts().unwrap().pending, 2);
    }

    #[test]
    fn a_claim_marks_the_row_active_and_records_the_attempt() {
        let mut store = SqliteStore::open_in_memory().unwrap();
        store.insert_delivery(&new_delivery("d-1")).unwrap();
        let claimed = store.claim_due(4, Duration::from_secs(60)).unwrap();
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].attempts, 1);
        assert_eq!(claimed[0].native_id, "7");
        assert_eq!(claimed[0].scope.as_deref(), Some("Vedaru/linear-cli-rs"));
        // Claimed, so not due again while the lease holds.
        assert!(store
            .claim_due(4, Duration::from_secs(60))
            .unwrap()
            .is_empty());
        assert_eq!(store.counts().unwrap().active, 1);
    }

    #[test]
    fn an_expired_lease_is_reclaimed_so_a_crash_cannot_lose_work() {
        let mut store = SqliteStore::open_in_memory().unwrap();
        store.insert_delivery(&new_delivery("d-1")).unwrap();
        // A zero-length lease is already expired on the next tick.
        assert_eq!(store.claim_due(1, Duration::ZERO).unwrap().len(), 1);
        let reclaimed = store.claim_due(1, Duration::from_secs(60)).unwrap();
        assert_eq!(reclaimed.len(), 1);
        assert_eq!(reclaimed[0].attempts, 2);
    }

    #[test]
    fn completion_and_failure_move_the_row_as_intended() {
        let mut store = SqliteStore::open_in_memory().unwrap();
        store.insert_delivery(&new_delivery("d-1")).unwrap();
        store.insert_delivery(&new_delivery("d-2")).unwrap();
        let claimed = store.claim_due(10, Duration::from_secs(60)).unwrap();

        store.complete(claimed[0].id).unwrap();
        store
            .fail(claimed[1].id, "boom", Some(Duration::ZERO))
            .unwrap();
        let counts = store.counts().unwrap();
        assert_eq!(counts.done, 1);
        assert_eq!(counts.pending, 1);

        // The retry is immediately due and keeps its error message.
        let retried = store.claim_due(1, Duration::from_secs(60)).unwrap();
        assert_eq!(retried.len(), 1);
        assert_eq!(retried[0].last_error.as_deref(), Some("boom"));

        store.fail(retried[0].id, "again", None).unwrap();
        assert_eq!(store.counts().unwrap().dead, 1);
        assert!(store
            .claim_due(10, Duration::from_secs(60))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn health_reports_a_live_connection() {
        let mut store = SqliteStore::open_in_memory().unwrap();
        assert!(store.health().is_ok());
    }
}
