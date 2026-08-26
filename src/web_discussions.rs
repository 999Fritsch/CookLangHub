//! Discussions on a Recipe.
//!
//! A Discussion is a Forgejo issue, and Forgejo holds every word of it. This
//! module adds no second discussion store: it reads and writes through the
//! Forgejo HTTP API and keeps nothing of its own.
//!
//! Every request carries the credential of the person who is signed in, so
//! Forgejo decides who can read a Discussion and who can write in it. The
//! application computes no permission.
//!
//! Forgejo can have Issues off for a repository. Then the Recipe has no
//! Discussions area, the pages here answer that it does not exist, and the
//! application never turns Issues on again.
//!
//! A Discussion is about the whole Recipe. There is no comment on one
//! ingredient and no comment on one step.

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

use crate::create_recipe::MAIN_BRANCH;
use crate::forgejo::{ForgejoUser, Repository};
use crate::recipe::{self, RECIPE_FILE};
use crate::secret::Secret;
use crate::session::{self, COOKIE_NAME};
use crate::web::{AppState, Layout, MaybeUser};
use crate::web_recipes::{RecipeArea, areas};

/// How many Discussions one page shows.
const PAGE_SIZE: u32 = 50;

/// Shown when Forgejo refuses an action, or cannot answer.
const FORGEJO_REFUSED: &str =
    "Forgejo did not accept this action. Open the Recipe in Forgejo to see what is possible there.";

/// Where the Discussions area of a Recipe lives.
///
/// `None` means the Recipe has no Discussions area. That happens when
/// Forgejo has Issues off for the repository. The Recipe page then offers
/// nothing here, and the application never turns Issues on again.
pub fn area_href(owner: &str, slug: &str, repository: &Repository) -> Option<String> {
    if repository.has_issues {
        Some(format!("/recipes/{owner}/{slug}/discussions"))
    } else {
        None
    }
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/recipes/{owner}/{slug}/discussions", get(list).post(start))
        .route("/recipes/{owner}/{slug}/discussions/{number}", get(one))
        .route(
            "/recipes/{owner}/{slug}/discussions/{number}/comments",
            post(write_comment),
        )
        .route(
            "/recipes/{owner}/{slug}/discussions/{number}/state",
            post(change_state),
        )
}

/// The Forgejo credential of the signed-in person, if there is one.
///
/// A public Recipe is readable without a session, so this can be `None`.
async fn token(state: &AppState, jar: &CookieJar) -> Option<Secret<String>> {
    let cookie = jar.get(COOKIE_NAME)?;
    session::access_token(&state.pool, &state.cipher, cookie.value())
        .await
        .ok()
        .flatten()
}

/// The Recipe that a Discussion belongs to.
struct Subject {
    repository: Repository,
    /// The title a cook sees. It lives in the Recipe, not in the name.
    title: String,
    areas: Vec<RecipeArea>,
}

/// Read the Recipe behind a Discussions page.
///
/// `None` means there is no Discussions area to show: either Forgejo did not
/// give the repository to this person, or Issues are off for it.
async fn subject(
    state: &AppState,
    token: Option<&Secret<String>>,
    owner: &str,
    slug: &str,
) -> Option<Subject> {
    let credential = token.cloned().unwrap_or_else(|| Secret::new(String::new()));

    let repository = match state.forgejo.repository(&credential, owner, slug).await {
        Ok(repository) => repository,
        Err(error) => {
            tracing::info!(%error, %owner, %slug, "cannot read the Recipe repository");
            return None;
        }
    };

    // Forgejo is the authority. If it has Issues off, this Recipe has no
    // Discussions area at all.
    if !repository.has_issues {
        tracing::info!(%owner, %slug, "the Recipe has no Discussions area: Forgejo has Issues off");
        return None;
    }

    let branch = if repository.default_branch.is_empty() {
        MAIN_BRANCH
    } else {
        &repository.default_branch
    };
    let title = state
        .forgejo
        .raw_file(token, owner, slug, branch, RECIPE_FILE)
        .await
        .ok()
        .and_then(|bytes| recipe::parse(&String::from_utf8_lossy(&bytes)).title)
        .unwrap_or_else(|| repository.name.clone());

    let areas = areas(owner, slug, &repository);

    Some(Subject {
        repository,
        title,
        areas,
    })
}

/// The answer when a Recipe has no Discussions area.
///
/// The area is absent rather than empty, because Forgejo says this
/// repository has no Issues. The application does not offer a Discussion it
/// cannot store, and it does not turn Issues on to make one possible.
fn absent() -> Response {
    (
        StatusCode::NOT_FOUND,
        "This Recipe has no Discussions area.",
    )
        .into_response()
}

