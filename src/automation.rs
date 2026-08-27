//! The automation identity, and the Cookbooks that follow a Recipe.
//!
//! # Who makes an automatic Version
//!
//! A Cookbook that follows a Recipe moves to each new Version of that
//! Recipe, and each move makes one Version of the Cookbook. That Version has
//! an author, and the author must not be a person. Nobody may have their
//! name on a change they did not make, and the person reading the Cookbook
//! page did not make it either.
//!
//! So one dedicated Forgejo account is the author of every automatic
//! Version. An administrator makes the account in Forgejo, gives this
//! application one access token for it, and Forgejo stays the authority for
//! who that account is and for what it may reach. The application asks
//! Forgejo who the token belongs to rather than being told, so a wrong name
//! can never be recorded.
//!
//! The account is an ordinary one. It is not an administrator, and it holds
//! no permission of its own: a person gives it write access to their own
//! Cookbook when a Recipe in that Cookbook starts to follow, and that access
//! is taken away again when the last one stops.
//!
//! # Which Cookbooks the automation looks at
//!
//! Forgejo answers that, and this application keeps no list. The automation
//! account can reach exactly the Cookbooks that somebody gave it write
//! access to, so a search with its own credential names them and nothing
//! else. Git is then read for each of them, because `.gitmodules` and the
//! recorded Version are the authority for what a Cookbook holds.
//!
//! Two rules follow, and both are acceptance criteria of this feature.
//! Removing the access in Forgejo stops the automation, because the search
//! and the push both stop. And nothing here ever grants access again: only
//! a person who changes a Recipe to Following does that.
//!
//! # What stops the automation
//!
//! A state that this application cannot act on is reported and never
//! repaired. The Recipe that no longer has the Versions a Cookbook follows,
//! the automation that has no credential, and the automation that lost its
//! access are each named on the Cookbook page, with **Open in Forgejo**
//! beside them. The application does not choose another Version to follow.

use sqlx::sqlite::SqlitePool;

use crate::cookbook::{self, Reference};
use crate::crypto::Cipher;
use crate::forgejo::{ForgejoClient, ForgejoError, Ownership, Repository};
use crate::git::{GitAdapter, Identity, WriteReference};
use crate::secret::Secret;
use crate::session::now;

/// The access mode the automation needs on a Cookbook that it advances.
///
/// A Forgejo access mode, and the smallest one that can publish a Version.
const WRITE: &str = "write";

/// Shown when a Cookbook follows a Recipe and no automation is registered.
pub const NO_CREDENTIAL_MESSAGE: &str = "A Recipe in this Cookbook follows updates, and CookLangHub has no automation account for this installation. The Cookbook does not move to a new Version of that Recipe. Ask the administrator of this installation to register one.";

/// Shown when the automation lost its access to a Cookbook.
pub const NO_ACCESS_MESSAGE: &str = "A Recipe in this Cookbook follows updates, and the automation account of CookLangHub cannot write to this Cookbook. The Cookbook does not move to a new Version of that Recipe. Open the Cookbook in Forgejo to give the access again.";

/// Shown beside one Recipe whose Versions the Cookbook can no longer follow.
pub const NOTHING_TO_FOLLOW_MESSAGE: &str = "CookLangHub cannot follow this Recipe any more. The Recipe does not hold the Versions that this Cookbook follows. CookLangHub keeps the Version it has and selects no other.";

