//! The profile of one cook.
//!
//! A profile is a view of Forgejo and nothing else. The application keeps
//! no profile record: the name, the picture, and the Recipes all come from
//! Forgejo while the page is built.
//!
//! Two permission questions decide the whole page, and Forgejo answers both.
//!
//! 1. May this person be seen at all? Forgejo answers 404 for a profile
//!    that its visibility setting hides from the asker, and an asker with
//!    no credential is a visitor. A limited profile therefore has no page
//!    for a visitor, and a private profile has none for almost anybody. The
//!    application never decides this for itself, and it never says a name
//!    or shows a picture that Forgejo would not give it.
//! 2. Which Recipes may be seen? Forgejo answers that too, with the
//!    credential of the person who is looking. A Recipe that Forgejo does
//!    not name never reaches this page, so a private Recipe of somebody
//!    else cannot appear here.
//!
//! The picture comes from this application and not from Forgejo, because
//! the Content Security Policy allows an image from this origin only.

use std::sync::Arc;

use askama::Template;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum_extra::extract::CookieJar;

use crate::forgejo::{ForgejoClient, ForgejoError, Ownership, Repository, RepositoryQuery};
use crate::index;
use crate::secret::Secret;
use crate::web::{AppState, Layout, MaybeUser, RecipeCard};

/// The topics that mark a Cookbook repository.
///
/// Cookbooks own this now, so it is read from there rather than written
/// twice. Two copies of a marker agree until the day one of them changes.
use crate::cookbook::COOKBOOK_TOPICS;

/// The topic that one search asks Forgejo about.
///
/// Forgejo matches one topic per search, and a Recipe and a Cookbook share
/// this one, so a single search finds both and the application then splits
/// the answer.
const SEARCH_TOPIC: &str = COOKBOOK_TOPICS[0];

/// How many repositories the application asks Forgejo for at a time.
const SEARCH_PAGE: u32 = 50;

/// The most repositories that one profile page covers.
///
/// The page shows every one of them. A second, smaller cap would make the
/// two counts in the heading disagree with the two lists below them, and a
/// count that a person cannot check is worse than a long page. The bound
/// keeps one page view to a small, fixed number of requests, and a profile
/// that reaches it says so.
const MAX_LISTED: usize = 200;

/// Shown when Forgejo cannot answer. The page shows nothing because nothing
/// is known, and not because this cook has no Recipes. One message covers
/// every list, so that no page says a softer thing than another.
const NO_FORGEJO: &str = crate::outage::LIST_MESSAGE;

/// Shown when a profile holds more than one page shows.
const TOO_MANY: &str = "This cook has more Recipes and Cookbooks than one page shows. The ones that changed last come first.";

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/cooks/{login}", get(show))
        .route("/cooks/{login}/avatar", get(avatar))
}

/// Where the profile of one cook lives.
///
/// Every **Owned by** line can become a link with this, so the address is
/// written once.
pub fn address(login: &str) -> String {
    format!("/cooks/{login}")
}

/// Who a profile is about.
pub struct Person {
    pub login: String,
    /// The name to show. Forgejo lets the full name be empty, and the login
    /// stands in for it then.
    pub name: String,
    /// Whether this application can show the picture of this cook.
    pub avatar: bool,
    /// Where Forgejo shows this cook.
    pub url: String,
}

/// One Cookbook on a profile.
///
/// A Cookbook card carries the same **Owned by** line as a Recipe card. It
/// opens in Forgejo, because CookLangHub has no Cookbook page yet.
pub struct CookbookCard {
    pub owner: String,
    pub name: String,
    pub private: bool,
    /// Where Forgejo shows this Cookbook.
    pub url: String,
}

#[derive(Template)]
#[template(path = "profile.html")]
struct ProfileTemplate {
    layout: Layout,
    /// The Forgejo of this installation, for the **Open in Forgejo** link
    /// that a notice carries.
    forgejo_url: String,
    /// Who this page is about, when Forgejo shows them at all.
    person: Option<Person>,
    /// Why the page holds no profile, when it holds none.
    message: Option<String>,
    /// The Recipes of this cook that the person looking may see. The name
    /// is the one that `browse_cards.html` reads.
    recipes: Vec<RecipeCard>,
    notice: Option<String>,
    empty: String,
    cookbooks: Vec<CookbookCard>,
    cookbook_notice: Option<String>,
    cookbook_empty: String,
}