/// The answer when one Discussion cannot be shown.
///
/// The Recipe has a Discussions area, but Forgejo does not give this
/// Discussion to this person: it is gone, or it never existed, or they may
/// not read it. Forgejo decides, and the application says only that much.
fn no_discussion() -> Response {
    (StatusCode::NOT_FOUND, "This Discussion is not available.").into_response()
}

/// One Discussion in the list.
struct DiscussionRow {
    number: i64,
    title: String,
    author: String,
    started: String,
    comments: i64,
    open: bool,
}

/// One Discussion on its own page.
struct DiscussionView {
    number: i64,
    title: String,
    author: String,
    started: String,
    open: bool,
    /// The first message, as text.
    ///
    /// Forgejo stores Markdown. The application shows the characters that
    /// the person wrote, escaped, and keeps the line breaks with a CSS rule.
    /// It does not turn Markdown into HTML: text from another person is not
    /// trusted, and a safe rendering needs a sanitiser that this prototype
    /// does not have.
    body: String,
}

/// One comment inside a Discussion.
struct CommentView {
    author: String,
    written: String,
    /// The comment, as text. See [`DiscussionView::body`].
    body: String,
}

#[derive(Template)]
#[template(path = "recipe_discussions.html")]
struct DiscussionsTemplate {
    layout: Layout,
    owner: String,
    slug: String,
    title: String,
    areas: Vec<RecipeArea>,
    /// Where the Discussions of this Recipe live in Forgejo.
    forgejo_url: String,
    discussions: Vec<DiscussionRow>,
    /// What the person typed, kept when the action did not work.
    form_title: String,
    form_message: String,
    errors: Vec<String>,
}

#[derive(Template)]
#[template(path = "recipe_discussion.html")]
struct DiscussionTemplate {
    layout: Layout,
    owner: String,
    slug: String,
    title: String,
    areas: Vec<RecipeArea>,
    /// Where this Discussion lives in Forgejo.
    forgejo_url: String,
    discussion: DiscussionView,
    comments: Vec<CommentView>,
    form_message: String,
    errors: Vec<String>,
}

async fn list(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug)): Path<(String, String)>,
) -> Response {
    let token = token(&state, &jar).await;
    let Some(subject) = subject(&state, token.as_ref(), &owner, &slug).await else {
        return absent();
    };

    render_list(
        &state,
        &headers,
        current.as_ref(),
        token.as_ref(),
        subject,
        &owner,
        &slug,
        String::new(),
        String::new(),
        Vec::new(),
    )
    .await
}

/// What the start form sends.
#[derive(Debug, Deserialize)]
struct StartForm {
    title: String,
    #[serde(default)]
    message: String,
}

async fn start(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug)): Path<(String, String)>,
    Form(form): Form<StartForm>,
) -> Response {
    let Some(token) = token(&state, &jar).await else {
        return Redirect::to("/auth/sign-in").into_response();
    };
    let Some(subject) = subject(&state, Some(&token), &owner, &slug).await else {
        return absent();
    };

    let title = form.title.trim().to_string();
    let message = form.message.trim().to_string();

    let mut errors = Vec::new();
    if title.is_empty() {
        errors.push("A Discussion needs a title.".to_string());
    }

    if errors.is_empty() {
        match state
            .forgejo
            .create_issue(&token, &owner, &slug, &title, &message)
            .await
        {
            Ok(issue) => {
                tracing::info!(%owner, %slug, number = issue.number, "started a Discussion");
                return Redirect::to(&format!(
                    "/recipes/{owner}/{slug}/discussions/{}",
                    issue.number
                ))
                .into_response();
            }
            Err(error) => {
                tracing::info!(%error, %owner, %slug, "cannot start a Discussion");
                errors.push(FORGEJO_REFUSED.to_string());
            }
        }
    }

    render_list(
        &state,
        &headers,
        current.as_ref(),
        Some(&token),
        subject,
        &owner,
        &slug,
        title,
        message,
        errors,
    )
    .await
}

async fn one(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug, number)): Path<(String, String, i64)>,
) -> Response {
    let token = token(&state, &jar).await;
    let Some(subject) = subject(&state, token.as_ref(), &owner, &slug).await else {
        return absent();
    };

    render_one(
        &state,
        &headers,
        current.as_ref(),
        token.as_ref(),
        subject,
        &owner,
        &slug,
        number,
        String::new(),
        Vec::new(),
    )
    .await
}

/// What the comment form sends.
#[derive(Debug, Deserialize)]
struct CommentForm {
    #[serde(default)]
    message: String,
}

