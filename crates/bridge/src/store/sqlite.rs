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
use crate::domain::{Action, ConnectorId, EntityKind, EntityRef};
use crate::error::{Error, Result};
use crate::store::{Counts, Delivery, InsertOutcome, Link, NewDelivery, ReferenceLink, Store};

/// Schema revision, applied with `include_str!` so the SQL ships inside the
/// binary: a service deployed as a single file must not need the source tree to
/// migrate its own database.
const MIGRATIONS: &[&str] = &[
    include_str!("../../migrations/0001_init.sql"),
    include_str!("../../migrations/0002_link_project.sql"),
];

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Every column a delivery read selects, in the order [`delivery_from_row`] reads them.
///
/// One list in one place: two readers that disagree about a column's position is a bug that
/// surfaces as a delivery whose error message belongs to another delivery.
const DELIVERY_COLUMNS: &str =
    "id, connector, delivery_id, event, kind, action, scope, native_id, body, attempts, last_error";

/// One row of [`DELIVERY_COLUMNS`], as a [`Delivery`].
fn delivery_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Delivery> {
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
}

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
            // WAL is persistent per database, but setting it is idempotent and cheap.
            //
            // Deliberately not fatal: switching the journal mode cannot be waited out
            // with `busy_timeout`, so when another connection holds the database for a
            // moment the switch fails - and a store that works in rollback-journal mode
            // is enormously better than a thread that dies because a *performance*
            // setting could not be applied.
            if let Err(error) = conn.pragma_update(None, "journal_mode", "WAL") {
                log::warn!("could not switch the store to WAL ({error}); continuing without it");
            }
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
        let mut statement = self.conn.prepare(&format!(
            "UPDATE deliveries
                SET status = 'active', attempts = attempts + 1, available_at = ?2
              WHERE id IN (
                    SELECT id FROM deliveries
                     WHERE (status = 'pending' AND available_at <= ?1)
                        OR (status = 'active' AND available_at <= ?1)
                     ORDER BY available_at ASC
                     LIMIT ?3
              )
              RETURNING {DELIVERY_COLUMNS}"
        ))?;
        let rows =
            statement.query_map(params![now, lease_until, limit as i64], delivery_from_row)?;
        let mut claimed = Vec::new();
        for row in rows {
            claimed.push(row?);
        }
        Ok(claimed)
    }

    fn find_delivery(&mut self, id: i64) -> Result<Option<Delivery>> {
        let mut statement = self.conn.prepare(&format!(
            "SELECT {DELIVERY_COLUMNS} FROM deliveries WHERE id = ?1"
        ))?;
        let mut rows = statement.query_map(params![id], delivery_from_row)?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    fn dead_deliveries(&mut self, limit: usize) -> Result<Vec<Delivery>> {
        let mut statement = self.conn.prepare(&format!(
            "SELECT {DELIVERY_COLUMNS} FROM deliveries
              WHERE status = 'dead'
              ORDER BY id DESC
              LIMIT ?1"
        ))?;
        let rows = statement.query_map(params![limit as i64], delivery_from_row)?;
        let mut dead = Vec::new();
        for row in rows {
            dead.push(row?);
        }
        Ok(dead)
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

    fn upsert_link(&mut self, link: &Link) -> Result<()> {
        self.conn.execute(
            "INSERT INTO entity_links
                 (left_connector, left_scope, left_kind, left_id,
                  right_connector, right_scope, right_kind, right_id,
                  last_synced_hash, project, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT (left_connector, left_scope, left_id,
                          right_connector, right_scope, right_id)
             DO UPDATE SET
                 -- Preserve a recorded hash when the caller has none: a caller
                 -- that knows the pairing but not the content must not erase what
                 -- was last written, or the next echo looks like a real change.
                 last_synced_hash = COALESCE(excluded.last_synced_hash, entity_links.last_synced_hash),
                 -- The same for the recorded project: a pairing written for the
                 -- hash alone (a sweep's baseline) must not forget which board the
                 -- issue was placed on. Clearing it is explicit, through
                 -- `set_link_project`, because `None` there means \"taken off\".
                 project = COALESCE(excluded.project, entity_links.project),
                 updated_at = excluded.updated_at",
            params![
                link.left.connector.as_str(),
                scope_of(&link.left),
                link.left.kind.as_str(),
                link.left.native_id,
                link.right.connector.as_str(),
                scope_of(&link.right),
                link.right.kind.as_str(),
                link.right.native_id,
                link.last_synced_hash,
                link.project,
                now_millis(),
            ],
        )?;
        Ok(())
    }

    fn find_links(&mut self, side: &EntityRef) -> Result<Vec<Link>> {
        let mut statement = self.conn.prepare(&format!(
            "SELECT {LINK_COLUMNS} FROM entity_links
              WHERE (left_connector = ?1 AND left_scope = ?2 AND left_id = ?3)
                 OR (right_connector = ?1 AND right_scope = ?2 AND right_id = ?3)
              ORDER BY id"
        ))?;
        let rows = statement.query_map(
            params![side.connector.as_str(), scope_of(side), side.native_id],
            link_from_row,
        )?;
        let mut links = Vec::new();
        for row in rows {
            links.push(row?);
        }
        Ok(links)
    }

    fn find_link(&mut self, side: &EntityRef, counterpart: &ConnectorId) -> Result<Option<Link>> {
        let mut statement = self.conn.prepare(&format!(
            "SELECT {LINK_COLUMNS} FROM entity_links
              WHERE (left_connector = ?1 AND left_scope = ?2 AND left_id = ?3
                     AND right_connector = ?4)
                 OR (right_connector = ?1 AND right_scope = ?2 AND right_id = ?3
                     AND left_connector = ?4)
              ORDER BY id
              LIMIT 1"
        ))?;
        let mut rows = statement.query_map(
            params![
                side.connector.as_str(),
                scope_of(side),
                side.native_id,
                counterpart.as_str()
            ],
            link_from_row,
        )?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    fn delete_links(&mut self, side: &EntityRef) -> Result<()> {
        self.conn.execute(
            "DELETE FROM entity_links
              WHERE (left_connector = ?1 AND left_scope = ?2 AND left_id = ?3)
                 OR (right_connector = ?1 AND right_scope = ?2 AND right_id = ?3)",
            params![side.connector.as_str(), scope_of(side), side.native_id],
        )?;
        Ok(())
    }

    fn set_link_project(&mut self, side: &EntityRef, project: Option<&str>) -> Result<()> {
        // Every pairing this issue is an end of, so it does not matter which way
        // round the link was written (a mirror may run either direction).
        self.conn.execute(
            "UPDATE entity_links SET project = ?4
              WHERE (left_connector = ?1 AND left_scope = ?2 AND left_id = ?3)
                 OR (right_connector = ?1 AND right_scope = ?2 AND right_id = ?3)",
            params![
                side.connector.as_str(),
                scope_of(side),
                side.native_id,
                project
            ],
        )?;
        Ok(())
    }

    fn link_project(&mut self, side: &EntityRef) -> Result<Option<String>> {
        let found: Option<Option<String>> = self
            .conn
            .query_row(
                "SELECT project FROM entity_links
                  WHERE (left_connector = ?1 AND left_scope = ?2 AND left_id = ?3)
                     OR (right_connector = ?1 AND right_scope = ?2 AND right_id = ?3)
                  ORDER BY id
                  LIMIT 1",
                params![side.connector.as_str(), scope_of(side), side.native_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found.flatten())
    }

    fn record_reference(&mut self, link: &ReferenceLink) -> Result<()> {
        // Written once and left alone: the row says which reference was carried out and where
        // it pointed, and neither changes afterwards.
        self.conn.execute(
            "INSERT INTO reference_links
                 (source_connector, source_scope, source_kind, source_id,
                  target_connector, target_scope, target_kind, target_id, url, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT (source_connector, source_scope, source_id,
                          target_connector, target_scope, target_id)
             DO NOTHING",
            params![
                link.source.connector.as_str(),
                scope_of(&link.source),
                link.source.kind.as_str(),
                link.source.native_id,
                link.target.connector.as_str(),
                scope_of(&link.target),
                link.target.kind.as_str(),
                link.target.native_id,
                link.url,
                now_millis(),
            ],
        )?;
        Ok(())
    }

    fn reference_exists(&mut self, source: &EntityRef, target: &EntityRef) -> Result<bool> {
        let found: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM reference_links
                 WHERE source_connector = ?1 AND source_scope = ?2 AND source_id = ?3
                   AND target_connector = ?4 AND target_scope = ?5 AND target_id = ?6",
                params![
                    source.connector.as_str(),
                    scope_of(source),
                    source.native_id,
                    target.connector.as_str(),
                    scope_of(target),
                    target.native_id,
                ],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found.is_some())
    }

    fn references_for(&mut self, target: &EntityRef) -> Result<Vec<ReferenceLink>> {
        let mut statement = self.conn.prepare(
            "SELECT source_connector, source_scope, source_kind, source_id,
                    target_connector, target_scope, target_kind, target_id, url
               FROM reference_links
              WHERE target_connector = ?1 AND target_scope = ?2 AND target_id = ?3
              ORDER BY id",
        )?;
        let rows = statement.query_map(
            params![
                target.connector.as_str(),
                scope_of(target),
                target.native_id
            ],
            |row| {
                Ok(ReferenceLink {
                    source: entity_from_row(row, 0)?,
                    target: entity_from_row(row, 4)?,
                    url: row.get(8)?,
                })
            },
        )?;
        let mut links = Vec::new();
        for row in rows {
            links.push(row?);
        }
        Ok(links)
    }
}

const LINK_COLUMNS: &str = "left_connector, left_scope, left_kind, left_id, \
                            right_connector, right_scope, right_kind, right_id, \
                            last_synced_hash, project";

fn link_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Link> {
    Ok(Link {
        left: entity_from_row(row, 0)?,
        right: entity_from_row(row, 4)?,
        last_synced_hash: row.get(8)?,
        project: row.get(9)?,
    })
}

/// Read one entity from four consecutive columns (connector, scope, kind, id).
fn entity_from_row(row: &rusqlite::Row<'_>, offset: usize) -> rusqlite::Result<EntityRef> {
    let connector = ConnectorId::new(row.get::<_, String>(offset)?);
    let scope: String = row.get(offset + 1)?;
    let kind = kind_from_name(&row.get::<_, String>(offset + 2)?);
    let id: String = row.get(offset + 3)?;
    let mut reference = EntityRef::new(connector, kind, id);
    if !scope.is_empty() {
        reference = reference.with_scope(scope);
    }
    Ok(reference)
}

/// An absent scope is stored as `''` so it can take part in a UNIQUE constraint.
fn scope_of(reference: &EntityRef) -> &str {
    reference.scope.as_deref().unwrap_or("")
}

/// Unknown kinds and actions come from a newer provider; they are stored and
/// reported rather than dropped, and they never abort a parse.
fn kind_from_name(name: &str) -> EntityKind {
    match name {
        "issue" => EntityKind::Issue,
        "comment" => EntityKind::Comment,
        "reference" => EntityKind::Reference,
        "project" => EntityKind::Project,
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
mod tests;
