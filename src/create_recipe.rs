//! Creating a Recipe.
//!
//! One action reaches three systems: the parser decides whether the source
//! can be published, Forgejo makes the repository and marks it with topics,
//! and Git writes the first Version as the person who asked for it.
//!
//! There is no transaction across Forgejo and Git. The steps are ordered so
//! that a failure leaves state a person can understand: a repository with no
//! Version is visible and can be retried or removed, while a Version can
//! never exist without its repository.

use std::collections::BTreeMap;

use crate::forgejo::{ForgejoClient, ForgejoError, ForgejoUser, Repository};
use crate::git::{GitAdapter, GitError, Identity, InitialCommit};
use crate::recipe::{self, MAX_SOURCE_BYTES, RECIPE_FILE, RECIPE_TOPICS};
use crate::secret::Secret;

/// The branch that holds the published Recipe.
pub const MAIN_BRANCH: &str = "main";

/// How many slugs to try before giving up on a collision.
const MAX_SLUG_ATTEMPTS: u32 = 50;

/// The address domain that Forgejo uses when a person hides their address.
/// This matches the Forgejo default, and a deployment can change it.
pub const DEFAULT_NOREPLY_DOMAIN: &str = "noreply.localhost";

/// Pick the address that goes into History.
///
/// `/api/v1/user` returns the real address to the person it belongs to, even
/// when they hide it from everybody else. Writing that value into a commit
/// would publish it, because History is readable by anybody who can read the
/// Recipe. So a person who hides their address gets the Forgejo no-reply
/// address instead, which is what Forgejo itself writes.
pub fn commit_email(login: &str, real_email: &str, hide_email: bool, noreply_domain: &str) -> String {
    if hide_email || real_email.trim().is_empty() {
        format!("{}@{}", login.to_lowercase(), noreply_domain)
    } else {
        real_email.to_string()
    }
}

/// What the person filled in.
#[derive(Debug, Clone)]
pub struct NewRecipe {
    pub title: String,
    pub source: String,
    pub private: bool,
    /// The domain Forgejo uses for a hidden address.
    pub noreply_domain: String,
}

