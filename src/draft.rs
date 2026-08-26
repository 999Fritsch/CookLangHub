//! Drafts: the work a person has not published yet.
//!
//! A draft lives in Forgejo and nowhere else. The browser keeps nothing, so
//! a person can close the tab, pick up a telephone, and carry on with the
//! same text. That is the whole point of this module, and it is why the
//! editor posts every change here instead of writing it to the machine it
//! is running on.
//!
//! Underneath, a draft is one Version on a branch of its own, built on the
//! published Version the person started from. It never reaches the
//! published branch, so History shows published Versions only. One person
//! has one branch for one Recipe, which is what gives them one draft.
//!
//! A save carries the draft Version it started from. When the stored draft
//! has moved on, the save is refused and the person is told why. The
//! application never joins two drafts together and never picks a winner.
//!
//! Nothing here removes a draft on a timer. A draft goes when the person
//! publishes it or discards it, and at no other moment.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Form;
use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::post;
use axum_extra::extract::CookieJar;
use serde::{Deserialize, Serialize};

use crate::create_recipe::{self, MAIN_BRANCH};
use crate::git::{DraftState, GitError, Identity, SaveDraft};
use crate::recipe::{MAX_SOURCE_BYTES, RECIPE_FILE};
use crate::secret::Secret;
use crate::web::{AppState, Layout, MaybeUser};
use crate::web_edit::{NO_WRITE_MESSAGE, Refused, refusal};
use crate::web_recipes::{Actor, actor};

/// Where a draft lives. One person has one draft for one Recipe, so the
/// name carries the person and nothing else.
const DRAFT_PREFIX: &str = "draft/";

/// The description that every draft Version carries.
///
/// It is never read by a person: publishing writes a Version of its own,
/// with the change note the person wrote.
const DRAFT_MESSAGE: &str = "Draft";

/// The longest login this application will build a draft name from.
const MAX_LOGIN_CHARS: usize = 64;

/// Shown while a person writes, after each save.
pub const SAVED_MESSAGE: &str = "CookLangHub saved your draft.";

/// Shown when a second tab, or another device, saved a newer draft.
///
/// The application refuses the save and says so. It never joins the two
/// texts, and it never lets one of them replace the other quietly.
pub const STALE_MESSAGE: &str = "CookLangHub did not save this text. Your draft changed in a different tab or on a different device. Copy your text. Then open the Recipe again to get the newest draft.";

/// Shown when the save did not reach Forgejo.
pub const UNSAVED_MESSAGE: &str =
    "CookLangHub cannot save your draft at the moment. Your text is still on this page. Try again.";

/// Shown on the editor while a draft is open.
pub const NOTICE_MESSAGE: &str = "This is your draft. CookLangHub saves it while you write. It is not part of the Recipe until you publish it.";

/// Shown when this application cannot make a draft name for this person.
pub const NO_DRAFT_MESSAGE: &str = "CookLangHub cannot keep a draft for this Recipe. Your text is still on this page. To keep your work, publish a Version.";

/// Shown when the draft could not be removed.
pub const NOT_DISCARDED_MESSAGE: &str =
    "CookLangHub cannot remove your draft at the moment. The draft is still there. Try again.";

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/recipes/{owner}/{slug}/draft", post(save))
        .route("/recipes/{owner}/{slug}/draft/discard", post(discard))
}

/// The branch that holds the draft of one person.
///
/// Gives `None` when the login cannot become a safe name. Forgejo does not
/// hand out a login like that, but the value arrives from outside this
/// application, so it is checked rather than trusted.
pub fn branch(login: &str) -> Option<String> {
    if !is_safe_login(login) {
        return None;
    }
    Some(format!("{DRAFT_PREFIX}{login}"))
}

/// Whether a login can become part of a name that Git accepts.
fn is_safe_login(login: &str) -> bool {
    if login.is_empty() || login.chars().count() > MAX_LOGIN_CHARS {
        return false;
    }

    if !login
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return false;
    }

    // A leading dash reads as an option to a command. A dot at either end,
    // a doubled dot, and the `.lock` ending are names that Git refuses.
    !(login.starts_with('-')
        || login.starts_with('.')
        || login.ends_with('.')
        || login.contains("..")
        || login.ends_with(".lock"))
}

