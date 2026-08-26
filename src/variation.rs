//! Variations: an independent Recipe that was made from another Recipe.
//!
//! A Variation is a Forgejo fork, and that is the whole of it. Forgejo copies
//! the content and the History, it gives the copy the visibility of the
//! source, and it records where the copy came from. This application adds no
//! marker to Git and keeps no lineage of its own, so there is nothing here
//! that can disagree with Forgejo.
//!
//! Two things happen after Forgejo answers. The copy gets the Recipe topics,
//! because Forgejo does not copy them and a repository without them is not a
//! Recipe in this application. And a Variation that a person asked for from
//! an earlier Version has its published branch moved back to that Version,
//! through the Git adapter, before anybody can read it.
//!
//! Forgejo stops recording the relationship when the source Recipe is
//! deleted. The Variation is then an ordinary Recipe that holds everything it
//! held before, and this application must not invent a source that Forgejo no
//! longer names. While Forgejo does name a source that this person cannot
//! read, the page says that the source is not available and offers **Open in
//! Forgejo**.

use crate::create_recipe;
use crate::forgejo::{ForgejoClient, ForgejoError, ForgejoUser, Repository};
use crate::git::{GitAdapter, GitError};
use crate::recipe::{self, RECIPE_FILE, RECIPE_TOPICS};
use crate::secret::Secret;

/// How many names to offer before giving up on a collision.
const MAX_NAME_ATTEMPTS: u32 = 50;

/// How many Versions a person can start a Variation from.
///
/// This is the page of History that a person reads, so a Version that they
/// can select is a Version that they can see.
pub const VERSION_WINDOW: u32 = 50;

/// How many Variations one page shows.
pub const MAX_VARIATIONS: u32 = 50;

/// What Forgejo says when the person already has a Variation of this Recipe.
///
/// Forgejo allows one Variation of one Recipe for one person, and it answers
/// 409 for the second. The same status also means that the name is used, so
/// the two are told apart by what Forgejo wrote.
const ALREADY_FORKED: &str = "already forked";

/// A Variation that now exists.
#[derive(Debug, Clone)]
pub struct CreatedVariation {
    pub owner: String,
    pub slug: String,
}

#[derive(Debug, thiserror::Error)]
pub enum VariationError {
    /// Forgejo does not give this Recipe to this person: it is gone, it
    /// never existed, or they may not see it. Forgejo decides.
    #[error("this Recipe is not available")]
    NoSource,
    /// This person already has a Variation of this Recipe.
    #[error("you have a Variation of this Recipe already")]
    AlreadyThere,
    /// The Version asked for is not a published Version of this Recipe.
    #[error("this Recipe does not hold that Version")]
    NoVersion,
    /// Every name that the application offered was taken.
    #[error("cannot find a free name for the Variation")]
    NoFreeName,
    #[error(transparent)]
    Forgejo(#[from] ForgejoError),
    #[error(transparent)]
    Git(#[from] GitError),
}

/// Where a Recipe came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Forgejo records no source for this Recipe.
    None,
    /// The source Recipe, as this person can read it.
    Recipe(SourceRecipe),
    /// Forgejo names a source that this person cannot read. It is private
    /// to them now, or Forgejo cannot answer for it.
    Unavailable,
}

impl Source {
    pub fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }

    pub fn recipe(&self) -> Option<&SourceRecipe> {
        match self {
            Self::Recipe(recipe) => Some(recipe),
            _ => None,
        }
    }

    pub fn is_unavailable(&self) -> bool {
        matches!(self, Self::Unavailable)
    }
}

/// The Recipe that a Variation was made from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRecipe {
    pub owner: String,
    pub slug: String,
    /// The title a cook sees. It lives in the Recipe, not in the name.
    pub title: String,
}

impl SourceRecipe {
    pub fn href(&self) -> String {
        format!("/recipes/{}/{}", self.owner, self.slug)
    }
}

