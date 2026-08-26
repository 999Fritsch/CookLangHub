-- Keep a sign-in alive without asking the person to sign in again.
--
-- Forgejo gives an access token that lives one hour, and a refresh token to
-- get another one. The refresh token was already stored here from the first
-- day; nothing ever read it back, so every sign-in died after an hour while
-- the browser still showed the person as signed in.
--
-- Neither column is authoritative. Forgejo owns the grant and can refuse it
-- at any moment, and losing this table costs one sign-in.

-- When the stored access token stops working, as Forgejo reported it.
-- NULL means the application does not know, which is true for every row
-- written before this migration, so those are renewed the first time they
-- are used.
ALTER TABLE session ADD COLUMN access_token_expires_at INTEGER;

-- Set while one request is renewing this session.
--
-- Forgejo gives a new refresh token every time and refuses the old one, so
-- two requests that renew the same session at the same moment would spend
-- the same one-use token twice and the second would be told the sign-in had
-- ended. This column is the claim that stops that. A claim older than the
-- renewal itself can take is treated as abandoned, so a request that dies
-- part way through cannot lock a person out.
ALTER TABLE session ADD COLUMN renewing_at INTEGER;
