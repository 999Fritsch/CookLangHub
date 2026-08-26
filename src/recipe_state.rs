//! The Recipe states that the cooking interface cannot show.
//!
//! Git accepts anything, and a person with a Git client is allowed to use
//! it. So a Recipe can hold Cooklang that the parser refuses, no
//! `recipe.cook` at all, no published Version, a file above the friendly
//! limit, or two photos where a Recipe holds one.
//!
//! Every one of those states is legitimate. This module names the state, and
//! says what a person can do about it. It corrects nothing: each repair is
//! an action that a person starts, and every diagnosis carries **Open in
//! Forgejo**.
//!
//! The published Recipe lives on `main`. This module reads that one name and
//! never another: a Recipe whose published Version is gone gets a diagnosis
//! and not content that the application guessed.

use crate::create_recipe::MAIN_BRANCH;
use crate::forgejo::{ForgejoError, Repository};
use crate::recipe::{self, MAX_SOURCE_BYTES, Parsed, RECIPE_FILE};
use crate::secret::Secret;
use crate::upload::{self, Photos};
use crate::web::AppState;

/// How many published Versions the search for a valid one reads.
///
/// Each one costs a read of the Recipe at that Version, so the search stops
/// rather than walk a History of years. A Recipe that has been broken for
/// longer than this still has **Open in Forgejo**.
const MAX_VERSIONS_READ: usize = 20;

// Forgejo refuses a larger page of Versions than fifty, so a larger number
// here would ask for more than one answer can carry.
const _: () = assert!(MAX_VERSIONS_READ > 0 && MAX_VERSIONS_READ < 50);

/// Said when the published Version of a Recipe is not there.
const NO_PUBLISHED_MESSAGE: &str = "CookLangHub cannot find the published Version of this Recipe. Somebody changed this Recipe outside CookLangHub. CookLangHub does not select a different Version on its own. To correct this, open the Recipe in Forgejo.";

/// Said when a Recipe holds no `recipe.cook`.
const NO_FILE_MESSAGE: &str = "This Recipe has no recipe.cook file. Somebody removed it outside CookLangHub. CookLangHub does not write a new one on its own. To correct this, restore the last valid Version, or open the Recipe in Forgejo.";

/// Said when the Recipe file is larger than the friendly limit.
const TOO_LARGE_MESSAGE: &str = "This Recipe is larger than 1 MB, and CookLangHub shows a Recipe of that size no further. Every byte of it is safe, and CookLangHub changed nothing. To read it, open the Recipe in Forgejo.";

/// Said when the Recipe file is not text that the application can read.
const NOT_TEXT_MESSAGE: &str = "This Recipe is not UTF-8 text. Each character that CookLangHub could not read appears below as a replacement mark. To see the exact content, open the Recipe in Forgejo.";

/// Said when the parser refuses the Cooklang of a Recipe.
const INVALID_MESSAGE: &str = "CookLangHub cannot read the Cooklang of this Recipe. The messages below say what the parser found. To correct this, edit the Recipe, restore the last valid Version, or open the Recipe in Forgejo.";

/// Said when Forgejo does not answer for a Recipe that is there.
const UNREADABLE_MESSAGE: &str = "CookLangHub cannot read this Recipe at the moment. Nothing changed. Try again later, or open the Recipe in Forgejo.";

/// Said on every diagnosis.
///
/// This is the promise of the whole module, so the person reads it on the
/// page and not only in the documentation.
pub const UNTOUCHED_MESSAGE: &str =
    "CookLangHub corrected nothing. Every repair is an action that you start.";

/// A state of a Recipe that the cooking interface cannot show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// There is no published Version to read.
    NoPublishedVersion,
    /// The published Version holds no `recipe.cook`.
    NoRecipeFile,
    /// The Recipe file is larger than the friendly limit.
    TooLarge,
    /// The Recipe file is not UTF-8 text.
    NotText,
    /// The parser refused the Cooklang. The messages are for the person.
    Invalid(Vec<String>),
    /// Forgejo did not answer. The state itself is unknown.
    Unreadable,
}

