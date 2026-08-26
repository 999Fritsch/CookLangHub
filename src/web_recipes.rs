//! Pages for creating and reading a Recipe.

use std::sync::Arc;

use askama::Template;
use axum::Form;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::Router;
use axum_extra::extract::CookieJar;
use serde::Deserialize;

use crate::create_recipe::{self, CreateError, NewRecipe};
use crate::forgejo::ForgejoUser;
use crate::recipe::{self, RECIPE_FILE};
use crate::render::{self, RenderedRecipe};
use crate::secret::Secret;
use crate::session::{self, COOKIE_NAME};
use crate::web::{AppState, Layout, MaybeUser};

/// The other areas of a Recipe. Each one arrives in a later ticket, and the
/// page names them so that the shape of a Recipe is clear from the start.
const RECIPE_AREAS: [&str; 5] = [
    "History",
    "Suggestions",
    "Discussions",
    "Variations",
    "Sharing",
];

/// Shown when the stored file is not text that the application can read.
const NOT_TEXT_MESSAGE: &str = "This Recipe is not UTF-8 text. Each character that could not be read appears below as a replacement mark. Open the Recipe in Forgejo to see the exact content.";

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/recipes/new", get(new_form).post(create))
        .route("/recipes/{owner}/{slug}", get(show))
}

/// A signed-in person plus the credential to act as them in Forgejo.
struct Actor {
    user: ForgejoUser,
    token: Secret<String>,
}

/// Read the session and fetch the Forgejo identity behind it.
///
/// The identity comes from Forgejo rather than from the session row so that
/// the address obeys the current privacy setting of that person.
async fn actor(state: &AppState, jar: &CookieJar) -> Option<Actor> {
    let cookie = jar.get(COOKIE_NAME)?;
    let token = session::access_token(&state.pool, &state.cipher, cookie.value())
        .await
        .ok()
        .flatten()?;
    let user = state.forgejo.current_user(&token).await.ok()?;
    Some(Actor { user, token })
}

#[derive(Template)]
#[template(path = "recipe_new.html")]
struct NewTemplate {
    layout: Layout,
    title: String,
    source: String,
    private: bool,
    errors: Vec<String>,
}

async fn new_form(State(state): State<Arc<AppState>>, MaybeUser(user): MaybeUser) -> Response {
    if user.is_none() {
        return Redirect::to("/auth/sign-in").into_response();
    }
    let _ = &state;

    respond(NewTemplate {
        layout: Layout::new(user.as_ref()),
        title: String::new(),
        source: String::new(),
        // Public is the default.
        private: false,
        errors: Vec::new(),
    })
}

/// What the create form sends.
#[derive(Debug, Deserialize)]
struct CreateForm {
    title: String,
    #[serde(default)]
    source: String,
    /// Absent means public, because the form sends this only when the
    /// person picks Private.
    #[serde(default)]
    visibility: Option<String>,
}

async fn create(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Form(form): Form<CreateForm>,
) -> Response {
    let Some(actor) = actor(&state, &jar).await else {
        return Redirect::to("/auth/sign-in").into_response();
    };

    let private = form.visibility.as_deref() == Some("private");

    let input = NewRecipe {
        title: form.title.clone(),
        source: form.source.clone(),
        private,
        noreply_domain: state.forgejo_noreply_domain.clone(),
    };

    let result = create_recipe::create(
        &state.forgejo,
        state.git.as_ref(),
        &actor.token,
        &actor.user,
        input,
    )
    .await;

    match result {
        Ok(created) => {
            tracing::info!(
                owner = %created.owner,
                slug = %created.slug,
                warnings = created.warnings.len(),
                "created a Recipe"
            );
            Redirect::to(&format!("/recipes/{}/{}", created.owner, created.slug)).into_response()
        }
        Err(error) => {
            // The person keeps what they typed, and sees why it stopped.
            let errors = match &error {
                CreateError::Invalid { errors } => errors.clone(),
                other => vec![other.to_string()],
            };

            tracing::info!(%error, "a Recipe was not created");

            respond(NewTemplate {
                layout: Layout::new(current.as_ref()),
                title: form.title,
                source: form.source,
                private,
                errors,
            })
        }
    }
}

#[derive(Template)]
#[template(path = "recipe_show.html")]
struct ShowTemplate {
    layout: Layout,
    owner: String,
    title: String,
    /// The Recipe as a cook reads it.
    cooked: RenderedRecipe,
    /// The Cooklang behind it, kept for anybody who wants to look.
    source: String,
    forgejo_url: String,
    areas: [&'static str; 5],
    warnings: Vec<String>,
    errors: Vec<String>,
}

async fn show(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug)): Path<(String, String)>,
) -> Response {
    // A public Recipe is readable without a session. Forgejo applies the
    // permissions, so a private one needs the credential of somebody who
    // may see it.
    let token = actor(&state, &jar).await.map(|a| a.token);

    let repository = match state
        .forgejo
        .repository(
            token.as_ref().unwrap_or(&Secret::new(String::new())),
            &owner,
            &slug,
        )
        .await
    {
        Ok(repository) => repository,
        Err(error) => {
            tracing::info!(%error, %owner, %slug, "cannot read the Recipe repository");
            return (StatusCode::NOT_FOUND, "This Recipe is not available.").into_response();
        }
    };

    let bytes = match state
        .forgejo
        .raw_file(
            token.as_ref(),
            &owner,
            &slug,
            &repository.default_branch,
            RECIPE_FILE,
        )
        .await
    {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::info!(%error, %owner, %slug, "cannot read the Recipe file");
            Vec::new()
        }
    };

    // A Recipe written through this application is always UTF-8 text. Git
    // accepts any bytes though, so a direct push can put something else
    // there. Say so plainly instead of showing replacement characters that
    // look like a fault in the Recipe itself.
    let valid_text = std::str::from_utf8(&bytes).is_ok();
    let source = String::from_utf8_lossy(&bytes).to_string();

    let mut errors = Vec::new();
    if !valid_text {
        tracing::info!(%owner, %slug, "the Recipe file is not UTF-8 text");
        errors.push(NOT_TEXT_MESSAGE.to_string());
    }

    let parsed = recipe::parse(&source);
    errors.extend(parsed.errors.iter().map(|d| d.message.clone()));

    // A Recipe the parser refused cannot be cooked, so the page shows the
    // diagnosis and the source instead of a broken rendering.
    let cooked = recipe::parse_recipe(&source)
        .as_ref()
        .map(render::render)
        .unwrap_or_default();

    respond(ShowTemplate {
        layout: Layout::new(current.as_ref()),
        owner,
        title: parsed.title.unwrap_or_else(|| repository.name.clone()),
        cooked,
        source,
        forgejo_url: state.forgejo.web_url(&repository.full_name),
        areas: RECIPE_AREAS,
        warnings: parsed.warnings.iter().map(|d| d.message.clone()).collect(),
        errors,
    })
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
