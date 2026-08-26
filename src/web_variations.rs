//! The Variations area of a Recipe.
//!
//! A Variation is a Forgejo fork. Forgejo holds the relationship, so this
//! page asks Forgejo two questions on every request: where this Recipe came
//! from, and which Recipes were made from it. Nothing is stored here, and no
//! marker is written into Git.
//!
//! Forgejo also decides who may do this. A person who cannot read a Recipe
//! cannot make a Variation of it, because Forgejo refuses both, and a private
//! Variation stays out of the list of somebody who may not read it.
//!
//! The page also says what the source Recipe holds that this Recipe does
//! not. That answer comes from the two Histories that Forgejo already keeps,
//! and it is read again on every request. Nothing is applied until a person
//! presses **Update from original**, and a change that Git cannot join
//! leaves both Recipes exactly as they were.
//!
//! Every action on this page is a form that posts. The page needs no script.

use std::sync::Arc;

use askama::Template;
use axum::Form;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum_extra::extract::CookieJar;
use serde::Deserialize;

use crate::forgejo::Repository;
use crate::git::Identity;
use crate::recipe::{self, RECIPE_FILE};
use crate::secret::Secret;
use crate::variation::{self, Published, SourceRecipe, Updated, Upstream, VariationError};
use crate::web::{AppState, Layout, MaybeUser, RecipeCard};
use crate::web_recipes::{RecipeArea, areas};

/// The longest identifier of a Version that the address can carry.
const MAX_VERSION_CHARS: usize = 64;

/// The shortest identifier that names one Version without doubt.
const MIN_VERSION_CHARS: usize = 7;

/// Shown when the person already has a Variation of this Recipe.
const ALREADY_THERE_MESSAGE: &str =
    "You have a Variation of this Recipe already. It is in the list below.";

/// Shown when the Version to start from is not one of this Recipe.
const NO_VERSION_MESSAGE: &str =
    "This Recipe does not hold that Version. Open the History and select a Version again.";

/// Shown when every name that the application offered was taken.
const NO_NAME_MESSAGE: &str = "CookLangHub cannot find a free name for this Variation. Open the Recipe in Forgejo to make one there.";

/// Shown when Forgejo or Git does not answer.
const UNREACHABLE_MESSAGE: &str =
    "CookLangHub cannot make a Variation at the moment. Nothing changed. Try again.";

/// Shown when the list of Variations is empty.
const NO_VARIATIONS_MESSAGE: &str = "Nobody has made a Variation of this Recipe yet.";

/// Shown when this Recipe holds every Version of the source Recipe.
const CURRENT_MESSAGE: &str =
    "This Recipe holds every Version of the source Recipe. There is nothing to bring.";

/// Shown when the two Histories are too far apart to compare on this page.
const UNKNOWN_MESSAGE: &str = "CookLangHub cannot say if the source Recipe has newer Versions. Open the Recipe in Forgejo to see what it holds.";

/// Shown when an update had nothing to bring.
const NOTHING_MESSAGE: &str = "The source Recipe has no Version that this Recipe does not hold. CookLangHub made no new Version.";

/// Shown when Git cannot put the two sides together.
const CONFLICT_MESSAGE: &str = "CookLangHub cannot join the changes of the source Recipe with the changes of this Recipe. This Recipe did not change, and the source Recipe did not change.";

/// Shown when the person may read this Recipe but may not change it.
const NO_WRITE_MESSAGE: &str =
    "You can read this Recipe, but you cannot change it. Ask the owner to make you an Editor.";

/// Shown when this Recipe was made from no other Recipe.
const NOT_A_VARIATION_MESSAGE: &str =
    "This Recipe is not a Variation, so there is no source Recipe to update it from.";

/// Shown when Forgejo names a source Recipe that this person cannot read.
const NO_SOURCE_MESSAGE: &str = "The source Recipe is not available. It is private now, or it is gone. This Recipe holds everything that it held before.";

/// Shown when Forgejo or Git does not answer while an update runs.
const UPDATE_UNREACHABLE_MESSAGE: &str =
    "CookLangHub cannot update this Recipe at the moment. Nothing changed. Try again.";

