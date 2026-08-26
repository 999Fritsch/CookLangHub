-- The automation identity of this installation.
--
-- A Cookbook that follows a Recipe moves to each new Version of that Recipe,
-- and each move makes one Cookbook Version. Somebody has to be the author of
-- that Version. It must not be a person: nobody may have their name on a
-- change they did not make. A dedicated Forgejo account is the author
-- instead, and this row says which account and holds its credential.
--
-- One row, because one installation talks to one Forgejo. The credential is
-- encrypted with the installation key, the same way the OAuth client secret
-- and the webhook secret are.
--
-- This table holds no Recipe state and no Cookbook state. What a Cookbook
-- holds, and which Version of each Recipe, lives in Git. Losing this row
-- stops the automation and costs one command; it destroys nothing.
CREATE TABLE automation (
    id              INTEGER PRIMARY KEY CHECK (id = 1),
    -- The Forgejo account that the credential belongs to. Forgejo is asked
    -- who it is rather than told, so a wrong name cannot be recorded.
    login           TEXT    NOT NULL,
    -- The name that History shows for an automatic Version.
    name            TEXT    NOT NULL,
    -- The identifier Forgejo gave the account. A rename changes the login
    -- and this stays, and it is what a search for the Cookbooks the
    -- automation may write to asks with.
    forgejo_user_id INTEGER NOT NULL,
    -- Encrypted with the installation key.
    token           BLOB    NOT NULL,
    updated_at      INTEGER NOT NULL
);
