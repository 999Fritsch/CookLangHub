//! Archive and delete.
//!
//! Archive is the ordinary reversible action. It is one Forgejo setting, and
//! Forgejo is the only place it lives: [`crate::forgejo::Repository::archived`]
//! is read again on every request that could change a Recipe, and this
//! application holds no archived flag of its own.
//!
//! # What Forgejo does with an archived repository
//!
//! This was measured against a real Forgejo 15 and not read out of a
//! document, because the answer is narrower than the word suggests.
//!
//! Forgejo refuses, with 423: writing a file, opening a Discussion, writing
//! a comment in a Discussion or a Suggestion, and accepting a Suggestion.
//!
//! Forgejo allows: closing and reopening a Discussion or a Suggestion,
//! changing visibility, changing the topics, giving and taking access,
//! Favorite, Notify me, and making a Variation of it. It also keeps
//! reporting `push` and `admin` for the person, so the permission answer of
//! Forgejo says nothing at all about the archive. A caller that asks only
//! "can this person write?" gets `true` for an archived Recipe.
//!
//! That last fact is why the refusal lives in one guard over the POST and
//! not in each handler: there is no permission answer that carries it.
//!
//! # What the guard refuses
//!
//! Every POST under one Recipe or one Cookbook, with four exceptions, each
//! of which Forgejo itself still allows and none of which changes the
//! Recipe:
//!
//! * `archive` and everything under it, which is how a person takes the
//!   Recipe out of the archive again, and how they delete it.
//! * `favorite` and `notify`, which are marks that belong to the person.
//! * `variations`, which makes a new Recipe and writes nothing to this one.
//! * `sharing` and everything under it, so that the Owner keeps control of
//!   who can read an archived Recipe.
//!
//! # The impact report
//!
//! Three questions, and Forgejo answers each one separately. None of the
//! three can be answered completely, and the report says so rather than
//! showing a short list that reads as "nothing will break".
//!
//! Measured, again against a real Forgejo: the Owner of a Recipe asks for
//! its Variations and Forgejo answers **200 with an empty list** when the
//! only Variation is private and belongs to somebody else. The Owner of a
//! Recipe asks for a private Cookbook of another person that holds it and
//! Forgejo answers 404. Neither answer says that something was left out.
//!
//! So [`Affected`] carries whether Forgejo answered at all, and the page
//! carries [`PARTIAL_MESSAGE`] over every list.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Response};
use axum_extra::extract::CookieJar;

use crate::forgejo::Repository;
use crate::secret::Secret;
use crate::web::{AppState, MaybeUser};

/// What the badge says for a Recipe or a Cookbook that is archived.
pub const ARCHIVED_LABEL: &str = "Archived";

/// What the badge says for one that is not.
pub const IN_USE_LABEL: &str = "In use";

/// Why a change was refused.
///
/// The message names the state and the one action that lifts it. It says
/// nothing about Forgejo, because a person who reads it has an interface
/// that can do the work.
pub const ARCHIVED_MESSAGE: &str = "This Recipe is archived. An archived Recipe is read-only. To change it, take it out of the archive first.";

/// The same, for a Cookbook.
pub const ARCHIVED_COOKBOOK_MESSAGE: &str = "This Cookbook is archived. An archived Cookbook is read-only. To change it, take it out of the archive first.";

/// What an archived Recipe refuses, in the words a cook reads.
pub const READ_ONLY_MESSAGE: &str = "Nobody can publish a Version of an archived Recipe. Nobody can write in its Discussions and its Suggestions. Everybody who could read it can still read it.";

/// The same, for a Cookbook.
pub const READ_ONLY_COOKBOOK_MESSAGE: &str = "Nobody can add a Recipe to an archived Cookbook, and nobody can remove one. Everybody who could read it can still read it.";

/// What a permanent deletion costs.
pub const DELETE_WARNING: &str = "A deletion is permanent. CookLangHub cannot get this Recipe back, and Forgejo cannot get it back. To keep the Recipe and its History, archive it instead.";

/// The same, for a Cookbook.
pub const DELETE_COOKBOOK_WARNING: &str = "A deletion is permanent. CookLangHub cannot get this Cookbook back, and Forgejo cannot get it back. To keep the Cookbook and its History, archive it instead.";

/// Why the impact report can be short.
///
/// This sentence is the heart of the report. Forgejo answers only for what
/// this person can see, and it gives no sign that it left something out.
pub const PARTIAL_MESSAGE: &str = "This report shows only what Forgejo shows you. A private Cookbook of a different person, or a private Variation of a different person, is not in it.";

/// Shown for one list that Forgejo did not answer for.
pub const UNANSWERED_MESSAGE: &str = "Forgejo did not answer, so CookLangHub cannot show this list. Open the Recipe in Forgejo to see it.";

