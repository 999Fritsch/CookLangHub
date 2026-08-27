//! The Suggestions area of a Recipe, and the editor that makes one.
//!
//! Forgejo holds every Suggestion. This module reads Forgejo, draws what it
//! says, and pushes the work of a person to it through the Git adapter. It
//! keeps nothing.
//!
//! A person who can read a Recipe but not write to it comes here from the
//! editor. They change the text, and the first change becomes a Suggestion.
//! The application saves the work while they write, into the same
//! Suggestion, and the published Recipe does not move at all.
//!
//! The page works with no script. The text area carries the Cooklang and
//! **Save Suggestion** and **Ready for review** are ordinary form buttons.
//! Only the saving while a person writes needs the script, and losing it
//! loses nothing that was already saved.

use std::sync::Arc;

use askama::Template;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use axum_extra::extract::CookieJar;
use futures::StreamExt;
use serde::{Deserialize, Serialize};

use crate::create_recipe::{self, MAIN_BRANCH};
use crate::forgejo::{ForgejoUser, Ownership, PullRequest, Repository};
use crate::git::Identity;
use crate::recipe::{self, MAX_SOURCE_BYTES, RECIPE_FILE};
use crate::render::{self, RenderedRecipe};
use crate::secret::Secret;
use crate::suggestion::{self, Mine, State as SuggestionState, SuggestionError};
use crate::web::{AppState, Layout, MaybeUser};
use crate::web_edit::{Refused, refusal};
use crate::web_history::Comparison;
use crate::web_recipes::{Actor, RecipeArea, actor, areas};

/// Where the Suggestions area of a Recipe lives.
pub fn area_href(owner: &str, slug: &str) -> String {
    format!("/recipes/{owner}/{slug}/suggestions")
}

/// Where a person writes their own Suggestion for a Recipe.
pub fn editor_href(owner: &str, slug: &str) -> String {
    format!("/recipes/{owner}/{slug}/suggest")
}

/// Where the Suggestions of one person come together.
///
/// The area covers every Recipe, so it belongs to the person and not to one
/// Recipe. That is why it has no owner and no name in its address.
pub const INBOX_HREF: &str = "/suggestions";

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(INBOX_HREF, get(inbox))
        .route("/recipes/{owner}/{slug}/suggestions", get(list))
        .route("/recipes/{owner}/{slug}/suggestions/{number}", get(one))
        .route(
            "/recipes/{owner}/{slug}/suggestions/{number}/comments",
            post(write_comment),
        )
        .route(
            "/recipes/{owner}/{slug}/suggestions/{number}/accept",
            post(accept),
        )
        .route(
            "/recipes/{owner}/{slug}/suggestions/{number}/decline",
            post(decline),
        )
        .route(
            "/recipes/{owner}/{slug}/suggest",
            get(editor).post(save_while_writing),
        )
        .route("/recipes/{owner}/{slug}/suggest/save", post(save_form))
}

/// Shown when the Recipe holds no Version yet.
const NO_VERSION_MESSAGE: &str = "This Recipe has no Version yet, so there is nothing to suggest a change to. Open the Recipe in Forgejo.";

/// Shown when the stored file is not text that the application can read.
const NOT_TEXT_MESSAGE: &str = "This Recipe is not UTF-8 text, so the editor cannot open it. Open the Recipe in Forgejo to see the exact content.";

/// Shown when Git does not answer.
const UNREACHABLE_MESSAGE: &str =
    "CookLangHub cannot read this Recipe at the moment. Nothing changed. Try again.";

/// Shown when Forgejo does not give the conversation of a Suggestion.
const NO_CONVERSATION_MESSAGE: &str = "CookLangHub cannot read the conversation of this Suggestion. Open the Recipe in Forgejo to read it there.";

/// Shown when Forgejo does not give the Suggestions of a person.
const NO_INBOX_MESSAGE: &str =
    "CookLangHub cannot read your Suggestions at the moment. Open Forgejo to see them.";

/// Shown when the person can reach more Recipes than one page reads.
const PARTIAL_INBOX_MESSAGE: &str =
    "You can reach more Recipes than this page shows. To see every Suggestion, open Forgejo.";

/// How many Recipes the inbox reads at the same time.
const INBOX_CONCURRENCY: usize = 8;

/// How many Suggestions one list of the inbox shows.
const MAX_INBOX_ROWS: usize = 50;

/// The Recipe that a Suggestion belongs to.
struct Subject {
    repository: Repository,
    /// The title a cook sees. It lives in the Recipe, not in the name.
    title: String,
    areas: Vec<RecipeArea>,
    /// The branch that carries the published Recipe.
    branch: String,
}

impl Subject {
    fn forgejo_url(&self, forgejo: &crate::forgejo::ForgejoClient) -> String {
        forgejo.web_url(&self.repository.full_name)
    }
}

/// Read the Recipe behind a Suggestions page.
///
/// `None` means Forgejo did not give the Recipe to this person: it is gone,
/// it never existed, or they may not read it. Forgejo decides.
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

    let branch = if repository.default_branch.is_empty() {
        MAIN_BRANCH.to_string()
    } else {
        repository.default_branch.clone()
    };

    let title = state
        .forgejo
        .raw_file(token, owner, slug, &branch, RECIPE_FILE)
        .await
        .ok()
        .and_then(|bytes| recipe::parse(&String::from_utf8_lossy(&bytes)).title)
        .unwrap_or_else(|| repository.name.clone());

    let areas = areas(owner, slug, &repository);

    Some(Subject {
        repository,
        title,
        areas,
        branch,
    })
}

/// The answer when a Recipe is not available to this person.
fn no_recipe() -> Response {
    (StatusCode::NOT_FOUND, "This Recipe is not available.").into_response()
}

/// The answer when one Suggestion cannot be shown.
fn no_suggestion() -> Response {
    (StatusCode::NOT_FOUND, "This Suggestion is not available.").into_response()
}

/// One Suggestion in the list.
struct SuggestionRow {
    number: i64,
    title: String,
    author: String,
    made: String,
    state: &'static str,
    /// The class that colours the state. See [`pill`].
    pill: &'static str,
    comments: i64,
    /// Whether the person who is looking made this Suggestion.
    mine: bool,
    /// Whether Forgejo cannot put this Suggestion into the Recipe.
    conflict: bool,
}

/// One comment in the conversation of a Suggestion.
struct CommentView {
    author: String,
    written: String,
    /// The comment, as text.
    ///
    /// Forgejo stores Markdown. The application shows the characters that
    /// the person wrote, escaped, and keeps the line breaks with a CSS rule.
    /// It does not turn Markdown into HTML: text from another person is not
    /// trusted.
    body: String,
}

/// One Suggestion in the inbox of a person.
///
/// The inbox covers every Recipe, so each row has to name the Recipe it
/// belongs to as well as the Suggestion itself.
struct InboxRow {
    owner: String,
    slug: String,
    /// The title a cook reads for the Recipe.
    recipe: String,
    number: i64,
    title: String,
    author: String,
    made: String,
    state: &'static str,
    /// The class that colours the state. See [`pill`].
    pill: &'static str,
    comments: i64,
}

/// One Suggestion on its own page.
struct SuggestionView {
    number: i64,
    title: String,
    author: String,
    made: String,
    state: &'static str,
    /// The class that colours the state. See [`pill`].
    pill: &'static str,
    /// Whether somebody can still write in it.
    open: bool,
    /// Whether an Editor can read it now.
    ready: bool,
    /// The words that go with the Suggestion, as text.
    ///
    /// Forgejo stores Markdown. The application shows the characters that
    /// the person wrote, escaped, and keeps the line breaks with a CSS
    /// rule. It does not turn Markdown into HTML: text from another person
    /// is not trusted.
    note: String,
    comments: i64,
    mine: bool,
    /// Whether this application can write in the Suggestion. A Suggestion
    /// that somebody made outside CookLangHub is changed in Forgejo.
    here: bool,
    /// Whether Forgejo cannot put this Suggestion into the Recipe, because
    /// the Suggestion and the published Recipe changed the same words.
    conflict: bool,
    /// Whether an Editor can accept it now. This says nothing about who is
    /// looking: Forgejo decides that, and [`SuggestionTemplate::can_act`]
    /// carries the answer.
    acceptable: bool,
}