/// Where the Variations area of a Recipe lives.
pub fn area_href(owner: &str, slug: &str) -> String {
    format!("/recipes/{owner}/{slug}/variations")
}

/// Where **Update from original** posts to.
fn update_href(owner: &str, slug: &str) -> String {
    format!("/recipes/{owner}/{slug}/variations/update")
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/recipes/{owner}/{slug}/variations",
            get(variations).post(create),
        )
        .route("/recipes/{owner}/{slug}/variations/update", post(update))
}

// ---------------------------------------------------------------------
// The Recipe behind the page
// ---------------------------------------------------------------------

/// The Recipe that this Variations page belongs to.
struct Subject {
    repository: Repository,
    /// The title a cook sees. It lives in the Recipe, not in the name.
    title: String,
    forgejo_url: String,
}

/// Read the Recipe behind the page.
///
/// `None` means Forgejo did not give the Recipe to this person: it is gone,
/// it never existed, or they may not see it. Forgejo decides, and this is
/// what lets an anonymous person read the Variations of a public Recipe and
/// nothing more.
async fn subject(
    state: &AppState,
    token: Option<&Secret<String>>,
    owner: &str,
    slug: &str,
) -> Option<Subject> {
    let repository = match state.forgejo.repository_as(token, owner, slug).await {
        Ok(repository) => repository,
        Err(error) => {
            tracing::info!(%error, %owner, %slug, "cannot read the Recipe repository");
            return None;
        }
    };

    let title = state
        .forgejo
        .raw_file(token, owner, slug, repository.branch(), RECIPE_FILE)
        .await
        .ok()
        .and_then(|bytes| recipe::parse(&String::from_utf8_lossy(&bytes)).title)
        .unwrap_or_else(|| repository.name.clone());

    let forgejo_url = state.forgejo.web_url(&repository.full_name);

    Some(Subject {
        repository,
        title,
        forgejo_url,
    })
}

/// The answer when there is no Recipe to show Variations for.
fn missing() -> Response {
    (StatusCode::NOT_FOUND, "This Recipe is not available.").into_response()
}

/// Turn what the address carries into the identifier of a Version.
///
/// The value reaches Forgejo and the Git adapter, so only the shape that Git
/// uses passes: hexadecimal, and long enough to name one Version.
fn version_id(raw: &str) -> Option<String> {
    let trimmed = raw.trim();

    if trimmed.len() < MIN_VERSION_CHARS || trimmed.len() > MAX_VERSION_CHARS {
        return None;
    }

    if !trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }

    Some(trimmed.to_ascii_lowercase())
}

/// The day and the clock of a moment that Git wrote.
///
/// Git writes RFC 3339, for example `2026-08-26T09:41:00+02:00`. Two
/// Versions can share a day, so the clock stays.
fn moment(timestamp: &str) -> String {
    let timestamp = timestamp.trim();
    let Some((day, rest)) = timestamp.split_once('T') else {
        return timestamp.to_string();
    };

    let clock: String = rest.chars().take(5).collect();
    if clock.len() < 5 {
        return day.to_string();
    }

    format!("{day} {clock}")
}

/// The first line of what a person wrote about a Version.
fn description(message: &str) -> String {
    let first = message.lines().next().unwrap_or_default().trim();
    if first.is_empty() {
        "No description".to_string()
    } else {
        first.to_string()
    }
}

// ---------------------------------------------------------------------
// The page
// ---------------------------------------------------------------------

/// The Version that a Variation will start at, when it is not the published
/// one.
struct StartVersion {
    /// The identifier that the form carries. A person never reads it.
    id: String,
    description: String,
    moment: String,
}

/// The card that says what the source Recipe holds that this one does not.
///
/// The card is on the page only while Forgejo names a source Recipe that
/// this person can read. Every field is read again on every request, and
/// none of it is stored.
struct UpdateCard {
    /// Where **Update from original** posts to.
    action: String,
    /// What the card says about the Versions of the source Recipe.
    message: String,
    /// Where the Changes page of the source Recipe compares the Version
    /// that both Recipes hold with the newest one. History already draws
    /// that comparison in cooking words, so this page links to it.
    changes: Option<String>,
    /// Whether the card offers **Update from original**. Forgejo decides.
    can_update: bool,
    /// Why the last update did not happen.
    problem: Option<&'static str>,
    /// Whether **Open in Forgejo** belongs in the card. It does when the
    /// state is one that this interface cannot put right.
    forgejo: bool,
}