/// Make a Variation of a Recipe.
///
/// `version` is the Version to start from. `None` starts at the published
/// Version, which is the Recipe as it is read now.
pub async fn create(
    forgejo: &ForgejoClient,
    git: &dyn GitAdapter,
    token: &Secret<String>,
    user: &ForgejoUser,
    owner: &str,
    slug: &str,
    version: Option<&str>,
) -> Result<CreatedVariation, VariationError> {
    // Forgejo decides whether this person may see the Recipe at all. It
    // refuses the copy as well, so this is not a second permission rule: it
    // is what lets the answer say "not available" instead of "refused",
    // which would tell somebody that a private Recipe exists.
    let source = forgejo
        .repository(token, owner, slug)
        .await
        .map_err(|error| {
            tracing::info!(%error, %owner, %slug, "cannot read the Recipe to make a Variation of");
            VariationError::NoSource
        })?;

    // A Version that the person asked for is checked before anything is
    // made, so a refusal leaves nothing behind to clean up.
    if let Some(version) = version
        && !holds_version(forgejo, token, owner, slug, source.branch(), version).await
    {
        return Err(VariationError::NoVersion);
    }

    let made = fork(forgejo, token, owner, slug, &source.name).await?;

    // Forgejo finishes recording a new repository a moment after it answers.
    // The person is about to be sent to the Variation, so wait here rather
    // than show them an empty Recipe.
    create_recipe::wait_until_recorded(forgejo, token, &user.login, &made.name).await;

    // A Variation is a Recipe. Forgejo does not copy the topics, and a
    // repository without them appears in no Recipe list, so they are set
    // here and not left to the person.
    forgejo
        .set_topics(token, &user.login, &made.name, &RECIPE_TOPICS)
        .await?;

    if let Some(version) = version {
        // The copy holds every Version of the source, so the branch only has
        // to move back to the one the person read.
        let remote = forgejo.git_url(&made.full_name);

        if let Err(error) = git
            .move_branch(&remote, token, source.branch(), version)
            .await
        {
            // A Variation that starts at the wrong Version is worse than no
            // Variation at all. This copy is seconds old and holds nothing
            // that anybody wrote, so it goes and the person is told.
            tracing::warn!(%error, owner = %user.login, slug = %made.name, "cannot start the Variation at that Version");
            withdraw(forgejo, token, &user.login, &made.name).await;
            return Err(error.into());
        }
    }

    Ok(CreatedVariation {
        owner: user.login.clone(),
        slug: made.name,
    })
}

/// Whether the published Versions of a Recipe hold this one.
///
/// The window is the page of History that a person reads. A Version that is
/// older than that cannot be selected, and a Version that lives outside the
/// published Recipe is not a published Version at all.
async fn holds_version(
    forgejo: &ForgejoClient,
    token: &Secret<String>,
    owner: &str,
    slug: &str,
    branch: &str,
    version: &str,
) -> bool {
    match forgejo
        .list_commits(Some(token), owner, slug, branch, VERSION_WINDOW)
        .await
    {
        Ok(commits) => commits
            .iter()
            .any(|commit| commit.sha.eq_ignore_ascii_case(version)),
        Err(error) => {
            tracing::info!(%error, %owner, %slug, "cannot read the published Versions of the Recipe");
            false
        }
    }
}

/// Ask Forgejo for the copy, working around a name that is already used.
///
/// The person never sees this. They asked for a Variation of a Recipe, and
/// the name of a repository is not something they chose.
async fn fork(
    forgejo: &ForgejoClient,
    token: &Secret<String>,
    owner: &str,
    slug: &str,
    base: &str,
) -> Result<Repository, VariationError> {
    for attempt in 1..=MAX_NAME_ATTEMPTS {
        let candidate = recipe::slug_attempt(base, attempt);

        match forgejo
            .fork_repository(token, owner, slug, &candidate)
            .await
        {
            Ok(repository) => return Ok(repository),
            Err(ForgejoError::Status { status: 409, body }) => {
                // Forgejo answers 409 for two different states. One of them
                // no other name can solve.
                if body.to_lowercase().contains(ALREADY_FORKED) {
                    return Err(VariationError::AlreadyThere);
                }
                continue;
            }
            Err(ForgejoError::Status { status: 422, body }) if body.contains("already exist") => {
                continue;
            }
            Err(error) => return Err(error.into()),
        }
    }

    Err(VariationError::NoFreeName)
}