#[derive(Template)]
#[template(path = "suggestions.html")]
struct SuggestionsTemplate {
    layout: Layout,
    owner: String,
    slug: String,
    title: String,
    areas: Vec<RecipeArea>,
    /// Where the Suggestions of this Recipe live in Forgejo.
    forgejo_url: String,
    suggestions: Vec<SuggestionRow>,
    errors: Vec<String>,
}

#[derive(Template)]
#[template(path = "suggestion.html")]
struct SuggestionTemplate {
    layout: Layout,
    owner: String,
    slug: String,
    title: String,
    areas: Vec<RecipeArea>,
    forgejo_url: String,
    errors: Vec<String>,
    /// The Suggestion, when there is one. A person who is about to make
    /// their first one reads the page without it.
    suggestion: Option<SuggestionView>,
    /// Whether this page is the editor of the Suggestion of this person.
    editable: bool,
    /// What a person reads about a Suggestion that was made elsewhere.
    elsewhere: &'static str,
    /// The Cooklang exactly as it is stored.
    source: String,
    /// The published Version the Suggestion is measured against.
    base_version: String,
    /// The Version the Suggestion holds, or empty when there is none yet.
    /// The editor sends it back with each save, and that is what lets a
    /// save from a tab that has fallen behind be refused.
    draft_version: String,
    /// What the person reads about their Suggestion.
    notice: &'static str,
    /// The note that goes with the Suggestion.
    note: String,
    // The review. These carry nothing while a person writes: the editor
    // draws the Suggestion of that person and never the review of it.
    /// What the Suggestion changes in the published Recipe.
    comparison: Comparison,
    /// The conversation, oldest first, as Forgejo holds it.
    comments: Vec<CommentView>,
    /// What the person typed in the comment box, kept when it did not work.
    form_message: String,
    /// Whether Forgejo lets this person accept and decline here.
    can_act: bool,
    /// What a person reads when the two sides changed the same words.
    conflict_note: &'static str,
    /// What a person reads while the Suggestion is still being written.
    not_ready_note: &'static str,
    // The preview fields. `recipe_preview.html` is included here and reads
    // them, so this page and the Recipe page render from one parser.
    preview_title: String,
    cooked: RenderedRecipe,
    warnings: Vec<String>,
    parse_errors: Vec<String>,
}

impl SuggestionTemplate {
    /// Fill in the preview from the Cooklang on the page.
    fn with_preview(mut self, fallback_title: &str) -> Self {
        let parsed = recipe::parse(&self.source);

        self.preview_title = parsed
            .title
            .clone()
            .unwrap_or_else(|| fallback_title.to_string());
        // A source the parser refused cannot be shown as a Recipe, so the
        // preview shows the messages and nothing else.
        self.cooked = recipe::parse_recipe(&self.source)
            .as_ref()
            .map(render::render)
            .unwrap_or_default();
        self.warnings = parsed.warnings.iter().map(|d| d.message.clone()).collect();
        self.parse_errors = parsed.errors.iter().map(|d| d.message.clone()).collect();
        self
    }
}

/// The Suggestions area of a Recipe.
async fn list(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug)): Path<(String, String)>,
) -> Response {
    let token = crate::web::viewer_token(&state, &jar).await;
    let Some(subject) = subject(&state, token.as_ref(), &owner, &slug).await else {
        return no_recipe();
    };

    let me = current
        .as_ref()
        .map(|user| user.login.clone())
        .unwrap_or_default();

    let mut errors = Vec::new();
    let suggestions = match suggestion::list(&state.forgejo, token.as_ref(), &owner, &slug).await {
        Ok(found) => found
            .into_iter()
            .map(|pull| SuggestionRow {
                number: pull.number,
                mine: !me.is_empty() && pull.author() == me,
                author: author(&pull.user),
                made: short_date(&pull.created_at),
                state: suggestion::state_of(&pull).label(),
                pill: pill(suggestion::state_of(&pull)),
                comments: pull.comments,
                conflict: suggestion::has_conflict(&pull),
                title: suggestion::plain_title(&pull.title).to_string(),
            })
            .collect(),
        Err(error) => {
            tracing::info!(%error, %owner, %slug, "cannot read the Suggestions");
            errors.push(
                "CookLangHub cannot read the Suggestions of this Recipe. Open the Recipe in Forgejo to see them."
                    .to_string(),
            );
            Vec::new()
        }
    };

    respond(SuggestionsTemplate {
        layout: Layout::new(current.as_ref()).on(&headers, &area_href(&owner, &slug)),
        forgejo_url: format!("{}/pulls", subject.forgejo_url(&state.forgejo)),
        owner: owner.clone(),
        slug: slug.clone(),
        title: subject.title,
        areas: subject.areas,
        suggestions,
        errors,
    })
}

/// One Suggestion, as anybody who can read the Recipe sees it.
///
/// This is the review. It carries the Changes, the conversation, and the two
/// actions of an Editor.
async fn one(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug, number)): Path<(String, String, i64)>,
) -> Response {
    let token = crate::web::viewer_token(&state, &jar).await;

    review(
        &state,
        &headers,
        current.as_ref(),
        token.as_ref(),
        &owner,
        &slug,
        number,
        String::new(),
        Vec::new(),
    )
    .await
}

/// Read one Suggestion and draw the review of it.
///
/// Everything on the page comes from Forgejo and from Git. The application
/// keeps no part of a Suggestion, so a page drawn here can only report.
#[allow(clippy::too_many_arguments)]
async fn review(
    state: &AppState,
    headers: &HeaderMap,
    current: Option<&crate::session::CurrentUser>,
    token: Option<&Secret<String>>,
    owner: &str,
    slug: &str,
    number: i64,
    form_message: String,
    mut errors: Vec<String>,
) -> Response {
    let Some(subject) = subject(state, token, owner, slug).await else {
        return no_recipe();
    };

    let pull = match state.forgejo.pull_request(token, owner, slug, number).await {
        Ok(pull) => pull,
        Err(error) => {
            tracing::info!(%error, %owner, %slug, number, "cannot read the Suggestion");
            return no_suggestion();
        }
    };

    let me = current.map(|user| user.login.clone()).unwrap_or_default();

    // The Cooklang that the Suggestion proposes. A Recipe that this
    // application cannot read is not drawn as one.
    let source = state
        .forgejo
        .raw_file(token, owner, slug, &pull.head.sha, RECIPE_FILE)
        .await
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_default();

    // What the Recipe becomes if an Editor accepts this. Forgejo names the
    // published Version on the base side, so the comparison answers the
    // question the Editor actually has, and not what was true when the
    // person started to write.
    let published = if pull.base.sha.is_empty() {
        subject.branch.clone()
    } else {
        pull.base.sha.clone()
    };
    let (comparison, trouble) =
        crate::web_history::comparison(state, token, owner, slug, &published, &pull.head.sha).await;
    errors.extend(trouble);

    // Forgejo keeps the conversation of a Suggestion with the conversations
    // of the Recipe, so this is the same reader that a Discussion uses.
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
            tracing::info!(%error, %owner, %slug, number, "cannot read the conversation");
            errors.push(NO_CONVERSATION_MESSAGE.to_string());
            Vec::new()
        }
    };

    // Forgejo decides who may accept and who may decline. The application
    // asks it and works nothing out for itself.
    let can_act = match token {
        Some(token) => state
            .forgejo
            .can_write(token, owner, slug)
            .await
            .unwrap_or(false),
        None => false,
    };

    let view = view_of(&pull, &me);

    respond(
        SuggestionTemplate {
            layout: Layout::new(current).on(
                headers,
                &format!("/recipes/{owner}/{slug}/suggestions/{number}"),
            ),
            forgejo_url: format!("{}/pulls/{number}", subject.forgejo_url(&state.forgejo)),
            owner: owner.to_string(),
            slug: slug.to_string(),
            title: subject.title.clone(),
            areas: subject.areas,
            errors,
            suggestion: Some(view),
            editable: false,
            elsewhere: suggestion::ELSEWHERE_MESSAGE,
            source,
            base_version: String::new(),
            draft_version: String::new(),
            notice: "",
            note: String::new(),
            comparison,
            comments,
            form_message,
            can_act,
            conflict_note: suggestion::CONFLICT_MESSAGE,
            not_ready_note: suggestion::NOT_READY_MESSAGE,
            preview_title: String::new(),
            cooked: RenderedRecipe::default(),
            warnings: Vec::new(),
            parse_errors: Vec::new(),
        }
        .with_preview(&subject.title),
    )
}

