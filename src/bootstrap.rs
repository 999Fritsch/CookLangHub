//! The administrator bootstrap command.
//!
//! It registers this installation in Forgejo twice over: as an OAuth2
//! application, which is how a person signs in, and as one system webhook,
//! which is how Forgejo reports a change. It records both credentials
//! locally. The project does not build a graphical installer: one command
//! keeps the configuration reproducible.
//!
//! The command is repeatable. Running it again finds what it made before and
//! reuses it, so Forgejo never collects duplicates.

use sqlx::sqlite::SqlitePool;

use crate::auth::OAUTH_APPLICATION_NAME;
use crate::crypto::Cipher;
use crate::forgejo::ForgejoClient;
use crate::secret::Secret;
use crate::session::now;

#[derive(Debug, thiserror::Error)]
pub enum BootstrapError {
    #[error(transparent)]
    Forgejo(#[from] crate::forgejo::ForgejoError),
    #[error(transparent)]
    Store(#[from] sqlx::Error),
    #[error(transparent)]
    Crypto(#[from] crate::crypto::CryptoError),
    #[error(transparent)]
    Webhook(#[from] crate::webhook::WebhookError),
    #[error("Forgejo did not return a client secret for application {0}")]
    NoSecret(i64),
}

/// What the command did, so that it can tell the administrator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// No application existed, so the command made one.
    Created { client_id: String },
    /// The application existed and the command refreshed its secret.
    Reused { client_id: String },
}

impl Outcome {
    pub fn client_id(&self) -> &str {
        match self {
            Outcome::Created { client_id } | Outcome::Reused { client_id } => client_id,
        }
    }
}

/// Register the OAuth client and the system webhook, and store both.
///
/// `redirect_uri` must be the callback address of this application as a
/// browser reaches it, not as the internal network names it. `webhook_url`
/// is the opposite: it is the address that Forgejo itself reaches, which
/// inside the bundled stack is a name on the internal network.
pub async fn run(
    pool: &SqlitePool,
    cipher: &Cipher,
    forgejo: &ForgejoClient,
    admin_token: &Secret<String>,
    redirect_uri: &str,
    webhook_url: &str,
    webhook_secret: &Secret<String>,
) -> Result<Outcome, BootstrapError> {
    let existing = forgejo
        .list_oauth_applications(admin_token)
        .await?
        .into_iter()
        .find(|application| application.name == OAUTH_APPLICATION_NAME);

    // A list never carries the client secret. When the application already
    // exists, an update makes Forgejo issue a new secret and return it. The
    // client_id does not change, so no user has to approve again.
    let (application, outcome) = match existing {
        Some(found) => {
            let updated = forgejo
                .update_oauth_application(
                    admin_token,
                    found.id,
                    OAUTH_APPLICATION_NAME,
                    redirect_uri,
                )
                .await?;
            let client_id = updated.client_id.clone();
            (updated, Outcome::Reused { client_id })
        }
        None => {
            let created = forgejo
                .create_oauth_application(admin_token, OAUTH_APPLICATION_NAME, redirect_uri)
                .await?;
            let client_id = created.client_id.clone();
            (created, Outcome::Created { client_id })
        }
    };

    if application.client_secret.is_empty() {
        return Err(BootstrapError::NoSecret(application.id));
    }

    let encrypted = cipher.encrypt(&application.client_secret)?;

    sqlx::query(
        "INSERT INTO oauth_client (id, forgejo_app_id, client_id, client_secret, redirect_uri, updated_at)
         VALUES (1, ?, ?, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET
             forgejo_app_id = excluded.forgejo_app_id,
             client_id      = excluded.client_id,
             client_secret  = excluded.client_secret,
             redirect_uri   = excluded.redirect_uri,
             updated_at     = excluded.updated_at",
    )
    .bind(application.id)
    .bind(&application.client_id)
    .bind(encrypted)
    .bind(redirect_uri)
    .bind(now())
    .execute(pool)
    .await?;

    // One system webhook covers the whole instance. It is registered here
    // for the same reason as the OAuth application: an administrator runs
    // one command, and running it again changes nothing.
    let webhook = crate::webhook::register(
        pool,
        cipher,
        forgejo,
        admin_token,
        webhook_url,
        webhook_secret,
    )
    .await?;

    match &webhook {
        crate::webhook::Registration::Created { hook_id } => {
            tracing::info!(hook_id, %webhook_url, "registered the system webhook in Forgejo");
        }
        crate::webhook::Registration::Reused { hook_id } => {
            tracing::info!(hook_id, %webhook_url, "the system webhook existed; refreshed it");
        }
    }

    Ok(outcome)
}