/// What happens to each Cookbook that holds the Recipe.
pub const COOKBOOKS_MESSAGE: &str = "Each Cookbook below keeps its entry for this Recipe. The entry becomes broken, and it stays visible. CookLangHub repairs none of them.";

/// What happens to each Variation of the Recipe.
pub const VARIATIONS_MESSAGE: &str = "Each Variation below stays. It keeps every word and all of its History. Forgejo stops naming this Recipe as the source of it.";

/// What happens to each open Suggestion.
///
/// Measured against a real Forgejo: a Suggestion lives inside the Recipe,
/// so Forgejo removes it with the Recipe. It is not closed and it is not
/// moved. A Suggestion that came from a copy of the Recipe is the other way
/// round: when that copy goes, Forgejo closes the Suggestion and keeps it.
pub const SUGGESTIONS_MESSAGE: &str = "Forgejo holds each Suggestion inside the Recipe, so it deletes each Suggestion below with the Recipe. CookLangHub cannot keep them.";

/// What happens to each Recipe of a Cookbook that is deleted.
pub const COOKBOOK_RECIPES_MESSAGE: &str = "Each Recipe below stays. A Cookbook holds a Recipe by reference, so deleting the Cookbook deletes no Recipe.";

/// How many Suggestions the report reads.
const SUGGESTION_WINDOW: u32 = 50;

// --------------------------------------------------------------- the guard

/// Which object a path belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Recipe,
    Cookbook,
}

impl Kind {
    /// The message for a change that this kind refuses while it is archived.
    pub fn archived_message(self) -> &'static str {
        match self {
            Self::Recipe => ARCHIVED_MESSAGE,
            Self::Cookbook => ARCHIVED_COOKBOOK_MESSAGE,
        }
    }

    /// The first part of an address of this kind.
    pub fn area(self) -> &'static str {
        match self {
            Self::Recipe => "recipes",
            Self::Cookbook => "cookbooks",
        }
    }
}

/// What a POST under one object is allowed to do while it is archived.
///
/// Each of these is something Forgejo itself still allows on an archived
/// repository, and none of them changes the Recipe or the Cookbook. See the
/// module documentation for the measurements behind the list.
const ALWAYS_ALLOWED: [&str; 5] = ["archive", "favorite", "notify", "variations", "sharing"];

/// The object that a POST would change, when the path names one.
///
/// The address of every page of one object begins `/recipes/{owner}/{slug}`
/// or `/cookbooks/{owner}/{slug}`, so the object is read out of the address
/// and never out of a route table. A path with nothing after the name of the
/// object changes nothing, and a path in [`ALWAYS_ALLOWED`] is one that
/// Forgejo allows while the repository is archived.
///
/// Pure, so that the whole list of exceptions is a unit test and not a
/// container.
pub fn guarded(path: &str) -> Option<(Kind, String, String)> {
    let mut parts = path.split('/').filter(|part| !part.is_empty());

    let kind = match parts.next()? {
        "recipes" => Kind::Recipe,
        "cookbooks" => Kind::Cookbook,
        _ => return None,
    };

    let owner = parts.next()?.to_string();
    let slug = parts.next()?.to_string();

    // The rest of the address says what the POST would do. Nothing after the
    // object is not a change to it.
    let action = parts.next()?;
    if ALWAYS_ALLOWED.contains(&action) {
        // `variations` makes a new Recipe. `variations/update` writes to
        // this one, so only the bare address is allowed.
        if action == "variations" && parts.next().is_some() {
            return Some((kind, owner, slug));
        }
        return None;
    }

    Some((kind, owner, slug))
}