async fn write_comment(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug, number)): Path<(String, String, i64)>,
    Form(form): Form<CommentForm>,
) -> Response {
    let Some(token) = token(&state, &jar).await else {
        return Redirect::to("/auth/sign-in").into_response();
    };
    let Some(subject) = subject(&state, Some(&token), &owner, &slug).await else {
        return absent();
    };

    let message = form.message.trim().to_string();

    let mut errors = Vec::new();
    if message.is_empty() {
        errors.push("A comment needs words.".to_string());
    }

    if errors.is_empty() {
        match state
            .forgejo
            .create_issue_comment(&token, &owner, &slug, number, &message)
            .await
        {
            Ok(_) => {
                tracing::info!(%owner, %slug, number, "wrote a comment in a Discussion");
                return Redirect::to(&format!("/recipes/{owner}/{slug}/discussions/{number}"))
                    .into_response();
            }
            Err(error) => {
                tracing::info!(%error, %owner, %slug, number, "cannot write the comment");
                errors.push(FORGEJO_REFUSED.to_string());
            }
        }
    }

    render_one(
        &state,
        &headers,
        current.as_ref(),
        Some(&token),
        subject,
        &owner,
        &slug,
        number,
        message,
        errors,
    )
    .await
}

/// What the close and open form sends.
#[derive(Debug, Deserialize)]
struct StateForm {
    state: String,
}

async fn change_state(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug, number)): Path<(String, String, i64)>,
    Form(form): Form<StateForm>,
) -> Response {
    let Some(token) = token(&state, &jar).await else {
        return Redirect::to("/auth/sign-in").into_response();
    };
    let Some(subject) = subject(&state, Some(&token), &owner, &slug).await else {
        return absent();
    };

    // Only these two words reach Forgejo. A value that a form carries is
    // never sent on as it arrived.
    let Some(wanted) = wanted_state(&form.state) else {
        return render_one(
            &state,
            &headers,
            current.as_ref(),
            Some(&token),
            subject,
            &owner,
            &slug,
            number,
            String::new(),
            vec!["A Discussion can be open or closed, and nothing else.".to_string()],
        )
        .await;
    };

    let mut errors = Vec::new();
    match state
        .forgejo
        .set_issue_state(&token, &owner, &slug, number, wanted)
        .await
    {
        Ok(_) => {
            tracing::info!(%owner, %slug, number, wanted, "changed the state of a Discussion");
            return Redirect::to(&format!("/recipes/{owner}/{slug}/discussions/{number}"))
                .into_response();
        }
        Err(error) => {
            tracing::info!(%error, %owner, %slug, number, "cannot change the state of the Discussion");
            errors.push(FORGEJO_REFUSED.to_string());
        }
    }

    render_one(
        &state,
        &headers,
        current.as_ref(),
        Some(&token),
        subject,
        &owner,
        &slug,
        number,
        String::new(),
        errors,
    )
    .await
}

/// Read and draw the list of Discussions.
#[allow(clippy::too_many_arguments)]
async fn render_list(
    state: &AppState,
    headers: &HeaderMap,
    current: Option<&crate::session::CurrentUser>,
    token: Option<&Secret<String>>,
    subject: Subject,
    owner: &str,
    slug: &str,
    form_title: String,
    form_message: String,
    mut errors: Vec<String>,
) -> Response {
    let here = format!("/recipes/{owner}/{slug}/discussions");
    let forgejo_url = format!(
        "{}/issues",
        state.forgejo.web_url(&subject.repository.full_name)
    );

    let discussions = match state
        .forgejo
        .list_issues(token, owner, slug, PAGE_SIZE)
        .await
    {
        Ok(issues) => issues
            .into_iter()
            // A Suggestion is a pull request. It has its own area.
            .filter(|issue| issue.is_discussion())
            .map(|issue| DiscussionRow {
                number: issue.number,
                open: issue.is_open(),
                author: author(&issue.user),
                started: short_date(&issue.created_at),
                comments: issue.comments,
                title: issue.title,
            })
            .collect(),
        Err(error) => {
            tracing::info!(%error, %owner, %slug, "cannot read the Discussions");
            errors.push(
                "The application cannot read the Discussions of this Recipe. Open the Recipe in Forgejo to see them."
                    .to_string(),
            );
            Vec::new()
        }
    };

    respond(DiscussionsTemplate {
        layout: Layout::new(current).on(headers, &here),
        owner: owner.to_string(),
        slug: slug.to_string(),
        title: subject.title,
        areas: subject.areas,
        forgejo_url,
        discussions,
        form_title,
        form_message,
        errors,
    })
}

