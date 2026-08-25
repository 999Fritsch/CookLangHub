-- Operational state only. This database is rebuildable and never holds
-- authoritative Recipe or Cookbook state.
CREATE TABLE installation (
    id              INTEGER PRIMARY KEY CHECK (id = 1),
    installation_id TEXT    NOT NULL,
    created_at      TEXT    NOT NULL DEFAULT (datetime('now'))
);