impl ProfileTemplate {
    /// The page that carries no profile: a diagnosis and Forgejo.
    fn refusal(layout: Layout, forgejo_url: String, message: String) -> Self {
        Self {
            layout,
            forgejo_url,
            person: None,
            message: Some(message),
            recipes: Vec::new(),
            notice: None,
            empty: String::new(),
            cookbooks: Vec::new(),
            cookbook_notice: None,
            cookbook_empty: String::new(),
        }
    }
}

/// Show the profile of one cook.
///
/// Forgejo is asked about the person first. It answers 404 for a profile
/// that it hides from the asker, and this page then says only that, so a
/// hidden profile gives away no name, no picture, and no count.
async fn show(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(viewer): MaybeUser,
    Path(login): Path<String>,
) -> Response {
    let layout = Layout::new(viewer.as_ref()).on(&headers, &address(&login));
    let forgejo_url = state.forgejo.public_url().to_string();

    // The credential of the person who is looking. `None` is a visitor, and
    // Forgejo then answers every question below as it answers anybody with
    // no account.
    let token = crate::web::viewer_token(&state, &jar).await;

    let person = match state.forgejo.user_as(token.as_ref(), &login).await {
        Ok(person) => person,
        Err(ForgejoError::Status { status: 404, .. }) => {
            // Forgejo gives the same answer for a cook who is not there and
            // for a cook it hides, and this page repeats that. Saying which
            // of the two it is would defeat the setting.
            let message = if viewer.is_some() {
                "Forgejo does not show this profile to you."
            } else {
                "Forgejo does not show this profile to a visitor. Sign in, then try again."
            };

            return (
                StatusCode::NOT_FOUND,
                respond(ProfileTemplate::refusal(
                    layout,
                    forgejo_url,
                    message.to_string(),
                )),
            )
                .into_response();
        }
        Err(error) => {
            tracing::warn!(%error, %login, "cannot ask Forgejo about this cook");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                respond(ProfileTemplate::refusal(
                    layout,
                    forgejo_url,
                    NO_FORGEJO.to_string(),
                )),
            )
                .into_response();
        }
    };

    let name = person.display_name().to_string();

    let card = Person {
        login: person.login.clone(),
        name: name.clone(),
        avatar: !person.avatar_url.trim().is_empty(),
        url: state.forgejo.web_url(&person.login),
    };

    // Forgejo names what this credential may see. A row of the index is
    // never permission to show anything, so the question goes to Forgejo
    // on every request and the index only supplies the words on a card.
    let (repositories, truncated) = match owned(&state.forgejo, token.as_ref(), person.id).await {
        Ok(found) => found,
        Err(error) => {
            tracing::warn!(%error, %login, "cannot ask Forgejo for the Recipes of this cook");
            return respond(ProfileTemplate {
                layout,
                forgejo_url,
                person: Some(card),
                message: None,
                recipes: Vec::new(),
                notice: Some(NO_FORGEJO.to_string()),
                empty: String::new(),
                cookbooks: Vec::new(),
                cookbook_notice: None,
                cookbook_empty: String::new(),
            })
            .into_response();
        }
    };

    let recipes: Vec<Repository> = repositories
        .iter()
        .filter(|repository| index::is_recipe(repository))
        .cloned()
        .collect();

    let cookbooks: Vec<CookbookCard> = repositories
        .iter()
        .filter(|repository| repository.has_topics(&COOKBOOK_TOPICS))
        .map(|repository| CookbookCard {
            owner: repository.owner.login.clone(),
            name: repository.name.clone(),
            private: repository.private,
            url: format!("/cookbooks/{}/{}", repository.owner.login, repository.name),
        })
        .collect();

    let cards: Vec<RecipeCard> =
        index::entries(&state.pool, &state.forgejo, token.as_ref(), &recipes)
            .await
            .into_iter()
            .map(card_of)
            .collect();

    // A Cookbook had no page here when this was written, so every card led
    // to Forgejo and said why. Cookbooks have a page now.
    // A Cookbook has a page here now, so no card needs explaining.
    let cookbook_notice: Option<String> = None;

    respond(ProfileTemplate {
        layout,
        forgejo_url,
        person: Some(card),
        message: None,
        recipes: cards,
        notice: truncated.then(|| TOO_MANY.to_string()),
        empty: format!("{name} has no Recipe that you can see."),
        cookbooks,
        cookbook_notice,
        cookbook_empty: format!("{name} has no Cookbook that you can see."),
    })
    .into_response()
}