/// Whether a value is the identifier of a Version.
///
/// The editor sends these back, so they arrive from outside. A value that
/// is not a plain identifier could read as an option to a command, so only
/// the shape Git writes is accepted.
pub fn is_version(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.chars().all(|c| c.is_ascii_hexdigit())
}

/// Read the draft of this person, when they have one.
pub async fn read(
    state: &AppState,
    token: &Secret<String>,
    owner: &str,
    slug: &str,
    login: &str,
) -> Result<Option<DraftState>, GitError> {
    let Some(branch) = branch(login) else {
        tracing::warn!(%login, "this login cannot carry a draft");
        return Ok(None);
    };

    let remote = state.forgejo.git_url(&format!("{owner}/{slug}"));
    state.git.draft_state(&remote, token, &branch).await
}

/// Remove the draft of this person.
///
/// A draft that is not there is the state that was asked for, so this
/// succeeds. Nothing else in the application ever calls it: a draft goes
/// when a person publishes it or discards it.
pub async fn remove(
    state: &AppState,
    token: &Secret<String>,
    owner: &str,
    slug: &str,
    login: &str,
) -> Result<(), GitError> {
    let Some(branch) = branch(login) else {
        return Ok(());
    };

    let remote = state.forgejo.git_url(&format!("{owner}/{slug}"));
    state.git.remove_branch(&remote, token, &branch).await
}

/// What the editor sends when it saves.
#[derive(Debug, Deserialize)]
struct SaveForm {
    #[serde(default)]
    source: String,
    /// The published Version the draft is built on.
    #[serde(default)]
    base_version: String,
    /// The draft Version this tab started from. Empty when the person had
    /// no draft when the page opened.
    #[serde(default)]
    draft_version: String,
}

/// What the editor reads back from a save.
#[derive(Debug, Serialize)]
struct Answer {
    /// The draft Version to send with the next save.
    version: String,
    /// The words for the person.
    message: String,
}

fn answer(status: StatusCode, version: &str, message: &str) -> Response {
    (
        status,
        Json(Answer {
            version: version.to_string(),
            message: message.to_string(),
        }),
    )
        .into_response()
}

/// Check that this person may write to this Recipe.
///
/// Forgejo decides. The application reads the permission that Forgejo
/// computed for this credential and never works it out for itself.
async fn writable(
    state: &AppState,
    actor: &Actor,
    owner: &str,
    slug: &str,
) -> Result<crate::forgejo::Repository, StatusCode> {
    let repository = match state.forgejo.repository(&actor.token, owner, slug).await {
        Ok(repository) => repository,
        Err(error) => {
            tracing::info!(%error, %owner, %slug, "cannot read the Recipe repository");
            return Err(StatusCode::NOT_FOUND);
        }
    };

    match state.forgejo.can_write(&actor.token, owner, slug).await {
        Ok(true) => Ok(repository),
        Ok(false) => Err(StatusCode::FORBIDDEN),
        Err(error) => {
            tracing::warn!(%error, %owner, %slug, "cannot read the permissions of this person");
            Err(StatusCode::FORBIDDEN)
        }
    }
}

