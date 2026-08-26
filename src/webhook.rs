//! The Forgejo system webhook.
//!
//! Forgejo tells the application when a repository changes, so that the
//! Recipe index is current within a moment instead of within a restart. One
//! system webhook covers the whole instance, which is what keeps the
//! integration to the supported Forgejo interfaces and out of its database.
//!
//! Every message is authenticated. Forgejo signs the body with HMAC-SHA256
//! and the shared secret, and this module compares the signature in constant
//! time. A body that does not match is refused and changes nothing, because
//! anybody can reach this address.
//!
//! The webhook is a speed improvement and never the only way the index stays
//! correct. A message that is lost, refused, or never sent is repaired by
//! the reconciliation in [`crate::index`].

use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::Sha256;
use sqlx::sqlite::SqlitePool;

use crate::crypto::Cipher;
use crate::forgejo::ForgejoClient;
use crate::secret::Secret;
use crate::session::now;
use crate::web::AppState;

/// Where Forgejo posts.
pub const PATH: &str = "/webhooks/forgejo";

/// What Forgejo must report.
///
/// `repository` covers a Recipe that appears or goes. `push` covers a
/// Version, which is where a changed title comes from.
pub const EVENTS: [&str; 2] = ["repository", "push"];

/// The headers that carry the signature. Forgejo sends both names, and
/// which one arrives depends on the release.
const SIGNATURE_HEADERS: [&str; 2] = ["x-forgejo-signature", "x-gitea-signature"];

/// The headers that name the event.
const EVENT_HEADERS: [&str; 2] = ["x-forgejo-event", "x-gitea-event"];

