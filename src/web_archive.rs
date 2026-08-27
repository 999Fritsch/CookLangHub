//! The Archive area of a Recipe and of a Cookbook.
//!
//! One screen holds both lifecycle actions, because a person who came here
//! to clean up has to read what each of the two costs before they choose.
//! Archive is reversible and it keeps History. Delete is permanent.
//!
//! Forgejo owns both. Archive is one Forgejo setting, and this application
//! keeps no copy of it. Delete is one Forgejo call, and there is nothing
//! left to delete anywhere else.
//!
//! Every action is a form that posts. A GET on this screen reads and never
//! writes, and the delete address answers a GET with the impact report and
//! nothing else. [`crate::archive`] holds the model behind the screen, and
//! the measurements of Forgejo that the wording rests on.

use std::sync::Arc;

use askama::Template;
use axum::Form;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum_extra::extract::CookieJar;
use serde::Deserialize;

use crate::archive::{self, Impact, Kind};
use crate::cookbook;
use crate::forgejo::{ForgejoError, Repository};
use crate::session::CurrentUser;
use crate::web::{AppState, Layout, MaybeUser};
use crate::web_recipes::{Actor, RecipeArea, areas};

/// Where the Archive area of a Recipe lives.
pub fn area_href(owner: &str, slug: &str) -> String {
    format!("/recipes/{owner}/{slug}/archive")
}

/// Where the Archive area of a Cookbook lives.
pub fn cookbook_area_href(owner: &str, slug: &str) -> String {
    format!("/cookbooks/{owner}/{slug}/archive")
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/recipes/{owner}/{slug}/archive", get(show))
        .route("/recipes/{owner}/{slug}/archive/state", post(set_state))
        .route(
            "/recipes/{owner}/{slug}/archive/delete",
            get(confirm_delete).post(delete),
        )
        .route("/cookbooks/{owner}/{slug}/archive", get(cookbook_show))
        .route(
            "/cookbooks/{owner}/{slug}/archive/state",
            post(cookbook_set_state),
        )
        .route(
            "/cookbooks/{owner}/{slug}/archive/delete",
            get(cookbook_confirm_delete).post(cookbook_delete),
        )
}

// ------------------------------------------------------------- the subject

/// What Forgejo says about one object and the person who asks about it.
struct Subject {
    actor: Actor,
    repository: Repository,
    /// Whether Forgejo says the person who asks owns this object.
    is_owner: bool,
    /// The title a person reads. It lives in the content, not in the name.
    title: String,
}

/// Why a page or an action cannot go on.
enum Stop {
    /// Nobody is signed in.
    SignIn,
    /// Forgejo does not show this object to this person.
    Unknown,
    /// Forgejo says this person does not own it.
    NotOwner(Box<Subject>),
}

/// Read the object and ask Forgejo who this person is to it.
///
/// This runs before a page is drawn and again before any form acts, because
/// a check that happens only in the interface is not a check.
async fn subject(
    state: &AppState,
    jar: &CookieJar,
    kind: Kind,
    owner: &str,
    slug: &str,
) -> Result<Subject, Stop> {
    let Some(actor) = crate::web_recipes::actor(state, jar).await else {
        return Err(Stop::SignIn);
    };

    // Forgejo applies its own permissions here, so an object that this
    // person may not see never reaches the next line.
    let repository = match state.forgejo.repository(&actor.token, owner, slug).await {
        Ok(repository) => {
            let right_kind = match kind {
                Kind::Recipe => crate::index::is_recipe(&repository),
                Kind::Cookbook => cookbook::is_cookbook(&repository),
            };
            if !right_kind {
                return Err(Stop::Unknown);
            }
            repository
        }
        Err(error) => {
            tracing::info!(%error, %owner, %slug, "cannot read the repository for its Archive area");
            return Err(Stop::Unknown);
        }
    };

    // Two answers, and Forgejo gives both: who the object belongs to, and
    // what this person may do with it. Forgejo keeps reporting `push` for an
    // archived repository, so this answer says nothing about the archive.
    let permission = state
        .forgejo
        .repository_permission(&actor.token, owner, slug, &actor.user.login)
        .await;

    let holds_the_keys = match &permission {
        Ok(permission) => matches!(permission.permission.as_str(), "owner" | "admin"),
        Err(error) => {
            tracing::info!(%error, %owner, %slug, "Forgejo gave no permission for this person");
            false
        }
    };

    // A Forgejo Administrator holds the keys to something that is not
    // theirs. They are not the Owner, and this screen belongs to the Owner,
    // so they go to Forgejo instead.
    let is_owner = repository.owner.login == actor.user.login && holds_the_keys;

    let title = match kind {
        Kind::Recipe => {
            archive::recipe_title(state, Some(&actor.token), owner, slug, &repository).await
        }
        Kind::Cookbook => cookbook_title(state, &actor, &repository).await,
    };

    let found = Subject {
        actor,
        repository,
        is_owner,
        title,
    };

    if found.is_owner {
        Ok(found)
    } else {
        Err(Stop::NotOwner(Box::new(found)))
    }
}