/// Save the draft.
///
/// The editor posts here while a person writes. Nothing is kept in the
/// browser, so the answer carries the draft Version that the next save must
/// send back. That value is the whole of the stale check: when the stored
/// draft no longer holds it, somebody else wrote, and this save is refused.
async fn save(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
    Path((owner, slug)): Path<(String, String)>,
    Form(form): Form<SaveForm>,
) -> Response {
    let Some(actor) = actor(&state, &jar).await else {
        return answer(
            StatusCode::UNAUTHORIZED,
            "",
            "Sign in again to save your draft.",
        );
    };

    if !is_version(&form.base_version) {
        return answer(
            StatusCode::BAD_REQUEST,
            "",
            "CookLangHub does not know which Version you started from. Open the Recipe again.",
        );
    }

    let expected = form.draft_version.trim();
    if !expected.is_empty() && !is_version(expected) {
        return answer(StatusCode::BAD_REQUEST, "", STALE_MESSAGE);
    }

    if form.source.len() > MAX_SOURCE_BYTES {
        return answer(
            StatusCode::PAYLOAD_TOO_LARGE,
            expected,
            "The Recipe source is larger than 1 MB, so CookLangHub did not save your draft.",
        );
    }

    let repository = match writable(&state, &actor, &owner, &slug).await {
        Ok(repository) => repository,
        Err(StatusCode::NOT_FOUND) => {
            return answer(
                StatusCode::NOT_FOUND,
                expected,
                "This Recipe is not available.",
            );
        }
        Err(status) => return answer(status, expected, NO_WRITE_MESSAGE),
    };

    let Some(branch) = branch(&actor.user.login) else {
        tracing::warn!(login = %actor.user.login, "this login cannot carry a draft");
        return answer(StatusCode::CONFLICT, "", NO_DRAFT_MESSAGE);
    };

    let published_branch = if repository.default_branch.is_empty() {
        MAIN_BRANCH.to_string()
    } else {
        repository.default_branch.clone()
    };

    // A draft never becomes History, and it is replaced whole on the next
    // keystroke, so it carries the no-reply address always. A draft is
    // readable by anybody who can read the Recipe, and an address that is
    // published by accident cannot be taken back.
    let identity = Identity {
        name: actor.user.display_name().to_string(),
        email: create_recipe::commit_email(
            &actor.user.login,
            &actor.user.email,
            true,
            &state.forgejo_noreply_domain,
        ),
    };

    let mut files = BTreeMap::new();
    files.insert(
        RECIPE_FILE.to_string(),
        normalize(&form.source).into_bytes(),
    );

    let saved = state
        .git
        .save_draft(SaveDraft {
            remote_url: &state.forgejo.git_url(&format!("{owner}/{slug}")),
            token: &actor.token,
            identity: &identity,
            published_branch: &published_branch,
            branch: &branch,
            message: DRAFT_MESSAGE,
            base_version: form.base_version.trim(),
            expected: (!expected.is_empty()).then_some(expected),
            files,
        })
        .await;

    match saved {
        Ok(version) => answer(StatusCode::OK, &version, SAVED_MESSAGE),
        // Somebody wrote first. The stored draft keeps what it had, and
        // the text on this page is not lost: it stays on the page.
        Err(GitError::Stale) => {
            tracing::info!(%owner, %slug, login = %actor.user.login, "a stale draft save was refused");
            answer(StatusCode::CONFLICT, expected, STALE_MESSAGE)
        }
        Err(error) => {
            tracing::warn!(%error, %owner, %slug, "cannot save a draft");
            answer(StatusCode::BAD_GATEWAY, expected, UNSAVED_MESSAGE)
        }
    }
}

/// Discard the draft.
///
/// This is a form, so it works with no script at all. It is one of the two
/// ways a draft ever goes, and the person asks for it every time.
async fn discard(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug)): Path<(String, String)>,
) -> Response {
    let here = format!("/recipes/{owner}/{slug}/edit");
    let layout = || Layout::new(current.as_ref()).on(&headers, &here);

    let Some(actor) = actor(&state, &jar).await else {
        return Redirect::to("/auth/sign-in").into_response();
    };

    let repository = match writable(&state, &actor, &owner, &slug).await {
        Ok(repository) => repository,
        Err(StatusCode::NOT_FOUND) => {
            return refusal(layout(), &owner, &slug, Refused::Missing);
        }
        Err(status) => {
            return refusal(
                layout(),
                &owner,
                &slug,
                Refused::Blocked {
                    status,
                    message: NO_WRITE_MESSAGE,
                    forgejo_url: state.forgejo.web_url(&format!("{owner}/{slug}")),
                },
            );
        }
    };

    match remove(&state, &actor.token, &owner, &slug, &actor.user.login).await {
        Ok(()) => {
            tracing::info!(%owner, %slug, login = %actor.user.login, "a draft was discarded");
            Redirect::to(&here).into_response()
        }
        // The draft is still there. Say so, and offer the tool that can
        // act on it. Nothing here repairs the state quietly.
        Err(error) => {
            tracing::warn!(%error, %owner, %slug, "cannot discard a draft");
            refusal(
                layout(),
                &owner,
                &slug,
                Refused::Blocked {
                    status: StatusCode::BAD_GATEWAY,
                    message: NOT_DISCARDED_MESSAGE,
                    forgejo_url: state.forgejo.web_url(&repository.full_name),
                },
            )
        }
    }
}