/// What a person reads about one Suggestion.
fn view_of(pull: &PullRequest, me: &str) -> SuggestionView {
    let state = suggestion::state_of(pull);

    SuggestionView {
        number: pull.number,
        title: suggestion::plain_title(&pull.title).to_string(),
        author: author(&pull.user),
        made: short_date(&pull.created_at),
        state: state.label(),
        pill: pill(state),
        open: state.is_open(),
        ready: state == SuggestionState::Ready,
        note: pull.body.clone(),
        comments: pull.comments,
        mine: !me.is_empty() && pull.author() == me,
        here: pull.is_agit(),
        conflict: suggestion::has_conflict(pull),
        acceptable: suggestion::can_accept(pull),
    }
}

/// The CookCLI pill that colours one state.
///
/// One class for one state, and never two colour classes on one element.
fn pill(state: SuggestionState) -> &'static str {
    match state {
        SuggestionState::Editing => "metadata-difficulty",
        SuggestionState::Ready => "metadata-cuisine",
        SuggestionState::Accepted => "metadata-course",
        SuggestionState::Declined => "metadata-servings",
    }
}

// ---------------------------------------------------------------------
// The conversation, and the two actions of an Editor
// ---------------------------------------------------------------------

/// What the comment form sends.
#[derive(Debug, Deserialize)]
struct CommentForm {
    #[serde(default)]
    message: String,
}

/// Write a comment on a Suggestion.
///
/// Forgejo decides who may write here, and it lets anybody with read access
/// do it. That is what makes a review a conversation rather than a verdict:
/// a person who cannot change the Recipe can still say what they think of a
/// Suggestion for it.
async fn write_comment(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug, number)): Path<(String, String, i64)>,
    Form(form): Form<CommentForm>,
) -> Response {
    let Some(token) = crate::web::viewer_token(&state, &jar).await else {
        return Redirect::to("/auth/sign-in").into_response();
    };

    // A comment is a conversation and not a note on a Suggestion, so it is
    // as long as the person needs. This is what a Discussion does as well.
    let message = form.message.trim().to_string();

    if message.is_empty() {
        return review(
            &state,
            &headers,
            current.as_ref(),
            Some(&token),
            &owner,
            &slug,
            number,
            String::new(),
            vec![suggestion::EMPTY_COMMENT_MESSAGE.to_string()],
        )
        .await;
    }

    match state
        .forgejo
        .create_issue_comment(&token, &owner, &slug, number, &message)
        .await
    {
        Ok(_) => {
            tracing::info!(%owner, %slug, number, "wrote a comment on a Suggestion");
            Redirect::to(&format!("/recipes/{owner}/{slug}/suggestions/{number}")).into_response()
        }
        Err(error) => {
            tracing::info!(%error, %owner, %slug, number, "cannot write the comment");
            review(
                &state,
                &headers,
                current.as_ref(),
                Some(&token),
                &owner,
                &slug,
                number,
                message,
                vec![suggestion::REFUSED_MESSAGE.to_string()],
            )
            .await
        }
    }
}

/// What went wrong with an action, in words a cook reads.
fn refused(error: &SuggestionError) -> (StatusCode, &'static str) {
    match error {
        SuggestionError::Conflict => (StatusCode::CONFLICT, suggestion::CONFLICT_MESSAGE),
        SuggestionError::NotReady => (StatusCode::CONFLICT, suggestion::NOT_READY_MESSAGE),
        SuggestionError::Gone => (StatusCode::CONFLICT, suggestion::ALREADY_MESSAGE),
        _ => (StatusCode::BAD_GATEWAY, suggestion::REFUSED_MESSAGE),
    }
}

/// Accept a Suggestion, so that it becomes one published Version.
async fn accept(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug, number)): Path<(String, String, i64)>,
) -> Response {
    act(
        &state,
        &headers,
        current.as_ref(),
        &jar,
        &owner,
        &slug,
        number,
        Deed::Accept,
    )
    .await
}

/// Decline a Suggestion, and keep every word of it.
async fn decline(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug, number)): Path<(String, String, i64)>,
) -> Response {
    act(
        &state,
        &headers,
        current.as_ref(),
        &jar,
        &owner,
        &slug,
        number,
        Deed::Decline,
    )
    .await
}

/// What an Editor asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Deed {
    Accept,
    Decline,
}

/// Do what an Editor asked, or say why nothing happened.
///
/// Both actions need the same three answers first: the Recipe, the
/// Suggestion, and whether Forgejo lets this person write here. The check
/// happens on the request and not only on the page, because a request can
/// arrive without the page in front of it.
#[allow(clippy::too_many_arguments)]
async fn act(
    state: &AppState,
    headers: &HeaderMap,
    current: Option<&crate::session::CurrentUser>,
    jar: &CookieJar,
    owner: &str,
    slug: &str,
    number: i64,
    deed: Deed,
) -> Response {
    let Some(actor) = actor(state, jar).await else {
        return Redirect::to("/auth/sign-in").into_response();
    };

    let Some(subject) = subject(state, Some(&actor.token), owner, slug).await else {
        return no_recipe();
    };

    let pull = match state
        .forgejo
        .pull_request(Some(&actor.token), owner, slug, number)
        .await
    {
        Ok(pull) => pull,
        Err(error) => {
            tracing::info!(%error, %owner, %slug, number, "cannot read the Suggestion");
            return no_suggestion();
        }
    };

    // Forgejo owns this decision. A person who may only read the Recipe can
    // comment on a Suggestion for it, and can do nothing else.
    let can_act = state
        .forgejo
        .can_write(&actor.token, owner, slug)
        .await
        .unwrap_or(false);

    if !can_act {
        tracing::info!(%owner, %slug, number, login = %actor.user.login, "a person without write access asked to act on a Suggestion");
        let body = review(
            state,
            headers,
            current,
            Some(&actor.token),
            owner,
            slug,
            number,
            String::new(),
            vec![suggestion::NO_ACCESS_MESSAGE.to_string()],
        )
        .await;
        return (StatusCode::FORBIDDEN, body).into_response();
    }

    let done = match deed {
        Deed::Accept => {
            suggestion::accept(
                &state.forgejo,
                &actor.token,
                owner,
                slug,
                &pull,
                &subject.title,
            )
            .await
        }
        Deed::Decline => {
            suggestion::decline(&state.forgejo, &actor.token, owner, slug, &pull).await
        }
    };

    match done {
        Ok(()) => {
            tracing::info!(%owner, %slug, number, ?deed, "acted on a Suggestion");
            Redirect::to(&format!("/recipes/{owner}/{slug}/suggestions/{number}")).into_response()
        }
        Err(error) => {
            let (status, message) = refused(&error);
            tracing::info!(%error, %owner, %slug, number, ?deed, "cannot act on the Suggestion");
            let body = review(
                state,
                headers,
                current,
                Some(&actor.token),
                owner,
                slug,
                number,
                String::new(),
                vec![message.to_string()],
            )
            .await;
            (status, body).into_response()
        }
    }
}

/// Everything the editor needs before it can draw a page.
struct Opened {
    subject: Subject,
    /// The published Version the Suggestion is measured against.
    base_version: String,
    /// The Suggestion of this person, when they have one.
    pull: Option<PullRequest>,
}

/// Why the editor of a Suggestion cannot open.
///
/// This is what happened and not a page, so both ways a person asks get the
/// same reason: one draws the diagnosis, and the other answers the editor
/// in words while it saves.
enum Stop {
    /// Forgejo does not give this Recipe to this person: it is gone, it
    /// never existed, or they may not read it.
    NoRecipe,
    /// The Recipe is there, and the interface cannot handle its state.
    Blocked {
        status: StatusCode,
        message: &'static str,
        forgejo_url: String,
    },
}