impl UpdateCard {
    /// Whether the card has anything for a person to press.
    ///
    /// A cook who only reads a Variation that holds every Version of its
    /// source Recipe gets the sentence and no row of buttons at all.
    fn has_actions(&self) -> bool {
        self.can_update || self.changes.is_some() || self.forgejo
    }
}

#[derive(Template)]
#[template(path = "variations.html")]
struct VariationsTemplate {
    layout: Layout,
    owner: String,
    slug: String,
    title: String,
    areas: Vec<RecipeArea>,
    forgejo_url: String,
    /// The Recipe that this one was made from, while this person can read
    /// it. Forgejo records the relationship and this application does not.
    source: Option<SourceRecipe>,
    /// Whether Forgejo names a source Recipe that this person cannot read.
    /// The page then says that the source is not available.
    source_unavailable: bool,
    /// What the source Recipe holds that this Recipe does not, and what a
    /// person can do about it.
    update: Option<UpdateCard>,
    /// The Recipes that were made from this one. The card grid reads this
    /// field, so it carries the name the grid uses.
    recipes: Vec<RecipeCard>,
    /// What the card grid says when the list is empty.
    empty: String,
    /// The message that the card grid shows above the list. It says what
    /// Forgejo holds that this page cannot draw as a Recipe.
    notice: Option<String>,
    /// The Version a Variation will start at, when a person chose one.
    start: Option<StartVersion>,
    errors: Vec<String>,
}

/// What the address can carry to the page.
#[derive(Debug, Deserialize)]
struct VariationsQuery {
    /// The Version to start a Variation at. The Version page links here
    /// with it, so that a person who reads an earlier Version gets a
    /// Variation of what they are reading.
    #[serde(default)]
    from: String,
}

async fn variations(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug)): Path<(String, String)>,
    Query(query): Query<VariationsQuery>,
) -> Response {
    let token = crate::web::viewer_token(&state, &jar).await;

    let Some(subject) = subject(&state, token.as_ref(), &owner, &slug).await else {
        return missing();
    };

    let start = match version_id(&query.from) {
        Some(wanted) => start_version(&state, token.as_ref(), &owner, &slug, &wanted).await,
        None => None,
    };

    render(
        &state,
        &headers,
        current.as_ref(),
        token.as_ref(),
        &subject,
        &owner,
        &slug,
        start,
        Vec::new(),
        None,
    )
    .await
}

/// Read the Version that a person asked to start their Variation at.
///
/// `None` means Forgejo does not hold that Version for this Recipe. The page
/// then offers a Variation of the published Recipe, which is what a person
/// gets when they come to this page without reading an earlier Version.
async fn start_version(
    state: &AppState,
    token: Option<&Secret<String>>,
    owner: &str,
    slug: &str,
    wanted: &str,
) -> Option<StartVersion> {
    let commit = match state.forgejo.commit(token, owner, slug, wanted).await {
        Ok(commit) => commit,
        Err(error) => {
            tracing::info!(%error, %owner, %slug, "cannot read the Version to start a Variation at");
            return None;
        }
    };

    Some(StartVersion {
        id: commit.sha.clone(),
        description: description(&commit.commit.message),
        moment: moment(
            commit
                .commit
                .author
                .as_ref()
                .map(|identity| identity.date.as_str())
                .unwrap_or_default(),
        ),
    })
}

/// What the page says about copies that are not Recipes.
///
/// Somebody can copy a Recipe in Forgejo itself, and Forgejo marks no copy as
/// a Recipe. This application does not repair that and does not hide it: it
/// says how many there are, and the list beside this message offers **Open in
/// Forgejo**.
fn other_copies(count: usize) -> Option<String> {
    match count {
        0 => None,
        1 => Some(
            "Forgejo holds one more copy of this Recipe. CookLangHub does not show it, because it is not a Recipe."
                .to_string(),
        ),
        many => Some(format!(
            "Forgejo holds {many} more copies of this Recipe. CookLangHub does not show them, because they are not Recipes."
        )),
    }
}