/// The title of a Cookbook, as its README gives it.
async fn cookbook_title(state: &AppState, actor: &Actor, repository: &Repository) -> String {
    let bytes = state
        .forgejo
        .raw_file(
            Some(&actor.token),
            &repository.owner.login,
            &repository.name,
            repository.branch(),
            cookbook::README_FILE,
        )
        .await
        .unwrap_or_default();

    cookbook::read_readme(&bytes)
        .title
        .unwrap_or_else(|| repository.name.clone())
}

// ---------------------------------------------------------- the two pages

#[derive(Template)]
#[template(path = "recipe_archive.html")]
struct RecipeTemplate {
    layout: Layout,
    owner: String,
    slug: String,
    title: String,
    forgejo_url: String,
    areas: Vec<RecipeArea>,
    /// Whether Forgejo says the person who asks owns this Recipe.
    is_owner: bool,
    /// Whether Forgejo holds this Recipe as archived.
    archived: bool,
    /// Where each form posts to, and where a cancel returns to.
    archive_path: String,
    /// Show the delete confirmation instead of the controls.
    confirming: bool,
    /// What a deletion would reach. Read only for the confirmation.
    impact: Option<Impact>,
    archived_label: &'static str,
    in_use_label: &'static str,
    read_only_message: &'static str,
    delete_warning: &'static str,
    partial_message: &'static str,
    unanswered_message: &'static str,
    cookbooks_message: &'static str,
    variations_message: &'static str,
    suggestions_message: &'static str,
    errors: Vec<String>,
}

#[derive(Template)]
#[template(path = "cookbook_archive.html")]
struct CookbookTemplate {
    layout: Layout,
    owner: String,
    slug: String,
    title: String,
    forgejo_url: String,
    /// `cookbook_areas.html` needs both of these.
    area: &'static str,
    can_change: bool,
    is_owner: bool,
    archived: bool,
    archive_path: String,
    confirming: bool,
    /// The Recipes that this Cookbook holds, for the confirmation. Each one
    /// stays exactly as it is.
    recipes: Vec<cookbook::Held>,
    /// How many of them this person cannot open. They stay as well, and the
    /// page must not name any of them.
    hidden: usize,
    archived_label: &'static str,
    in_use_label: &'static str,
    read_only_message: &'static str,
    delete_warning: &'static str,
    partial_message: &'static str,
    recipes_message: &'static str,
    errors: Vec<String>,
}