impl Stop {
    /// What happened, in words a cook reads, and how hard it is.
    fn words(&self) -> (StatusCode, &'static str) {
        match self {
            Self::NoRecipe => (StatusCode::NOT_FOUND, "This Recipe is not available."),
            Self::Blocked {
                status, message, ..
            } => (*status, message),
        }
    }
}

/// Turn a stop into the page that the person reads.
///
/// A state the interface cannot handle is diagnosed and offers **Open in
/// Forgejo**. It is never repaired here.
fn stopped(layout: Layout, owner: &str, slug: &str, stop: Stop) -> Response {
    match stop {
        Stop::NoRecipe => no_recipe(),
        Stop::Blocked {
            status,
            message,
            forgejo_url,
        } => refusal(
            layout,
            owner,
            slug,
            Refused::Blocked {
                status,
                message,
                forgejo_url,
            },
        ),
    }
}

/// Read the Recipe, the published Version, and the Suggestion of a person.
async fn open(state: &AppState, actor: &Actor, owner: &str, slug: &str) -> Result<Opened, Stop> {
    let Some(subject) = subject(state, Some(&actor.token), owner, slug).await else {
        return Err(Stop::NoRecipe);
    };

    let forgejo_url = subject.forgejo_url(&state.forgejo);
    let blocked = |status: StatusCode, message: &'static str| Stop::Blocked {
        status,
        message,
        forgejo_url: forgejo_url.clone(),
    };

    // An archived Recipe is read-only. Forgejo refuses to hold a Suggestion
    // for one, so the editor says so before a person types instead of after.
    if subject.repository.archived {
        tracing::info!(%owner, %slug, "a Suggestion was asked for on an archived Recipe");
        return Err(blocked(
            StatusCode::LOCKED,
            crate::archive::ARCHIVED_MESSAGE,
        ));
    }

    // Git holds History, so Git says which Version is published now.
    let remote = state.forgejo.git_url(&format!("{owner}/{slug}"));
    let base_version = match state
        .git
        .branch_head(&remote, &actor.token, &subject.branch)
        .await
    {
        Ok(Some(version)) => version,
        Ok(None) => return Err(blocked(StatusCode::CONFLICT, NO_VERSION_MESSAGE)),
        Err(error) => {
            tracing::warn!(%error, %owner, %slug, "cannot read the published Version");
            return Err(blocked(StatusCode::BAD_GATEWAY, UNREACHABLE_MESSAGE));
        }
    };

    let pull = match suggestion::mine(&state.forgejo, &actor.token, owner, slug, &actor.user.login)
        .await
    {
        Ok(Mine::None) => None,
        Ok(Mine::One(pull)) => Some(*pull),
        // The interface cannot say which Suggestion to write in, and it
        // must not guess. Say so, and offer the tool that can act on it.
        Ok(Mine::Several) => {
            tracing::info!(%owner, %slug, login = %actor.user.login, "this person has more than one open Suggestion");
            return Err(blocked(StatusCode::CONFLICT, suggestion::TOO_MANY_MESSAGE));
        }
        Err(error) => {
            tracing::warn!(%error, %owner, %slug, "cannot read the Suggestions of this person");
            return Err(blocked(StatusCode::BAD_GATEWAY, UNREACHABLE_MESSAGE));
        }
    };

    Ok(Opened {
        subject,
        base_version,
        pull,
    })
}

/// Show the editor for the Suggestion of this person.
async fn editor(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug)): Path<(String, String)>,
) -> Response {
    let Some(actor) = actor(&state, &jar).await else {
        return Redirect::to("/auth/sign-in").into_response();
    };

    let layout = || Layout::new(current.as_ref()).on(&headers, &editor_href(&owner, &slug));

    let opened = match open(&state, &actor, &owner, &slug).await {
        Ok(opened) => opened,
        Err(stop) => return stopped(layout(), &owner, &slug, stop),
    };

    // A Suggestion comes first. The person left unfinished work here,
    // possibly on another device, and the editor has to open on that and
    // not on the published Recipe.
    let read_at = match &opened.pull {
        Some(pull) => pull.head.sha.clone(),
        None => opened.base_version.clone(),
    };

    let bytes = match state
        .forgejo
        .raw_file(Some(&actor.token), &owner, &slug, &read_at, RECIPE_FILE)
        .await
    {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::info!(%error, %owner, %slug, "cannot read the Recipe file");
            Vec::new()
        }
    };

    // A Recipe written through this application is always UTF-8 text. Git
    // accepts any bytes, so a direct push can put something else there.
    let Ok(source) = std::str::from_utf8(&bytes) else {
        tracing::info!(%owner, %slug, "the Recipe file is not UTF-8 text");
        return stopped(
            layout(),
            &owner,
            &slug,
            Stop::Blocked {
                status: StatusCode::CONFLICT,
                message: NOT_TEXT_MESSAGE,
                forgejo_url: opened.subject.forgejo_url(&state.forgejo),
            },
        );
    };

    let note = opened
        .pull
        .as_ref()
        .map(|pull| pull.body.clone())
        .unwrap_or_default();

    page(
        &state,
        &headers,
        current.as_ref(),
        opened,
        &owner,
        &slug,
        source.to_string(),
        note,
        &actor.user.login,
        Vec::new(),
    )
}

/// Draw the editor of a Suggestion.
#[allow(clippy::too_many_arguments)]
fn page(
    state: &AppState,
    headers: &HeaderMap,
    current: Option<&crate::session::CurrentUser>,
    opened: Opened,
    owner: &str,
    slug: &str,
    source: String,
    note: String,
    me: &str,
    errors: Vec<String>,
) -> Response {
    let view = opened.pull.as_ref().map(|pull| view_of(pull, me));
    let number = opened.pull.as_ref().map(|pull| pull.number);

    let forgejo_url = match number {
        Some(number) => format!(
            "{}/pulls/{number}",
            opened.subject.forgejo_url(&state.forgejo)
        ),
        None => opened.subject.forgejo_url(&state.forgejo),
    };

    respond(
        SuggestionTemplate {
            layout: Layout::new(current).on(headers, &editor_href(owner, slug)),
            owner: owner.to_string(),
            slug: slug.to_string(),
            title: opened.subject.title.clone(),
            areas: opened.subject.areas,
            forgejo_url,
            errors,
            suggestion: view,
            editable: true,
            elsewhere: suggestion::ELSEWHERE_MESSAGE,
            source,
            base_version: opened.base_version.clone(),
            draft_version: opened
                .pull
                .as_ref()
                .map(|pull| pull.head.sha.clone())
                .unwrap_or_default(),
            notice: if opened.pull.is_some() {
                suggestion::NOTICE_MESSAGE
            } else {
                suggestion::NEW_MESSAGE
            },
            note,
            // The editor draws no review. The person who is writing follows
            // the link on the page to read their work as an Editor does.
            comparison: Comparison { groups: Vec::new() },
            comments: Vec::new(),
            form_message: String::new(),
            can_act: false,
            conflict_note: suggestion::CONFLICT_MESSAGE,
            not_ready_note: suggestion::NOT_READY_MESSAGE,
            preview_title: String::new(),
            cooked: RenderedRecipe::default(),
            warnings: Vec::new(),
            parse_errors: Vec::new(),
        }
        .with_preview(&opened.subject.title),
    )
}

/// What the editor sends when it saves.
#[derive(Debug, Deserialize)]
struct SaveForm {
    #[serde(default)]
    source: String,
    /// The published Version the Suggestion is measured against.
    #[serde(default)]
    base_version: String,
    /// The Version the Suggestion held when this page opened. Empty when
    /// the person had no Suggestion then.
    #[serde(default)]
    draft_version: String,
    /// The words that go with the Suggestion.
    #[serde(default)]
    note: String,
    /// `save` or `ready`.
    #[serde(default)]
    action: String,
}