impl Problem {
    /// The heading that names this state.
    pub fn heading(&self) -> &'static str {
        match self {
            Self::NoPublishedVersion => "This Recipe has no published Version",
            Self::NoRecipeFile => "This Recipe has no Recipe file",
            Self::TooLarge => "This Recipe is too large to show",
            Self::NotText => "This Recipe is not text",
            Self::Invalid(_) => "This Recipe is broken",
            Self::Unreadable => "This Recipe cannot be read at the moment",
        }
    }

    /// What is wrong, and what a person can do about it.
    pub fn message(&self) -> &'static str {
        match self {
            Self::NoPublishedVersion => NO_PUBLISHED_MESSAGE,
            Self::NoRecipeFile => NO_FILE_MESSAGE,
            Self::TooLarge => TOO_LARGE_MESSAGE,
            Self::NotText => NOT_TEXT_MESSAGE,
            Self::Invalid(_) => INVALID_MESSAGE,
            Self::Unreadable => UNREADABLE_MESSAGE,
        }
    }

    /// What the parser said, when the parser is what refused the Recipe.
    pub fn details(&self) -> &[String] {
        match self {
            Self::Invalid(messages) => messages,
            _ => &[],
        }
    }

    /// Whether the page can offer the source as it is stored.
    ///
    /// A file that is too large is not put on the page, and a Version that
    /// is not there has no source at all.
    pub fn shows_source(&self) -> bool {
        matches!(self, Self::NotText | Self::Invalid(_))
    }

    /// Every state, for a test that has to cover all of them.
    pub fn each() -> Vec<Problem> {
        vec![
            Self::NoPublishedVersion,
            Self::NoRecipeFile,
            Self::TooLarge,
            Self::NotText,
            Self::Invalid(vec!["the parser said this".to_string()]),
            Self::Unreadable,
        ]
    }
}

/// What the Recipe page found when it looked at a Recipe.
pub struct Reading {
    /// The state that stops the page, when there is one.
    pub problem: Option<Problem>,
    /// The Cooklang, as far as the application could read it. Empty when
    /// there is nothing that a person can be shown.
    pub source: String,
    /// What the parser made of the source. `None` when the source never
    /// reached the parser.
    pub parsed: Option<Parsed>,
    /// What photos the Recipe holds.
    pub photos: Photos,
}

impl Reading {
    /// A reading that found one state and nothing else.
    fn of(problem: Problem, photos: Photos) -> Self {
        Self {
            problem: Some(problem),
            source: String::new(),
            parsed: None,
            photos,
        }
    }
}

/// Whether Forgejo answered "there is nothing here".
///
/// Forgejo answers 404 for a name it does not hold, and 409 for a
/// repository that holds nothing at all. Both mean the same to a reader:
/// there is no published Version.
fn is_missing(error: &ForgejoError) -> bool {
    matches!(error, ForgejoError::Status { status, .. } if *status == 404 || *status == 409)
}

/// Read a Recipe, and name the state when the interface cannot show it.
///
/// Nothing here writes. A state that this application cannot handle is
/// diagnosed exactly as it is found.
pub async fn read(
    state: &AppState,
    token: Option<&Secret<String>>,
    owner: &str,
    slug: &str,
    repository: &Repository,
) -> Reading {
    // A Recipe that holds nothing has no published Version to read, and
    // Forgejo needs no question to answer that.
    if repository.empty {
        return Reading::of(Problem::NoPublishedVersion, Photos::None);
    }

    let entries = match state
        .forgejo
        .list_root_entries(token, owner, slug, MAIN_BRANCH)
        .await
    {
        Ok(entries) => entries,
        Err(error) if is_missing(&error) => {
            tracing::info!(%owner, %slug, "this Recipe has no published Version");
            return Reading::of(Problem::NoPublishedVersion, Photos::None);
        }
        Err(error) => {
            tracing::warn!(%error, %owner, %slug, "cannot list the files of a Recipe");
            return Reading::of(Problem::Unreadable, Photos::None);
        }
    };

    // One listing answers both questions: which photos the Recipe holds,
    // and whether the Recipe file is there at all.
    let names: Vec<String> = entries.iter().map(|entry| entry.name.clone()).collect();
    let photos = upload::photos_in(&names);

    let Some(file) = entries.iter().find(|entry| entry.name == RECIPE_FILE) else {
        tracing::info!(%owner, %slug, "this Recipe has no Recipe file");
        return Reading::of(Problem::NoRecipeFile, photos);
    };

    // The size comes from Forgejo, so a file far above the limit is refused
    // without ever being read into memory.
    if file.size > MAX_SOURCE_BYTES as u64 {
        tracing::info!(%owner, %slug, size = file.size, "this Recipe is larger than the friendly limit");
        return Reading::of(Problem::TooLarge, photos);
    }

    let bytes = match state
        .forgejo
        .raw_file(token, owner, slug, MAIN_BRANCH, RECIPE_FILE)
        .await
    {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(%error, %owner, %slug, "cannot read the Recipe file");
            return Reading::of(Problem::Unreadable, photos);
        }
    };

    // A Recipe written through this application is always UTF-8 text. Git
    // accepts any bytes, so a change made outside it can put something else
    // there. Say so plainly instead of showing replacement characters that
    // look like a fault in the Recipe itself.
    if std::str::from_utf8(&bytes).is_err() {
        tracing::info!(%owner, %slug, "the Recipe file is not UTF-8 text");
        return Reading {
            problem: Some(Problem::NotText),
            source: String::from_utf8_lossy(&bytes).into_owned(),
            parsed: None,
            photos,
        };
    }

    let source = String::from_utf8_lossy(&bytes).into_owned();
    let parsed = recipe::parse(&source);

    let problem = if parsed.is_valid() {
        None
    } else {
        tracing::info!(%owner, %slug, errors = parsed.errors.len(), "this Recipe is broken");
        Some(Problem::Invalid(
            parsed.errors.iter().map(|d| d.message.clone()).collect(),
        ))
    };

    Reading {
        problem,
        source,
        parsed: Some(parsed),
        photos,
    }
}