/// Draw the Archive area of a Recipe from what Forgejo says right now.
async fn draw_recipe(
    state: &AppState,
    headers: &HeaderMap,
    current: Option<&CurrentUser>,
    found: &Subject,
    confirming: bool,
    errors: Vec<String>,
) -> Response {
    let owner = found.repository.owner.login.clone();
    let slug = found.repository.name.clone();
    let archive_path = area_href(&owner, &slug);

    // The report is read only for the page that shows it. A person who is
    // not the Owner never reaches this, and neither does the plain page.
    let impact = if confirming && found.is_owner {
        Some(archive::impact_of(state, &found.actor.token, &owner, &slug).await)
    } else {
        None
    };

    respond(RecipeTemplate {
        layout: Layout::new(current).on(headers, &archive_path),
        title: found.title.clone(),
        forgejo_url: state.forgejo.web_url(&found.repository.full_name),
        areas: areas(&owner, &slug, &found.repository),
        is_owner: found.is_owner,
        archived: found.repository.archived,
        owner,
        slug,
        archive_path,
        confirming,
        impact,
        archived_label: archive::ARCHIVED_LABEL,
        in_use_label: archive::IN_USE_LABEL,
        read_only_message: archive::READ_ONLY_MESSAGE,
        delete_warning: archive::DELETE_WARNING,
        partial_message: archive::PARTIAL_MESSAGE,
        unanswered_message: archive::UNANSWERED_MESSAGE,
        cookbooks_message: archive::COOKBOOKS_MESSAGE,
        variations_message: archive::VARIATIONS_MESSAGE,
        suggestions_message: archive::SUGGESTIONS_MESSAGE,
        errors,
    })
}

/// Draw the Archive area of a Cookbook from what Forgejo says right now.
async fn draw_cookbook(
    state: &AppState,
    headers: &HeaderMap,
    current: Option<&CurrentUser>,
    found: &Subject,
    confirming: bool,
    errors: Vec<String>,
) -> Response {
    let owner = found.repository.owner.login.clone();
    let slug = found.repository.name.clone();
    let archive_path = cookbook_area_href(&owner, &slug);

    let mut recipes: Vec<cookbook::Held> = Vec::new();
    let mut hidden = 0;

    if confirming && found.is_owner {
        let contents =
            cookbook::references(&state.forgejo, Some(&found.actor.token), &found.repository).await;

        let held = cookbook::held_recipes(
            &state.pool,
            &state.forgejo,
            Some(&found.actor.token),
            &contents,
        )
        .await;

        hidden = held.iter().filter(|one| !one.available).count();
        recipes = held.into_iter().filter(|one| one.available).collect();
    }

    respond(CookbookTemplate {
        layout: Layout::new(current).on(headers, &archive_path),
        title: found.title.clone(),
        forgejo_url: state.forgejo.web_url(&found.repository.full_name),
        area: "archive",
        // `cookbook_areas.html` uses this to decide whether the areas that
        // belong to the Owner show at all. A person who reached this page
        // keeps the same navigation that the Sharing area gives them.
        can_change: true,
        is_owner: found.is_owner,
        archived: found.repository.archived,
        owner,
        slug,
        archive_path,
        confirming,
        recipes,
        hidden,
        archived_label: archive::ARCHIVED_LABEL,
        in_use_label: archive::IN_USE_LABEL,
        read_only_message: archive::READ_ONLY_COOKBOOK_MESSAGE,
        delete_warning: archive::DELETE_COOKBOOK_WARNING,
        partial_message: archive::PARTIAL_MESSAGE,
        recipes_message: archive::COOKBOOK_RECIPES_MESSAGE,
        errors,
    })
}

/// Turn a stop into the answer that a person gets.
async fn refuse(
    state: &AppState,
    headers: &HeaderMap,
    current: Option<&CurrentUser>,
    kind: Kind,
    stop: Stop,
) -> Response {
    match stop {
        Stop::SignIn => Redirect::to("/auth/sign-in").into_response(),
        Stop::Unknown => match kind {
            Kind::Recipe => {
                (StatusCode::NOT_FOUND, "This Recipe is not available.").into_response()
            }
            Kind::Cookbook => {
                (StatusCode::NOT_FOUND, "This Cookbook is not available.").into_response()
            }
        },
        Stop::NotOwner(found) => {
            // This person can read it, so the page tells them who owns it
            // and offers Forgejo. It carries no control.
            let page = match kind {
                Kind::Recipe => {
                    draw_recipe(state, headers, current, &found, false, Vec::new()).await
                }
                Kind::Cookbook => {
                    draw_cookbook(state, headers, current, &found, false, Vec::new()).await
                }
            };
            (StatusCode::FORBIDDEN, page).into_response()
        }
    }
}