/// What the editor reads back from a save.
#[derive(Debug, Serialize)]
struct Answer {
    /// The Version to send with the next save.
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

/// What went wrong, in words a cook reads, and how hard it is.
fn trouble(error: &SuggestionError) -> (StatusCode, &'static str) {
    match error {
        SuggestionError::Stale => (StatusCode::CONFLICT, suggestion::STALE_MESSAGE),
        SuggestionError::Gone => (StatusCode::CONFLICT, suggestion::GONE_MESSAGE),
        SuggestionError::TooMany => (StatusCode::CONFLICT, suggestion::TOO_MANY_MESSAGE),
        SuggestionError::NoTopic => (StatusCode::CONFLICT, suggestion::NO_TOPIC_MESSAGE),
        _ => (StatusCode::BAD_GATEWAY, suggestion::UNSAVED_MESSAGE),
    }
}

/// Everything a save needs, once the request is checked.
struct Ready {
    source: String,
    identity: Identity,
    opened: Opened,
}

/// Check the request and read what the save needs.
async fn prepare(
    state: &AppState,
    actor: &Actor,
    owner: &str,
    slug: &str,
    form: &SaveForm,
) -> Result<Ready, (StatusCode, &'static str)> {
    if !crate::draft::is_version(form.base_version.trim()) {
        return Err((
            StatusCode::BAD_REQUEST,
            "CookLangHub does not know which Version you started from. Open the Recipe again.",
        ));
    }

    let expected = form.draft_version.trim();
    if !expected.is_empty() && !crate::draft::is_version(expected) {
        return Err((StatusCode::CONFLICT, suggestion::STALE_MESSAGE));
    }

    if form.source.len() > MAX_SOURCE_BYTES {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            "The Recipe source is larger than 1 MB, so CookLangHub did not save your Suggestion.",
        ));
    }

    // A save answers in words rather than with a page, so the reason comes
    // back as words. It is the same reason either way.
    let opened = open(state, actor, owner, slug)
        .await
        .map_err(|stop| stop.words())?;

    // A Suggestion is replaced whole on the next keystroke, and it is
    // readable by anybody who can read the Recipe, so it carries the
    // no-reply address always. An address that is published by accident
    // cannot be taken back.
    let identity = Identity {
        name: actor.user.display_name().to_string(),
        email: create_recipe::commit_email(
            &actor.user.login,
            &actor.user.email,
            true,
            &state.forgejo_noreply_domain,
        ),
    };

    Ok(Ready {
        source: normalize(&form.source),
        identity,
        opened,
    })
}

/// Save the work of this person into their Suggestion.
async fn keep(
    state: &AppState,
    actor: &Actor,
    owner: &str,
    slug: &str,
    ready: &Ready,
    expected: Option<&str>,
) -> Result<suggestion::Saved, SuggestionError> {
    suggestion::save(suggestion::Save {
        forgejo: &state.forgejo,
        git: state.git.as_ref(),
        token: &actor.token,
        user: &actor.user,
        identity: &ready.identity,
        owner,
        slug,
        branch: &ready.opened.subject.branch,
        source: &ready.source,
        base_version: ready.opened.base_version.trim(),
        expected,
        recipe_title: &ready.opened.subject.title,
    })
    .await
}

/// Save the Suggestion while a person writes.
///
/// The editor posts here. Nothing is kept in the browser, so the answer
/// carries the Version that the next save must send back. That value is the
/// whole of the check: when the Suggestion no longer holds it, somebody
/// wrote first and this save is refused.
async fn save_while_writing(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
    Path((owner, slug)): Path<(String, String)>,
    Form(form): Form<SaveForm>,
) -> Response {
    let Some(actor) = actor(&state, &jar).await else {
        return answer(
            StatusCode::UNAUTHORIZED,
            "",
            "Sign in again to save your Suggestion.",
        );
    };

    let expected = form.draft_version.trim().to_string();

    let ready = match prepare(&state, &actor, &owner, &slug, &form).await {
        Ok(ready) => ready,
        Err((status, message)) => return answer(status, &expected, message),
    };

    match keep(
        &state,
        &actor,
        &owner,
        &slug,
        &ready,
        (!expected.is_empty()).then_some(expected.as_str()),
    )
    .await
    {
        Ok(saved) => {
            tracing::info!(%owner, %slug, number = saved.number, created = saved.created, "saved a Suggestion");
            answer(StatusCode::OK, &saved.version, suggestion::SAVED_MESSAGE)
        }
        Err(error) => {
            let (status, message) = trouble(&error);
            tracing::info!(%error, %owner, %slug, "cannot save a Suggestion");
            answer(status, &expected, message)
        }
    }
}

/// Save the Suggestion from the form, and mark it ready when asked.
///
/// This is the path that needs no script at all.
async fn save_form(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug)): Path<(String, String)>,
    Form(form): Form<SaveForm>,
) -> Response {
    let Some(actor) = actor(&state, &jar).await else {
        return Redirect::to("/auth/sign-in").into_response();
    };

    let expected = form.draft_version.trim().to_string();
    let note = clean_note(&form.note);
    let wants_review = form.action.trim() == "ready";

    let ready = match prepare(&state, &actor, &owner, &slug, &form).await {
        Ok(ready) => ready,
        Err((_, message)) => {
            return retry(
                &state,
                &headers,
                current.as_ref(),
                &actor,
                &owner,
                &slug,
                &form,
                vec![message.to_string()],
            )
            .await;
        }
    };

    let saved = match keep(
        &state,
        &actor,
        &owner,
        &slug,
        &ready,
        (!expected.is_empty()).then_some(expected.as_str()),
    )
    .await
    {
        Ok(saved) => saved,
        Err(error) => {
            let (_, message) = trouble(&error);
            tracing::info!(%error, %owner, %slug, "cannot save a Suggestion");
            return retry(
                &state,
                &headers,
                current.as_ref(),
                &actor,
                &owner,
                &slug,
                &form,
                vec![message.to_string()],
            )
            .await;
        }
    };

    // The state and the words of a Suggestion are the title and the body
    // that Forgejo holds, so both are written there and nowhere else.
    //
    // A failure here is told to the person rather than hidden. The work is
    // saved either way, so what they lose is the mark and not the text.
    if wants_review || !note.is_empty() {
        let written = match state
            .forgejo
            .pull_request(Some(&actor.token), &owner, &slug, saved.number)
            .await
        {
            Ok(pull) => {
                let already_ready = !suggestion::is_editing(&pull.title);
                suggestion::set_state(
                    &state.forgejo,
                    &actor.token,
                    &owner,
                    &slug,
                    &pull,
                    wants_review || already_ready,
                    &note,
                )
                .await
            }
            Err(error) => Err(error.into()),
        };

        if let Err(error) = written {
            tracing::warn!(%error, %owner, %slug, number = saved.number, "cannot change the state of the Suggestion");
            return retry(
                &state,
                &headers,
                current.as_ref(),
                &actor,
                &owner,
                &slug,
                &form,
                vec![suggestion::REFUSED_MESSAGE.to_string()],
            )
            .await;
        }
    }

    tracing::info!(%owner, %slug, number = saved.number, ready = wants_review, "saved a Suggestion from the form");

    Redirect::to(&format!(
        "/recipes/{owner}/{slug}/suggestions/{}",
        saved.number
    ))
    .into_response()
}

/// Draw the editor again with the text of the person and a reason.
#[allow(clippy::too_many_arguments)]
async fn retry(
    state: &AppState,
    headers: &HeaderMap,
    current: Option<&crate::session::CurrentUser>,
    actor: &Actor,
    owner: &str,
    slug: &str,
    form: &SaveForm,
    errors: Vec<String>,
) -> Response {
    match open(state, actor, owner, slug).await {
        Ok(opened) => page(
            state,
            headers,
            current,
            opened,
            owner,
            slug,
            normalize(&form.source),
            clean_note(&form.note),
            &actor.user.login,
            errors,
        ),
        Err(stop) => stopped(
            Layout::new(current).on(headers, &editor_href(owner, slug)),
            owner,
            slug,
            stop,
        ),
    }
}

// ---------------------------------------------------------------------
// The inbox: every Suggestion that one person has to do with
// ---------------------------------------------------------------------

#[derive(Template)]
#[template(path = "suggestions_inbox.html")]
struct InboxTemplate {
    layout: Layout,
    /// The Suggestions that wait for this person to read them.
    review: Vec<InboxRow>,
    /// The Suggestions that this person made.
    mine: Vec<InboxRow>,
    /// Where the Suggestions of this person live in Forgejo.
    forgejo_url: String,
    /// Something a person must know that is not a failure.
    notice: String,
    errors: Vec<String>,
}