/// Why an update did not happen, as the card says it.
#[derive(Debug, Clone, Copy)]
struct UpdateProblem {
    message: &'static str,
    /// Whether **Open in Forgejo** belongs beside it. A state that this
    /// interface cannot put right needs it.
    forgejo: bool,
}

/// What the card says when the source Recipe moved on.
fn newer_message(count: usize) -> String {
    if count == 1 {
        "The source Recipe has one newer Version. CookLangHub changes this Recipe only when you ask for it.".to_string()
    } else {
        format!(
            "The source Recipe has {count} newer Versions. CookLangHub changes this Recipe only when you ask for it."
        )
    }
}

/// Build the card about the source Recipe.
///
/// Forgejo answers both questions here: what the source Recipe holds that
/// this Recipe does not, and whether this person may change this Recipe.
/// Neither answer is stored.
async fn update_card(
    state: &AppState,
    token: Option<&Secret<String>>,
    subject: &Subject,
    owner: &str,
    slug: &str,
    source: &SourceRecipe,
    problem: Option<UpdateProblem>,
) -> UpdateCard {
    let here = Published {
        owner,
        slug,
        branch: subject.repository.branch(),
    };

    let upstream =
        variation::newer_in_source(&state.forgejo, token, here, source.published()).await;

    // Forgejo decides who may change a Recipe. A person who may not still
    // reads what the source Recipe holds, and is offered no action.
    let can_update = match token {
        Some(token) => state
            .forgejo
            .can_write(token, owner, slug)
            .await
            .unwrap_or(false),
        None => false,
    };

    let (message, changes, unknown) = match upstream {
        Upstream::Current => (CURRENT_MESSAGE.to_string(), None, false),
        Upstream::Newer(newer) => (
            newer_message(newer.count),
            // History already compares two Versions in cooking words, and
            // both of these live in the source Recipe.
            Some(format!(
                "{}/changes?from={}&to={}",
                source.href(),
                newer.common,
                newer.version
            )),
            false,
        ),
        Upstream::Unknown => (UNKNOWN_MESSAGE.to_string(), None, true),
    };

    UpdateCard {
        action: update_href(owner, slug),
        message,
        changes,
        can_update,
        problem: problem.map(|problem| problem.message),
        forgejo: unknown || problem.is_some_and(|problem| problem.forgejo),
    }
}

/// Draw the page.
#[allow(clippy::too_many_arguments)]
async fn render(
    state: &AppState,
    headers: &HeaderMap,
    current: Option<&crate::session::CurrentUser>,
    token: Option<&Secret<String>>,
    subject: &Subject,
    owner: &str,
    slug: &str,
    start: Option<StartVersion>,
    errors: Vec<String>,
    problem: Option<UpdateProblem>,
) -> Response {
    let here = area_href(owner, slug);

    let came_from = variation::source_of(&state.forgejo, token, owner, slug).await;
    let made = variation::variations_of(&state.forgejo, token, owner, slug).await;

    // The card belongs to a Recipe that came from another one this person
    // can read. Forgejo names that Recipe, and nothing else does.
    let update = match came_from.recipe() {
        Some(source) => {
            Some(update_card(state, token, subject, owner, slug, source, problem).await)
        }
        None => None,
    };

    // The index holds the title and the few culinary facts that a card
    // shows. Forgejo named these Recipes, so the index only supplies the
    // words.
    let entries = crate::index::entries(&state.pool, &state.forgejo, token, &made.recipes).await;

    let recipes = entries
        .into_iter()
        .map(|entry| RecipeCard {
            owner: entry.owner,
            slug: entry.slug,
            title: entry.title,
            private: entry.private,
            thumbnail: entry.thumbnail,
            servings: entry.servings,
            tags: entry.tags,
            ingredients: entry.ingredients,
        })
        .collect();

    respond(VariationsTemplate {
        layout: Layout::new(current).on(headers, &here),
        owner: owner.to_string(),
        slug: slug.to_string(),
        title: subject.title.clone(),
        areas: areas(owner, slug, &subject.repository),
        forgejo_url: subject.forgejo_url.clone(),
        source_unavailable: came_from.is_unavailable(),
        source: came_from.recipe().cloned(),
        update,
        recipes,
        empty: NO_VARIATIONS_MESSAGE.to_string(),
        notice: other_copies(made.others),
        start,
        errors,
    })
}