/// Remove a copy that was made a moment ago and never shown to anybody.
///
/// This runs only on the path where the Variation could not be started at
/// the Version that the person asked for. It touches nothing that a person
/// wrote, and a failure here is reported and not hidden.
async fn withdraw(forgejo: &ForgejoClient, token: &Secret<String>, owner: &str, slug: &str) {
    if let Err(error) = forgejo.delete_repository(token, owner, slug).await {
        tracing::warn!(%error, %owner, %slug, "cannot remove the Variation that was not finished");
    }
}

/// Read where a Recipe came from.
///
/// Forgejo holds the relationship and this application holds none of it. A
/// source that Forgejo names but does not give to this person is
/// [`Source::Unavailable`], which is what a person sees when the source
/// Recipe went private or their access to it was taken away.
pub async fn source_of(
    forgejo: &ForgejoClient,
    token: Option<&Secret<String>>,
    owner: &str,
    slug: &str,
) -> Source {
    let lineage = match forgejo.repository_lineage(token, owner, slug).await {
        Ok(lineage) => lineage,
        Err(error) => {
            tracing::info!(%error, %owner, %slug, "cannot read where this Recipe comes from");
            return Source::None;
        }
    };

    if !lineage.fork {
        return Source::None;
    }

    // Forgejo says this Recipe was made from another one but does not name
    // it, so the source is one this person cannot reach.
    let Some(parent) = lineage.parent else {
        return Source::Unavailable;
    };

    let (source_owner, source_slug) = (parent.owner_login(), parent.repository_name());
    if source_owner.is_empty() || source_slug.is_empty() {
        return Source::Unavailable;
    }

    // Forgejo names the source to anybody who can read the Variation, so the
    // source is read again as this person before the page links to it.
    let Ok(repository) = forgejo
        .repository_as(token, source_owner, source_slug)
        .await
    else {
        return Source::Unavailable;
    };

    let title = title_of(forgejo, token, source_owner, source_slug, &repository).await;

    Source::Recipe(SourceRecipe {
        owner: source_owner.to_string(),
        slug: source_slug.to_string(),
        title,
    })
}

/// The title a cook sees for a Recipe. It lives in the Recipe file.
async fn title_of(
    forgejo: &ForgejoClient,
    token: Option<&Secret<String>>,
    owner: &str,
    slug: &str,
    repository: &Repository,
) -> String {
    forgejo
        .raw_file(token, owner, slug, repository.branch(), RECIPE_FILE)
        .await
        .ok()
        .and_then(|bytes| recipe::parse(&String::from_utf8_lossy(&bytes)).title)
        .unwrap_or_else(|| repository.name.clone())
}

/// What Forgejo holds that came from one Recipe.
#[derive(Debug, Default)]
pub struct Variations {
    /// The Variations that are Recipes in this application.
    pub recipes: Vec<Repository>,
    /// How many copies Forgejo holds that carry no Recipe topics.
    ///
    /// Somebody can copy a Recipe in Forgejo itself, and Forgejo does not
    /// mark the copy as a Recipe. This application does not mark it either,
    /// and it does not hide it: the page says how many there are and offers
    /// **Open in Forgejo**.
    pub others: usize,
}

