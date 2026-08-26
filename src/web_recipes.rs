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
use crate::recipe;
use crate::recipe_state::{self, Problem, ValidVersion};
use crate::render::{self, RenderedRecipe};
use crate::scale::View;
use crate::secret::Secret;

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
            href: Some(crate::web_history::area_href(owner, slug)),
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
    let token = crate::web::viewer_token(state, jar).await?;
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
    /// The Recipe as JSON, for Cook mode.
    cooking_data: String,
}

/// The page for a Recipe that the interface cannot cook.
///
/// Every field here is a diagnosis or a recovery option. There is no field
/// that changes the Recipe, because this page never does.
#[derive(Template)]
#[template(path = "recipe_broken.html")]
struct BrokenTemplate {
    layout: Layout,
    owner: String,
    slug: String,
    title: String,
    /// The heading that names the state.
    heading: &'static str,
    /// What is wrong, and what a person can do about it.
    message: &'static str,
    /// What the parser said, when the parser is what refused the Recipe.
    details: Vec<String>,
    /// The promise that the application corrected nothing.
    untouched: &'static str,
    /// The Cooklang as it is stored, when there is any to show.
    source: Option<String>,
    /// The newest Version that can be read, when there is one.
    last_valid: Option<ValidVersion>,
    /// Whether Forgejo lets this person start the repair.
    can_repair: bool,
    forgejo_url: String,
    areas: Vec<RecipeArea>,
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

    let forgejo_url = state.forgejo.web_url(&repository.full_name);
    let areas = areas(&owner, &slug, &repository);
    let layout = Layout::new(current.as_ref()).on(&headers, &here);

    // One place decides what state this Recipe is in. Everything that Git
    // allows and this interface cannot show is named there, and none of it
    // is corrected here.
    let reading = recipe_state::read(&state, token.as_ref(), &owner, &slug, &repository).await;

    let title = reading
        .parsed
        .as_ref()
        .and_then(|parsed| parsed.title.clone())
        .unwrap_or_else(|| repository.name.clone());

    if let Some(problem) = reading.problem {
        return broken(
            &state,
            token.as_ref(),
            layout,
            &owner,
            &slug,
            title,
            problem,
            reading.source,
            forgejo_url,
            areas,
        )
        .await;
    }

    // The parser ran inside the reading, so what it said comes from there.
    // A warning never stops a Recipe: the person decides whether it matters.
    let mut warnings: Vec<String> = reading
        .parsed
        .as_ref()
        .map(|parsed| parsed.warnings.iter().map(|d| d.message.clone()).collect())
        .unwrap_or_default();

    // A Recipe with two photos is a state that Git allows and this
    // interface cannot resolve. Say so, show none of them, and leave the
    // choice to a person.
    if reading.photos == upload::Photos::Several {
        warnings.push(upload::SEVERAL_PHOTOS_MESSAGE.to_string());
    }

    let can_change_photo = current.as_ref().is_some_and(|person| person.login == owner);

    let cooked = recipe::parse_recipe(&reading.source)
        .as_ref()
        .map(|parsed| render::render_with(parsed, &view, recipe::converter()))
        .unwrap_or_default();

    let cooking_data = crate::cooking::json(&title, &cooked);

    respond(ShowTemplate {
        layout,
        owner,
        slug,
        title,
        photo: reading.photos.is_some(),
        can_change_photo,
        cooked,
        source: reading.source,
        forgejo_url,
        areas,
        warnings,
        cooking_data,
    })
}