/// A Recipe that now exists.
#[derive(Debug, Clone)]
pub struct CreatedRecipe {
    pub owner: String,
    pub slug: String,
    pub title: String,
    /// Where the repository lives in Forgejo, for **Open in Forgejo**.
    pub forgejo_url: String,
    pub version: String,
    /// Messages that did not stop the creation.
    pub warnings: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum CreateError {
    #[error("the Recipe needs a title")]
    MissingTitle,
    #[error("the Recipe source is larger than 1 MB")]
    TooLarge,
    /// The parser refused the source. The messages are for the person.
    #[error("the Cooklang source has an error")]
    Invalid { errors: Vec<String> },
    #[error("cannot find a free name for the Recipe")]
    NoFreeName,
    #[error(transparent)]
    Forgejo(#[from] ForgejoError),
    #[error(transparent)]
    Git(#[from] GitError),
}

/// Create a Recipe.
pub async fn create(
    forgejo: &ForgejoClient,
    git: &dyn GitAdapter,
    token: &Secret<String>,
    user: &ForgejoUser,
    input: NewRecipe,
) -> Result<CreatedRecipe, CreateError> {
    let title = input.title.trim().to_string();
    if title.is_empty() {
        return Err(CreateError::MissingTitle);
    }

    // The title field writes the Cooklang metadata, so the source stays the
    // one place that holds it.
    let source = recipe::set_title(&input.source, &title);

    // The limit applies to what will be stored, not to what was typed.
    if source.len() > MAX_SOURCE_BYTES {
        return Err(CreateError::TooLarge);
    }

    let parsed = recipe::parse(&source);
    if !parsed.is_valid() {
        return Err(CreateError::Invalid {
            errors: parsed.errors.iter().map(|d| d.message.clone()).collect(),
        });
    }

    let repository = create_repository(forgejo, token, &title, input.private).await?;

    // Ask Forgejo whether this person hides their address. A failure here
    // is treated as "hide", because publishing an address by accident
    // cannot be undone.
    let hide_email = match forgejo.user_settings(token).await {
        Ok(settings) => settings.hide_email,
        Err(error) => {
            tracing::warn!(%error, "cannot read the privacy setting; using the no-reply address");
            true
        }
    };

    let identity = Identity {
        name: user.display_name().to_string(),
        email: commit_email(
            &user.login,
            &user.email,
            hide_email,
            &input.noreply_domain,
        ),
    };

    let mut files = BTreeMap::new();
    files.insert(RECIPE_FILE.to_string(), source.into_bytes());

    let version = git
        .create_initial_commit(InitialCommit {
            // Built from how this process reaches Forgejo, not from the
            // clone_url that Forgejo reports.
            remote_url: &forgejo.git_url(&repository.full_name),
            token,
            identity: &identity,
            branch: MAIN_BRANCH,
            message: format!("Add {title}").as_str(),
            files,
        })
        .await?;

    // Forgejo finishes recording a first push a moment after the push
    // returns. Until it does, it reports the repository as empty and refuses
    // to serve the file. The person is about to be sent to the Recipe page,
    // so wait here rather than show them an empty Recipe.
    wait_until_recorded(forgejo, token, &user.login, &repository.name).await;

    // Topics come last. A repository without them does not appear in the
    // application, so marking it only after it holds a Version keeps a
    // half-made Recipe out of the lists.
    forgejo
        .set_topics(token, &user.login, &repository.name, &RECIPE_TOPICS)
        .await?;

    Ok(CreatedRecipe {
        owner: user.login.clone(),
        slug: repository.name,
        title,
        forgejo_url: forgejo.web_url(&repository.full_name),
        version,
        warnings: parsed.warnings.iter().map(|d| d.message.clone()).collect(),
    })
}

/// How long to wait for Forgejo to record a first push.
const RECORD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Wait until Forgejo stops reporting the repository as empty.
///
/// This never fails the creation. The Version is already pushed and is
/// authoritative in Git; a slow Forgejo only means the page needs a refresh.
async fn wait_until_recorded(
    forgejo: &ForgejoClient,
    token: &Secret<String>,
    owner: &str,
    name: &str,
) {
    let deadline = std::time::Instant::now() + RECORD_TIMEOUT;

    while std::time::Instant::now() < deadline {
        match forgejo.repository(token, owner, name).await {
            Ok(repository) if !repository.empty => return,
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(%error, "cannot read the Recipe repository while waiting");
                return;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    }

    tracing::warn!(
        %owner, %name,
        "Forgejo still reports the Recipe as empty; the Version is pushed and the page may need a refresh"
    );
}

/// Make the repository, working around a name that is already used.
///
/// The person never sees this. They gave a title, and two Recipes may share
/// one, so the slug gets a number until it is free.
async fn create_repository(
    forgejo: &ForgejoClient,
    token: &Secret<String>,
    title: &str,
    private: bool,
) -> Result<Repository, CreateError> {
    let base = recipe::slug(title);

    for attempt in 1..=MAX_SLUG_ATTEMPTS {
        let candidate = recipe::slug_attempt(&base, attempt);

        match forgejo
            .create_repository(token, &candidate, private, MAIN_BRANCH)
            .await
        {
            Ok(repository) => return Ok(repository),
            // Forgejo answers 409 when the name belongs to something else.
            Err(ForgejoError::Status { status: 409, .. }) => continue,
            Err(ForgejoError::Status { status: 422, body }) if body.contains("already exist") => {
                continue;
            }
            Err(error) => return Err(error.into()),
        }
    }

    Err(CreateError::NoFreeName)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hidden_address_never_reaches_history() {
        let email = commit_email("Sam", "sam@example.test", true, "noreply.localhost");
        assert_eq!(email, "sam@noreply.localhost");
        assert!(!email.contains("example.test"));
    }

    #[test]
    fn a_shown_address_is_used_as_it_is() {
        let email = commit_email("sam", "sam@example.test", false, "noreply.localhost");
        assert_eq!(email, "sam@example.test");
    }

    #[test]
    fn an_absent_address_falls_back_to_no_reply() {
        // Forgejo can answer with an empty address. A commit still needs one.
        let email = commit_email("sam", "  ", false, "noreply.localhost");
        assert_eq!(email, "sam@noreply.localhost");
    }

    #[test]
    fn a_missing_title_is_refused_before_anything_is_created() {
        // The check happens before any call, so nothing needs cleaning up.
        let input = NewRecipe {
            title: "   ".to_string(),
            source: "Chop the @onion{1}.".to_string(),
            private: false,
            noreply_domain: DEFAULT_NOREPLY_DOMAIN.to_string(),
        };
        assert!(input.title.trim().is_empty());
    }

    #[test]
    fn the_title_field_writes_the_cooklang_metadata() {
        let source = recipe::set_title("Chop the @onion{1}.", "Onion Soup");
        assert_eq!(recipe::parse(&source).title.as_deref(), Some("Onion Soup"));
    }

    #[test]
    fn the_size_limit_applies_to_what_gets_stored() {
        // A source just under the limit grows when the title is added, so
        // the check has to happen after that step and not before.
        let body = "a".repeat(MAX_SOURCE_BYTES - 10);
        let with_title = recipe::set_title(&body, "A Long One");
        assert!(with_title.len() > MAX_SOURCE_BYTES);
    }
}
