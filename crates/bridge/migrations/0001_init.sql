-- Bridge schema, revision 1.
--
-- NOT YET DEPLOYED ANYWHERE: while that is true, this revision is amended in
-- place rather than superseded. The moment a real database exists outside a test
-- the rule changes and a revision is added instead - `migrate` records applied
-- revisions and never re-runs one.
--
-- Every table is keyed by (connector, scope, id) rather than by platform-name
-- columns: a third platform must not require a migration of shape, an operator
-- may run two connectors of the same kind (two forges) without their rows
-- colliding, and an id is only unique *within* its scope (a forge issue number is
-- per repository).
--
-- `deliveries` is the durable queue *and* the replay log: the raw body is kept so
-- a handler can be re-run against exactly what the provider sent, which is the
-- only way to diagnose a sync bug after deploying a fix.

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

-- Links between the same entity on two connectors, plus the hash of the content
-- last written across the link. The hash is what turns "the provider said it
-- changed" into "and it actually differs from what we wrote", which is how echoes
-- of our own writes are dropped.
--
-- Left/right are symmetric: a mapping may mirror either direction, and the
-- reconciler looks a link up from whichever side produced the event. `kind` is
-- recorded so a row is self-describing (`sync status` can print it) and so a link
-- can be resumed without re-deriving what it points at.
CREATE TABLE IF NOT EXISTS entity_links (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    left_connector   TEXT    NOT NULL,
    -- '' rather than NULL: an absent scope must still take part in the
    -- uniqueness rule, and SQLite treats NULLs in a UNIQUE constraint as distinct.
    left_scope       TEXT    NOT NULL DEFAULT '',
    left_kind        TEXT    NOT NULL,
    left_id          TEXT    NOT NULL,
    right_connector  TEXT    NOT NULL,
    right_scope      TEXT    NOT NULL DEFAULT '',
    right_kind       TEXT    NOT NULL,
    right_id         TEXT    NOT NULL,
    last_synced_hash TEXT,
    updated_at       INTEGER NOT NULL,
    UNIQUE (left_connector, left_scope, left_id, right_connector, right_scope, right_id)
);

CREATE INDEX IF NOT EXISTS entity_links_left ON entity_links (left_connector, left_scope, left_id);
CREATE INDEX IF NOT EXISTS entity_links_right ON entity_links (right_connector, right_scope, right_id);

-- Which entities a pull request or commit touched, so a merged PR can transition
-- every issue it referenced - and so the same reference is not attached twice.
CREATE TABLE IF NOT EXISTS reference_links (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    source_connector TEXT    NOT NULL,
    source_scope     TEXT    NOT NULL DEFAULT '',
    source_kind      TEXT    NOT NULL,
    source_id        TEXT    NOT NULL,
    target_connector TEXT    NOT NULL,
    target_scope     TEXT    NOT NULL DEFAULT '',
    target_kind      TEXT    NOT NULL,
    target_id        TEXT    NOT NULL,
    url              TEXT,
    created_at       INTEGER NOT NULL,
    UNIQUE (source_connector, source_scope, source_id, target_connector, target_scope, target_id)
);

CREATE INDEX IF NOT EXISTS reference_links_target
    ON reference_links (target_connector, target_scope, target_id);