/// The Variations that were made from a Recipe.
///
/// Forgejo names them, and this asks Forgejo again for each one that is
/// private, so that a Variation which this person may not read never appears
/// in the list and is never counted. The application computes no permission
/// of its own.
pub async fn variations_of(
    forgejo: &ForgejoClient,
    token: Option<&Secret<String>>,
    owner: &str,
    slug: &str,
) -> Variations {
    let found = match forgejo.list_forks(token, owner, slug, MAX_VARIATIONS).await {
        Ok(found) => found,
        Err(error) => {
            tracing::info!(%error, %owner, %slug, "cannot read the Variations of this Recipe");
            return Variations::default();
        }
    };

    let mut out = Variations {
        recipes: Vec::with_capacity(found.len()),
        others: 0,
    };

    for repository in found {
        // A private Variation counts for nothing until Forgejo gives it to
        // this person. Asking again costs one request for each private one,
        // and it keeps the decision where it belongs.
        if repository.private
            && forgejo
                .repository_as(token, &repository.owner.login, &repository.name)
                .await
                .is_err()
        {
            continue;
        }

        // The topics are the opt-in marker of a Recipe. A copy without them
        // is a repository in Forgejo, and this application must not draw it
        // as a Recipe.
        if crate::index::is_recipe(&repository) {
            out.recipes.push(repository);
        } else {
            out.others += 1;
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The words of the forge, which no message here may say.
    const FORGE_WORDS: [&str; 10] = [
        "commit",
        "branch",
        "diff",
        "repository",
        "fork",
        "patch",
        "head",
        "sha",
        "merge",
        "rebase",
    ];

    /// Whether a text says one of the words of the forge.
    ///
    /// Whole words only. `Sharing` is an area of a Recipe and `share` is
    /// what a person does with one, so neither may be read as the identifier
    /// that Git uses.
    fn says_forge_word(text: &str) -> Option<&'static str> {
        let lower = text.to_lowercase();

        if lower.contains("pull request") {
            return Some("pull request");
        }

        let spoken: Vec<&str> = lower
            .split(|c: char| !c.is_alphanumeric())
            .filter(|word| !word.is_empty())
            .collect();

        FORGE_WORDS.into_iter().find(|word| spoken.contains(word))
    }

    #[test]
    fn every_refusal_a_person_reads_uses_cooking_words() {
        for message in [
            VariationError::NoSource.to_string(),
            VariationError::AlreadyThere.to_string(),
            VariationError::NoVersion.to_string(),
            VariationError::NoFreeName.to_string(),
        ] {
            assert_eq!(
                says_forge_word(&message),
                None,
                "a word of the forge must not reach the person: {message}"
            );
        }
    }

    #[test]
    fn the_name_of_a_variation_steps_aside_from_one_that_is_taken() {
        // The person gave no name. A Recipe they already have with the same
        // name must not stop them, so the application offers the next one.
        assert_eq!(recipe::slug_attempt("chili", 1), "chili");
        assert_eq!(recipe::slug_attempt("chili", 2), "chili-2");

        // Every name that the loop offers is a name of its own, so a person
        // with many Recipes still gets a Variation.
        let offered: std::collections::BTreeSet<String> = (1..=MAX_NAME_ATTEMPTS)
            .map(|attempt| recipe::slug_attempt("chili", attempt))
            .collect();
        assert_eq!(offered.len(), MAX_NAME_ATTEMPTS as usize);
    }

    #[test]
    fn a_recipe_with_no_source_is_not_a_variation() {
        assert!(Source::None.is_none());
        assert!(Source::None.recipe().is_none());
        assert!(!Source::None.is_unavailable());
    }

    #[test]
    fn a_source_that_cannot_be_read_is_its_own_state() {
        // The page must be able to say "not available" rather than link to
        // a Recipe that answers nothing.
        assert!(Source::Unavailable.is_unavailable());
        assert!(Source::Unavailable.recipe().is_none());
        assert!(!Source::Unavailable.is_none());
    }

    #[test]
    fn the_address_of_a_source_recipe_is_the_recipe_address() {
        let source = SourceRecipe {
            owner: "sam".to_string(),
            slug: "chili".to_string(),
            title: "Chili sin Carne".to_string(),
        };
        assert_eq!(source.href(), "/recipes/sam/chili");
    }
}