/// Read and draw one Discussion with its comments.
#[allow(clippy::too_many_arguments)]
async fn render_one(
    state: &AppState,
    headers: &HeaderMap,
    current: Option<&crate::session::CurrentUser>,
    token: Option<&Secret<String>>,
    subject: Subject,
    owner: &str,
    slug: &str,
    number: i64,
    form_message: String,
    mut errors: Vec<String>,
) -> Response {
    let here = format!("/recipes/{owner}/{slug}/discussions/{number}");
    let forgejo_url = format!(
        "{}/issues/{number}",
        state.forgejo.web_url(&subject.repository.full_name)
    );

    let issue = match state.forgejo.issue(token, owner, slug, number).await {
        Ok(issue) => issue,
        Err(error) => {
            tracing::info!(%error, %owner, %slug, number, "cannot read the Discussion");
            return no_discussion();
        }
    };

    // A Suggestion is a pull request. It belongs to the Suggestions area, so
    // this page does not draw it as a Discussion.
    if !issue.is_discussion() {
        tracing::info!(%owner, %slug, number, "this is a Suggestion, not a Discussion");
        return no_discussion();
    }

    let comments = match state
        .forgejo
        .list_issue_comments(token, owner, slug, number)
        .await
    {
        Ok(comments) => comments
            .into_iter()
            .map(|comment| CommentView {
                author: author(&comment.user),
                written: short_date(&comment.created_at),
                body: comment.body,
            })
            .collect(),
        Err(error) => {
            tracing::info!(%error, %owner, %slug, number, "cannot read the comments");
            errors.push(
                "The application cannot read the comments of this Discussion. Open the Recipe in Forgejo to see them."
                    .to_string(),
            );
            Vec::new()
        }
    };

    respond(DiscussionTemplate {
        layout: Layout::new(current).on(headers, &here),
        owner: owner.to_string(),
        slug: slug.to_string(),
        title: subject.title,
        areas: subject.areas,
        forgejo_url,
        discussion: DiscussionView {
            number: issue.number,
            open: issue.is_open(),
            author: author(&issue.user),
            started: short_date(&issue.created_at),
            title: issue.title,
            body: issue.body,
        },
        comments,
        form_message,
        errors,
    })
}

/// The word that Forgejo understands for what the person asked for.
///
/// Anything else gives `None`, so a form can never send a value of its own
/// choosing to Forgejo.
fn wanted_state(value: &str) -> Option<&'static str> {
    match value.trim() {
        "open" => Some("open"),
        "closed" => Some("closed"),
        _ => None,
    }
}

/// The name to show for the person who wrote something.
fn author(user: &Option<ForgejoUser>) -> String {
    user.as_ref()
        .map(|user| user.display_name().to_string())
        .unwrap_or_else(|| "Somebody".to_string())
}

/// The day part of a Forgejo timestamp.
///
/// Forgejo writes RFC 3339, for example `2026-08-26T09:41:00+02:00`. A cook
/// needs the day and not the second.
fn short_date(timestamp: &str) -> String {
    timestamp
        .split('T')
        .next()
        .unwrap_or_default()
        .trim()
        .to_string()
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

    fn repository(has_issues: bool) -> Repository {
        Repository {
            name: "chili".to_string(),
            full_name: "sam/chili".to_string(),
            html_url: String::new(),
            clone_url: String::new(),
            default_branch: "main".to_string(),
            private: false,
            empty: false,
            has_issues,
            owner: RepositoryOwner {
                login: "sam".to_string(),
            },
        }
    }

    #[test]
    fn a_recipe_with_issues_on_has_a_discussions_area() {
        assert_eq!(
            area_href("sam", "chili", &repository(true)),
            Some("/recipes/sam/chili/discussions".to_string())
        );
    }

    #[test]
    fn a_recipe_with_issues_off_has_no_discussions_area() {
        assert_eq!(area_href("sam", "chili", &repository(false)), None);
    }

    #[test]
    fn only_open_and_closed_reach_forgejo() {
        assert_eq!(wanted_state("open"), Some("open"));
        assert_eq!(wanted_state("closed"), Some("closed"));

        for value in ["", "deleted", "OPEN", "open; drop", "true"] {
            assert_eq!(wanted_state(value), None, "`{value}` must not be sent on");
        }
    }

    #[test]
    fn a_timestamp_shows_the_day() {
        assert_eq!(short_date("2026-08-26T09:41:00+02:00"), "2026-08-26");
        assert_eq!(short_date(""), "");
    }

    #[test]
    fn a_missing_author_still_has_a_name() {
        assert_eq!(author(&None), "Somebody");

        let user = ForgejoUser {
            id: 1,
            login: "sam".to_string(),
            full_name: "Sam Cook".to_string(),
            avatar_url: String::new(),
            email: String::new(),
        };
        assert_eq!(author(&Some(user)), "Sam Cook");
    }
}
