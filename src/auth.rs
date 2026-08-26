//! Sign-in and sign-out through Forgejo.
//!
//! Forgejo is the identity provider. The browser never receives a Forgejo
//! credential: it gets an application session cookie, and the access token
//! stays on the server in encrypted form.
//!
//! The flow is OAuth2 authorization code with PKCE. Two values protect it:
//!   - `state` binds the answer to the sign-in that this application began,
//!     which stops a forged callback.
//!   - The PKCE verifier proves that the process that redeems the code is
//!     the one that asked for it.
//!
//! Both live in `login_attempt` for a few minutes and are used once.

use std::sync::Arc;

use axum::Router;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum_extra::extract::CookieJar;
use axum_extra::extract::cookie::{Cookie, SameSite};
use serde::Deserialize;
use sqlx::sqlite::SqlitePool;

use crate::crypto::{Cipher, digest, pkce_challenge, random_token};
use crate::secret::Secret;
use crate::session::{self, COOKIE_NAME, SESSION_LIFETIME_SECONDS};
use crate::web::AppState;

/// How long a started sign-in stays valid.
const ATTEMPT_LIFETIME_SECONDS: i64 = 60 * 10;

/// Entropy of the CSRF state and the PKCE verifier, in bytes.
const STATE_BYTES: usize = 32;
const VERIFIER_BYTES: usize = 64;

/// The name that the bootstrap command gives the OAuth client in Forgejo.
pub const OAUTH_APPLICATION_NAME: &str = "CookLangHub";

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/auth/sign-in", get(sign_in))
        .route("/auth/callback", get(callback))
        // Sign-out changes state, so it must not be reachable by a link that
        // somebody else can make a browser follow.
        .route("/auth/sign-out", post(sign_out))
}

/// The OAuth client that the bootstrap command registered.
#[derive(Debug, Clone)]
pub struct OAuthClient {
    pub client_id: String,
    pub client_secret: Secret<String>,
    pub redirect_uri: String,
}