#[derive(Debug, thiserror::Error)]
pub enum WebhookError {
    #[error(transparent)]
    Forgejo(#[from] crate::forgejo::ForgejoError),
    #[error(transparent)]
    Store(#[from] sqlx::Error),
    #[error(transparent)]
    Crypto(#[from] crate::crypto::CryptoError),
}

/// What the registration did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Registration {
    /// No webhook pointed here, so the command made one.
    Created { hook_id: i64 },
    /// A webhook pointed here already, and the command refreshed it.
    Reused { hook_id: i64 },
}

impl Registration {
    pub fn hook_id(&self) -> i64 {
        match self {
            Registration::Created { hook_id } | Registration::Reused { hook_id } => *hook_id,
        }
    }
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route(PATH, post(receive))
}

/// Register one system webhook in Forgejo, and store its secret.
///
/// The command is repeatable. It finds the webhook that the last run made,
/// refreshes that one, and removes any other that points here as well, so
/// that a second run never leaves Forgejo with two.
///
/// Finding it again needs care, because Forgejo 15 has a defect here:
/// `GET /api/v1/admin/hooks` answers with an empty list even directly after
/// `POST /api/v1/admin/hooks` created a webhook, while
/// `GET /api/v1/admin/hooks/{id}` answers correctly. The identifier that the
/// last run recorded is therefore what finds it, and the list is still read
/// in case a later Forgejo release starts reporting these webhooks.
pub async fn register(
    pool: &SqlitePool,
    cipher: &Cipher,
    forgejo: &ForgejoClient,
    admin_token: &Secret<String>,
    target_url: &str,
    secret: &Secret<String>,
) -> Result<Registration, WebhookError> {
    let recorded: Option<(i64,)> =
        sqlx::query_as("SELECT forgejo_hook_id FROM webhook WHERE id = 1")
            .fetch_optional(pool)
            .await?;

    let mut first = None;
    if let Some((hook_id,)) = recorded
        && forgejo.system_hook(admin_token, hook_id).await?.is_some()
    {
        first = Some(hook_id);
    }

    let mut duplicates: Vec<i64> = Vec::new();
    for hook in forgejo
        .list_system_hooks(admin_token)
        .await?
        .iter()
        .filter(|hook| hook.target_url() == target_url)
    {
        match first {
            None => first = Some(hook.id),
            Some(id) if id != hook.id => duplicates.push(hook.id),
            Some(_) => {}
        }
    }

    let registration = match first {
        Some(hook_id) => {
            forgejo
                .update_system_hook(admin_token, hook_id, target_url, secret, &EVENTS)
                .await?;
            Registration::Reused { hook_id }
        }
        None => {
            let created = forgejo
                .create_system_hook(admin_token, target_url, secret, &EVENTS)
                .await?;
            Registration::Created {
                hook_id: created.id,
            }
        }
    };

    for duplicate in duplicates {
        tracing::info!(
            hook_id = duplicate,
            "removing a second webhook to this address"
        );
        forgejo.delete_system_hook(admin_token, duplicate).await?;
    }

    sqlx::query(
        "INSERT INTO webhook (id, forgejo_hook_id, target_url, secret, updated_at)
         VALUES (1, ?, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET
             forgejo_hook_id = excluded.forgejo_hook_id,
             target_url      = excluded.target_url,
             secret          = excluded.secret,
             updated_at      = excluded.updated_at",
    )
    .bind(registration.hook_id())
    .bind(target_url)
    .bind(cipher.encrypt(secret.expose())?)
    .bind(now())
    .execute(pool)
    .await?;

    Ok(registration)
}

/// Read the secret that Forgejo signs with.
pub async fn secret(
    pool: &SqlitePool,
    cipher: &Cipher,
) -> Result<Option<Secret<String>>, WebhookError> {
    let row: Option<(Vec<u8>,)> = sqlx::query_as("SELECT secret FROM webhook WHERE id = 1")
        .fetch_optional(pool)
        .await?;

    match row {
        Some((encrypted,)) => Ok(Some(Secret::new(cipher.decrypt(&encrypted)?))),
        None => Ok(None),
    }
}

/// Whether the signature belongs to this body and this secret.
///
/// The comparison runs in constant time. A comparison that stops at the
/// first wrong byte tells an attacker how much of a guess was right, which
/// is enough to build the whole signature one byte at a time.
pub fn signature_matches(secret: &str, body: &[u8], signature: &str) -> bool {
    let Some(provided) = from_hex(signature) else {
        return false;
    };

    let mut mac = match Hmac::<Sha256>::new_from_slice(secret.as_bytes()) {
        Ok(mac) => mac,
        // HMAC takes a key of any length, so this cannot happen.
        Err(_) => return false,
    };
    mac.update(body);

    mac.verify_slice(&provided).is_ok()
}

/// The signature that Forgejo sends for this body and this secret.
pub fn sign(secret: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .expect("HMAC accepts a key of any length");
    mac.update(body);

    mac.finalize()
        .into_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn from_hex(value: &str) -> Option<Vec<u8>> {
    let value = value.trim();
    if value.is_empty() || !value.len().is_multiple_of(2) {
        return None;
    }

    (0..value.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&value[at..at + 2], 16).ok())
        .collect()
}

/// The part of a Forgejo message that this application reads.
///
/// Only the name of the repository matters. What it looks like now is a
/// question for Forgejo, because the message describes a moment that has
/// already passed.
#[derive(Debug, Deserialize)]
struct Message {
    #[serde(default)]
    action: String,
    #[serde(default)]
    repository: Option<MessageRepository>,
}

#[derive(Debug, Deserialize)]
struct MessageRepository {
    #[serde(default)]
    id: i64,
    name: String,
    owner: MessageOwner,
}

#[derive(Debug, Deserialize)]
struct MessageOwner {
    #[serde(default)]
    login: String,
}

async fn receive(State(state): State<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> Response {
    let secret = match secret(&state.pool, &state.cipher).await {
        Ok(Some(secret)) => secret,
        Ok(None) => {
            tracing::warn!(
                "a webhook message arrived before the administrator registered the webhook"
            );
            return StatusCode::UNAUTHORIZED.into_response();
        }
        Err(error) => {
            tracing::error!(%error, "cannot read the webhook secret");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let Some(signature) = first_header(&headers, &SIGNATURE_HEADERS) else {
        tracing::warn!("a webhook message arrived with no signature");
        return StatusCode::UNAUTHORIZED.into_response();
    };

    if !signature_matches(secret.expose(), &body, &signature) {
        tracing::warn!("a webhook message arrived with a signature that does not match");
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let event = first_header(&headers, &EVENT_HEADERS).unwrap_or_default();

    let message: Message = match serde_json::from_slice(&body) {
        Ok(message) => message,
        Err(error) => {
            tracing::warn!(%error, %event, "cannot read a webhook message");
            return StatusCode::BAD_REQUEST.into_response();
        }
    };

    let Some(repository) = message.repository else {
        // An event about something other than a repository changes nothing
        // in the Recipe index.
        return StatusCode::ACCEPTED.into_response();
    };

    apply(&state, &event, &message.action, &repository).await;

    // Forgejo repeats a message that failed. The index is current now, so
    // say so and let it stop.
    StatusCode::ACCEPTED.into_response()
}

/// Bring the indexes up to date with what the message named.
///
/// One message can only be about one repository, and the topics of that
/// repository say whether it is a Recipe, a Cookbook, or neither. Both
/// indexes are therefore asked, and each one keeps or drops the row by the
/// marker it looks for.
async fn apply(state: &AppState, event: &str, action: &str, repository: &MessageRepository) {
    let owner = repository.owner.login.as_str();
    let slug = repository.name.as_str();

    if event == "repository" && action == "deleted" {
        match crate::index::forget_repository(&state.pool, repository.id).await {
            Ok(removed) => tracing::info!(%owner, %slug, removed, "a Recipe was removed"),
            Err(error) => tracing::warn!(%error, %owner, %slug, "cannot remove a Recipe"),
        }
        match crate::cookbook::forget_repository(&state.pool, repository.id).await {
            Ok(removed) => tracing::info!(%owner, %slug, removed, "a Cookbook was removed"),
            Err(error) => tracing::warn!(%error, %owner, %slug, "cannot remove a Cookbook"),
        }
        return;
    }

    // Reading a private Recipe or Cookbook needs the credential of somebody
    // who may see it. The owner is the one person who always may, so use
    // their session when they have one. Without it, only a public one can be
    // read, and the reconciliation covers the rest.
    let token = owner_credential(&state.pool, &state.cipher, owner).await;

    let recipe =
        crate::index::refresh(&state.pool, &state.forgejo, token.as_ref(), owner, slug).await;
    let cookbook =
        crate::cookbook::refresh(&state.pool, &state.forgejo, token.as_ref(), owner, slug).await;

    tracing::info!(
        %event, %action, %owner, %slug, ?recipe, ?cookbook,
        "the indexes followed Forgejo"
    );
}

/// The credential of the person who owns a repository, when they have one.
async fn owner_credential(
    pool: &SqlitePool,
    cipher: &Cipher,
    owner: &str,
) -> Option<Secret<String>> {
    crate::session::signed_in_people(pool, cipher)
        .await
        .ok()?
        .into_iter()
        .find(|person| person.login.eq_ignore_ascii_case(owner))
        .map(|person| person.token)
}

fn first_header(headers: &HeaderMap, names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| headers.get(*name))
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "a-webhook-secret";
    const BODY: &[u8] = br#"{"action":"created","repository":{"id":1,"name":"chili"}}"#;

    #[test]
    fn a_signature_from_the_same_secret_matches() {
        assert!(signature_matches(SECRET, BODY, &sign(SECRET, BODY)));
    }

    #[test]
    fn another_secret_does_not_match() {
        assert!(!signature_matches(
            SECRET,
            BODY,
            &sign("a-different-secret", BODY)
        ));
    }

    #[test]
    fn a_changed_body_does_not_match() {
        let signature = sign(SECRET, BODY);
        assert!(!signature_matches(
            SECRET,
            b"{\"action\":\"deleted\"}",
            &signature
        ));
    }

    #[test]
    fn a_signature_that_is_not_hex_is_refused() {
        for value in ["", "  ", "not-hex", "abc", "zz", "0x1234"] {
            assert!(
                !signature_matches(SECRET, BODY, value),
                "`{value}` must not pass"
            );
        }
    }

    #[test]
    fn a_signature_of_the_wrong_length_is_refused() {
        let signature = sign(SECRET, BODY);
        assert!(!signature_matches(SECRET, BODY, &signature[..40]));
        assert!(!signature_matches(SECRET, BODY, &format!("{signature}00")));
    }

    #[test]
    fn the_signature_is_a_sha256_hmac_in_lower_case_hex() {
        let signature = sign(SECRET, BODY);
        assert_eq!(signature.len(), 64);
        assert!(signature.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(signature, signature.to_lowercase());
    }

    #[test]
    fn hex_reading_matches_what_signing_wrote() {
        let signature = sign(SECRET, BODY);
        assert_eq!(from_hex(&signature).map(|b| b.len()), Some(32));
        assert_eq!(from_hex("00ff"), Some(vec![0, 255]));
        assert_eq!(from_hex("0"), None);
    }

    #[test]
    fn either_header_name_carries_the_signature() {
        let mut headers = HeaderMap::new();
        headers.insert("x-gitea-event", "push".parse().unwrap());
        assert_eq!(
            first_header(&headers, &EVENT_HEADERS).as_deref(),
            Some("push")
        );

        let mut newer = HeaderMap::new();
        newer.insert("x-forgejo-event", "repository".parse().unwrap());
        assert_eq!(
            first_header(&newer, &EVENT_HEADERS).as_deref(),
            Some("repository")
        );
    }
}