/// Serve the picture of one cook from this application.
///
/// The Content Security Policy allows an image from this origin only, so
/// the bytes travel through here. Forgejo decides who may be seen: the
/// picture of a profile that Forgejo hides from the asker is not served,
/// and the address is checked against the Forgejo of this installation
/// before anything is fetched.
async fn avatar(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
    Path(login): Path<String>,
) -> Response {
    let token = stored_token(&state, &jar).await;

    let Ok(person) = state.forgejo.user_as(token.as_ref(), &login).await else {
        return StatusCode::NOT_FOUND.into_response();
    };

    crate::web::serve_avatar(&state, &person.avatar_url).await
}

/// The stored credential of the person who is looking, without a renewal.
///
/// Serving a picture needs the credential and not the name behind it, and
/// an image request must never drive a credential operation. The page
/// itself goes through `web::viewer_token`, which renews.
async fn stored_token(state: &AppState, jar: &CookieJar) -> Option<Secret<String>> {
    let cookie = jar.get(crate::session::COOKIE_NAME)?;
    crate::session::access_token(&state.pool, &state.cipher, cookie.value())
        .await
        .ok()
        .flatten()
}

/// Ask Forgejo which Cooklang repositories of one cook a credential may see.
///
/// This is the permission decision, and Forgejo makes it. A Recipe and a
/// Cookbook carry the same first topic, so one search finds both and the
/// caller splits the answer. The second value says whether the cap cut the
/// answer short.
async fn owned(
    forgejo: &ForgejoClient,
    token: Option<&Secret<String>>,
    owner_id: i64,
) -> Result<(Vec<Repository>, bool), ForgejoError> {
    let mut found: Vec<Repository> = Vec::new();
    let mut page = 1;

    loop {
        let batch = forgejo
            .search_repositories(
                token,
                &RepositoryQuery {
                    topic: SEARCH_TOPIC,
                    ownership: Ownership::OwnedBy(owner_id),
                    page,
                    limit: SEARCH_PAGE,
                },
            )
            .await?;

        let complete = batch.len() < SEARCH_PAGE as usize;
        found.extend(batch);

        if complete {
            return Ok((found, false));
        }
        if found.len() >= MAX_LISTED {
            found.truncate(MAX_LISTED);
            return Ok((found, true));
        }
        page += 1;
    }
}

fn card_of(entry: index::Indexed) -> RecipeCard {
    RecipeCard {
        owner: entry.owner,
        slug: entry.slug,
        title: entry.title,
        private: entry.private,
        servings: entry.servings,
        tags: entry.tags,
        ingredients: entry.ingredients,
        thumbnail: entry.thumbnail,
    }
}

fn respond<T: Template>(template: T) -> Response {
    match template.render() {
        Ok(body) => Html(body).into_response(),
        Err(error) => {
            tracing::error!(%error, "cannot render the profile template");
            (StatusCode::INTERNAL_SERVER_ERROR, "template error").into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_profile_address_is_built_from_the_login() {
        assert_eq!(address("sam"), "/cooks/sam");
    }

    #[test]
    fn a_recipe_needs_both_recipe_topics_and_a_cookbook_both_cookbook_topics() {
        let repository = |topics: &[&str]| Repository {
            id: 1,
            name: "chili".to_string(),
            full_name: "sam/chili".to_string(),
            html_url: String::new(),
            clone_url: String::new(),
            default_branch: "main".to_string(),
            private: false,
            empty: false,
            has_issues: true,
            topics: topics.iter().map(|topic| topic.to_string()).collect(),
            updated_at: String::new(),
            owner: crate::forgejo::RepositoryOwner {
                id: 1,
                login: "sam".to_string(),
            },
        };

        let recipe = repository(&["cooklang", "recipe"]);
        assert!(index::is_recipe(&recipe));
        assert!(!recipe.has_topics(&COOKBOOK_TOPICS));

        let cookbook = repository(&["cooklang", "cookbook"]);
        assert!(cookbook.has_topics(&COOKBOOK_TOPICS));
        assert!(!index::is_recipe(&cookbook));

        // A repository that carries the wider marker only is neither, so it
        // reaches no list on the page.
        let neither = repository(&["cooklang"]);
        assert!(!index::is_recipe(&neither));
        assert!(!neither.has_topics(&COOKBOOK_TOPICS));
    }
}