/// Read the registered OAuth client from the operational database.
pub async fn load_client(
    pool: &SqlitePool,
    cipher: &Cipher,
) -> Result<Option<OAuthClient>, AuthError> {
    let row: Option<(String, Vec<u8>, String)> = sqlx::query_as(
        "SELECT client_id, client_secret, redirect_uri FROM oauth_client WHERE id = 1",
    )
    .fetch_optional(pool)
    .await?;

    match row {
        Some((client_id, encrypted, redirect_uri)) => Ok(Some(OAuthClient {
            client_id,
            client_secret: Secret::new(cipher.decrypt(&encrypted)?),
            redirect_uri,
        })),
        None => Ok(None),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("the administrator did not run the bootstrap command yet")]
    NotBootstrapped,
    #[error("this sign-in is not one that the application started")]
    UnknownState,
    #[error("Forgejo refused the sign-in: {0}")]
    Provider(String),
    #[error(transparent)]
    Store(#[from] sqlx::Error),
    #[error(transparent)]
    Crypto(#[from] crate::crypto::CryptoError),
    #[error(transparent)]
    Session(#[from] crate::session::SessionError),
    #[error(transparent)]
    Forgejo(#[from] crate::forgejo::ForgejoError),
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let status = match self {
            AuthError::NotBootstrapped => StatusCode::SERVICE_UNAVAILABLE,
            AuthError::UnknownState | AuthError::Provider(_) => StatusCode::BAD_REQUEST,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };

        // The message is written for a person and carries no credential.
        tracing::warn!(error = %self, "sign-in did not finish");
        (status, self.to_string()).into_response()
    }
}

/// Begin a sign-in: record the attempt, then send the browser to Forgejo.
async fn sign_in(State(state): State<Arc<AppState>>) -> Result<Response, AuthError> {
    let client = load_client(&state.pool, &state.cipher)
        .await?
        .ok_or(AuthError::NotBootstrapped)?;

    let csrf_state = random_token(STATE_BYTES);
    let verifier = random_token(VERIFIER_BYTES);
    let created_at = session::now();

    sqlx::query(
        "INSERT INTO login_attempt (state, pkce_verifier, created_at, expires_at)
         VALUES (?, ?, ?, ?)",
    )
    .bind(digest(&csrf_state))
    .bind(&verifier)
    .bind(created_at)
    .bind(created_at + ATTEMPT_LIFETIME_SECONDS)
    .execute(&state.pool)
    .await?;

    let url = state.forgejo.authorize_url(
        &client.client_id,
        &client.redirect_uri,
        &csrf_state,
        &pkce_challenge(&verifier),
    );

    Ok(Redirect::to(&url).into_response())
}

/// What Forgejo sends back to the redirect address.
#[derive(Debug, Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

/// Finish a sign-in: check the state, redeem the code, start the session.
async fn callback(
    State(app): State<Arc<AppState>>,
    jar: CookieJar,
    Query(query): Query<CallbackQuery>,
) -> Result<(CookieJar, Redirect), AuthError> {
    if let Some(error) = query.error {
        let detail = query.error_description.unwrap_or(error);
        return Err(AuthError::Provider(detail));
    }

    let code = query.code.ok_or(AuthError::UnknownState)?;
    let returned_state = query.state.ok_or(AuthError::UnknownState)?;

    // Take the attempt. DELETE ... RETURNING makes the read and the removal
    // one step, so a replayed callback cannot use the same state twice.
    let attempt: Option<(String,)> = sqlx::query_as(
        "DELETE FROM login_attempt WHERE state = ? AND expires_at > ? RETURNING pkce_verifier",
    )
    .bind(digest(&returned_state))
    .bind(session::now())
    .fetch_optional(&app.pool)
    .await?;

    let verifier = attempt.ok_or(AuthError::UnknownState)?.0;

    let client = load_client(&app.pool, &app.cipher)
        .await?
        .ok_or(AuthError::NotBootstrapped)?;

    let tokens = app
        .forgejo
        .exchange_code(
            &client.client_id,
            &client.client_secret,
            &code,
            &client.redirect_uri,
            &verifier,
        )
        .await?;

    let access_token = Secret::new(tokens.access_token.clone());
    let refresh_token = tokens
        .refresh_token
        .as_ref()
        .map(|v| Secret::new(v.clone()));
    // Forgejo says how long this token lives. Keeping the moment means the
    // next request can tell a live token from a spent one without asking.
    let access_token_expires_at = tokens.expires_at(session::now());

    let user = app.forgejo.current_user(&access_token).await?;

    let session_token = session::create(
        &app.pool,
        &app.cipher,
        &user,
        &access_token,
        refresh_token.as_ref(),
        access_token_expires_at,
    )
    .await?;

    tracing::info!(login = %user.login, "a user signed in");

    let jar = jar.add(session_cookie(
        session_token.expose().clone(),
        SESSION_LIFETIME_SECONDS,
        app.cookie_secure,
    ));

    Ok((jar, Redirect::to("/")))
}

/// End the session and clear the cookie.
async fn sign_out(
    State(app): State<Arc<AppState>>,
    jar: CookieJar,
) -> Result<(CookieJar, Redirect), AuthError> {
    if let Some(cookie) = jar.get(COOKIE_NAME) {
        session::destroy(&app.pool, cookie.value()).await?;
    }

    // Overwrite with an expired cookie so the browser drops it.
    let jar = jar.add(session_cookie(String::new(), 0, app.cookie_secure));

    Ok((jar, Redirect::to("/")))
}

/// Build the session cookie.
///
/// HttpOnly keeps the token away from page scripts. Secure keeps it off a
/// plain connection. SameSite=Lax lets the return from Forgejo carry it
/// while a cross-site form post cannot.
fn session_cookie(value: String, max_age_seconds: i64, secure: bool) -> Cookie<'static> {
    let mut cookie = Cookie::new(COOKIE_NAME, value);
    cookie.set_path("/");
    cookie.set_http_only(true);
    cookie.set_secure(secure);
    cookie.set_same_site(SameSite::Lax);
    cookie.set_max_age(time::Duration::seconds(max_age_seconds));
    cookie
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_session_cookie_carries_every_required_attribute() {
        let cookie = session_cookie("abc".to_string(), 60, true);

        assert_eq!(cookie.http_only(), Some(true));
        assert_eq!(cookie.secure(), Some(true));
        assert_eq!(cookie.same_site(), Some(SameSite::Lax));
        assert_eq!(cookie.path(), Some("/"));
    }

    #[test]
    fn clearing_the_cookie_uses_an_empty_value_that_expires_at_once() {
        let cookie = session_cookie(String::new(), 0, true);

        assert_eq!(cookie.value(), "");
        assert_eq!(cookie.max_age(), Some(time::Duration::seconds(0)));
    }

    #[test]
    fn a_plain_http_deployment_can_turn_the_secure_attribute_off() {
        let cookie = session_cookie("abc".to_string(), 60, false);

        assert_eq!(cookie.secure(), Some(false));
        // The other protections stay whatever the connection is.
        assert_eq!(cookie.http_only(), Some(true));
        assert_eq!(cookie.same_site(), Some(SameSite::Lax));
    }
}