/// Refuse every change to an archived Recipe or Cookbook.
///
/// This runs over the POST and not over the page that draws the button, so
/// a form that a person kept open, or a request that never saw a page at
/// all, is refused exactly the same.
///
/// Forgejo is asked on each request, because Forgejo owns the state. When
/// Forgejo does not answer, the request goes on: an outage must never turn
/// into a diagnosis about a Recipe, and the handler behind this gives the
/// person the real reason.
pub async fn read_only(
    State(state): State<Arc<AppState>>,
    MaybeUser(current): MaybeUser,
    jar: CookieJar,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Response {
    if request.method() != axum::http::Method::POST {
        return next.run(request).await;
    }

    let Some((kind, owner, slug)) = guarded(request.uri().path()) else {
        return next.run(request).await;
    };

    // The credential comes from the one place that renews, the same as for
    // a page. This guard runs to the end before the handler starts, so the
    // two calls are one after the other and never two renewals of the same
    // one-use token at once. A private Recipe needs the credential: without
    // it Forgejo answers for an anonymous visitor and reports nothing.
    let token = crate::web::viewer_token(&state, &jar).await;
    let Ok(repository) = state
        .forgejo
        .repository_as(token.as_ref(), &owner, &slug)
        .await
    else {
        return next.run(request).await;
    };

    if !repository.archived {
        return next.run(request).await;
    }

    tracing::info!(%owner, %slug, path = %request.uri().path(), "refused a change to an archived repository");

    let page = crate::web_archive::refusal(
        &state,
        current.as_ref(),
        &headers,
        kind,
        &owner,
        &slug,
        &repository,
    );

    // 423 is the answer Forgejo gives for the same state, and this refusal
    // is the same refusal. It is not 403: this person may well have every
    // permission Forgejo hands out, and telling them they have none would
    // be false.
    (StatusCode::LOCKED, Html(page)).into_response()
}

// -------------------------------------------------------- the impact report

/// One answer of Forgejo, and whether Forgejo gave it.
///
/// An empty list and no answer at all are different facts, and a person who
/// is about to delete something needs to know which one they are reading.
#[derive(Debug, Clone)]
pub struct Affected<T> {
    /// Whether Forgejo answered the question.
    pub answered: bool,
    pub items: Vec<T>,
}

impl<T> Affected<T> {
    pub fn answered(items: Vec<T>) -> Self {
        Self {
            answered: true,
            items,
        }
    }

    pub fn unanswered() -> Self {
        Self {
            answered: false,
            items: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// One Variation, as the report names it.
#[derive(Debug, Clone)]
pub struct Named {
    pub owner: String,
    pub slug: String,
    pub title: String,
}

/// One open Suggestion, as the report names it.
#[derive(Debug, Clone)]
pub struct OpenSuggestion {
    pub number: i64,
    pub title: String,
    pub author: String,
}

/// What a permanent deletion of one Recipe reaches.
#[derive(Debug, Clone)]
pub struct Impact {
    pub cookbooks: Affected<crate::cookbook::Named>,
    pub variations: Affected<Named>,
    pub suggestions: Affected<OpenSuggestion>,
}

/// Read what a deletion of this Recipe would reach.
///
/// Three questions, asked of Forgejo one at a time. Nothing here is kept and
/// nothing is worked out from a list that this application holds.
pub async fn impact_of(
    state: &AppState,
    token: &Secret<String>,
    owner: &str,
    slug: &str,
) -> Impact {
    let cookbooks = match crate::cookbook::cookbooks_with(
        &state.pool,
        &state.forgejo,
        token,
        owner,
        slug,
    )
    .await
    {
        Some(found) => Affected::answered(found),
        None => Affected::unanswered(),
    };

    // The list that the Variations page shows is the list that the report
    // must show, so it comes from the same place. A Variation that this
    // person cannot read is not in it, and Forgejo gives no sign of that,
    // which is what PARTIAL_MESSAGE says on the page.
    let made = crate::variation::variations_of(&state.forgejo, Some(token), owner, slug).await;

    let variations = if made.answered {
        // The index holds the title that a cook gave each Variation. Forgejo
        // named these Recipes first, so the index only supplies the words.
        let entries =
            crate::index::entries(&state.pool, &state.forgejo, Some(token), &made.recipes).await;

        Affected::answered(
            entries
                .into_iter()
                .map(|entry| Named {
                    owner: entry.owner,
                    slug: entry.slug,
                    title: entry.title,
                })
                .collect(),
        )
    } else {
        Affected::unanswered()
    };

    let suggestions = match state
        .forgejo
        .list_pull_requests(Some(token), owner, slug, "open", SUGGESTION_WINDOW)
        .await
    {
        Ok(found) => Affected::answered(
            found
                .iter()
                .map(|pull| OpenSuggestion {
                    number: pull.number,
                    title: crate::suggestion::plain_title(&pull.title).to_string(),
                    author: pull.author().to_string(),
                })
                .collect(),
        ),
        Err(error) => {
            tracing::warn!(%error, %owner, %slug, "cannot read the Suggestions of this Recipe");
            Affected::unanswered()
        }
    };

    Impact {
        cookbooks,
        variations,
        suggestions,
    }
}

/// The title of a Recipe, for a page that has only its address.
pub async fn recipe_title(
    state: &AppState,
    token: Option<&Secret<String>>,
    owner: &str,
    slug: &str,
    repository: &Repository,
) -> String {
    state
        .forgejo
        .raw_file(
            token,
            owner,
            slug,
            repository.branch(),
            crate::recipe::RECIPE_FILE,
        )
        .await
        .ok()
        .and_then(|bytes| crate::recipe::parse(&String::from_utf8_lossy(&bytes)).title)
        .unwrap_or_else(|| repository.name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_change_to_one_recipe_is_guarded() {
        for path in [
            "/recipes/sam/chili/edit",
            "/recipes/sam/chili/draft",
            "/recipes/sam/chili/draft/discard",
            "/recipes/sam/chili/thumbnail",
            "/recipes/sam/chili/discussions",
            "/recipes/sam/chili/discussions/1/comments",
            "/recipes/sam/chili/discussions/1/state",
            "/recipes/sam/chili/history/abc/restore",
            "/recipes/sam/chili/suggest",
            "/recipes/sam/chili/suggest/save",
            "/recipes/sam/chili/variations/update",
        ] {
            assert_eq!(
                guarded(path),
                Some((Kind::Recipe, "sam".to_string(), "chili".to_string())),
                "`{path}` changes the Recipe and must be guarded"
            );
        }
    }

    #[test]
    fn every_change_to_one_cookbook_is_guarded() {
        for path in [
            "/cookbooks/sam/winter/recipes",
            "/cookbooks/sam/winter/recipes/remove",
            "/cookbooks/sam/winter/recipes/holding",
        ] {
            assert_eq!(
                guarded(path),
                Some((Kind::Cookbook, "sam".to_string(), "winter".to_string())),
                "`{path}` changes the Cookbook and must be guarded"
            );
        }
    }

    #[test]
    fn what_forgejo_still_allows_is_not_guarded() {
        // Each of these was measured against a real Forgejo on an archived
        // repository, and Forgejo allowed each one. None of them changes the
        // Recipe. See the module documentation.
        for path in [
            "/recipes/sam/chili/favorite",
            "/recipes/sam/chili/notify",
            "/recipes/sam/chili/variations",
            "/recipes/sam/chili/sharing/visibility",
            "/recipes/sam/chili/sharing/people",
            "/recipes/sam/chili/sharing/people/remove",
            "/recipes/sam/chili/archive/state",
            "/recipes/sam/chili/archive/delete",
            "/cookbooks/sam/winter/sharing/visibility",
            "/cookbooks/sam/winter/archive/state",
            "/cookbooks/sam/winter/archive/delete",
        ] {
            assert_eq!(guarded(path), None, "`{path}` must stay allowed");
        }
    }

    #[test]
    fn a_path_that_names_no_recipe_is_not_guarded() {
        for path in [
            "/",
            "/recipes/new",
            "/recipes/preview",
            "/recipes/sam/chili",
            "/cookbooks/new",
            "/cookbooks/sam/winter",
            "/preferences/theme",
            "/auth/sign-out",
            "/admin/index/rebuild",
        ] {
            assert_eq!(guarded(path), None, "`{path}` names no change to an object");
        }
    }

    #[test]
    fn an_answer_that_is_empty_is_not_an_answer_that_is_missing() {
        let empty: Affected<Named> = Affected::answered(Vec::new());
        let missing: Affected<Named> = Affected::unanswered();

        assert!(empty.is_empty());
        assert!(missing.is_empty());
        assert!(empty.answered, "Forgejo said that there are none");
        assert!(!missing.answered, "Forgejo said nothing at all");
    }

    #[test]
    fn the_report_says_what_it_cannot_see() {
        assert!(PARTIAL_MESSAGE.contains("private Cookbook"));
        assert!(PARTIAL_MESSAGE.contains("private Variation"));
    }

    #[test]
    fn the_words_of_the_forge_stay_out_of_every_message() {
        for message in [
            ARCHIVED_MESSAGE,
            ARCHIVED_COOKBOOK_MESSAGE,
            READ_ONLY_MESSAGE,
            READ_ONLY_COOKBOOK_MESSAGE,
            DELETE_WARNING,
            DELETE_COOKBOOK_WARNING,
            PARTIAL_MESSAGE,
            UNANSWERED_MESSAGE,
            COOKBOOKS_MESSAGE,
            VARIATIONS_MESSAGE,
            SUGGESTIONS_MESSAGE,
            COOKBOOK_RECIPES_MESSAGE,
        ] {
            let words: Vec<String> = message
                .to_lowercase()
                .split(|c: char| !c.is_alphanumeric())
                .filter(|word| !word.is_empty())
                .map(str::to_string)
                .collect();

            for forge_word in [
                "commit",
                "commits",
                "branch",
                "branches",
                "diff",
                "repository",
                "repo",
                "fork",
                "patch",
                "head",
                "sha",
                "merge",
                "rebase",
                "git",
                "checkout",
                "revert",
            ] {
                assert!(
                    !words.iter().any(|word| word == forge_word),
                    "`{message}` says `{forge_word}` to a cook"
                );
            }
        }
    }
}
