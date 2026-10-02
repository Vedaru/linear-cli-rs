-- Bridge schema, revision 2: the project an issue was placed on.
--
-- A forge reports an issue's project nowhere in its issue API, so the mirror could
-- neither stop re-placing an issue on every edit nor notice a project cleared: the
-- one place that knows is the pairing, and this records it there, beside the
-- content hash that already says what was last written across the link.
--
-- Nullable on purpose: every pairing written before this revision (and every issue
-- pairing that was never placed on a project) has none, which means "not on a
-- project" and is exactly what the reconciler compares against.

ALTER TABLE entity_links ADD COLUMN project TEXT;
