//! Favorite and Notify me.
//!
//! A Favorite is a Forgejo star. Notify me is a Forgejo watch. Forgejo holds
//! both, and this application holds neither. There is no row, no cache, and
//! no count here, so a Favorite that a person adds in Forgejo counts at once
//! and a Favorite that they remove there stops counting at once.
//!
//! Both controls change state, so each one is a POST form and never a link.
//! Neither needs a script, which keeps the page inside the
//! `default-src 'self'` policy.
//!
//! Notify me belongs to a Recipe. Follow updates belongs to a Cookbook. The
//! two are different behaviours and the words for them never mix.

use std::sync::Arc;

use askama::Template;
use axum::Router;
use axum::extract::{Form, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::post;
use axum_extra::extract::CookieJar;
use serde::Deserialize;

use crate::forgejo::{ForgejoClient, ForgejoError, Ownership, Repository, RepositoryQuery};
use crate::index;
use crate::recipe::RECIPE_TOPICS;
use crate::secret::Secret;
use crate::web::{AppState, Layout, MaybeUser};

/// How many repositories the application asks Forgejo for at a time.
const SEARCH_PAGE: u32 = 50;

/// The topic that a search asks Forgejo about.
///
/// Forgejo matches one topic per search, so the search asks for the wider
/// marker and [`index::is_recipe`] then keeps only what carries every one.
const SEARCH_TOPIC: &str = RECIPE_TOPICS[0];

// ------------------------------------------------------------------ state

/// What Forgejo says this person did with one Recipe.
///
/// Both answers come from Forgejo on every page view. Nothing here is
/// stored, so neither answer can be stale.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Marks {
    /// Whether this person made the Recipe a Favorite.
    pub favorite: bool,
    /// Whether this person asked Forgejo to notify them about the Recipe.
    pub notify: bool,
}

/// Ask Forgejo what one person did with one Recipe.
///
/// `None` for the token is an anonymous visitor. A visitor has no Favorites
/// and no notifications, so the page shows neither control to them.
///
/// A Forgejo that does not answer gives the same result as a person who did
/// nothing. The page then offers the action, and the action itself reports
/// any failure to the person.
pub async fn marks(
    forgejo: &ForgejoClient,
    token: Option<&Secret<String>>,
    owner: &str,
    slug: &str,
) -> Marks {
    let Some(token) = token else {
        return Marks::default();
    };

    let favorite = match forgejo.is_starred(token, owner, slug).await {
        Ok(answer) => answer,
        Err(error) => {
            tracing::warn!(%error, %owner, %slug, "cannot ask Forgejo about this Favorite");
            false
        }
    };

    let notify = match forgejo.is_watching(token, owner, slug).await {
        Ok(answer) => answer,
        Err(error) => {
            tracing::warn!(%error, %owner, %slug, "cannot ask Forgejo about these notifications");
            false
        }
    };

    Marks { favorite, notify }
}

// ---------------------------------------------------------------- listings

/// Ask Forgejo which Recipes a person made a Favorite.
///
/// Forgejo holds the list, and this reads it. The second value says whether
/// the answer was cut short at [`index::MAX_REPOSITORIES`].
pub async fn recipes(
    forgejo: &ForgejoClient,
    token: &Secret<String>,
) -> Result<(Vec<Repository>, bool), ForgejoError> {
    let mut found = Vec::new();
    let mut page = 1;

    loop {
        let batch = forgejo
            .starred_repositories(token, page, SEARCH_PAGE)
            .await?;

        let complete = batch.len() < SEARCH_PAGE as usize;

        // A person can make anything in Forgejo a Favorite. Only a Recipe
        // belongs on a list of Recipes.
        found.extend(batch.into_iter().filter(index::is_recipe));

        if complete {
            return Ok((found, false));
        }
        if found.len() >= index::MAX_REPOSITORIES {
            found.truncate(index::MAX_REPOSITORIES);
            return Ok((found, true));
        }
        page += 1;
    }
}

/// Ask Forgejo which Recipes a credential may see, most Favorited first.
///
/// This is [`index::visible`] with the order that Forgejo gives, and Forgejo
/// counts the stars. The application therefore keeps no count and cannot
/// show one that is out of date.
pub async fn most_favorited(
    forgejo: &ForgejoClient,
    token: Option<&Secret<String>>,
    ownership: Ownership,
) -> Result<(Vec<Repository>, bool), ForgejoError> {
    let mut found = Vec::new();
    let mut page = 1;

    loop {
        let batch = forgejo
            .search_repositories_by_stars(
                token,
                &RepositoryQuery {
                    topic: SEARCH_TOPIC,
                    ownership,
                    page,
                    limit: SEARCH_PAGE,
                },
            )
            .await?;

        let complete = batch.len() < SEARCH_PAGE as usize;
        found.extend(batch.into_iter().filter(index::is_recipe));

        if complete {
            return Ok((found, false));
        }
        if found.len() >= index::MAX_REPOSITORIES {
            found.truncate(index::MAX_REPOSITORIES);
            return Ok((found, true));
        }
        page += 1;
    }
}

// ------------------------------------------------------------------ pages

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/recipes/{owner}/{slug}/favorite", post(favorite))
        .route("/recipes/{owner}/{slug}/notify", post(notify))
}

