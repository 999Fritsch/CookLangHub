//! The administrator bootstrap command.
//!
//! It registers this installation as an OAuth2 application in Forgejo and
//! records the credential locally. The project does not build a graphical
//! installer: one command keeps the configuration reproducible.
//!
//! The command is repeatable. Running it again finds the application that it
//! made before and reuses it, so Forgejo never collects duplicates.

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

/// Register the OAuth client and store it.
///
/// `redirect_uri` must be the callback address of this application as a
/// browser reaches it, not as the internal network names it.
pub async fn run(
    pool: &SqlitePool,
    cipher: &Cipher,
    forgejo: &ForgejoClient,
    admin_token: &Secret<String>,
    redirect_uri: &str,
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

    Ok(outcome)
}