/// The Suggestions area of a person: one place for both directions.
///
/// **Needs my review** is what somebody else proposes to a Recipe that
/// Forgejo lets this person write to. **My suggestions** is what this person
/// proposed anywhere, and that second list cannot come from the Recipes of
/// this person: a Reader suggests a change to a Recipe that they neither own
/// nor work on, and that is the ordinary case.
async fn inbox(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
) -> Response {
    let Some(actor) = actor(&state, &jar).await else {
        return Redirect::to("/auth/sign-in").into_response();
    };

    let mut errors = Vec::new();
    let mut notice = String::new();

    let mine = mine_rows(&state, &actor, &mut errors).await;
    let review = review_rows(&state, &actor, &mut errors, &mut notice).await;

    respond(InboxTemplate {
        layout: Layout::new(current.as_ref()).on(&headers, INBOX_HREF),
        forgejo_url: format!("{}/pulls", state.forgejo.public_url()),
        review,
        mine,
        notice,
        errors,
    })
}

/// The Suggestions that this person made, newest first.
async fn mine_rows(state: &AppState, actor: &Actor, errors: &mut Vec<String>) -> Vec<InboxRow> {
    let found = match suggestion::made_by(&state.forgejo, &actor.token, &actor.user.login).await {
        Ok(found) => found,
        Err(error) => {
            tracing::info!(%error, login = %actor.user.login, "cannot read the Suggestions of this person");
            errors.push(NO_INBOX_MESSAGE.to_string());
            return Vec::new();
        }
    };

    let mut rows = Vec::new();
    for one in newest_first(found).into_iter().take(MAX_INBOX_ROWS) {
        let owner = one.owner().to_string();
        let slug = one.slug().to_string();
        let held = suggestion::found_state(&one);

        rows.push(InboxRow {
            recipe: recipe_title(state, &owner, &slug).await,
            owner,
            slug,
            number: one.number,
            title: suggestion::plain_title(&one.title).to_string(),
            author: author(&one.user),
            made: short_date(&one.created_at),
            state: held.label(),
            pill: pill(held),
            comments: one.comments,
        });
    }

    rows
}

/// Put the Suggestions that a search found in the order a person reads them.
///
/// The one that moved last comes first, because that is the one with news
/// in it. Forgejo writes both moments, and the moment it was made stands in
/// when the other one is missing.
fn newest_first(
    mut found: Vec<crate::forgejo::FoundPullRequest>,
) -> Vec<crate::forgejo::FoundPullRequest> {
    found.sort_by(|left, right| {
        let moment = |one: &crate::forgejo::FoundPullRequest| {
            if one.updated_at.is_empty() {
                one.created_at.clone()
            } else {
                one.updated_at.clone()
            }
        };
        moment(right).cmp(&moment(left))
    });
    found
}

/// The Suggestions that wait for this person to read them.
///
/// An Editor is somebody Forgejo lets write to the Recipe, so the Recipes to
/// look in are the ones this person owns or works on. Forgejo names them,
/// and Forgejo answers the write question for each Recipe that has something
/// waiting in it.
async fn review_rows(
    state: &AppState,
    actor: &Actor,
    errors: &mut Vec<String>,
    notice: &mut String,
) -> Vec<InboxRow> {
    let (repositories, truncated) = match crate::index::visible(
        &state.forgejo,
        Some(&actor.token),
        Ownership::ReachableBy(actor.user.id),
    )
    .await
    {
        Ok(found) => found,
        Err(error) => {
            tracing::info!(%error, login = %actor.user.login, "cannot ask Forgejo for the Recipes of this person");
            errors.push(NO_INBOX_MESSAGE.to_string());
            return Vec::new();
        }
    };

    // A short answer is not proof that nothing else exists, so a person who
    // reaches more Recipes than this reads must be told so.
    if truncated {
        *notice = PARTIAL_INBOX_MESSAGE.to_string();
    }

    let reads = repositories.into_iter().map(|repository| async move {
        let found = suggestion::list(
            &state.forgejo,
            Some(&actor.token),
            &repository.owner.login,
            &repository.name,
        )
        .await;
        (repository, found)
    });

    let answers: Vec<(
        Repository,
        Result<Vec<PullRequest>, crate::forgejo::ForgejoError>,
    )> = futures::stream::iter(reads)
        .buffer_unordered(INBOX_CONCURRENCY)
        .collect()
        .await;

    // A Suggestion that this person is writing themselves is not one they
    // review, and one that is still being written is not ready to be read.
    let mut waiting: Vec<(Repository, PullRequest)> = Vec::new();
    for (repository, found) in answers {
        let Ok(found) = found else {
            tracing::info!(
                repository = %repository.full_name,
                "cannot read the Suggestions of this Recipe for the inbox"
            );
            continue;
        };

        for pull in found {
            if suggestion::state_of(&pull) == SuggestionState::Ready
                && pull.author() != actor.user.login
            {
                waiting.push((repository.clone(), pull));
            }
        }
    }

    // The write question is asked once for each Recipe that has something
    // waiting, so the number of questions follows the Suggestions and never
    // the number of Recipes.
    let mut allowed: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
    for (repository, _) in &waiting {
        if allowed.contains_key(&repository.full_name) {
            continue;
        }
        let answer = state
            .forgejo
            .can_write(&actor.token, &repository.owner.login, &repository.name)
            .await
            .unwrap_or(false);
        allowed.insert(repository.full_name.clone(), answer);
    }

    waiting.retain(|(repository, _)| allowed.get(&repository.full_name).copied().unwrap_or(false));
    waiting.sort_by(|(_, left), (_, right)| right.updated_at.cmp(&left.updated_at));

    let mut rows = Vec::new();
    for (repository, pull) in waiting.into_iter().take(MAX_INBOX_ROWS) {
        let owner = repository.owner.login.clone();
        let slug = repository.name.clone();
        let held = suggestion::state_of(&pull);

        rows.push(InboxRow {
            recipe: recipe_title(state, &owner, &slug).await,
            owner,
            slug,
            number: pull.number,
            title: suggestion::plain_title(&pull.title).to_string(),
            author: author(&pull.user),
            made: short_date(&pull.created_at),
            state: held.label(),
            pill: pill(held),
            comments: pull.comments,
        });
    }

    rows
}

/// The title a cook reads for a Recipe.
///
/// The title lives in the Cooklang metadata, so a list without the index
/// would cost one read of every Recipe. This is exactly what the index is
/// for, and a Recipe that it does not hold yet shows the name that Forgejo
/// holds instead.
async fn recipe_title(state: &AppState, owner: &str, slug: &str) -> String {
    match crate::index::get(&state.pool, owner, slug).await {
        Ok(Some(entry)) => entry.title,
        Ok(None) => slug.to_string(),
        Err(error) => {
            tracing::warn!(%error, %owner, %slug, "cannot read the Recipe index");
            slug.to_string()
        }
    }
}

/// Make the text the browser sent into the text the person typed.
///
/// A form sends every line break as CR LF. Keeping them would make the
/// Suggestion differ from the published Recipe on every line.
fn normalize(source: &str) -> String {
    source.replace("\r\n", "\n").replace('\r', "\n")
}

/// The longest note that becomes the words of a Suggestion.
const MAX_NOTE_CHARS: usize = 500;

/// Make a note fit on a Suggestion.
fn clean_note(note: &str) -> String {
    note.trim()
        .chars()
        .take(MAX_NOTE_CHARS)
        .collect::<String>()
        .trim()
        .to_string()
}

/// The name to show for the person who made something.
fn author(user: &Option<ForgejoUser>) -> String {
    user.as_ref()
        .map(|user| user.display_name().to_string())
        .unwrap_or_else(|| "Somebody".to_string())
}