// ------------------------------------------------------------ the handlers

async fn show(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug)): Path<(String, String)>,
) -> Response {
    match subject(&state, &jar, Kind::Recipe, &owner, &slug).await {
        Ok(found) => {
            draw_recipe(
                &state,
                &headers,
                current.as_ref(),
                &found,
                false,
                Vec::new(),
            )
            .await
        }
        Err(stop) => refuse(&state, &headers, current.as_ref(), Kind::Recipe, stop).await,
    }
}

async fn cookbook_show(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug)): Path<(String, String)>,
) -> Response {
    match subject(&state, &jar, Kind::Cookbook, &owner, &slug).await {
        Ok(found) => {
            draw_cookbook(
                &state,
                &headers,
                current.as_ref(),
                &found,
                false,
                Vec::new(),
            )
            .await
        }
        Err(stop) => refuse(&state, &headers, current.as_ref(), Kind::Cookbook, stop).await,
    }
}

/// The step before a permanent deletion.
///
/// This is a page and not a dialog, because the report is part of the
/// decision and it must reach a person who runs no scripts. It reads and it
/// writes nothing: the deletion is the POST at the same address.
async fn confirm_delete(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug)): Path<(String, String)>,
) -> Response {
    match subject(&state, &jar, Kind::Recipe, &owner, &slug).await {
        Ok(found) => {
            draw_recipe(&state, &headers, current.as_ref(), &found, true, Vec::new()).await
        }
        Err(stop) => refuse(&state, &headers, current.as_ref(), Kind::Recipe, stop).await,
    }
}

async fn cookbook_confirm_delete(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug)): Path<(String, String)>,
) -> Response {
    match subject(&state, &jar, Kind::Cookbook, &owner, &slug).await {
        Ok(found) => {
            draw_cookbook(&state, &headers, current.as_ref(), &found, true, Vec::new()).await
        }
        Err(stop) => refuse(&state, &headers, current.as_ref(), Kind::Cookbook, stop).await,
    }
}

/// What the archive form sends.
#[derive(Debug, Deserialize)]
struct StateForm {
    /// `yes` puts it in the archive, `no` takes it out.
    #[serde(default)]
    archived: String,
}

async fn set_state(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug)): Path<(String, String)>,
    Form(form): Form<StateForm>,
) -> Response {
    let found = match subject(&state, &jar, Kind::Recipe, &owner, &slug).await {
        Ok(found) => found,
        Err(stop) => return refuse(&state, &headers, current.as_ref(), Kind::Recipe, stop).await,
    };

    let Some(archived) = wanted(&form.archived) else {
        return draw_recipe(
            &state,
            &headers,
            current.as_ref(),
            &found,
            false,
            vec!["Select Archive or Unarchive.".to_string()],
        )
        .await;
    };

    match state
        .forgejo
        .set_repository_archived(&found.actor.token, &owner, &slug, archived)
        .await
    {
        Ok(_) => {
            tracing::info!(%owner, %slug, archived, "the archive state of a Recipe changed");
            Redirect::to(&area_href(&owner, &slug)).into_response()
        }
        Err(error) => {
            tracing::warn!(%error, %owner, %slug, "cannot change the archive state");
            draw_recipe(
                &state,
                &headers,
                current.as_ref(),
                &found,
                false,
                vec![format!(
                    "Forgejo did not change the state of this Recipe: {}. Open the Recipe in Forgejo to see its state.",
                    short(&error)
                )],
            )
            .await
        }
    }
}