// ---------------------------------------------------------------------
// Create variation
// ---------------------------------------------------------------------

/// What the **Create variation** form sends.
#[derive(Debug, Deserialize)]
struct CreateForm {
    /// The Version to start at. Empty means the published Version.
    #[serde(default)]
    version: String,
}

async fn create(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug)): Path<(String, String)>,
    Form(form): Form<CreateForm>,
) -> Response {
    let Some(actor) = crate::web_recipes::actor(&state, &jar).await else {
        return Redirect::to("/auth/sign-in").into_response();
    };

    // Forgejo answers for the Recipe only while this person may read it, so
    // a Recipe they cannot see is not there at all.
    let Some(subject) = subject(&state, Some(&actor.token), &owner, &slug).await else {
        return missing();
    };

    let wanted = if form.version.trim().is_empty() {
        None
    } else {
        match version_id(&form.version) {
            Some(wanted) => Some(wanted),
            // A value that is not the shape of a Version never reaches
            // Forgejo or Git.
            None => {
                return refuse(
                    &state,
                    &headers,
                    current.as_ref(),
                    &actor.token,
                    &subject,
                    &owner,
                    &slug,
                    StatusCode::BAD_REQUEST,
                    NO_VERSION_MESSAGE,
                )
                .await;
            }
        }
    };

    let made = variation::create(
        &state.forgejo,
        state.git.as_ref(),
        &actor.token,
        &actor.user,
        &owner,
        &slug,
        wanted.as_deref(),
    )
    .await;

    let (status, message) = match made {
        Ok(made) => {
            tracing::info!(
                source_owner = %owner,
                source_slug = %slug,
                owner = %made.owner,
                slug = %made.slug,
                "made a Variation of a Recipe"
            );

            // Put the Variation in the index at once. It is a Recipe, and a
            // person is about to read it in a list.
            crate::index::refresh(
                &state.pool,
                &state.forgejo,
                Some(&actor.token),
                &made.owner,
                &made.slug,
            )
            .await;

            return Redirect::to(&format!("/recipes/{}/{}", made.owner, made.slug)).into_response();
        }
        // Forgejo did not give the Recipe to this person while the work ran.
        Err(VariationError::NoSource) => return missing(),
        Err(VariationError::AlreadyThere) => (StatusCode::CONFLICT, ALREADY_THERE_MESSAGE),
        Err(VariationError::NoVersion) => (StatusCode::BAD_REQUEST, NO_VERSION_MESSAGE),
        Err(VariationError::NoFreeName) => (StatusCode::CONFLICT, NO_NAME_MESSAGE),
        Err(error) => {
            tracing::warn!(%error, %owner, %slug, "cannot make a Variation of this Recipe");
            (StatusCode::OK, UNREACHABLE_MESSAGE)
        }
    };

    refuse(
        &state,
        &headers,
        current.as_ref(),
        &actor.token,
        &subject,
        &owner,
        &slug,
        status,
        message,
    )
    .await
}

/// Draw the page again with the reason that nothing was made.
#[allow(clippy::too_many_arguments)]
async fn refuse(
    state: &AppState,
    headers: &HeaderMap,
    current: Option<&crate::session::CurrentUser>,
    token: &Secret<String>,
    subject: &Subject,
    owner: &str,
    slug: &str,
    status: StatusCode,
    message: &str,
) -> Response {
    let body = render(
        state,
        headers,
        current,
        Some(token),
        subject,
        owner,
        slug,
        None,
        vec![message.to_string()],
        None,
    )
    .await;

    (status, body).into_response()
}

// ---------------------------------------------------------------------
// Update from original
// ---------------------------------------------------------------------