/// What the two forms send.
///
/// The form carries the state that the person asked for, and not the state
/// that the Recipe is in. Two clicks on the same button therefore end in the
/// state the button named, and a page that is a moment out of date cannot
/// turn a Favorite off by accident.
#[derive(Debug, Clone, Deserialize)]
struct Wanted {
    #[serde(default)]
    on: String,
}

impl Wanted {
    fn is_on(&self) -> bool {
        self.on.trim().eq_ignore_ascii_case("yes")
    }
}

async fn favorite(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug)): Path<(String, String)>,
    Form(form): Form<Wanted>,
) -> Response {
    let Some(token) = crate::web::viewer_token(&state, &jar).await else {
        return Redirect::to("/auth/sign-in").into_response();
    };

    let wanted = form.is_on();
    let done = if wanted {
        state.forgejo.star_repository(&token, &owner, &slug).await
    } else {
        state.forgejo.unstar_repository(&token, &owner, &slug).await
    };

    match done {
        Ok(()) => {
            tracing::info!(%owner, %slug, wanted, "a Favorite changed");
            Redirect::to(&format!("/recipes/{owner}/{slug}")).into_response()
        }
        Err(error) => {
            tracing::warn!(%error, %owner, %slug, wanted, "cannot change this Favorite");
            let message = if wanted {
                format!(
                    "Forgejo did not make this Recipe a Favorite: {}. Open the Recipe in Forgejo to make it a Favorite there.",
                    short(&error)
                )
            } else {
                format!(
                    "Forgejo did not remove this Favorite: {}. Open the Recipe in Forgejo to remove it there.",
                    short(&error)
                )
            };
            problem(&state, &headers, current.as_ref(), &owner, &slug, message)
        }
    }
}

async fn notify(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug)): Path<(String, String)>,
    Form(form): Form<Wanted>,
) -> Response {
    let Some(token) = crate::web::viewer_token(&state, &jar).await else {
        return Redirect::to("/auth/sign-in").into_response();
    };

    let wanted = form.is_on();
    let done = if wanted {
        state.forgejo.watch_repository(&token, &owner, &slug).await
    } else {
        state
            .forgejo
            .unwatch_repository(&token, &owner, &slug)
            .await
    };

    match done {
        Ok(()) => {
            tracing::info!(%owner, %slug, wanted, "the notifications for a Recipe changed");
            Redirect::to(&format!("/recipes/{owner}/{slug}")).into_response()
        }
        Err(error) => {
            tracing::warn!(%error, %owner, %slug, wanted, "cannot change these notifications");
            let message = if wanted {
                format!(
                    "Forgejo did not start the notifications for this Recipe: {}. Open the Recipe in Forgejo to start them there.",
                    short(&error)
                )
            } else {
                format!(
                    "Forgejo did not stop the notifications for this Recipe: {}. Open the Recipe in Forgejo to stop them there.",
                    short(&error)
                )
            };
            problem(&state, &headers, current.as_ref(), &owner, &slug, message)
        }
    }
}

#[derive(Template)]
#[template(path = "favorite_problem.html")]
struct ProblemTemplate {
    layout: Layout,
    /// Where the Recipe is, so the person gets back to it.
    back: String,
    message: String,
    /// Offered because the application cannot handle the state itself.
    forgejo_url: String,
}

/// Say what Forgejo refused, and hand the person the tool that can act.
///
/// Nothing changed, so there is nothing to undo. The application never
/// repairs the state on its own.
fn problem(
    state: &AppState,
    headers: &HeaderMap,
    current: Option<&crate::session::CurrentUser>,
    owner: &str,
    slug: &str,
    message: String,
) -> Response {
    let back = format!("/recipes/{owner}/{slug}");

    respond(ProblemTemplate {
        layout: Layout::new(current).on(headers, &back),
        back: back.clone(),
        message,
        forgejo_url: state.forgejo.web_url(&format!("{owner}/{slug}")),
    })
}

/// A short sentence about a Forgejo failure, for a person to read.
///
/// The whole body of a Forgejo answer belongs in the log and not on a page.
fn short(error: &ForgejoError) -> String {
    match error {
        ForgejoError::Status { status, .. } => format!("it answered {status}"),
        other => other.to_string(),
    }
}

fn respond<T: Template>(template: T) -> Response {
    match template.render() {
        Ok(body) => Html(body).into_response(),
        Err(error) => {
            tracing::error!(%error, "cannot render a template");
            (StatusCode::INTERNAL_SERVER_ERROR, "template error").into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wanted(on: &str) -> Wanted {
        Wanted { on: on.to_string() }
    }

    #[test]
    fn the_form_says_which_state_the_person_asked_for() {
        assert!(wanted("yes").is_on());
        assert!(wanted(" Yes ").is_on());
        assert!(!wanted("no").is_on());
    }

    #[test]
    fn a_form_without_the_field_turns_the_state_off() {
        // The form always names the state that the person asked for, so a
        // body without the field is not a request that this page made. Only
        // the word `yes` turns a Favorite on.
        assert!(!wanted("").is_on());
        assert!(!wanted("true").is_on());
        assert!(!wanted("1").is_on());
    }

    #[test]
    fn a_visitor_without_a_credential_has_no_marks() {
        let marks = Marks::default();
        assert!(!marks.favorite);
        assert!(!marks.notify);
    }

    #[test]
    fn a_forgejo_failure_becomes_a_short_sentence() {
        let status = ForgejoError::Status {
            status: 403,
            body: "gto_secret".to_string(),
        };
        assert_eq!(short(&status), "it answered 403");
    }
}