/// The day part of a Forgejo timestamp.
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

    #[test]
    fn the_areas_of_a_recipe_are_where_this_module_says() {
        assert_eq!(
            area_href("sam", "chili"),
            "/recipes/sam/chili/suggestions".to_string()
        );
        assert_eq!(
            editor_href("sam", "chili"),
            "/recipes/sam/chili/suggest".to_string()
        );
    }

    #[test]
    fn the_carriage_returns_a_browser_adds_come_out_again() {
        assert_eq!(normalize("a\r\nb\r\n"), "a\nb\n");
        assert_eq!(normalize("a\rb"), "a\nb");
        let clean = "---\ntitle: Chili\n---\n\nChop the @onion{1}.\n";
        assert_eq!(normalize(clean), clean);
    }

    #[test]
    fn a_very_long_note_is_cut_by_letter_and_stays_text() {
        let note = "ü".repeat(2000);
        let cut = clean_note(&note);
        assert_eq!(cut.chars().count(), MAX_NOTE_CHARS);
        assert!(cut.chars().all(|c| c == 'ü'));
    }

    #[test]
    fn an_empty_note_stays_empty_so_forgejo_keeps_what_it_has() {
        assert_eq!(clean_note("   "), "");
        assert_eq!(clean_note("\n\n"), "");
    }

    #[test]
    fn a_timestamp_shows_the_day() {
        assert_eq!(short_date("2026-08-26T09:41:00+02:00"), "2026-08-26");
        assert_eq!(short_date(""), "");
    }

    #[test]
    fn a_missing_author_still_has_a_name() {
        assert_eq!(author(&None), "Somebody");
    }

    #[test]
    fn a_refused_save_says_what_happened_and_how_hard_it_is() {
        // A refusal that the person can act on must not read as an outage,
        // and an outage must not read as a refusal.
        assert_eq!(
            trouble(&SuggestionError::Stale),
            (StatusCode::CONFLICT, suggestion::STALE_MESSAGE)
        );
        assert_eq!(
            trouble(&SuggestionError::Gone),
            (StatusCode::CONFLICT, suggestion::GONE_MESSAGE)
        );
        assert_eq!(
            trouble(&SuggestionError::TooMany),
            (StatusCode::CONFLICT, suggestion::TOO_MANY_MESSAGE)
        );
        assert_eq!(
            trouble(&SuggestionError::NoTopic),
            (StatusCode::CONFLICT, suggestion::NO_TOPIC_MESSAGE)
        );
        assert_eq!(
            trouble(&SuggestionError::Git(crate::git::GitError::Conflict)).0,
            StatusCode::BAD_GATEWAY
        );
    }

    /// One Suggestion, as Forgejo reports it.
    fn pull(number: i64, title: &str, login: &str) -> PullRequest {
        serde_json::from_value(serde_json::json!({
            "number": number,
            "title": title,
            "body": "More onion.",
            "state": "open",
            "flow": 1,
            "created_at": "2026-08-26T09:41:00+02:00",
            "user": { "id": 1, "login": login, "full_name": "" },
            "head": { "sha": "a".repeat(40), "ref": format!("refs/pull/{number}/head") },
        }))
        .expect("the answer must read")
    }

    fn editor_page(pull: Option<PullRequest>, errors: Vec<String>) -> String {
        let view = pull.as_ref().map(|pull| view_of(pull, "kim"));
        SuggestionTemplate {
            layout: Layout::new(None),
            owner: "sam".to_string(),
            slug: "chili".to_string(),
            title: "Chili".to_string(),
            areas: Vec::new(),
            forgejo_url: "https://forge.test/sam/chili".to_string(),
            errors,
            suggestion: view,
            editable: true,
            elsewhere: suggestion::ELSEWHERE_MESSAGE,
            source: "Chop the @onion{1}.".to_string(),
            base_version: "b".repeat(40),
            draft_version: pull
                .as_ref()
                .map(|pull| pull.head.sha.clone())
                .unwrap_or_default(),
            notice: if pull.is_some() {
                suggestion::NOTICE_MESSAGE
            } else {
                suggestion::NEW_MESSAGE
            },
            note: String::new(),
            comparison: Comparison { groups: Vec::new() },
            comments: Vec::new(),
            form_message: String::new(),
            can_act: false,
            conflict_note: suggestion::CONFLICT_MESSAGE,
            not_ready_note: suggestion::NOT_READY_MESSAGE,
            preview_title: String::new(),
            cooked: RenderedRecipe::default(),
            warnings: Vec::new(),
            parse_errors: Vec::new(),
        }
        .with_preview("Chili")
        .render()
        .expect("the page must render")
    }

    #[test]
    fn a_person_with_no_suggestion_yet_reads_how_one_is_made() {
        let page = editor_page(None, Vec::new());

        assert!(page.contains(suggestion::NEW_MESSAGE));
        assert!(page.contains("name=\"draft_version\" value=\"\""));
        assert!(page.contains("Save Suggestion"));
        // Saving while a person writes is a served file and a data
        // attribute, never an attribute that runs.
        assert!(page.contains("data-draft-url=\"/recipes/sam/chili/suggest\""));
        assert!(!page.contains("onclick="));
        assert!(!page.contains("onsubmit="));
    }

    #[test]
    fn a_person_with_a_suggestion_reads_its_state_and_can_mark_it_ready() {
        let version = "a".repeat(40);
        let page = editor_page(
            Some(pull(7, "WIP: Suggestion for Chili", "kim")),
            Vec::new(),
        );

        assert!(page.contains(&format!("name=\"draft_version\" value=\"{version}\"")));
        assert!(page.contains(suggestion::NOTICE_MESSAGE));
        assert!(page.contains("Editing in progress"));
        assert!(page.contains("Ready for review"));
        // The prefix that Forgejo reads must never reach the page.
        assert!(!page.contains("WIP"));
    }

    #[test]
    fn the_editor_carries_no_script_that_runs() {
        // The policy is `default-src 'self'`, and the page has to work with
        // scripts blocked.
        let page = editor_page(None, vec!["This did not work".to_string()]);

        for script in page.split("<script").skip(1) {
            assert!(
                script.starts_with(" src=\""),
                "the page must carry no inline script"
            );
        }
        assert!(page.contains("This did not work"));
    }

    #[test]
    fn the_list_names_every_suggestion_and_its_state() {
        let page = SuggestionsTemplate {
            layout: Layout::new(None),
            owner: "sam".to_string(),
            slug: "chili".to_string(),
            title: "Chili".to_string(),
            areas: Vec::new(),
            forgejo_url: "https://forge.test/sam/chili/pulls".to_string(),
            suggestions: vec![SuggestionRow {
                number: 7,
                title: "Suggestion for Chili".to_string(),
                author: "Kim".to_string(),
                made: "2026-08-26".to_string(),
                state: SuggestionState::Ready.label(),
                pill: pill(SuggestionState::Ready),
                comments: 2,
                mine: false,
                conflict: false,
            }],
            errors: Vec::new(),
        }
        .render()
        .expect("the page must render");

        assert!(page.contains("Suggestion for Chili"));
        assert!(page.contains("Ready for review"));
        assert!(page.contains("/recipes/sam/chili/suggestions/7"));
        assert!(page.contains("/recipes/sam/chili/suggest"));
    }

    /// One Suggestion, with the answer Forgejo gives about whether the two
    /// sides still fit together.
    fn pull_fitting(number: i64, title: &str, login: &str, fits: Option<bool>) -> PullRequest {
        serde_json::from_value(serde_json::json!({
            "number": number,
            "title": title,
            "body": "More onion.",
            "state": "open",
            "flow": 1,
            "mergeable": fits,
            "created_at": "2026-08-26T09:41:00+02:00",
            "user": { "id": 1, "login": login, "full_name": "" },
            "head": { "sha": "a".repeat(40), "ref": format!("refs/pull/{number}/head") },
            "base": { "sha": "b".repeat(40), "ref": "main" },
        }))
        .expect("the answer must read")
    }

    /// Somebody who is signed in, so that the page draws what they can do.
    fn signed_in() -> crate::session::CurrentUser {
        crate::session::CurrentUser {
            forgejo_user_id: 1,
            login: "sam".to_string(),
            display_name: "Sam Cook".to_string(),
            avatar_url: String::new(),
        }
    }

    /// The review of one Suggestion, as an Editor reads it.
    fn review_page(pull: &PullRequest, can_act: bool, comparison: Comparison) -> String {
        let me = signed_in();
        SuggestionTemplate {
            layout: Layout::new(Some(&me)),
            owner: "sam".to_string(),
            slug: "chili".to_string(),
            title: "Chili".to_string(),
            areas: Vec::new(),
            forgejo_url: "https://forge.test/sam/chili/pulls/7".to_string(),
            errors: Vec::new(),
            suggestion: Some(view_of(pull, "sam")),
            editable: false,
            elsewhere: suggestion::ELSEWHERE_MESSAGE,
            source: "Chop the @onion{2}.".to_string(),
            base_version: String::new(),
            draft_version: String::new(),
            notice: "",
            note: String::new(),
            comparison,
            comments: vec![CommentView {
                author: "Kim".to_string(),
                written: "2026-08-26".to_string(),
                body: "More onion, please.".to_string(),
            }],
            form_message: String::new(),
            can_act,
            conflict_note: suggestion::CONFLICT_MESSAGE,
            not_ready_note: suggestion::NOT_READY_MESSAGE,
            preview_title: String::new(),
            cooked: RenderedRecipe::default(),
            warnings: Vec::new(),
            parse_errors: Vec::new(),
        }
        .with_preview("Chili")
        .render()
        .expect("the page must render")
    }

    /// A comparison with one difference in it.
    fn one_change() -> Comparison {
        crate::web_history::Comparison {
            groups: vec![crate::web_history::Group {
                name: "Ingredients",
                differences: Vec::new(),
            }],
        }
    }

    #[test]
    fn an_editor_reads_the_changes_the_conversation_and_the_two_actions() {
        let page = review_page(
            &pull_fitting(7, "Suggestion for Chili", "kim", Some(true)),
            true,
            one_change(),
        );

        assert!(page.contains("Changes"));
        assert!(page.contains("Ingredients"));
        assert!(page.contains("Conversation"));
        assert!(page.contains("More onion, please."));
        assert!(page.contains("Accept Suggestion"));
        assert!(page.contains("Decline Suggestion"));
        assert!(page.contains("/recipes/sam/chili/suggestions/7/accept"));
        assert!(page.contains("/recipes/sam/chili/suggestions/7/decline"));
    }

    #[test]
    fn a_suggestion_with_a_conflict_is_marked_and_cannot_be_accepted() {
        let page = review_page(
            &pull_fitting(7, "Suggestion for Chili", "kim", Some(false)),
            true,
            one_change(),
        );

        assert!(
            page.contains("Cannot be accepted"),
            "the mark must be there"
        );
        assert!(page.contains(suggestion::CONFLICT_MESSAGE));
        assert!(
            !page.contains("/recipes/sam/chili/suggestions/7/accept"),
            "the interface must not offer an acceptance it will refuse"
        );
        // Declining a Suggestion that cannot be accepted is exactly what an
        // Editor needs to be able to do.
        assert!(page.contains("/recipes/sam/chili/suggestions/7/decline"));
    }

    #[test]
    fn a_suggestion_that_is_still_being_written_cannot_be_accepted() {
        let page = review_page(
            &pull_fitting(7, "WIP: Suggestion for Chili", "kim", Some(true)),
            true,
            one_change(),
        );

        assert!(page.contains("Editing in progress"));
        assert!(page.contains(suggestion::NOT_READY_MESSAGE));
        assert!(!page.contains("/recipes/sam/chili/suggestions/7/accept"));
        // Forgejo reads this prefix, and no cook ever does.
        assert!(!page.contains("WIP"));
    }

    #[test]
    fn a_person_who_cannot_change_the_recipe_reads_it_and_can_still_comment() {
        let page = review_page(
            &pull_fitting(7, "Suggestion for Chili", "kim", Some(true)),
            false,
            one_change(),
        );

        assert!(!page.contains("Accept Suggestion"));
        assert!(!page.contains("Decline Suggestion"));
        assert!(!page.contains("Your decision"));
        // A comment needs read access and no more.
        assert!(page.contains("/recipes/sam/chili/suggestions/7/comments"));
        assert!(page.contains("Write comment"));
    }

    #[test]
    fn the_review_carries_no_script_that_runs() {
        // The policy is `default-src 'self'`, and the page has to work with
        // scripts blocked. Accept and Decline are ordinary forms.
        let page = review_page(
            &pull_fitting(7, "Suggestion for Chili", "kim", Some(true)),
            true,
            one_change(),
        );

        // The frame carries the keyboard shortcuts, which are a served file
        // and an addition: every shortcut goes to a control that Tab reaches
        // as well. What must never appear is a script written into the page,
        // because the policy refuses one and a reader would be told nothing.
        for piece in page.split("<script").skip(1) {
            assert!(
                piece.trim_start().starts_with("src=\"/static/js/"),
                "a script written into the page cannot run under the policy: {piece:.80}"
            );
        }

        assert!(!page.contains("onclick="));
        assert!(!page.contains("onsubmit="));

        // And the review itself needs none of it: each decision is a form.
        assert!(page.contains("<form method=\"post\""));
    }

    fn inbox_row(number: i64, state: SuggestionState) -> InboxRow {
        InboxRow {
            owner: "sam".to_string(),
            slug: "chili".to_string(),
            recipe: "Chili sin Carne".to_string(),
            number,
            title: "Suggestion for Chili sin Carne".to_string(),
            author: "Kim".to_string(),
            made: "2026-08-26".to_string(),
            state: state.label(),
            pill: pill(state),
            comments: 1,
        }
    }

    #[test]
    fn the_inbox_holds_both_directions_and_names_the_recipe_of_each_row() {
        let page = InboxTemplate {
            layout: Layout::new(None),
            review: vec![inbox_row(7, SuggestionState::Ready)],
            mine: vec![inbox_row(9, SuggestionState::Accepted)],
            forgejo_url: "https://forge.test/pulls".to_string(),
            notice: String::new(),
            errors: Vec::new(),
        }
        .render()
        .expect("the page must render");

        assert!(page.contains("Needs my review"));
        assert!(page.contains("My suggestions"));
        // A row has to name the Recipe, because the inbox covers all of them.
        assert!(page.contains("Chili sin Carne"));
        assert!(page.contains("/recipes/sam/chili/suggestions/7"));
        assert!(page.contains("/recipes/sam/chili/suggestions/9"));
        assert!(page.contains("Ready for review"));
        assert!(page.contains("Accepted"));
    }

    #[test]
    fn an_empty_inbox_says_so_in_both_lists() {
        let page = InboxTemplate {
            layout: Layout::new(None),
            review: Vec::new(),
            mine: Vec::new(),
            forgejo_url: "https://forge.test/pulls".to_string(),
            notice: String::new(),
            errors: Vec::new(),
        }
        .render()
        .expect("the page must render");

        assert!(page.contains("No Suggestion waits for you."));
        assert!(page.contains("You made no Suggestion yet."));
    }

    #[test]
    fn a_refused_action_says_what_happened_and_how_hard_it_is() {
        // A refusal that the Editor can act on must not read as an outage,
        // and an outage must not read as a refusal.
        assert_eq!(
            refused(&SuggestionError::Conflict),
            (StatusCode::CONFLICT, suggestion::CONFLICT_MESSAGE)
        );
        assert_eq!(
            refused(&SuggestionError::NotReady),
            (StatusCode::CONFLICT, suggestion::NOT_READY_MESSAGE)
        );
        assert_eq!(
            refused(&SuggestionError::Gone),
            (StatusCode::CONFLICT, suggestion::ALREADY_MESSAGE)
        );
        assert_eq!(
            refused(&SuggestionError::Git(crate::git::GitError::Conflict)).0,
            StatusCode::BAD_GATEWAY
        );
    }

    #[test]
    fn the_newest_suggestion_comes_first_in_the_inbox() {
        let one = |number: i64, updated: &str| -> crate::forgejo::FoundPullRequest {
            serde_json::from_value(serde_json::json!({
                "number": number,
                "title": "Suggestion for Chili",
                "state": "open",
                "created_at": "2026-08-01T09:00:00Z",
                "updated_at": updated,
                "pull_request": { "merged": false },
                "repository": { "owner": "sam", "name": "chili", "full_name": "sam/chili" },
            }))
            .expect("the answer must read")
        };

        let sorted = newest_first(vec![
            one(1, "2026-08-01T09:00:00Z"),
            one(3, "2026-08-26T09:00:00Z"),
            one(2, "2026-08-10T09:00:00Z"),
        ]);

        let order: Vec<i64> = sorted.iter().map(|one| one.number).collect();
        assert_eq!(order, vec![3, 2, 1]);
    }
}