async fn cookbook_set_state(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug)): Path<(String, String)>,
    Form(form): Form<StateForm>,
) -> Response {
    let found = match subject(&state, &jar, Kind::Cookbook, &owner, &slug).await {
        Ok(found) => found,
        Err(stop) => return refuse(&state, &headers, current.as_ref(), Kind::Cookbook, stop).await,
    };

    let Some(archived) = wanted(&form.archived) else {
        return draw_cookbook(
            &state,
            &headers,
            current.as_ref(),
            &found,
            false,
            vec!["Select Archive or Unarchive.".to_string()],
        )
        .await;
    };

    match state
        .forgejo
        .set_repository_archived(&found.actor.token, &owner, &slug, archived)
        .await
    {
        Ok(_) => {
            tracing::info!(%owner, %slug, archived, "the archive state of a Cookbook changed");
            Redirect::to(&cookbook_area_href(&owner, &slug)).into_response()
        }
        Err(error) => {
            tracing::warn!(%error, %owner, %slug, "cannot change the archive state");
            draw_cookbook(
                &state,
                &headers,
                current.as_ref(),
                &found,
                false,
                vec![format!(
                    "Forgejo did not change the state of this Cookbook: {}. Open the Cookbook in Forgejo to see its state.",
                    short(&error)
                )],
            )
            .await
        }
    }
}

/// What the delete form sends.
#[derive(Debug, Deserialize)]
struct DeleteForm {
    /// `yes` after the person read the report and decided.
    #[serde(default)]
    confirm: String,
}

async fn delete(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug)): Path<(String, String)>,
    Form(form): Form<DeleteForm>,
) -> Response {
    let found = match subject(&state, &jar, Kind::Recipe, &owner, &slug).await {
        Ok(found) => found,
        Err(stop) => return refuse(&state, &headers, current.as_ref(), Kind::Recipe, stop).await,
    };

    // A Recipe goes only after the person confirms. The check lives here,
    // so a post that skips the report deletes nothing.
    if form.confirm != "yes" {
        return draw_recipe(&state, &headers, current.as_ref(), &found, true, Vec::new()).await;
    }

    match state
        .forgejo
        .delete_repository(&found.actor.token, &owner, &slug)
        .await
    {
        Ok(()) => {
            tracing::info!(%owner, %slug, "a Recipe was deleted");

            // The index is operational state and it is rebuildable. Forget
            // the row now rather than waiting for the message that Forgejo
            // sends, so the lists stop naming a Recipe that is gone.
            if let Err(error) = crate::index::forget(&state.pool, &owner, &slug).await {
                tracing::warn!(%error, %owner, %slug, "cannot forget the deleted Recipe");
            }

            Redirect::to("/").into_response()
        }
        Err(error) => {
            tracing::warn!(%error, %owner, %slug, "cannot delete the Recipe");
            draw_recipe(
                &state,
                &headers,
                current.as_ref(),
                &found,
                true,
                vec![format!(
                    "Forgejo did not delete this Recipe: {}. Open the Recipe in Forgejo to delete it there.",
                    short(&error)
                )],
            )
            .await
        }
    }
}

async fn cookbook_delete(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug)): Path<(String, String)>,
    Form(form): Form<DeleteForm>,
) -> Response {
    let found = match subject(&state, &jar, Kind::Cookbook, &owner, &slug).await {
        Ok(found) => found,
        Err(stop) => return refuse(&state, &headers, current.as_ref(), Kind::Cookbook, stop).await,
    };

    if form.confirm != "yes" {
        return draw_cookbook(&state, &headers, current.as_ref(), &found, true, Vec::new()).await;
    }

    match state
        .forgejo
        .delete_repository(&found.actor.token, &owner, &slug)
        .await
    {
        Ok(()) => {
            tracing::info!(%owner, %slug, "a Cookbook was deleted");

            if let Err(error) = cookbook::forget(&state.pool, &owner, &slug).await {
                tracing::warn!(%error, %owner, %slug, "cannot forget the deleted Cookbook");
            }

            Redirect::to("/cookbooks").into_response()
        }
        Err(error) => {
            tracing::warn!(%error, %owner, %slug, "cannot delete the Cookbook");
            draw_cookbook(
                &state,
                &headers,
                current.as_ref(),
                &found,
                true,
                vec![format!(
                    "Forgejo did not delete this Cookbook: {}. Open the Cookbook in Forgejo to delete it there.",
                    short(&error)
                )],
            )
            .await
        }
    }
}

