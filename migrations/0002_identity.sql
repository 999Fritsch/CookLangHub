-- Operational state for sign-in. None of this is authoritative: Forgejo owns
-- the identity, and every row here can be rebuilt by signing in again.

-- The OAuth client that the bootstrap command registers in Forgejo.
-- One row, because one installation talks to one Forgejo.
CREATE TABLE oauth_client (
    id             INTEGER PRIMARY KEY CHECK (id = 1),
    forgejo_app_id INTEGER NOT NULL,
    client_id      TEXT    NOT NULL,
    -- Encrypted. The key comes from the session secret.
    client_secret  BLOB    NOT NULL,
    redirect_uri   TEXT    NOT NULL,
    updated_at     INTEGER NOT NULL
);

-- Browser sessions. The id is the SHA-256 of the cookie value, so a person
-- who reads this table cannot build a working cookie from it.
CREATE TABLE session (
    id              TEXT    PRIMARY KEY,
    forgejo_user_id INTEGER NOT NULL,
    login           TEXT    NOT NULL,
    display_name    TEXT    NOT NULL,
    avatar_url      TEXT    NOT NULL,
    -- Both encrypted. They never reach the browser.
    access_token    BLOB    NOT NULL,
    refresh_token   BLOB,
    created_at      INTEGER NOT NULL,
    expires_at      INTEGER NOT NULL
);

CREATE INDEX session_expires_at ON session (expires_at);

-- One row per sign-in that started but did not finish. It carries the CSRF
-- state and the PKCE verifier. The row is used once and then removed.
CREATE TABLE login_attempt (
    state         TEXT    PRIMARY KEY,
    pkce_verifier TEXT    NOT NULL,
    created_at    INTEGER NOT NULL,
    expires_at    INTEGER NOT NULL
);
