//! Pages for creating and reading a Recipe.

use std::sync::Arc;

use askama::Template;
use axum::Router;
use axum::extract::{DefaultBodyLimit, Multipart, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::get;
use axum_extra::extract::CookieJar;

use crate::create_recipe::{self, CreateError, NewRecipe};
use crate::forgejo::{ForgejoUser, Repository};
use crate::recipe::{self, RECIPE_FILE};
use crate::render::{self, RenderedRecipe};
use crate::scale::View;
use crate::secret::Secret;
use crate::session::{self, COOKIE_NAME};
use crate::upload::{self, SourceMode};
use crate::web::{AppState, Layout, MaybeUser};

/// One area of a Recipe page.
///
/// The page names every area from the start, so that the shape of a Recipe
/// is clear before each area is built. An area whose ticket has not landed
/// shows as unavailable instead of disappearing.
pub struct RecipeArea {
    pub name: &'static str,
    /// Where the area lives. `None` means it is not built yet.
    pub href: Option<String>,
}

/// The areas of a Recipe, in the order the page shows them.
///
/// A ticket that builds an area fills in that one line. This keeps the
/// areas in one list, so no area is forgotten and no two tickets have to
/// edit the same line.
pub fn areas(owner: &str, slug: &str, repository: &Repository) -> Vec<RecipeArea> {
    let _ = (owner, slug);
    vec![
        RecipeArea {
            name: "History",
            href: None,
        },
        RecipeArea {
            name: "Suggestions",
            href: None,
        },
        RecipeArea {
            name: "Discussions",
            href: crate::web_discussions::area_href(owner, slug, repository),
        },
        RecipeArea {
            name: "Variations",
            href: None,
        },
        RecipeArea {
            name: "Sharing",
            href: Some(format!("/recipes/{owner}/{slug}/sharing")),
        },
    ]
}

/// Shown when the stored file is not text that the application can read.
const NOT_TEXT_MESSAGE: &str = "This Recipe is not UTF-8 text. Each character that could not be read appears below as a replacement mark. Open the Recipe in Forgejo to see the exact content.";

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/recipes/new",
            // The create form carries files now, so the request is larger
            // than the ordinary limit of the framework allows.
            get(new_form)
                .post(create)
                .layer(DefaultBodyLimit::max(upload::MAX_REQUEST_BYTES)),
        )
        .route("/recipes/{owner}/{slug}", get(show))
        .merge(crate::upload::router())
}

/// A signed-in person plus the credential to act as them in Forgejo.
pub(crate) struct Actor {
    pub user: ForgejoUser,
    pub token: Secret<String>,
}

/// Read the session and fetch the Forgejo identity behind it.
///
/// The identity comes from Forgejo rather than from the session row so that
/// the address obeys the current privacy setting of that person.
pub(crate) async fn actor(state: &AppState, jar: &CookieJar) -> Option<Actor> {
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
    /// Which of the two sources the person selected.
    mode: SourceMode,
    source: String,
    private: bool,
    errors: Vec<String>,
}

async fn new_form(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    MaybeUser(user): MaybeUser,
) -> Response {
    if user.is_none() {
        return Redirect::to("/auth/sign-in").into_response();
    }
    let _ = &state;

    respond(NewTemplate {
        layout: Layout::new(user.as_ref()).on(&headers, "/recipes/new"),
        title: String::new(),
        // Writing the Recipe here is the default.
        mode: SourceMode::default(),
        source: String::new(),
        // Public is the default.
        private: false,
        errors: Vec::new(),
    })
}

async fn create(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    multipart: Multipart,
) -> Response {
    let Some(actor) = actor(&state, &jar).await else {
        return Redirect::to("/auth/sign-in").into_response();
    };

    // The form carries files, so it arrives as multipart. Reading it never
    // fails: a body the application cannot read becomes a reason that the
    // form shows.
    let form = upload::read_create_form(multipart).await;
    let private = form.private;

    let content = match form.content() {
        Ok(content) => content,
        Err(refusals) => {
            return respond(NewTemplate {
                layout: Layout::new(current.as_ref()).on(&headers, "/recipes/new"),
                title: form.title,
                mode: form.mode,
                source: form.typed,
                private,
                errors: refusals.iter().map(ToString::to_string).collect(),
            });
        }
    };

    let input = NewRecipe {
        title: form.title.clone(),
        source: content.source,
        private,
        thumbnail: content.thumbnail,
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

            // Put the new Recipe in the index at once. Forgejo reports the
            // Version before the topics are set, so the message that
            // follows a creation describes a repository that is not yet a
            // Recipe. The application made this one and knows better.
            crate::index::refresh(
                &state.pool,
                &state.forgejo,
                Some(&actor.token),
                &created.owner,
                &created.slug,
            )
            .await;

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
                layout: Layout::new(current.as_ref()).on(&headers, "/recipes/new"),
                title: form.title,
                mode: form.mode,
                source: form.typed,
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
    /// The technical name of the Recipe, for an action that links to it.
    slug: String,
    title: String,
    /// Whether the page can show a photo of this Recipe.
    photo: bool,
    /// Whether this person can put a photo on this Recipe.
    can_change_photo: bool,
    /// The Recipe as a cook reads it.
    cooked: RenderedRecipe,
    /// The Cooklang behind it, kept for anybody who wants to look.
    source: String,
    forgejo_url: String,
    areas: Vec<RecipeArea>,
    warnings: Vec<String>,
    errors: Vec<String>,
    /// The Recipe as JSON, for Cook mode.
    cooking_data: String,
}

/// The last value that the address gives for a name.
///
/// The query arrives as pairs and not as a structure, so a name that
/// appears twice gives a page and not an error. The last value wins.
fn query_value<'a>(query: &'a [(String, String)], name: &str) -> Option<&'a str> {
    query
        .iter()
        .rev()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

async fn show(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug)): Path<(String, String)>,
    Query(query): Query<Vec<(String, String)>>,
) -> Response {
    let here = format!("/recipes/{owner}/{slug}");

    // How this cook wants to read the Recipe. These options change the view
    // and nothing else: no file is written and no Version appears.
    let view = View::from_query(
        query_value(&query, "servings"),
        query_value(&query, "units"),
    );

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

    let mut warnings: Vec<String> = parsed.warnings.iter().map(|d| d.message.clone()).collect();

    let photos = upload::photos(
        &state.forgejo,
        token.as_ref(),
        &owner,
        &slug,
        &upload::branch_of(&repository),
    )
    .await;

    // A Recipe with two photos is a state that Git allows and this
    // interface cannot resolve. Say so, and leave it to a person.
    if photos == upload::Photos::Several {
        warnings.push(upload::SEVERAL_PHOTOS_MESSAGE.to_string());
    }

    let can_change_photo = current.as_ref().is_some_and(|person| person.login == owner);

    // A Recipe the parser refused cannot be cooked, so the page shows the
    // diagnosis and the source instead of a broken rendering.
    let cooked = recipe::parse_recipe(&source)
        .as_ref()
        .map(|parsed| render::render_with(parsed, &view, recipe::converter()))
        .unwrap_or_default();

    let areas = areas(&owner, &slug, &repository);
    let title = parsed.title.unwrap_or_else(|| repository.name.clone());
    let cooking_data = crate::cooking::json(&title, &cooked);

    respond(ShowTemplate {
        layout: Layout::new(current.as_ref()).on(&headers, &here),
        owner,
        slug,
        title,
        photo: photos.is_some(),
        can_change_photo,
        cooked,
        source,
        forgejo_url: state.forgejo.web_url(&repository.full_name),
        areas,
        warnings,
        errors,
        cooking_data,
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
