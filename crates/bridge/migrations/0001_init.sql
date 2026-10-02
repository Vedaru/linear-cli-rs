-- Bridge schema, revision 1.
--
-- Every table is keyed by (connector, ...) rather than by platform-name columns:
-- a third platform must not require a migration of shape, and an operator may
-- run two connectors of the same kind (two forges) without their rows colliding.
--
-- `deliveries` is the durable queue *and* the replay log: the raw body is kept
-- so a handler can be re-run against exactly what the provider sent, which is
-- the only way to diagnose a sync bug after deploying a fix.

CREATE TABLE IF NOT EXISTS deliveries (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    connector   TEXT    NOT NULL,
    delivery_id TEXT    NOT NULL,
    event       TEXT    NOT NULL,
    kind        TEXT    NOT NULL,
    action      TEXT    NOT NULL,
    scope       TEXT,
    native_id   TEXT    NOT NULL,
    body        TEXT    NOT NULL,
    status      TEXT    NOT NULL DEFAULT 'pending',
    attempts    INTEGER NOT NULL DEFAULT 0,
    available_at INTEGER NOT NULL,
    last_error  TEXT,
    created_at  INTEGER NOT NULL,
    -- Idempotent intake: a provider retry with the same delivery id is a no-op
    -- insert, so the endpoint can answer 202 without duplicating work.
    UNIQUE (connector, delivery_id)
);

-- The claim query filters on (status, available_at); without this index it is a
-- full scan of the delivery log on every queue tick.
CREATE INDEX IF NOT EXISTS deliveries_due ON deliveries (status, available_at);

-- Links between the same entity on two platforms, plus the hash of the content
-- last written. The hash is what turns "the provider said it changed" into "and
-- it actually differs from what we wrote", which is how echoes of our own writes
-- are dropped. Consumed by the reconciler from M3 onwards.
CREATE TABLE IF NOT EXISTS entity_links (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    -- Left/right are symmetric: a mapping may mirror either direction, and the
    -- reconciler looks a link up from whichever side produced the event.
    left_connector  TEXT    NOT NULL,
    left_scope      TEXT,
    left_id         TEXT    NOT NULL,
    right_connector TEXT    NOT NULL,
    right_scope     TEXT,
    right_id        TEXT    NOT NULL,
    last_synced_hash TEXT,
    updated_at      INTEGER NOT NULL,
    UNIQUE (left_connector, left_id, right_connector, right_id)
);

CREATE INDEX IF NOT EXISTS entity_links_left ON entity_links (left_connector, left_id);
CREATE INDEX IF NOT EXISTS entity_links_right ON entity_links (right_connector, right_id);

-- Which entities a pull request or commit touched, so a merged PR can transition
-- every issue it referenced.
CREATE TABLE IF NOT EXISTS reference_links (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    connector   TEXT    NOT NULL,
    scope       TEXT,
    reference_id TEXT   NOT NULL,
    target_connector TEXT NOT NULL,
    target_id   TEXT    NOT NULL,
    url         TEXT,
    created_at  INTEGER NOT NULL,
    UNIQUE (connector, reference_id, target_connector, target_id)
);