/// Make the text the browser sent into the text the person typed.
///
/// A form sends every line break as CR LF. Keeping them would make the
/// draft differ from the published Recipe on every line, so they come out
/// again, exactly as they do when a person publishes.
fn normalize(source: &str) -> String {
    source.replace("\r\n", "\n").replace('\r', "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_person_gets_one_draft_name_for_one_recipe() {
        // The name carries the person and nothing else, so a second draft
        // for the same Recipe cannot exist.
        assert_eq!(branch("sam"), Some("draft/sam".to_string()));
        assert_eq!(branch("sam"), branch("sam"));
        assert_ne!(branch("sam"), branch("kim"));
        assert_eq!(branch("a-b_c.d"), Some("draft/a-b_c.d".to_string()));
    }

    #[test]
    fn a_login_that_git_would_refuse_gets_no_draft_name() {
        for login in [
            "",
            "   ",
            "-sam",
            ".sam",
            "sam.",
            "sa..m",
            "sam.lock",
            "sam/kim",
            "sam kim",
            "sam~1",
            "sam^",
            "sam:kim",
            "sam?",
            "sam*",
            "sam[1]",
            "sam\\kim",
            "sam\u{7f}",
            "../../etc",
            "--upload-pack=touch",
        ] {
            assert!(branch(login).is_none(), "`{login}` must not name a draft");
        }

        assert!(branch(&"a".repeat(MAX_LOGIN_CHARS + 1)).is_none());
    }

    #[test]
    fn only_the_shape_git_writes_counts_as_a_version() {
        assert!(is_version(&"a".repeat(40)));
        assert!(is_version(&"0".repeat(64)));
        assert!(is_version("0123456789abcdef0123456789abcdef01234567"));
    }

    #[test]
    fn a_version_that_could_read_as_an_option_is_refused() {
        for value in [
            "",
            "   ",
            "main",
            "HEAD",
            "--upload-pack=touch",
            "-x",
            &"a".repeat(39),
            &"a".repeat(41),
            &"g".repeat(40),
            "0123456789abcdef0123456789abcdef0123456 ",
        ] {
            assert!(!is_version(value), "`{value}` must not pass as a Version");
        }
    }

    #[test]
    fn the_carriage_returns_a_browser_adds_come_out_again() {
        assert_eq!(normalize("a\r\nb\r\n"), "a\nb\n");
        assert_eq!(normalize("a\rb"), "a\nb");
        let clean = "---\ntitle: Chili\n---\n\nChop the @onion{1}.\n";
        assert_eq!(normalize(clean), clean);
    }

    #[test]
    fn every_message_a_person_reads_uses_cooking_words() {
        // Draft, Version, Recipe. Never branch, commit, or push.
        for message in [
            SAVED_MESSAGE,
            STALE_MESSAGE,
            UNSAVED_MESSAGE,
            NOTICE_MESSAGE,
            NO_DRAFT_MESSAGE,
            NOT_DISCARDED_MESSAGE,
            NO_WRITE_MESSAGE,
        ] {
            let lower = message.to_lowercase();
            for word in [
                "branch",
                "commit",
                "push",
                "fork",
                "pull request",
                "merge",
                "rebase",
                "git",
            ] {
                assert!(
                    !lower.contains(word),
                    "`{word}` must not reach the person: {message}"
                );
            }
        }
    }

    #[test]
    fn the_editor_writes_nothing_into_the_browser() {
        // This is the rule the whole module rests on. A draft that lived in
        // the browser would be lost the moment a person picked up another
        // device, and every other promise here would go with it.
        //
        // Both files are checked: the one a person edits, and the one the
        // browser is served.
        for editor in [
            include_str!("../static/js/src/editor.js"),
            include_str!("../static/js/editor.js"),
        ] {
            for store in [
                "localStorage",
                "sessionStorage",
                "indexedDB",
                "openDatabase",
                "document.cookie",
            ] {
                assert!(
                    !editor.contains(store),
                    "`{store}` must not reach the editor: the draft lives in Forgejo"
                );
            }
        }
    }

    #[test]
    fn the_refusal_says_what_happened_and_what_to_do() {
        // A person who loses a save has to learn three things: that nothing
        // was saved, why, and what they can do next.
        let lower = STALE_MESSAGE.to_lowercase();
        assert!(lower.contains("did not save"));
        assert!(lower.contains("different tab"));
        assert!(lower.contains("copy your text"));
    }
}