#[derive(Debug, thiserror::Error)]
pub enum AutomationError {
    #[error(transparent)]
    Forgejo(#[from] ForgejoError),
    #[error(transparent)]
    Store(#[from] sqlx::Error),
    #[error(transparent)]
    Crypto(#[from] crate::crypto::CryptoError),
}

/// The account that is the author of every automatic Version.
#[derive(Debug, Clone)]
pub struct Automation {
    /// The Forgejo account name.
    pub login: String,
    /// The name that History shows.
    pub name: String,
    /// The identifier Forgejo gave the account.
    pub forgejo_user_id: i64,
    pub token: Secret<String>,
}

impl Automation {
    /// Who History records for an automatic Version.
    ///
    /// The address is the Forgejo no-reply address of the account, so
    /// Forgejo shows the automation account beside the Version and no real
    /// address is published. History is readable by anybody who can read
    /// the Cookbook.
    pub fn identity(&self, noreply_domain: &str) -> Identity {
        Identity {
            name: self.name.clone(),
            email: crate::create_recipe::commit_email(&self.login, "", true, noreply_domain),
        }
    }
}

/// Record the credential of the automation account.
///
/// Forgejo is asked who the token belongs to, so the account that is stored
/// is the account that the token can act as. An administrator runs this
/// once, and running it again replaces the credential.
pub async fn record(
    pool: &SqlitePool,
    cipher: &Cipher,
    forgejo: &ForgejoClient,
    token: &Secret<String>,
) -> Result<Automation, AutomationError> {
    let user = forgejo.current_user(token).await?;

    // An administrator credential would give the automation every Cookbook
    // of the installation, and the automation must reach only the Cookbooks
    // that somebody gave it. This is a warning and not a refusal, because
    // Forgejo stays the authority and can take the permission away.
    if forgejo.is_administrator(token).await.unwrap_or(false) {
        tracing::warn!(
            login = %user.login,
            "the automation account administers this Forgejo; an ordinary account is enough"
        );
    }

    sqlx::query(
        "INSERT INTO automation (id, login, name, forgejo_user_id, token, updated_at)
         VALUES (1, ?, ?, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET
             login           = excluded.login,
             name            = excluded.name,
             forgejo_user_id = excluded.forgejo_user_id,
             token           = excluded.token,
             updated_at      = excluded.updated_at",
    )
    .bind(&user.login)
    .bind(user.display_name())
    .bind(user.id)
    .bind(cipher.encrypt(token.expose())?)
    .bind(now())
    .execute(pool)
    .await?;

    tracing::info!(login = %user.login, "the automation account is registered");

    Ok(Automation {
        login: user.login.clone(),
        name: user.display_name().to_string(),
        forgejo_user_id: user.id,
        token: Secret::new(token.expose().to_string()),
    })
}

/// Read the credential of the automation account, when there is one.
pub async fn credential(
    pool: &SqlitePool,
    cipher: &Cipher,
) -> Result<Option<Automation>, AutomationError> {
    let row: Option<(String, String, i64, Vec<u8>)> =
        sqlx::query_as("SELECT login, name, forgejo_user_id, token FROM automation WHERE id = 1")
            .fetch_optional(pool)
            .await?;

    let Some((login, name, forgejo_user_id, token)) = row else {
        return Ok(None);
    };

    Ok(Some(Automation {
        login,
        name,
        forgejo_user_id,
        token: Secret::new(cipher.decrypt(&token)?),
    }))
}

/// The automation account, or nothing, without a fault to handle.
///
/// A page and a webhook both want the same thing here: act when there is an
/// automation account, and say so plainly when there is not.
pub async fn of(pool: &SqlitePool, cipher: &Cipher) -> Option<Automation> {
    match credential(pool, cipher).await {
        Ok(found) => found,
        Err(error) => {
            tracing::warn!(%error, "cannot read the automation credential");
            None
        }
    }
}

/// Whether a Cookbook follows any of the Recipes it holds.
pub fn follows_anything(references: &[Reference]) -> bool {
    references
        .iter()
        .any(|reference| reference.follow.is_some())
}

/// Give the automation the access a Cookbook needs, or take away the access
/// it does not need.
///
/// The automation gets write access to a Cookbook that follows a Recipe, and
/// to no other Cookbook. `actor` is the credential of the person who made
/// the change, so Forgejo decides whether they may give the access at all.
///
/// A refusal here is not a fault of the change that the person asked for.
/// Git holds what the Cookbook holds, and the person said what they wanted,
/// so that is written either way. The Cookbook page then reports that the
/// automation cannot run, and repairs nothing.
pub async fn align(
    forgejo: &ForgejoClient,
    actor: &Secret<String>,
    cookbook: &Repository,
    references: &[Reference],
    automation: Option<&Automation>,
) {
    let Some(automation) = automation else {
        if follows_anything(references) {
            tracing::warn!("{NO_CREDENTIAL_MESSAGE}");
        }
        return;
    };

    let owner = &cookbook.owner.login;
    let slug = &cookbook.name;

    let outcome = if follows_anything(references) {
        forgejo
            .add_collaborator(actor, owner, slug, &automation.login, WRITE)
            .await
    } else {
        forgejo
            .remove_collaborator(actor, owner, slug, &automation.login)
            .await
    };

    match outcome {
        Ok(()) => tracing::info!(
            %owner, %slug,
            following = follows_anything(references),
            "the access of the automation to this Cookbook matches what it holds"
        ),
        Err(error) => tracing::warn!(
            %error, %owner, %slug,
            "cannot change the access of the automation to this Cookbook"
        ),
    }
}

/// Whether the automation can publish a Version of this Cookbook now.
///
/// The question is asked with the credential of the automation itself, so
/// the answer is the one that its next push gets. Forgejo decides it.
pub async fn can_write(
    forgejo: &ForgejoClient,
    automation: &Automation,
    owner: &str,
    slug: &str,
) -> bool {
    forgejo
        .can_write(&automation.token, owner, slug)
        .await
        .unwrap_or(false)
}

/// What one run of the automation did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// How many Cookbooks were read.
    pub scanned: usize,
    /// How many Cookbook Versions the automation made.
    pub advanced: usize,
    /// How many Recipes could not be followed, and were reported instead.
    pub stopped: usize,
}

/// Move every Cookbook that follows a Recipe to the Version that Recipe has.
///
/// `recipe` names the one Recipe that changed, which is what the webhook
/// knows. `None` looks at every Recipe that any reachable Cookbook follows,
/// which is what a restart needs, because a message that arrived while this
/// application was stopped reached nobody.
///
/// Nothing is written to a Recipe. Only a Cookbook gains a Version, and only
/// when the Version it records is not the Version the Recipe has.
pub async fn advance(
    pool: &SqlitePool,
    cipher: &Cipher,
    forgejo: &ForgejoClient,
    git: &dyn GitAdapter,
    noreply_domain: &str,
    recipe: Option<(&str, &str)>,
) -> Report {
    let mut report = Report::default();

    let Some(automation) = of(pool, cipher).await else {
        // Say it once, and only when a Cookbook actually wants it. A
        // installation with no Following Recipe needs no automation.
        tracing::debug!("no automation account is registered");
        return report;
    };

    // Forgejo names the Cookbooks that the automation may reach. That set is
    // exactly the set somebody gave it write access to, so it is both the
    // permission decision and the list, and neither is kept here.
    let cookbooks = match cookbook::visible(
        forgejo,
        Some(&automation.token),
        Ownership::ReachableBy(automation.forgejo_user_id),
    )
    .await
    {
        Ok((found, truncated)) => {
            if truncated {
                tracing::warn!("the automation reaches more Cookbooks than one search covers");
            }
            found
        }
        Err(error) => {
            tracing::warn!(%error, "cannot ask Forgejo which Cookbooks the automation may write");
            return report;
        }
    };

    let identity = automation.identity(noreply_domain);

    for cookbook in &cookbooks {
        let contents = cookbook::references(forgejo, Some(&automation.token), cookbook).await;

        // A Cookbook that did not answer records nothing this can judge. A
        // Version must never be made from a question that was never
        // answered.
        if !contents.complete {
            tracing::info!(
                owner = %cookbook.owner.login,
                slug = %cookbook.name,
                "cannot read what this Cookbook holds; it is left as it is"
            );
            continue;
        }

        report.scanned += 1;

        for reference in &contents.references {
            let Some(branch) = reference.follow.as_deref() else {
                continue;
            };
            let Some((recipe_owner, recipe_slug)) =
                cookbook::recipe_named_by(forgejo, &reference.url)
            else {
                // A Recipe of another Forgejo. The application never
                // repairs such an address and never fetches through it.
                continue;
            };

            if let Some((owner, slug)) = recipe
                && !(owner.eq_ignore_ascii_case(&recipe_owner)
                    && slug.eq_ignore_ascii_case(&recipe_slug))
            {
                continue;
            }

            advance_one(
                pool,
                forgejo,
                git,
                &automation,
                &identity,
                cookbook,
                reference,
                branch,
                (&recipe_owner, &recipe_slug),
                &mut report,
            )
            .await;
        }
    }

    if report.advanced > 0 || report.stopped > 0 {
        tracing::info!(
            scanned = report.scanned,
            advanced = report.advanced,
            stopped = report.stopped,
            "the Cookbooks that follow a Recipe are up to date"
        );
    }

    // The Diagnostics page reports when this last ran and what it found. A
    // Cookbook that the automation cannot write to is counted as a failure
    // there, because somebody has to give the access again in Forgejo.
    crate::diagnostics::record_sweep(
        pool,
        crate::diagnostics::AUTOMATION,
        report.scanned as i64,
        report.advanced as i64,
        0,
        report.stopped as i64,
    )
    .await;

    report
}

/// Move one Cookbook to the Version that one Recipe has now.
#[allow(clippy::too_many_arguments)]
async fn advance_one(
    pool: &SqlitePool,
    forgejo: &ForgejoClient,
    git: &dyn GitAdapter,
    automation: &Automation,
    identity: &Identity,
    cookbook: &Repository,
    reference: &Reference,
    branch: &str,
    recipe: (&str, &str),
    report: &mut Report,
) {
    let (recipe_owner, recipe_slug) = recipe;
    let owner = &cookbook.owner.login;
    let slug = &cookbook.name;

    // Git is the authority for which Version a Recipe has. Nothing is
    // parsed: a Recipe that no longer holds valid Cooklang keeps advancing,
    // because Following follows the published Recipe and not the parser.
    let recipe_url = forgejo.git_url(&format!("{recipe_owner}/{recipe_slug}"));

    let head = match git
        .branch_head(&recipe_url, &automation.token, branch)
        .await
    {
        Ok(Some(head)) => head,
        Ok(None) => {
            // The Recipe no longer holds what this Cookbook follows. Say so
            // and select nothing else. The Cookbook keeps the Version it has.
            tracing::warn!(
                %owner, %slug, path = %reference.path,
                "{NOTHING_TO_FOLLOW_MESSAGE}"
            );
            report.stopped += 1;
            return;
        }
        Err(error) => {
            tracing::info!(
                %error, %owner, %slug, path = %reference.path,
                "cannot read the Versions of a Recipe that this Cookbook follows"
            );
            report.stopped += 1;
            return;
        }
    };

    // The Cookbook already holds it. A Version that changes nothing must
    // never reach History, because History is for a person to read.
    if reference.version.as_deref() == Some(head.as_str()) {
        return;
    }

    let title = recipe_title(pool, recipe_owner, recipe_slug, &reference.path).await;

    match git
        .write_reference(WriteReference {
            remote_url: &forgejo.git_url(&cookbook.full_name),
            token: &automation.token,
            identity,
            branch: cookbook.branch(),
            message: &format!("Update {title}"),
            path: &reference.path,
            url: &reference.url,
            version: &head,
            // The Cookbook keeps following. One advance never turns
            // Following into Pinned.
            follow: Some(branch),
        })
        .await
    {
        Ok(version) => {
            tracing::info!(
                %owner, %slug, path = %reference.path, %version,
                "a Cookbook followed a Recipe to a new Version"
            );
            report.advanced += 1;
        }
        Err(error) => {
            // The access can have been taken away in Forgejo. The automation
            // stops here and this application does not give the access again.
            tracing::warn!(
                %error, %owner, %slug, path = %reference.path,
                "the automation cannot publish a Version of this Cookbook"
            );
            report.stopped += 1;
        }
    }
}

/// The Recipe title that History records for an automatic Version.
///
/// The index only supplies words for a message. A Recipe that is not in it
/// falls back to the name the Cookbook holds it at, so a message is never
/// missing and no read of Forgejo is needed for one.
async fn recipe_title(pool: &SqlitePool, owner: &str, slug: &str, path: &str) -> String {
    crate::index::get(pool, owner, slug)
        .await
        .ok()
        .flatten()
        .map(|entry| entry.title)
        .unwrap_or_else(|| path.to_string())
}

/// Follow one Recipe that Forgejo reported a change to.
///
/// The webhook calls this. It is a speed improvement and never the only way
/// a Cookbook follows: [`advance`] with no Recipe repairs everything that a
/// lost message left behind.
pub async fn follow_recipe(state: &crate::web::AppState, owner: &str, slug: &str) -> Report {
    advance(
        &state.pool,
        &state.cipher,
        &state.forgejo,
        state.git.as_ref(),
        &state.forgejo_noreply_domain,
        Some((owner, slug)),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(path: &str, follow: Option<&str>) -> Reference {
        Reference {
            path: path.to_string(),
            url: format!("http://forge.test/sam/{path}.git"),
            follow: follow.map(str::to_string),
            version: Some("a".repeat(40)),
        }
    }

    fn automation() -> Automation {
        Automation {
            login: "cooklanghub-bot".to_string(),
            name: "CookLangHub".to_string(),
            forgejo_user_id: 7,
            token: Secret::new("a-token".to_string()),
        }
    }

    #[test]
    fn a_cookbook_needs_the_automation_only_when_it_follows_something() {
        assert!(!follows_anything(&[]));
        assert!(!follows_anything(&[reference("chili", None)]));
        assert!(follows_anything(&[
            reference("chili", None),
            reference("toast", Some("main")),
        ]));
    }

    #[test]
    fn an_automatic_version_carries_the_automation_account_and_no_real_address() {
        let identity = automation().identity("noreply.localhost");

        assert_eq!(identity.name, "CookLangHub");
        assert_eq!(identity.email, "cooklanghub-bot@noreply.localhost");
    }

    #[test]
    fn the_credential_of_the_automation_never_reaches_a_log() {
        let printed = format!("{:?}", automation());
        assert!(!printed.contains("a-token"), "got: {printed}");
    }

    #[test]
    fn every_diagnostic_names_no_git_word() {
        // `Sharing` holds `sha`, so each word is matched whole.
        for message in [
            NO_CREDENTIAL_MESSAGE,
            NO_ACCESS_MESSAGE,
            NOTHING_TO_FOLLOW_MESSAGE,
        ] {
            for word in [
                "branch",
                "commit",
                "sha",
                "git",
                "submodule",
                "gitlink",
                "repository",
                "fork",
                "pull",
            ] {
                assert!(
                    !message
                        .to_lowercase()
                        .split(|c: char| !c.is_ascii_alphanumeric())
                        .any(|found| found == word),
                    "`{word}` must not reach a person: {message}"
                );
            }
        }
    }

    #[test]
    fn every_diagnostic_says_what_the_state_is_and_what_stays() {
        // A diagnostic that only says something is wrong leaves a person
        // with nothing to do.
        assert!(NO_CREDENTIAL_MESSAGE.contains("administrator"));
        assert!(NO_ACCESS_MESSAGE.contains("Forgejo"));
        assert!(NOTHING_TO_FOLLOW_MESSAGE.contains("selects no other"));
    }
}