// ------------------------------------------------------------- the refusal

#[derive(Template)]
#[template(path = "archived_blocked.html")]
struct BlockedTemplate {
    layout: Layout,
    /// `recipes` or `cookbooks`, so the two links reach the right pages.
    area: &'static str,
    owner: String,
    slug: String,
    /// `Recipe` or `Cookbook`, for the words on the page.
    noun: &'static str,
    message: &'static str,
    forgejo_url: String,
    archive_path: String,
}

/// The page that the read-only guard answers with.
///
/// It says what the state is and hands the person the one control that
/// lifts it. It writes nothing and it repairs nothing.
pub(crate) fn refusal(
    state: &AppState,
    current: Option<&CurrentUser>,
    headers: &HeaderMap,
    kind: Kind,
    owner: &str,
    slug: &str,
    repository: &Repository,
) -> String {
    let here = format!("/{}/{owner}/{slug}", kind.area());
    let archive_path = match kind {
        Kind::Recipe => area_href(owner, slug),
        Kind::Cookbook => cookbook_area_href(owner, slug),
    };

    let template = BlockedTemplate {
        layout: Layout::new(current).on(headers, &here),
        area: kind.area(),
        owner: owner.to_string(),
        slug: slug.to_string(),
        noun: match kind {
            Kind::Recipe => "Recipe",
            Kind::Cookbook => "Cookbook",
        },
        message: kind.archived_message(),
        forgejo_url: state.forgejo.web_url(&repository.full_name),
        archive_path,
    };

    template.render().unwrap_or_else(|error| {
        tracing::error!(%error, "cannot render the refusal");
        String::new()
    })
}

// ------------------------------------------------------------------ shared

