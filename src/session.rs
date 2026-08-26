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
             access_token, refresh_token, created_at, expires_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(digest(&token))
    .bind(user.id)
    .bind(&user.login)
    .bind(user.display_name())
    .bind(&user.avatar_url)
    .bind(encrypted_access)
    .bind(encrypted_refresh)
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
        let token = create(&pool, &cipher, &user, &Secret::new("gto_x".into()), None)
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
        let token = create(&pool, &cipher, &user, &Secret::new("gto_x".into()), None)
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
        let token = create(&pool, &cipher, &user, &Secret::new("gto_x".into()), None)
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
        let token = create(&pool, &cipher, &user, &Secret::new("gto_x".into()), None)
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
}