/// One published Version whose Cooklang the parser accepts.
pub struct ValidVersion {
    /// The identifier that the address carries. A person never reads it.
    pub id: String,
    /// The day and the clock that Git recorded, as History writes them.
    pub moment: String,
}

/// Find the newest published Version that the application can read.
///
/// The search starts behind the published Version, because the published
/// Version is the one that cannot be read. It reads no further than
/// [`MAX_VERSIONS_READ`] Versions.
///
/// Nothing is written. The answer is an offer, and the repair happens only
/// when a person asks for it.
pub async fn last_valid_version(
    state: &AppState,
    token: Option<&Secret<String>>,
    owner: &str,
    slug: &str,
) -> Option<ValidVersion> {
    let commits = match state
        .forgejo
        .list_commits(
            token,
            owner,
            slug,
            MAIN_BRANCH,
            // One more than the search reads, because the published Version
            // is the first of them and the search steps over it.
            MAX_VERSIONS_READ as u32 + 1,
        )
        .await
    {
        Ok(commits) => commits,
        Err(error) => {
            tracing::info!(%error, %owner, %slug, "cannot read the published Versions of a broken Recipe");
            return None;
        }
    };

    for commit in commits.iter().skip(1).take(MAX_VERSIONS_READ) {
        let Some(bytes) =
            crate::web_history::source_at(state, token, owner, slug, &commit.sha).await
        else {
            continue;
        };

        let Ok(source) = std::str::from_utf8(&bytes) else {
            continue;
        };

        if !recipe::parse(source).is_valid() {
            continue;
        }

        let written = commit
            .commit
            .author
            .as_ref()
            .map(|identity| identity.date.as_str())
            .unwrap_or_default();

        return Some(ValidVersion {
            id: commit.sha.clone(),
            moment: crate::web_history::moment(written),
        });
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The words of the forge, which no diagnosis may say.
    ///
    /// Whole words only. `Sharing` is an area of a Recipe and `share` is
    /// what a person does with one, so neither may be read as the
    /// identifier that Git uses.
    const FORGE_WORDS: [&str; 11] = [
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
        "push",
    ];

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
    fn every_state_says_what_is_wrong_and_offers_forgejo() {
        for problem in Problem::each() {
            assert!(!problem.heading().is_empty());
            assert!(
                problem.message().contains("Forgejo"),
                "every diagnosis must offer Forgejo: {}",
                problem.message()
            );
        }

        assert!(!UNTOUCHED_MESSAGE.is_empty());
    }

    #[test]
    fn every_message_a_person_reads_uses_cooking_words() {
        for problem in Problem::each() {
            for text in [problem.heading(), problem.message()] {
                assert_eq!(
                    says_forge_word(text),
                    None,
                    "a word of the forge must not reach the person: {text}"
                );
            }
        }

        assert_eq!(says_forge_word(UNTOUCHED_MESSAGE), None);
    }

    #[test]
    fn the_promise_of_the_page_is_that_nothing_was_corrected() {
        // The rule of this module, in the words the person reads.
        assert!(UNTOUCHED_MESSAGE.contains("corrected nothing"));
        assert!(UNTOUCHED_MESSAGE.contains("you start"));
    }

    #[test]
    fn the_parser_messages_travel_with_the_broken_state_and_no_other() {
        let broken = Problem::Invalid(vec!["a timer needs a unit".to_string()]);
        assert_eq!(broken.details(), ["a timer needs a unit"]);

        for problem in Problem::each() {
            if !matches!(problem, Problem::Invalid(_)) {
                assert!(
                    problem.details().is_empty(),
                    "{problem:?} carries no detail"
                );
            }
        }
    }

    #[test]
    fn the_source_is_offered_only_where_there_is_one_to_read() {
        // A file above the limit never reaches the page, and a Version that
        // is not there has no source at all.
        assert!(Problem::NotText.shows_source());
        assert!(Problem::Invalid(Vec::new()).shows_source());

        for problem in [
            Problem::NoPublishedVersion,
            Problem::NoRecipeFile,
            Problem::TooLarge,
            Problem::Unreadable,
        ] {
            assert!(!problem.shows_source(), "{problem:?} has no source to show");
        }
    }

    #[test]
    fn an_empty_answer_from_forgejo_means_there_is_no_published_version() {
        for status in [404, 409] {
            assert!(is_missing(&ForgejoError::Status {
                status,
                body: String::new(),
            }));
        }

        // Anything else is an outage and not a diagnosis of the Recipe.
        assert!(!is_missing(&ForgejoError::Status {
            status: 500,
            body: String::new(),
        }));
        assert!(!is_missing(&ForgejoError::Unreachable(
            "no route".to_string()
        )));
    }
}