/// Show the state of a Recipe that the interface cannot cook.
///
/// The page says what is wrong and hands the person what they can act on:
/// the source as it is stored, the last valid Version, a repair that they
/// start themselves, and **Open in Forgejo**. It writes nothing.
#[allow(clippy::too_many_arguments)]
async fn broken(
    state: &AppState,
    token: Option<&Secret<String>>,
    layout: Layout,
    owner: &str,
    slug: &str,
    title: String,
    problem: Problem,
    source: String,
    forgejo_url: String,
    areas: Vec<RecipeArea>,
) -> Response {
    // Forgejo is not answering, so there is no History to offer and no
    // repair that could be trusted to run.
    let searchable = !matches!(problem, Problem::Unreadable | Problem::NoPublishedVersion);

    let last_valid = if searchable {
        recipe_state::last_valid_version(state, token, owner, slug).await
    } else {
        None
    };

    // Forgejo decides who may add a Version, so the repair is offered only
    // to somebody it says can.
    let can_repair = match (last_valid.as_ref(), token) {
        (Some(_), Some(token)) => state
            .forgejo
            .can_write(token, owner, slug)
            .await
            .unwrap_or(false),
        _ => false,
    };

    // A state that Forgejo cannot answer for is an outage and not a
    // property of the Recipe, so it answers as one. Every other state is
    // the true state of a Recipe that is there, and this page is the
    // correct answer for it.
    let status = if matches!(problem, Problem::Unreadable) {
        StatusCode::BAD_GATEWAY
    } else {
        StatusCode::OK
    };

    let body = respond(BrokenTemplate {
        layout,
        owner: owner.to_string(),
        slug: slug.to_string(),
        title,
        heading: problem.heading(),
        message: problem.message(),
        details: problem.details().to_vec(),
        untouched: recipe_state::UNTOUCHED_MESSAGE,
        source: problem.shows_source().then_some(source),
        last_valid,
        can_repair,
        forgejo_url,
        areas,
    });

    (status, body).into_response()
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
    use crate::forgejo::RepositoryOwner;

    fn repository() -> Repository {
        Repository {
            id: 1,
            name: "chili".to_string(),
            full_name: "sam/chili".to_string(),
            html_url: String::new(),
            clone_url: String::new(),
            default_branch: "main".to_string(),
            private: false,
            empty: false,
            has_issues: true,
            topics: vec!["cooklang".to_string(), "recipe".to_string()],
            updated_at: String::new(),
            owner: RepositoryOwner {
                id: 1,
                login: "sam".to_string(),
            },
        }
    }

    /// The diagnosis page, as one person sees it.
    fn diagnosis(
        problem: Problem,
        source: &str,
        last_valid: Option<ValidVersion>,
        can_repair: bool,
        signed_in: bool,
    ) -> String {
        let mut layout = Layout::new(None);
        layout.signed_in = signed_in;

        BrokenTemplate {
            layout,
            owner: "sam".to_string(),
            slug: "chili".to_string(),
            title: "Chili".to_string(),
            heading: problem.heading(),
            message: problem.message(),
            details: problem.details().to_vec(),
            untouched: recipe_state::UNTOUCHED_MESSAGE,
            source: problem.shows_source().then_some(source.to_string()),
            last_valid,
            can_repair,
            forgejo_url: "https://forge.test/sam/chili".to_string(),
            areas: areas("sam", "chili", &repository()),
        }
        .render()
        .expect("the page must render")
    }

    fn valid_version() -> ValidVersion {
        ValidVersion {
            id: "a".repeat(40),
            moment: "2026-08-26 09:41".to_string(),
        }
    }

    #[test]
    fn every_state_is_diagnosed_and_offers_forgejo() {
        for problem in Problem::each() {
            let page = diagnosis(problem.clone(), "", None, false, false);

            assert!(
                page.contains(problem.heading()),
                "{problem:?} must name itself"
            );
            assert!(
                page.contains("Open in Forgejo"),
                "{problem:?} must offer the escape hatch"
            );
            assert!(
                page.contains("https://forge.test/sam/chili"),
                "{problem:?} must carry the address of the Recipe in Forgejo"
            );
            assert!(
                page.contains(recipe_state::UNTOUCHED_MESSAGE),
                "{problem:?} must say that nothing was corrected"
            );
        }
    }

    #[test]
    fn a_broken_recipe_offers_the_source_the_last_version_and_a_repair() {
        // The three recovery options of the ticket, on one page.
        let broken = Problem::Invalid(vec!["a timer needs a unit".to_string()]);
        let page = diagnosis(
            broken,
            "Wait ~{5%bananas}.",
            Some(valid_version()),
            true,
            true,
        );

        // The source, exactly as it is stored.
        assert!(page.contains("Wait ~{5%bananas}."));
        // What the parser found.
        assert!(page.contains("a timer needs a unit"));
        // The last valid Version.
        assert!(page.contains(&format!("/recipes/sam/chili/history/{}", "a".repeat(40))));
        assert!(page.contains("Read the last valid Version"));
        // The repair. It publishes a Version, so it is a form and never a
        // link, and nothing runs until a person presses it.
        assert!(page.contains(&format!(
            "action=\"/recipes/sam/chili/history/{}/restore\"",
            "a".repeat(40)
        )));
        assert!(page.contains("method=\"post\""));
        assert!(page.contains("<button type=\"submit\""));
    }

    #[test]
    fn a_person_who_cannot_write_gets_no_repair() {
        let broken = Problem::Invalid(vec!["a timer needs a unit".to_string()]);
        let page = diagnosis(
            broken,
            "Wait ~{5%bananas}.",
            Some(valid_version()),
            false,
            false,
        );

        assert!(!page.contains("/restore"), "the repair must not be offered");
        assert!(!page.contains("Repair this Recipe"));
        // Reading the last valid Version needs no permission at all.
        assert!(page.contains("Read the last valid Version"));
        assert!(page.contains("Open in Forgejo"));
    }

    #[test]
    fn a_recipe_with_no_earlier_version_still_says_what_is_wrong() {
        let page = diagnosis(Problem::NoRecipeFile, "", None, true, true);

        assert!(page.contains("This Recipe has no Recipe file"));
        assert!(!page.contains("/restore"), "there is nothing to restore");
        assert!(!page.contains("Read the last valid Version"));
        assert!(page.contains("Open in Forgejo"));
    }

    #[test]
    fn a_file_that_is_too_large_never_reaches_the_page() {
        // Putting a megabyte of it on the page is the fault this state
        // exists to avoid.
        let page = diagnosis(Problem::TooLarge, &"x".repeat(2048), None, false, false);

        assert!(
            !page.contains(&"x".repeat(64)),
            "the file must stay off the page"
        );
        assert!(page.contains("larger than 1 MB"));
        assert!(page.contains("Open in Forgejo"));
    }

    #[test]
    fn the_page_carries_no_script_that_runs() {
        // The policy is `default-src 'self'`, and the page has to work with
        // scripts blocked.
        let page = diagnosis(
            Problem::Invalid(vec!["a timer needs a unit".to_string()]),
            "Wait ~{5%bananas}.",
            Some(valid_version()),
            true,
            true,
        );

        assert!(!page.contains("onclick="));
        assert!(!page.contains("onsubmit="));
        for script in page.split("<script").skip(1) {
            assert!(
                script.starts_with(" src=\""),
                "the page must carry no inline script"
            );
        }
    }

    #[test]
    fn a_broken_recipe_keeps_the_other_areas_reachable() {
        // History is a recovery route, so the page must not cut it off.
        let page = diagnosis(Problem::NoRecipeFile, "", None, false, false);

        for area in [
            "History",
            "Suggestions",
            "Discussions",
            "Variations",
            "Sharing",
        ] {
            assert!(page.contains(area), "the page must name `{area}`");
        }
    }
}
