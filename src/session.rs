//! Browser sessions.
//!
//! A session lives in SQLite, so it survives a restart of the application.
//! The browser holds only a random token. The database holds the SHA-256 of
//! that token, so a person who reads the table cannot build a cookie from
//! it. The Forgejo access token sits beside the row in encrypted form and
//! never reaches the browser.

use sqlx::sqlite::SqlitePool;

use crate::crypto::{Cipher, digest, random_token};
use crate::forgejo::ForgejoUser;
use crate::secret::Secret;

/// Name of the session cookie.
pub const COOKIE_NAME: &str = "cooklanghub_session";

/// How long a session lasts without a new sign-in.
pub const SESSION_LIFETIME_SECONDS: i64 = 60 * 60 * 24 * 30;

/// Entropy of the session token, in bytes.
const TOKEN_BYTES: usize = 32;

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("cannot reach the session store: {0}")]
    Store(#[from] sqlx::Error),
    #[error("cannot protect the stored credential: {0}")]
    Crypto(#[from] crate::crypto::CryptoError),
}

/// The signed-in user, as a handler sees it.
#[derive(Debug, Clone)]
pub struct CurrentUser {
    pub forgejo_user_id: i64,
    pub login: String,
    pub display_name: String,
    pub avatar_url: String,
}

/// Seconds since the Unix epoch.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// Start a session and return the token that the browser must send back.
///
/// The caller puts the returned token in a cookie. The application never
/// stores it, only its digest.
pub async fn create(
    pool: &SqlitePool,
    cipher: &Cipher,
    user: &ForgejoUser,
    access_token: &Secret<String>,
    refresh_token: Option<&Secret<String>>,
    // When Forgejo says the access token stops working. Without it the
    // first request would renew a token that was just issued.
    access_token_expires_at: Option<i64>,
) -> Result<Secret<String>, SessionError> {
    let token = random_token(TOKEN_BYTES);
    let created_at = now();
    let expires_at = created_at + SESSION_LIFETIME_SECONDS;

    let encrypted_access = cipher.encrypt(access_token.expose())?;
    let encrypted_refresh = match refresh_token {
        Some(value) => Some(cipher.encrypt(value.expose())?),
        None => None,
    };

    sqlx::query(
        "INSERT INTO session (
             id, forgejo_user_id, login, display_name, avatar_url,
             access_token, refresh_token, access_token_expires_at,
             created_at, expires_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(digest(&token))
    .bind(user.id)
    .bind(&user.login)
    .bind(user.display_name())
    .bind(&user.avatar_url)
    .bind(encrypted_access)
    .bind(encrypted_refresh)
    .bind(access_token_expires_at)
    .bind(created_at)
    .bind(expires_at)
    .execute(pool)
    .await?;

    Ok(Secret::new(token))
}

/// Read the user for a session token, if the session exists and is current.
pub async fn lookup(pool: &SqlitePool, token: &str) -> Result<Option<CurrentUser>, SessionError> {
    let row: Option<(i64, String, String, String)> = sqlx::query_as(
        "SELECT forgejo_user_id, login, display_name, avatar_url
         FROM session WHERE id = ? AND expires_at > ?",
    )
    .bind(digest(token))
    .bind(now())
    .fetch_optional(pool)
    .await?;

    Ok(row.map(
        |(forgejo_user_id, login, display_name, avatar_url)| CurrentUser {
            forgejo_user_id,
            login,
            display_name,
            avatar_url,
        },
    ))
}

/// How long before the deadline a token counts as spent.
///
/// A token that dies while a request is already in flight is a refusal that
/// the person sees, so renewal happens a little early.
const RENEW_MARGIN_SECONDS: i64 = 120;

/// How long one renewal may hold its claim before another request takes it.
///
/// A request that stops part way through renewal must not lock a person out
/// of their own session, so a claim this old counts as abandoned.
const CLAIM_SECONDS: i64 = 30;

/// How long to wait for the request that is already renewing.
const WAIT_MILLISECONDS: u64 = 100;
const WAIT_TRIES: u32 = 30;

/// The Forgejo access token of a session, renewed first if it is spent.
///
/// This is the credential seam for anything a person does. It renews before
/// the token is used rather than after Forgejo refuses it, because the same
/// token is also the password that Git uses to push, and a push that fails
/// part way through is worse than one that never started.
///
/// `Ok(None)` means there is no usable credential any more: Forgejo refused
/// the grant, so the session is removed and the person is signed out. This
/// application ends a sign-in only when Forgejo itself refuses to renew it,
/// never because one read answered 401.
///
/// Only one request renews a session at a time. Forgejo gives a new refresh
/// token each time and refuses the old one, so two renewals at once would
/// spend the same one-use token twice and the loser would be told, wrongly,
/// that the sign-in had ended.
pub async fn live_token(
    pool: &SqlitePool,
    cipher: &Cipher,
    forgejo: &crate::forgejo::ForgejoClient,
    client: &crate::auth::OAuthClient,
    token: &str,
) -> Result<Option<Secret<String>>, SessionError> {
    let id = digest(token);

    let Some(state) = read_credential(pool, cipher, &id).await? else {
        return Ok(None);
    };

    if !state.is_spent(now()) {
        return Ok(Some(state.access_token));
    }

    let Some(refresh_token) = state.refresh_token else {
        // Nothing to renew with. Forgejo does give one at sign-in, so this
        // is a session from before renewal existed. Ask for a sign-in
        // rather than let every page fail quietly.
        tracing::info!("a sign-in has no way to renew itself and ended");
        destroy_by_id(pool, &id).await?;
        return Ok(None);
    };

    if !claim(pool, &id).await? {
        // Another request is renewing this session. Wait for it rather than
        // spend the same one-use refresh token a second time.
        return wait_for_renewal(pool, cipher, &id).await;
    }

    match forgejo
        .refresh_access_token(&client.client_id, &client.client_secret, &refresh_token)
        .await
    {
        Ok(fresh) => {
            store_renewed(pool, cipher, &id, &fresh, now()).await?;
            tracing::info!("renewed a sign-in with Forgejo");
            Ok(Some(Secret::new(fresh.access_token)))
        }
        // Forgejo is away. It refused nothing, because it answered nothing,
        // so the sign-in stays. A short outage must not sign everybody out,
        // and the renewal happens on the next request that finds Forgejo
        // again. The page has no credential now and says so.
        Err(error) if crate::outage::is_outage(&error) => {
            tracing::warn!(%error, "cannot renew a sign-in while Forgejo is away");
            release(pool, &id).await?;
            Ok(None)
        }
        Err(error) => {
            // Forgejo refused: the person withdrew the permission, an
            // administrator closed the account, or the refresh token is
            // spent. The sign-in is over, so end it instead of hiding it.
            tracing::info!(%error, "Forgejo refused to renew a sign-in");
            destroy_by_id(pool, &id).await?;
            Ok(None)
        }
    }
}

/// The stored shape of a credential: access token, refresh token, deadline.
type CredentialRow = (Vec<u8>, Option<Vec<u8>>, Option<i64>);

/// What one session holds for talking to Forgejo.
struct Credential {
    access_token: Secret<String>,
    refresh_token: Option<Secret<String>>,
    /// When the access token stops working, when this is known.
    expires_at: Option<i64>,
}

impl Credential {
    /// Whether the token needs renewing before it is used.
    ///
    /// A session whose deadline is unknown counts as spent, so a row from
    /// before renewal existed is renewed once and then knows its deadline.
    fn is_spent(&self, moment: i64) -> bool {
        match self.expires_at {
            Some(deadline) => deadline - RENEW_MARGIN_SECONDS <= moment,
            None => true,
        }
    }
}

async fn read_credential(
    pool: &SqlitePool,
    cipher: &Cipher,
    id: &str,
) -> Result<Option<Credential>, SessionError> {
    let row: Option<CredentialRow> = sqlx::query_as(
        "SELECT access_token, refresh_token, access_token_expires_at
         FROM session WHERE id = ? AND expires_at > ?",
    )
    .bind(id)
    .bind(now())
    .fetch_optional(pool)
    .await?;

    let Some((access, refresh, expires_at)) = row else {
        return Ok(None);
    };

    Ok(Some(Credential {
        access_token: Secret::new(cipher.decrypt(&access)?),
        refresh_token: match refresh {
            Some(value) => Some(Secret::new(cipher.decrypt(&value)?)),
            None => None,
        },
        expires_at,
    }))
}

/// Take the right to renew this session, if nobody else holds it.
///
/// The claim and its test are one statement, so two requests cannot both
/// believe that they took it.
async fn claim(pool: &SqlitePool, id: &str) -> Result<bool, SessionError> {
    let moment = now();
    let result = sqlx::query(
        "UPDATE session SET renewing_at = ?
         WHERE id = ? AND (renewing_at IS NULL OR renewing_at < ?)",
    )
    .bind(moment)
    .bind(id)
    .bind(moment - CLAIM_SECONDS)
    .execute(pool)
    .await?;

    Ok(result.rows_affected() == 1)
}

/// Let go of the claim without a renewal.
///
/// A renewal that Forgejo never answered has to be tried again as soon as
/// Forgejo is back, and a claim that is still held would make the next
/// request wait for a renewal that is not happening.
async fn release(pool: &SqlitePool, id: &str) -> Result<(), SessionError> {
    sqlx::query("UPDATE session SET renewing_at = NULL WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Wait for the request that holds the claim, then read what it wrote.
///
/// Gives back the stored token when the wait runs out. That token may be
/// spent, and one refused read is a better answer than spending a one-use
/// refresh token twice and ending a sign-in that was working.
async fn wait_for_renewal(
    pool: &SqlitePool,
    cipher: &Cipher,
    id: &str,
) -> Result<Option<Secret<String>>, SessionError> {
    for _ in 0..WAIT_TRIES {
        tokio::time::sleep(std::time::Duration::from_millis(WAIT_MILLISECONDS)).await;

        let Some(state) = read_credential(pool, cipher, id).await? else {
            // The other request ended the session while this one waited.
            return Ok(None);
        };

        if !state.is_spent(now()) {
            return Ok(Some(state.access_token));
        }
    }

    tracing::warn!("waited for another request to renew a sign-in, and it did not");
    Ok(read_credential(pool, cipher, id)
        .await?
        .map(|state| state.access_token))
}

/// Write what Forgejo gave back, and let go of the claim.
async fn store_renewed(
    pool: &SqlitePool,
    cipher: &Cipher,
    id: &str,
    fresh: &crate::forgejo::TokenResponse,
    moment: i64,
) -> Result<(), SessionError> {
    let access = cipher.encrypt(&fresh.access_token)?;
    // Forgejo refuses the old refresh token from now on, so the new one has
    // to replace it. An answer without one leaves the stored one in place
    // rather than emptying the column.
    let refresh = match &fresh.refresh_token {
        Some(value) => Some(cipher.encrypt(value)?),
        None => None,
    };

    sqlx::query(
        "UPDATE session
         SET access_token = ?,
             refresh_token = COALESCE(?, refresh_token),
             access_token_expires_at = ?,
             renewing_at = NULL
         WHERE id = ?",
    )
    .bind(access)
    .bind(refresh)
    .bind(fresh.expires_at(moment))
    .bind(id)
    .execute(pool)
    .await?;

    Ok(())
}

/// Remove a session by the stored form of its token.
async fn destroy_by_id(pool: &SqlitePool, id: &str) -> Result<(), SessionError> {
    sqlx::query("DELETE FROM session WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Read the Forgejo access token of a session.
///
/// Later tickets call Forgejo as the signed-in person and need this. It
/// stays out of every response body.
pub async fn access_token(
    pool: &SqlitePool,
    cipher: &Cipher,
    token: &str,
) -> Result<Option<Secret<String>>, SessionError> {
    let row: Option<(Vec<u8>,)> =
        sqlx::query_as("SELECT access_token FROM session WHERE id = ? AND expires_at > ?")
            .bind(digest(token))
            .bind(now())
            .fetch_optional(pool)
            .await?;

    match row {
        Some((encrypted,)) => Ok(Some(Secret::new(cipher.decrypt(&encrypted)?))),
        None => Ok(None),
    }
}

/// A person with a live session, and the credential to act as them.
#[derive(Debug, Clone)]
pub struct SignedInPerson {
    pub forgejo_user_id: i64,
    pub login: String,
    pub token: Secret<String>,
}

/// Every person who is signed in, once each.
///
/// The reconciliation asks Forgejo what each of these people may see. It
/// reads only, and it never reaches further than that person reaches for
/// themselves, because it carries their credential and no other.
pub async fn signed_in_people(
    pool: &SqlitePool,
    cipher: &Cipher,
) -> Result<Vec<SignedInPerson>, SessionError> {
    let rows: Vec<(i64, String, Vec<u8>)> = sqlx::query_as(
        "SELECT forgejo_user_id, login, access_token
         FROM session WHERE expires_at > ?
         ORDER BY created_at DESC",
    )
    .bind(now())
    .fetch_all(pool)
    .await?;

    let mut people: Vec<SignedInPerson> = Vec::new();
    for (forgejo_user_id, login, encrypted) in rows {
        // The newest session of a person comes first, so a later one adds
        // nothing.
        if people.iter().any(|p| p.forgejo_user_id == forgejo_user_id) {
            continue;
        }

        match cipher.decrypt(&encrypted) {
            Ok(token) => people.push(SignedInPerson {
                forgejo_user_id,
                login,
                token: Secret::new(token),
            }),
            Err(error) => tracing::warn!(%error, %login, "cannot read a stored credential"),
        }
    }

    Ok(people)
}

/// End one session. The token stops working at once.
pub async fn destroy(pool: &SqlitePool, token: &str) -> Result<(), SessionError> {
    sqlx::query("DELETE FROM session WHERE id = ?")
        .bind(digest(token))
        .execute(pool)
        .await?;
    Ok(())
}

/// Remove sessions and sign-in attempts that expired.
pub async fn prune(pool: &SqlitePool) -> Result<u64, SessionError> {
    let moment = now();

    let sessions = sqlx::query("DELETE FROM session WHERE expires_at <= ?")
        .bind(moment)
        .execute(pool)
        .await?
        .rows_affected();

    let attempts = sqlx::query("DELETE FROM login_attempt WHERE expires_at <= ?")
        .bind(moment)
        .execute(pool)
        .await?
        .rows_affected();

    Ok(sessions + attempts)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn fixture() -> (SqlitePool, Cipher, ForgejoUser) {
        let pool = crate::db::connect("sqlite://:memory:").await.unwrap();
        let cipher = Cipher::from_session_secret("test-secret").unwrap();
        let user = ForgejoUser {
            id: 7,
            login: "sam".to_string(),
            full_name: "Sam Cook".to_string(),
            avatar_url: "http://forgejo:3000/avatars/7".to_string(),
            email: "sam@example.test".to_string(),
        };
        (pool, cipher, user)
    }

    #[tokio::test]
    async fn a_new_session_can_be_looked_up() {
        let (pool, cipher, user) = fixture().await;
        let token = create(
            &pool,
            &cipher,
            &user,
            &Secret::new("gto_x".into()),
            None,
            None,
        )
        .await
        .unwrap();

        let found = lookup(&pool, token.expose()).await.unwrap().unwrap();
        assert_eq!(found.login, "sam");
        assert_eq!(found.display_name, "Sam Cook");
        assert_eq!(found.forgejo_user_id, 7);
    }

    #[tokio::test]
    async fn the_database_never_holds_the_session_token() {
        let (pool, cipher, user) = fixture().await;
        let token = create(
            &pool,
            &cipher,
            &user,
            &Secret::new("gto_x".into()),
            None,
            None,
        )
        .await
        .unwrap();

        let stored: (String,) = sqlx::query_as("SELECT id FROM session")
            .fetch_one(&pool)
            .await
            .unwrap();

        assert_ne!(stored.0, token.expose().as_str());
        assert_eq!(stored.0, digest(token.expose()));
    }

    #[tokio::test]
    async fn the_database_never_holds_the_access_token_in_the_clear() {
        let (pool, cipher, user) = fixture().await;
        create(
            &pool,
            &cipher,
            &user,
            &Secret::new("gto_supersecret".into()),
            None,
            None,
        )
        .await
        .unwrap();

        let stored: (Vec<u8>,) = sqlx::query_as("SELECT access_token FROM session")
            .fetch_one(&pool)
            .await
            .unwrap();

        let as_text = String::from_utf8_lossy(&stored.0);
        assert!(!as_text.contains("gto_supersecret"));
    }

    #[tokio::test]
    async fn the_access_token_comes_back_for_the_session_holder() {
        let (pool, cipher, user) = fixture().await;
        let token = create(
            &pool,
            &cipher,
            &user,
            &Secret::new("gto_supersecret".into()),
            None,
            None,
        )
        .await
        .unwrap();

        let found = access_token(&pool, &cipher, token.expose())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.expose(), "gto_supersecret");
    }

    #[tokio::test]
    async fn a_destroyed_session_stops_working() {
        let (pool, cipher, user) = fixture().await;
        let token = create(
            &pool,
            &cipher,
            &user,
            &Secret::new("gto_x".into()),
            None,
            None,
        )
        .await
        .unwrap();

        destroy(&pool, token.expose()).await.unwrap();

        assert!(lookup(&pool, token.expose()).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn an_unknown_token_matches_no_session() {
        let (pool, _, _) = fixture().await;
        assert!(lookup(&pool, "not-a-real-token").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn an_expired_session_matches_nothing_and_is_pruned() {
        let (pool, cipher, user) = fixture().await;
        let token = create(
            &pool,
            &cipher,
            &user,
            &Secret::new("gto_x".into()),
            None,
            None,
        )
        .await
        .unwrap();

        sqlx::query("UPDATE session SET expires_at = ?")
            .bind(now() - 1)
            .execute(&pool)
            .await
            .unwrap();

        assert!(lookup(&pool, token.expose()).await.unwrap().is_none());
        assert_eq!(prune(&pool).await.unwrap(), 1);
    }

    fn credential(expires_at: Option<i64>) -> Credential {
        Credential {
            access_token: Secret::new("token".to_string()),
            refresh_token: Some(Secret::new("refresh".to_string())),
            expires_at,
        }
    }

    #[test]
    fn a_token_is_spent_before_its_deadline_not_after() {
        let now = 1_000_000;
        // Still good, with room to spare.
        assert!(!credential(Some(now + RENEW_MARGIN_SECONDS + 60)).is_spent(now));
        // Inside the margin: renew now rather than fail during a request.
        assert!(credential(Some(now + RENEW_MARGIN_SECONDS - 1)).is_spent(now));
        assert!(credential(Some(now)).is_spent(now));
        assert!(credential(Some(now - 1)).is_spent(now));
    }

    #[test]
    fn a_session_that_never_recorded_a_deadline_is_renewed_once() {
        // Every row written before renewal existed has no deadline. It must
        // renew on first use rather than be trusted forever.
        assert!(credential(None).is_spent(1_000_000));
    }

    #[test]
    fn a_strange_lifetime_from_forgejo_does_not_become_a_deadline() {
        use crate::forgejo::TokenResponse;
        let with = |expires_in| TokenResponse {
            access_token: "a".to_string(),
            refresh_token: None,
            expires_in,
        };

        assert_eq!(with(Some(3600)).expires_at(1_000), Some(4_600));
        // Zero or negative is not a lifetime. Treating it as one would put
        // the session into a renewal on every single request.
        assert_eq!(with(Some(0)).expires_at(1_000), None);
        assert_eq!(with(Some(-5)).expires_at(1_000), None);
        assert_eq!(with(None).expires_at(1_000), None);
    }

    #[test]
    fn a_credential_never_prints_itself() {
        use crate::forgejo::TokenResponse;
        let printed = format!(
            "{:?}",
            TokenResponse {
                access_token: "eyJhbGciOiJ.access.secret".to_string(),
                refresh_token: Some("eyJhbGciOiJ.refresh.secret".to_string()),
                expires_in: Some(3600),
            }
        );

        assert!(!printed.contains("secret"), "got `{printed}`");
        assert!(printed.contains(crate::secret::REDACTED));
        // The lifetime is not a secret and is worth having in a log.
        assert!(printed.contains("3600"));
    }

    #[tokio::test]
    async fn only_one_request_may_renew_a_session_at_a_time() {
        let (pool, cipher, user) = fixture().await;
        let token = create(
            &pool,
            &cipher,
            &user,
            &Secret::new("gto_supersecret".into()),
            Some(&Secret::new("gto_refresh".into())),
            None,
        )
        .await
        .unwrap();
        let id = digest(token.expose());

        // Forgejo refuses a refresh token that was already spent, so a
        // second request must not be allowed to spend the same one.
        assert!(claim(&pool, &id).await.unwrap(), "the first request claims");
        assert!(
            !claim(&pool, &id).await.unwrap(),
            "the second request must wait instead of renewing as well"
        );

        // A claim that was never let go must not lock the person out.
        sqlx::query("UPDATE session SET renewing_at = ? WHERE id = ?")
            .bind(now() - CLAIM_SECONDS - 1)
            .bind(&id)
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            claim(&pool, &id).await.unwrap(),
            "an abandoned claim must be takeable"
        );
    }
}