/// Bring what the source Recipe holds into this Variation.
///
/// This runs only because a person pressed the button. Nothing on this page
/// and nothing behind it applies a change of the source Recipe by itself.
async fn update(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug)): Path<(String, String)>,
) -> Response {
    let Some(actor) = crate::web_recipes::actor(&state, &jar).await else {
        return Redirect::to("/auth/sign-in").into_response();
    };

    let Some(subject) = subject(&state, Some(&actor.token), &owner, &slug).await else {
        return missing();
    };

    // Forgejo decides who may change a Recipe. The check happens here and
    // not only on the page, because this request can arrive without the
    // page.
    if !state
        .forgejo
        .can_write(&actor.token, &owner, &slug)
        .await
        .unwrap_or(false)
    {
        tracing::info!(%owner, %slug, login = %actor.user.login, "a person who cannot change this Recipe asked for an update");
        return refuse(
            &state,
            &headers,
            current.as_ref(),
            &actor.token,
            &subject,
            &owner,
            &slug,
            StatusCode::FORBIDDEN,
            NO_WRITE_MESSAGE,
        )
        .await;
    }

    // Forgejo holds the relationship, so Forgejo says what the source
    // Recipe is. This application keeps none of it.
    let source = match variation::source_of(&state.forgejo, Some(&actor.token), &owner, &slug).await
    {
        variation::Source::Recipe(source) => source,
        variation::Source::None => {
            return refuse(
                &state,
                &headers,
                current.as_ref(),
                &actor.token,
                &subject,
                &owner,
                &slug,
                StatusCode::BAD_REQUEST,
                NOT_A_VARIATION_MESSAGE,
            )
            .await;
        }
        variation::Source::Unavailable => {
            return refuse(
                &state,
                &headers,
                current.as_ref(),
                &actor.token,
                &subject,
                &owner,
                &slug,
                StatusCode::CONFLICT,
                NO_SOURCE_MESSAGE,
            )
            .await;
        }
    };

    let identity = identity_of(&state, &actor).await;
    let here = Published {
        owner: &owner,
        slug: &slug,
        branch: subject.repository.branch(),
    };

    let done = variation::update_from_source(
        &state.forgejo,
        state.git.as_ref(),
        &actor.token,
        &identity,
        here,
        &source,
    )
    .await;

    let (status, problem) = match done {
        Ok(Updated::Version(version)) => {
            tracing::info!(%owner, %slug, %version, "updated a Variation from its source Recipe");

            // The Recipe holds something else now, so the index has to say
            // what it holds. It is a Recipe in every list.
            crate::index::refresh(
                &state.pool,
                &state.forgejo,
                Some(&actor.token),
                &owner,
                &slug,
            )
            .await;

            // History shows the new Version at the top, which is the
            // clearest answer that the update happened.
            return Redirect::to(&crate::web_history::area_href(&owner, &slug)).into_response();
        }
        Ok(Updated::Nothing) => (
            StatusCode::OK,
            UpdateProblem {
                message: NOTHING_MESSAGE,
                forgejo: false,
            },
        ),
        // The state the interface cannot handle. Both Recipes are exactly
        // as they were, and the person is told where to go on.
        Err(VariationError::Conflict) => (
            StatusCode::CONFLICT,
            UpdateProblem {
                message: CONFLICT_MESSAGE,
                forgejo: true,
            },
        ),
        Err(error) => {
            tracing::warn!(%error, %owner, %slug, "cannot update this Recipe from its source Recipe");
            (
                StatusCode::OK,
                UpdateProblem {
                    message: UPDATE_UNREACHABLE_MESSAGE,
                    forgejo: false,
                },
            )
        }
    };

    let body = render(
        &state,
        &headers,
        current.as_ref(),
        Some(&actor.token),
        &subject,
        &owner,
        &slug,
        None,
        Vec::new(),
        Some(problem),
    )
    .await;

    (status, body).into_response()
}