/// What the archive form asked for, when it asked for one of the two.
fn wanted(value: &str) -> Option<bool> {
    match value {
        "yes" => Some(true),
        "no" => Some(false),
        _ => None,
    }
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
    use crate::archive::{Affected, Named, OpenSuggestion};
    use crate::forgejo::RepositoryOwner;

    fn repository(archived: bool) -> Repository {
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
            archived,
        }
    }

    /// The Archive area of a Recipe, as one person sees it.
    fn page(archived: bool, is_owner: bool, confirming: bool, impact: Option<Impact>) -> String {
        let mut layout = Layout::new(None);
        layout.signed_in = true;

        RecipeTemplate {
            layout,
            owner: "sam".to_string(),
            slug: "chili".to_string(),
            title: "Chili".to_string(),
            forgejo_url: "https://forge.test/sam/chili".to_string(),
            areas: areas("sam", "chili", &repository(archived)),
            is_owner,
            archived,
            archive_path: "/recipes/sam/chili/archive".to_string(),
            confirming,
            impact,
            archived_label: archive::ARCHIVED_LABEL,
            in_use_label: archive::IN_USE_LABEL,
            read_only_message: archive::READ_ONLY_MESSAGE,
            delete_warning: archive::DELETE_WARNING,
            partial_message: archive::PARTIAL_MESSAGE,
            unanswered_message: archive::UNANSWERED_MESSAGE,
            cookbooks_message: archive::COOKBOOKS_MESSAGE,
            variations_message: archive::VARIATIONS_MESSAGE,
            suggestions_message: archive::SUGGESTIONS_MESSAGE,
            errors: Vec::new(),
        }
        .render()
        .expect("the page must render")
    }

    fn full_impact() -> Impact {
        Impact {
            cookbooks: Affected::answered(vec![crate::cookbook::Named {
                owner: "sam".to_string(),
                slug: "winter".to_string(),
                title: "Winter Food".to_string(),
            }]),
            variations: Affected::answered(vec![Named {
                owner: "kim".to_string(),
                slug: "chili".to_string(),
                title: "Chili with beans".to_string(),
            }]),
            suggestions: Affected::answered(vec![OpenSuggestion {
                number: 4,
                title: "Less salt".to_string(),
                author: "robin".to_string(),
            }]),
        }
    }

    #[test]
    fn a_recipe_that_is_in_use_offers_archive_and_never_unarchive() {
        let html = page(false, true, false, None);

        assert!(html.contains(archive::IN_USE_LABEL));
        assert!(html.contains("Archive this Recipe"));
        assert!(!html.contains("Take this Recipe out of the archive"));
        // Archive changes state, so it is a form and never a link.
        assert!(html.contains("action=\"/recipes/sam/chili/archive/state\""));
        assert!(html.contains("value=\"yes\""));
    }

    #[test]
    fn an_archived_recipe_offers_the_way_out() {
        let html = page(true, true, false, None);

        assert!(html.contains(archive::ARCHIVED_LABEL));
        assert!(html.contains(archive::READ_ONLY_MESSAGE));
        assert!(html.contains("Take this Recipe out of the archive"));
        assert!(html.contains("value=\"no\""));
    }

    #[test]
    fn the_plain_page_never_deletes_and_only_leads_to_the_report() {
        let html = page(false, true, false, None);

        // The way to a deletion is a link to a page that reads. The form
        // that deletes lives only on that page.
        assert!(html.contains("href=\"/recipes/sam/chili/archive/delete\""));
        assert!(
            !html.contains("action=\"/recipes/sam/chili/archive/delete\""),
            "the plain page must carry no form that deletes"
        );
        assert!(html.contains(archive::DELETE_WARNING));
    }

    #[test]
    fn the_report_names_the_recipe_and_all_three_lists() {
        let html = page(false, true, true, Some(full_impact()));

        // A confirmation that names what is destroyed.
        assert!(html.contains("Delete Chili"));
        // The three questions, each answered.
        assert!(html.contains("Winter Food"));
        assert!(html.contains("Chili with beans"));
        assert!(html.contains("Less salt"));
        assert!(html.contains("robin"));
        // What happens to each of the three.
        assert!(html.contains(archive::COOKBOOKS_MESSAGE));
        assert!(html.contains(archive::VARIATIONS_MESSAGE));
        assert!(html.contains(archive::SUGGESTIONS_MESSAGE));
        // And what the report cannot see.
        assert!(html.contains(archive::PARTIAL_MESSAGE));
        // The deletion is a form, and it carries the confirmation.
        assert!(html.contains("action=\"/recipes/sam/chili/archive/delete\""));
        assert!(html.contains("method=\"post\""));
        assert!(html.contains("name=\"confirm\" value=\"yes\""));
    }

    #[test]
    fn a_list_that_forgejo_did_not_answer_never_reads_as_nothing_will_break() {
        let impact = Impact {
            cookbooks: Affected::unanswered(),
            variations: Affected::unanswered(),
            suggestions: Affected::answered(Vec::new()),
        };
        let html = page(false, true, true, Some(impact));

        assert!(
            html.contains(archive::UNANSWERED_MESSAGE),
            "a list with no answer must say so"
        );
        // The list that was answered says the true thing instead.
        assert!(html.contains("No Suggestion is open."));
    }

    #[test]
    fn a_person_who_does_not_own_the_recipe_gets_no_control() {
        let html = page(false, false, false, None);

        assert!(!html.contains("action=\"/recipes/sam/chili/archive/state\""));
        assert!(!html.contains("/archive/delete"));
        assert!(html.contains("Open in Forgejo"));
    }

    #[test]
    fn the_page_carries_no_script_that_runs() {
        for html in [
            page(false, true, false, None),
            page(true, true, false, None),
            page(false, true, true, Some(full_impact())),
        ] {
            assert!(!html.contains("onclick="));
            assert!(!html.contains("onsubmit="));
            for script in html.split("<script").skip(1) {
                assert!(
                    script.starts_with(" src=\""),
                    "the page must carry no inline script"
                );
            }
        }
    }

    #[test]
    fn the_archive_form_takes_one_of_two_answers_and_nothing_else() {
        assert_eq!(wanted("yes"), Some(true));
        assert_eq!(wanted("no"), Some(false));
        for other in ["", "true", "Yes", "1", "delete"] {
            assert_eq!(wanted(other), None, "`{other}` must not act");
        }
    }
}
