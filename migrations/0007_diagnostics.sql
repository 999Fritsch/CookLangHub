-- What the diagnostics page needs to name a fault.
--
-- An administrator has to find the cause of a fault without a read of the
-- source code. Two facts make that possible and neither of them can be
-- worked out from Forgejo or from Git: when a sweep last ran and what it
-- found, and when a webhook message last arrived.
--
-- This is operational state and nothing else. Every row is rebuildable: the
-- next sweep writes its own row, and the next message writes the moment
-- again. Deleting these values costs an administrator the history of the
-- last run and destroys no Recipe, no Cookbook, and no Version.
CREATE TABLE sweep (
    -- Which sweep this row describes. One row for each.
    name     TEXT    PRIMARY KEY,
    -- When the run finished, in seconds since the epoch.
    ran_at   INTEGER NOT NULL,
    -- How many things the run looked at.
    scanned  INTEGER NOT NULL,
    -- How many the run wrote or moved.
    changed  INTEGER NOT NULL,
    -- How many the run took out of an index.
    removed  INTEGER NOT NULL,
    -- How many questions the run could not answer. A number above zero is
    -- what tells an administrator that the run was not complete.
    failures INTEGER NOT NULL
);

-- When Forgejo last reported a change to this application.
--
-- A webhook that Forgejo holds but never posts to is the common fault of a
-- new installation: Forgejo cannot reach the address that the bootstrap
-- gave it. Nothing else in the system shows that state, because the indexes
-- stay correct through the reconciliation and look healthy.
--
-- Empty until the first message arrives.
ALTER TABLE webhook ADD COLUMN last_message_at INTEGER;