/// Who carries the new Version that an update makes.
///
/// The address obeys the privacy setting of that person in Forgejo. A
/// question that Forgejo does not answer counts as "hide", because an
/// address that is published by accident cannot be taken back.
async fn identity_of(state: &AppState, actor: &crate::web_recipes::Actor) -> Identity {
    let hide_email = match state.forgejo.user_settings(&actor.token).await {
        Ok(settings) => settings.hide_email,
        Err(error) => {
            tracing::warn!(%error, "cannot read the privacy setting; using the no-reply address");
            true
        }
    };

    Identity {
        name: actor.user.display_name().to_string(),
        email: crate::create_recipe::commit_email(
            &actor.user.login,
            &actor.user.email,
            hide_email,
            &state.forgejo_noreply_domain,
        ),
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

    #[test]
    fn only_the_shape_of_a_version_identifier_reaches_forgejo_and_git() {
        assert_eq!(version_id("A1B2C3D4E5F6"), Some("a1b2c3d4e5f6".to_string()));
        assert_eq!(version_id(" abcdef1 "), Some("abcdef1".to_string()));

        for value in [
            "",
            "abc",
            "../../admin/users",
            "main",
            "abcdefg",
            "abcdef1;rm",
            "refs/heads/main",
            &"a".repeat(65),
        ] {
            assert_eq!(version_id(value), None, "`{value}` must not be sent on");
        }
    }

    #[test]
    fn a_copy_that_is_not_a_recipe_is_counted_and_never_drawn() {
        assert_eq!(other_copies(0), None, "nothing to say means no message");
        assert!(
            other_copies(1)
                .expect("a message")
                .contains("one more copy")
        );
        assert!(
            other_copies(3)
                .expect("a message")
                .contains("3 more copies")
        );
    }

    #[test]
    fn a_moment_shows_the_day_and_the_clock() {
        assert_eq!(moment("2026-08-26T09:41:00+02:00"), "2026-08-26 09:41");
        assert_eq!(moment("2026-08-26"), "2026-08-26");
        assert_eq!(moment(""), "");
    }

    #[test]
    fn a_version_without_a_description_still_names_itself() {
        assert_eq!(description("Add Chili\n\nThe first one."), "Add Chili");
        assert_eq!(description("   "), "No description");
    }

    #[test]
    fn the_area_of_a_recipe_sits_under_the_recipe() {
        assert_eq!(
            area_href("sam", "chili"),
            "/recipes/sam/chili/variations",
            "the address of the area must stay under the Recipe"
        );
        assert_eq!(
            update_href("sam", "chili"),
            "/recipes/sam/chili/variations/update",
            "the action must stay under the area it belongs to"
        );
    }

    #[test]
    fn the_card_counts_the_newer_versions_of_the_source_recipe() {
        assert!(newer_message(1).contains("one newer Version"));
        assert!(newer_message(4).contains("4 newer Versions"));

        // The whole point of the card: nothing happens on its own.
        for count in [1, 4] {
            assert!(
                newer_message(count).contains("only when you ask for it"),
                "the card must say that nothing is applied by itself"
            );
        }
    }

    #[test]
    fn every_message_a_person_reads_uses_cooking_words() {
        // Whole words only. `Sharing` is an area of a Recipe, so the check
        // must not read `sha` inside another word.
        let forge_words = [
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

        let one = other_copies(1).expect("one copy has a message");
        let many = other_copies(4).expect("four copies have a message");
        let newer_one = newer_message(1);
        let newer_many = newer_message(4);

        for message in [
            ALREADY_THERE_MESSAGE,
            NO_VERSION_MESSAGE,
            NO_NAME_MESSAGE,
            UNREACHABLE_MESSAGE,
            NO_VARIATIONS_MESSAGE,
            CURRENT_MESSAGE,
            UNKNOWN_MESSAGE,
            NOTHING_MESSAGE,
            CONFLICT_MESSAGE,
            NO_WRITE_MESSAGE,
            NOT_A_VARIATION_MESSAGE,
            NO_SOURCE_MESSAGE,
            UPDATE_UNREACHABLE_MESSAGE,
            one.as_str(),
            many.as_str(),
            newer_one.as_str(),
            newer_many.as_str(),
        ] {
            let lower = message.to_lowercase();
            assert!(
                !lower.contains("pull request"),
                "a word of the forge must not reach the person: {message}"
            );

            let spoken: Vec<&str> = lower
                .split(|c: char| !c.is_alphanumeric())
                .filter(|word| !word.is_empty())
                .collect();

            for word in forge_words {
                assert!(
                    !spoken.contains(&word),
                    "`{word}` must not reach the person: {message}"
                );
            }
        }
    }
}
