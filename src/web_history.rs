//! History, Changes, and Restore.
//!
//! Git holds the History of a Recipe. This module keeps no copy of it: it
//! reads the published Versions through the Forgejo HTTP API, and it reads
//! the content of one Version through the same API. Nothing here becomes a
//! second store.
//!
//! History shows the published Versions only. The list starts at the branch
//! that carries the published Recipe, so work that sits somewhere else never
//! reaches this page.
//!
//! The comparison comes from the parsed Recipe on each side, and never from
//! a comparison of text. That is what lets the page speak about ingredients,
//! cookware, and steps, and say no word of the forge.
//!
//! Restore adds one new Version that holds the content of an older Version.
//! It goes through the same publication path as an edit, so History keeps
//! every earlier Version and this application never rewrites it.

use std::collections::BTreeMap;
use std::sync::Arc;

use askama::Template;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum_extra::extract::CookieJar;
use serde::Deserialize;

use crate::create_recipe::{self, MAIN_BRANCH};
use crate::forgejo::{Commit, Repository};
use crate::git::{GitError, Identity, PublishVersion};
use crate::recipe::{self, RECIPE_FILE};
use crate::render::{self, Component, RenderedRecipe};
use crate::secret::Secret;
use crate::web::{AppState, Layout, MaybeUser};
use crate::web_recipes::{RecipeArea, areas};

/// How many Versions one page of History shows.
///
/// Forgejo refuses a larger page than this, so a larger number here would
/// promise more than the answer can hold.
const PAGE_SIZE: u32 = 50;

/// The longest identifier of a Version that the address can carry.
const MAX_VERSION_CHARS: usize = 64;

/// The shortest identifier that names one Version without doubt.
const MIN_VERSION_CHARS: usize = 7;

/// The most step pairs that the comparison examines carefully.
///
/// Above this the page pairs the steps by position instead. A Recipe that
/// long is not one a cook reads, and the page must still answer.
const MAX_STEP_PAIRS: usize = 40_000;

/// Shown when the application cannot read the Versions of a Recipe.
const NO_HISTORY_MESSAGE: &str = "CookLangHub cannot read the History of this Recipe. Open the Recipe in Forgejo to see it there.";

/// Shown when a Version holds something the application cannot read.
const UNREADABLE_MESSAGE: &str = "CookLangHub cannot read one of these two Versions as a Recipe. Open the Recipe in Forgejo to see the content.";

/// Shown when the person may read the Recipe but may not add a Version.
const NO_WRITE_MESSAGE: &str = "You can read this Recipe, but you cannot change it. Ask the owner to share it with you as an Editor.";

/// Shown when a restore would change nothing.
const SAME_CONTENT_MESSAGE: &str =
    "This Version holds the same Recipe as the published Version. CookLangHub made no new Version.";

/// Shown when the Recipe holds no Version yet.
const NO_BASE_MESSAGE: &str =
    "This Recipe has no published Version, so CookLangHub cannot add one here.";

/// Shown when Git or Forgejo does not answer.
const UNREACHABLE_MESSAGE: &str =
    "CookLangHub cannot write to this Recipe at the moment. Nothing changed. Try again.";

/// Shown when Git cannot join the restored content with the published one.
const CONFLICT_MESSAGE: &str = "Somebody published a change while this restore ran. CookLangHub did not change the published Recipe. Open the History again, and start the restore again.";

/// Where the History area of a Recipe lives.
pub fn area_href(owner: &str, slug: &str) -> String {
    format!("/recipes/{owner}/{slug}/history")
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/recipes/{owner}/{slug}/history", get(history))
        .route("/recipes/{owner}/{slug}/history/{version}", get(version))
        .route(
            "/recipes/{owner}/{slug}/history/{version}/restore",
            post(restore),
        )
        .route("/recipes/{owner}/{slug}/changes", get(changes))
}

// ---------------------------------------------------------------------
// The Recipe behind every page here
// ---------------------------------------------------------------------

/// The Recipe that a History page belongs to.
struct Subject {
    repository: Repository,
    /// The title a cook sees. It lives in the Recipe, not in the name.
    title: String,
    forgejo_url: String,
}

impl Subject {
    /// The branch that carries the published Recipe.
    fn branch(&self) -> &str {
        if self.repository.default_branch.is_empty() {
            MAIN_BRANCH
        } else {
            &self.repository.default_branch
        }
    }
}

/// Read the Recipe behind a History page.
///
/// `None` means Forgejo did not give the Recipe to this person: it is gone,
/// it never existed, or they may not see it. Forgejo decides, and this is
/// what lets an anonymous person read the History of a public Recipe and
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

    let forgejo_url = state.forgejo.web_url(&repository.full_name);

    Some(Subject {
        repository,
        title,
        forgejo_url,
    })
}

/// The answer when there is no Recipe to show a History for.
fn missing() -> Response {
    (StatusCode::NOT_FOUND, "This Recipe is not available.").into_response()
}

/// The answer when one Version cannot be shown.
fn no_version() -> Response {
    (StatusCode::NOT_FOUND, "This Version is not available.").into_response()
}

/// The Forgejo credential of the person who is looking, when they have one.
async fn token(state: &AppState, jar: &CookieJar) -> Option<Secret<String>> {
    crate::web::viewer_token(state, jar).await
}

// ---------------------------------------------------------------------
// One Version, as a person reads it
// ---------------------------------------------------------------------

/// One published Version in the History list.
struct VersionRow {
    /// The identifier that the address carries. A person never reads it.
    id: String,
    /// What the person wrote about the change.
    description: String,
    author: String,
    moment: String,
    /// Whether this Version is the one the Recipe shows now.
    published: bool,
    /// The words that name this Version in the comparison control.
    label: String,
}

impl VersionRow {
    fn of(commit: &Commit, published: bool) -> Self {
        let description = description(&commit.commit.message);
        let moment = moment(commit_date(commit));
        let label = format!("{moment} · {description}");

        Self {
            id: commit.sha.clone(),
            description,
            author: author(commit),
            moment,
            published,
            label,
        }
    }
}

/// The moment Git recorded for a Version.
fn commit_date(commit: &Commit) -> &str {
    commit
        .commit
        .author
        .as_ref()
        .map(|identity| identity.date.as_str())
        .unwrap_or_default()
}

/// The name to show for the person who published a Version.
///
/// The Forgejo account comes first, because that is the name the rest of
/// this application shows. A Version written outside CookLangHub can name
/// somebody Forgejo has no account for, and then the name Git holds is the
/// best there is.
fn author(commit: &Commit) -> String {
    if let Some(user) = &commit.author {
        let name = user.display_name().trim();
        if !name.is_empty() {
            return name.to_string();
        }
    }

    let written = commit
        .commit
        .author
        .as_ref()
        .map(|identity| identity.name.trim())
        .unwrap_or_default();

    if written.is_empty() {
        "Somebody".to_string()
    } else {
        written.to_string()
    }
}

/// The first line of what the person wrote about a Version.
fn description(message: &str) -> String {
    let first = message.lines().next().unwrap_or_default().trim();
    if first.is_empty() {
        "No description".to_string()
    } else {
        first.to_string()
    }
}

/// The day and the clock of a moment that Git wrote.
///
/// Git writes RFC 3339, for example `2026-08-26T09:41:00+02:00`. Two
/// Versions can share a day, so the clock stays.
///
/// Public inside the crate so that a diagnosis can name the Version it
/// offers with the same words that History uses for it.
pub(crate) fn moment(timestamp: &str) -> String {
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

/// Turn what the address carries into the identifier of a Version.
///
/// The value reaches a Forgejo address, so only the shape Git uses passes:
/// hexadecimal, and long enough to name one Version.
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

// ---------------------------------------------------------------------
// The comparison
// ---------------------------------------------------------------------

/// How one part of a Recipe is different between two Versions.
pub struct Difference {
    /// Added, Removed, or Changed.
    pub mark: &'static str,
    /// The CookCLI pill class that colours the mark.
    pub pill: &'static str,
    /// What the difference is about: a name, or `Step 3`.
    pub subject: String,
    /// The value in the earlier Version.
    pub was: Option<String>,
    /// The value in the later Version.
    pub now: Option<String>,
}

impl Difference {
    fn added(subject: String, now: Option<String>) -> Self {
        Self {
            mark: "Added",
            pill: "metadata-cuisine",
            subject,
            was: None,
            now,
        }
    }

    fn removed(subject: String, was: Option<String>) -> Self {
        Self {
            mark: "Removed",
            pill: "metadata-cook",
            subject,
            was,
            now: None,
        }
    }

    fn changed(subject: String, was: Option<String>, now: Option<String>) -> Self {
        Self {
            mark: "Changed",
            pill: "metadata-course",
            subject,
            was,
            now,
        }
    }
}

/// One part of the Recipe, with everything that is different in it.
pub struct Group {
    pub name: &'static str,
    pub differences: Vec<Difference>,
}

/// What is different between two Versions of a Recipe.
pub struct Comparison {
    pub groups: Vec<Group>,
}

impl Comparison {
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }
}

/// One Version, ready to compare.
struct Side {
    title: String,
    cooked: RenderedRecipe,
}

/// Compare two Versions of a Recipe.
///
/// Both sides come from the parser, so the answer speaks about ingredients,
/// cookware, and steps. No line of text is held against another line.
fn compare(before: &Side, after: &Side) -> Comparison {
    let mut groups = Vec::new();

    if before.title != after.title {
        groups.push(Group {
            name: "Name",
            differences: vec![Difference::changed(
                "The name of the Recipe".to_string(),
                Some(before.title.clone()),
                Some(after.title.clone()),
            )],
        });
    }

    let ingredients = compare_things(&before.cooked.ingredients, &after.cooked.ingredients);
    if !ingredients.is_empty() {
        groups.push(Group {
            name: "Ingredients",
            differences: ingredients,
        });
    }

    let cookware = compare_things(&before.cooked.cookware, &after.cooked.cookware);
    if !cookware.is_empty() {
        groups.push(Group {
            name: "Cookware",
            differences: cookware,
        });
    }

    let steps = compare_steps(&steps_of(&before.cooked), &steps_of(&after.cooked));
    if !steps.is_empty() {
        groups.push(Group {
            name: "Steps",
            differences: steps,
        });
    }

    Comparison { groups }
}

/// Compare two lists of things a cook gathers.
///
/// The name makes two entries the same thing, because that is what a cook
/// looks for. A different amount of the same name is one change, and not a
/// removal with an addition after it.
fn compare_things(before: &[Component], after: &[Component]) -> Vec<Difference> {
    let mut out = Vec::new();

    for thing in after {
        match before.iter().find(|held| held.name == thing.name) {
            None => out.push(Difference::added(
                thing.name.clone(),
                thing.quantity.clone(),
            )),
            Some(held) if held.quantity != thing.quantity => out.push(Difference::changed(
                thing.name.clone(),
                held.quantity.clone(),
                thing.quantity.clone(),
            )),
            Some(_) => {}
        }
    }

    for thing in before {
        if !after.iter().any(|held| held.name == thing.name) {
            out.push(Difference::removed(
                thing.name.clone(),
                thing.quantity.clone(),
            ));
        }
    }

    out
}

/// One step of a Recipe, as words.
struct StepLine {
    number: u32,
    text: String,
}

/// Every step of a Recipe, in the order a cook does them.
///
/// A paragraph that carries no instruction is not a step, so it stays out.
/// The words are the words the Recipe page shows, amounts included.
fn steps_of(cooked: &RenderedRecipe) -> Vec<StepLine> {
    let mut out = Vec::new();

    for section in &cooked.sections {
        for block in &section.blocks {
            if !block.is_step() {
                continue;
            }

            let text: String = block
                .pieces
                .iter()
                .map(|piece| {
                    if piece.kind.is_text() {
                        piece.text.clone()
                    } else {
                        piece.badge_text()
                    }
                })
                .collect();

            out.push(StepLine {
                number: block.number,
                text: text.split_whitespace().collect::<Vec<_>>().join(" "),
            });
        }
    }

    out
}

/// What happened to one step between two Versions.
enum Edit {
    Kept,
    Removed(usize),
    Added(usize),
}

/// Compare the steps of two Versions.
///
/// A step that moved because another step went in front of it must not read
/// as a change, so the two lists are matched on the steps they share before
/// anything is called added or removed.
fn compare_steps(before: &[StepLine], after: &[StepLine]) -> Vec<Difference> {
    let script = edits(before, after);
    let mut out = Vec::new();
    let mut at = 0;

    while at < script.len() {
        if matches!(script[at], Edit::Kept) {
            at += 1;
            continue;
        }

        // A run of removals beside a run of additions is one rewrite. Read
        // them in pairs, so a step whose words changed says exactly that.
        let mut removed = Vec::new();
        let mut added = Vec::new();

        while at < script.len() {
            match script[at] {
                Edit::Removed(index) => removed.push(index),
                Edit::Added(index) => added.push(index),
                Edit::Kept => break,
            }
            at += 1;
        }

        let pairs = removed.len().min(added.len());

        for pair in 0..pairs {
            let (was, now) = (&before[removed[pair]], &after[added[pair]]);
            out.push(Difference::changed(
                format!("Step {}", now.number),
                Some(was.text.clone()),
                Some(now.text.clone()),
            ));
        }

        for index in &removed[pairs..] {
            let step = &before[*index];
            out.push(Difference::removed(
                format!("Step {}", step.number),
                Some(step.text.clone()),
            ));
        }

        for index in &added[pairs..] {
            let step = &after[*index];
            out.push(Difference::added(
                format!("Step {}", step.number),
                Some(step.text.clone()),
            ));
        }
    }

    out
}

/// Match two lists of steps on the steps they share.
///
/// This is the classical longest common subsequence. A Recipe longer than
/// the limit is matched by position instead, which reads worse but always
/// answers.
fn edits(before: &[StepLine], after: &[StepLine]) -> Vec<Edit> {
    let (rows, columns) = (before.len(), after.len());

    if rows.saturating_mul(columns) > MAX_STEP_PAIRS {
        return (0..rows)
            .map(Edit::Removed)
            .chain((0..columns).map(Edit::Added))
            .collect();
    }

    let width = columns + 1;
    let mut shared = vec![0u32; (rows + 1) * width];

    for row in (0..rows).rev() {
        for column in (0..columns).rev() {
            shared[row * width + column] = if before[row].text == after[column].text {
                shared[(row + 1) * width + column + 1] + 1
            } else {
                shared[(row + 1) * width + column].max(shared[row * width + column + 1])
            };
        }
    }

    let mut script = Vec::new();
    let (mut row, mut column) = (0, 0);

    while row < rows && column < columns {
        if before[row].text == after[column].text {
            script.push(Edit::Kept);
            row += 1;
            column += 1;
        } else if shared[(row + 1) * width + column] >= shared[row * width + column + 1] {
            script.push(Edit::Removed(row));
            row += 1;
        } else {
            script.push(Edit::Added(column));
            column += 1;
        }
    }

    while row < rows {
        script.push(Edit::Removed(row));
        row += 1;
    }

    while column < columns {
        script.push(Edit::Added(column));
        column += 1;
    }

    script
}

// ---------------------------------------------------------------------
// The pages
// ---------------------------------------------------------------------

#[derive(Template)]
#[template(path = "history.html")]
struct HistoryTemplate {
    layout: Layout,
    owner: String,
    slug: String,
    title: String,
    areas: Vec<RecipeArea>,
    forgejo_url: String,
    versions: Vec<VersionRow>,
    /// The Version each side of the comparison control starts on.
    earlier: String,
    later: String,
    errors: Vec<String>,
}

#[derive(Template)]
#[template(path = "version.html")]
struct VersionTemplate {
    layout: Layout,
    owner: String,
    slug: String,
    title: String,
    areas: Vec<RecipeArea>,
    forgejo_url: String,
    /// The Version on the page.
    version: VersionRow,
    /// The Recipe as a cook reads it.
    cooked: RenderedRecipe,
    /// The Cooklang behind it, kept for anybody who wants to look.
    source: String,
    /// Whether this person can add a Version to this Recipe.
    can_restore: bool,
    errors: Vec<String>,
}

#[derive(Template)]
#[template(path = "changes.html")]
struct ChangesTemplate {
    layout: Layout,
    owner: String,
    slug: String,
    title: String,
    areas: Vec<RecipeArea>,
    forgejo_url: String,
    earlier: VersionRow,
    later: VersionRow,
    comparison: Comparison,
    errors: Vec<String>,
}

/// Read the published Versions of a Recipe, newest first.
async fn published_versions(
    state: &AppState,
    token: Option<&Secret<String>>,
    owner: &str,
    slug: &str,
    branch: &str,
) -> Option<Vec<VersionRow>> {
    let commits = match state
        .forgejo
        .list_commits(token, owner, slug, branch, PAGE_SIZE)
        .await
    {
        Ok(commits) => commits,
        Err(error) => {
            tracing::info!(%error, %owner, %slug, "cannot read the published Versions");
            return None;
        }
    };

    Some(
        commits
            .iter()
            .enumerate()
            // The list starts at the published branch, so the first entry
            // is the Version the Recipe page shows now.
            .map(|(place, commit)| VersionRow::of(commit, place == 0))
            .collect(),
    )
}

async fn history(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug)): Path<(String, String)>,
) -> Response {
    let here = area_href(&owner, &slug);
    let token = token(&state, &jar).await;

    let Some(subject) = subject(&state, token.as_ref(), &owner, &slug).await else {
        return missing();
    };

    let (versions, errors) =
        match published_versions(&state, token.as_ref(), &owner, &slug, subject.branch()).await {
            Some(versions) => (versions, Vec::new()),
            None => (Vec::new(), vec![NO_HISTORY_MESSAGE.to_string()]),
        };

    // The control starts on the two newest Versions, which is the
    // comparison a person asks for most.
    let later = versions
        .first()
        .map(|row| row.id.clone())
        .unwrap_or_default();
    let earlier = versions
        .get(1)
        .map(|row| row.id.clone())
        .unwrap_or_else(|| later.clone());

    let areas = areas(&owner, &slug, &subject.repository);

    respond(HistoryTemplate {
        layout: Layout::new(current.as_ref()).on(&headers, &here),
        owner,
        slug,
        title: subject.title,
        areas,
        forgejo_url: subject.forgejo_url,
        versions,
        earlier,
        later,
        errors,
    })
}

/// Read the Cooklang of one Version.
///
/// Public inside the crate so that the diagnosis of a broken Recipe can
/// look for the last valid Version through this one reader.
pub(crate) async fn source_at(
    state: &AppState,
    token: Option<&Secret<String>>,
    owner: &str,
    slug: &str,
    version: &str,
) -> Option<Vec<u8>> {
    match state
        .forgejo
        .raw_file(token, owner, slug, version, RECIPE_FILE)
        .await
    {
        Ok(bytes) => Some(bytes),
        Err(error) => {
            tracing::info!(%error, %owner, %slug, "cannot read the Recipe of a Version");
            None
        }
    }
}

async fn version(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug, wanted)): Path<(String, String, String)>,
) -> Response {
    let token = token(&state, &jar).await;

    let Some(wanted) = version_id(&wanted) else {
        return no_version();
    };
    let Some(subject) = subject(&state, token.as_ref(), &owner, &slug).await else {
        return missing();
    };

    let can_restore = match token.as_ref() {
        Some(token) => state
            .forgejo
            .can_write(token, &owner, &slug)
            .await
            .unwrap_or(false),
        None => false,
    };

    render_version(
        &state,
        &headers,
        current.as_ref(),
        token.as_ref(),
        &subject,
        &owner,
        &slug,
        &wanted,
        can_restore,
        Vec::new(),
    )
    .await
}

/// Read one Version and draw it as a Recipe.
#[allow(clippy::too_many_arguments)]
async fn render_version(
    state: &AppState,
    headers: &HeaderMap,
    current: Option<&crate::session::CurrentUser>,
    token: Option<&Secret<String>>,
    subject: &Subject,
    owner: &str,
    slug: &str,
    wanted: &str,
    can_restore: bool,
    mut errors: Vec<String>,
) -> Response {
    let here = format!("/recipes/{owner}/{slug}/history/{wanted}");

    let commit = match state.forgejo.commit(token, owner, slug, wanted).await {
        Ok(commit) => commit,
        Err(error) => {
            tracing::info!(%error, %owner, %slug, "cannot read a Version");
            return no_version();
        }
    };

    // The Version on the page is the published one when the History list
    // begins with it.
    let published = published_versions(state, token, owner, slug, subject.branch())
        .await
        .and_then(|versions| versions.first().map(|row| row.id == commit.sha))
        .unwrap_or(false);

    let Some(bytes) = source_at(state, token, owner, slug, wanted).await else {
        return no_version();
    };

    let valid_text = std::str::from_utf8(&bytes).is_ok();
    let source = String::from_utf8_lossy(&bytes).to_string();

    if !valid_text {
        tracing::info!(%owner, %slug, "a Version holds a Recipe file that is not UTF-8 text");
        errors.push(
            "This Version is not UTF-8 text. Open the Recipe in Forgejo to see the exact content."
                .to_string(),
        );
    }

    let parsed = recipe::parse(&source);
    errors.extend(parsed.errors.iter().map(|d| d.message.clone()));

    let cooked = recipe::parse_recipe(&source)
        .as_ref()
        .map(render::render)
        .unwrap_or_default();

    let title = parsed
        .title
        .clone()
        .unwrap_or_else(|| subject.repository.name.clone());

    respond(VersionTemplate {
        layout: Layout::new(current).on(headers, &here),
        owner: owner.to_string(),
        slug: slug.to_string(),
        title,
        areas: areas(owner, slug, &subject.repository),
        forgejo_url: subject.forgejo_url.clone(),
        version: VersionRow::of(&commit, published),
        cooked,
        source,
        can_restore,
        errors,
    })
}

/// What the comparison control sends.
#[derive(Debug, Deserialize)]
struct ChangesQuery {
    #[serde(default)]
    from: String,
    #[serde(default)]
    to: String,
}

async fn changes(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug)): Path<(String, String)>,
    Query(query): Query<ChangesQuery>,
) -> Response {
    let here = format!("/recipes/{owner}/{slug}/changes");
    let token = token(&state, &jar).await;

    let (Some(from), Some(to)) = (version_id(&query.from), version_id(&query.to)) else {
        return no_version();
    };

    let Some(subject) = subject(&state, token.as_ref(), &owner, &slug).await else {
        return missing();
    };

    let earlier = state
        .forgejo
        .commit(token.as_ref(), &owner, &slug, &from)
        .await;
    let later = state
        .forgejo
        .commit(token.as_ref(), &owner, &slug, &to)
        .await;

    let (Ok(earlier), Ok(later)) = (earlier, later) else {
        tracing::info!(%owner, %slug, "cannot read one of the two Versions to compare");
        return no_version();
    };

    let before = side(&state, token.as_ref(), &owner, &slug, &from).await;
    let after = side(&state, token.as_ref(), &owner, &slug, &to).await;

    let mut errors = Vec::new();
    let comparison = match (&before, &after) {
        (Some(before), Some(after)) => compare(before, after),
        // One of the two Versions is not a Recipe this application can
        // read. Say so, rather than call every ingredient removed.
        _ => {
            errors.push(UNREADABLE_MESSAGE.to_string());
            Comparison { groups: Vec::new() }
        }
    };

    let areas = areas(&owner, &slug, &subject.repository);

    respond(ChangesTemplate {
        layout: Layout::new(current.as_ref()).on(&headers, &here),
        owner,
        slug,
        title: subject.title,
        areas,
        forgejo_url: subject.forgejo_url,
        earlier: VersionRow::of(&earlier, false),
        later: VersionRow::of(&later, false),
        comparison,
        errors,
    })
}

/// Read one Version and parse it, ready to compare.
///
/// `None` means the parser refused the content of that Version.
async fn side(
    state: &AppState,
    token: Option<&Secret<String>>,
    owner: &str,
    slug: &str,
    version: &str,
) -> Option<Side> {
    let bytes = source_at(state, token, owner, slug, version).await?;
    let source = std::str::from_utf8(&bytes).ok()?;
    let cooked = recipe::parse_recipe(source).as_ref().map(render::render)?;

    Some(Side {
        title: recipe::parse(source)
            .title
            .unwrap_or_else(|| "This Recipe".to_string()),
        cooked,
    })
}

// ---------------------------------------------------------------------
// Restore
// ---------------------------------------------------------------------

/// What a restore did.
enum Restored {
    /// A new Version holds the old content.
    Done,
    /// Nothing changed, and this is why.
    Refused(&'static str),
    /// Forgejo does not let this person add a Version here.
    Forbidden(&'static str),
    /// There is no such Version to restore.
    Gone,
}

async fn restore(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug, wanted)): Path<(String, String, String)>,
) -> Response {
    let Some(wanted) = version_id(&wanted) else {
        return no_version();
    };

    let Some(actor) = crate::web_recipes::actor(&state, &jar).await else {
        return Redirect::to("/auth/sign-in").into_response();
    };

    let Some(subject) = subject(&state, Some(&actor.token), &owner, &slug).await else {
        return missing();
    };

    // Forgejo decides who may add a Version. The check happens here and not
    // only on the page, because this request can arrive without the page.
    let can_write = state
        .forgejo
        .can_write(&actor.token, &owner, &slug)
        .await
        .unwrap_or(false);

    let outcome =
        attempt_restore(&state, &actor, &subject, &owner, &slug, &wanted, can_write).await;

    let (status, message) = match outcome {
        Restored::Done => return Redirect::to(&area_href(&owner, &slug)).into_response(),
        Restored::Gone => return no_version(),
        Restored::Forbidden(message) => (StatusCode::FORBIDDEN, message),
        Restored::Refused(message) => (StatusCode::OK, message),
    };

    let body = render_version(
        &state,
        &headers,
        current.as_ref(),
        Some(&actor.token),
        &subject,
        &owner,
        &slug,
        &wanted,
        can_write,
        vec![message.to_string()],
    )
    .await;

    (status, body).into_response()
}

/// Add one new Version that holds the content of an older Version.
///
/// History is never rewritten. The new Version sits on top of the published
/// one, which is why every earlier Version is still there afterwards.
async fn attempt_restore(
    state: &AppState,
    actor: &crate::web_recipes::Actor,
    subject: &Subject,
    owner: &str,
    slug: &str,
    wanted: &str,
    can_write: bool,
) -> Restored {
    if !can_write {
        tracing::info!(%owner, %slug, login = %actor.user.login, "a person without write access asked for a restore");
        return Restored::Forbidden(NO_WRITE_MESSAGE);
    }

    let Some(bytes) = source_at(state, Some(&actor.token), owner, slug, wanted).await else {
        return Restored::Gone;
    };

    let Ok(commit) = state
        .forgejo
        .commit(Some(&actor.token), owner, slug, wanted)
        .await
    else {
        return Restored::Gone;
    };

    // Git holds History, so Git says which Version is published now. The
    // new Version is built on that one.
    let remote = state.forgejo.git_url(&format!("{owner}/{slug}"));
    let head = match state
        .git
        .branch_head(&remote, &actor.token, subject.branch())
        .await
    {
        Ok(Some(head)) => head,
        Ok(None) => return Restored::Refused(NO_BASE_MESSAGE),
        Err(error) => {
            tracing::warn!(%error, %owner, %slug, "cannot read the published Version");
            return Restored::Refused(UNREACHABLE_MESSAGE);
        }
    };

    // A restore that changes nothing must not leave an empty Version in
    // History, because a person reads History.
    let current = source_at(state, Some(&actor.token), owner, slug, &head).await;
    if current.as_deref() == Some(bytes.as_slice()) {
        return Restored::Refused(SAME_CONTENT_MESSAGE);
    }

    // Ask Forgejo whether this person hides their address. A failure here
    // counts as "hide", because publishing an address by accident cannot be
    // undone.
    let hide_email = match state.forgejo.user_settings(&actor.token).await {
        Ok(settings) => settings.hide_email,
        Err(error) => {
            tracing::warn!(%error, "cannot read the privacy setting; using the no-reply address");
            true
        }
    };

    let identity = Identity {
        name: actor.user.display_name().to_string(),
        email: create_recipe::commit_email(
            &actor.user.login,
            &actor.user.email,
            hide_email,
            &state.forgejo_noreply_domain,
        ),
    };

    let mut files = BTreeMap::new();
    files.insert(RECIPE_FILE.to_string(), bytes);

    let message = restore_message(&moment(commit_date(&commit)));

    match state
        .git
        .publish_version(PublishVersion {
            remote_url: &remote,
            token: &actor.token,
            identity: &identity,
            branch: subject.branch(),
            message: &message,
            base_version: &head,
            files,
        })
        .await
    {
        Ok(version) => {
            tracing::info!(%owner, %slug, %version, "restored an older Version as a new Version");
            Restored::Done
        }
        Err(GitError::Conflict) => {
            tracing::info!(%owner, %slug, "a restore could not be joined with the published Recipe");
            Restored::Refused(CONFLICT_MESSAGE)
        }
        Err(error) => {
            tracing::warn!(%error, %owner, %slug, "cannot restore a Version");
            Restored::Refused(UNREACHABLE_MESSAGE)
        }
    }
}

/// The description that a restore writes into History.
fn restore_message(moment: &str) -> String {
    if moment.is_empty() {
        "Restore an older Version".to_string()
    } else {
        format!("Restore the Version of {moment}")
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
    use crate::forgejo::{CommitDetail, CommitIdentity, ForgejoUser};

    /// The words of the forge, which no page here may say.
    const FORGE_WORDS: [&str; 10] = [
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

    /// Whether a text says one of the words of the forge.
    ///
    /// Whole words only. `Sharing` is an area of a Recipe and `share` is
    /// what a person does with one, so neither may be read as the
    /// identifier that Git uses.
    fn says_forge_word(text: &str) -> Option<&'static str> {
        let lower = text.to_lowercase();

        if lower.contains("pull request") {
            return Some("pull request");
        }

        let spoken: Vec<&str> = lower
            .split(|c: char| !c.is_alphanumeric())
            .filter(|word| !word.is_empty())
            .collect();

        FORGE_WORDS.into_iter().find(|word| spoken.contains(word))
    }

    fn side_of(title: &str, source: &str) -> Side {
        Side {
            title: title.to_string(),
            cooked: recipe::parse_recipe(source)
                .as_ref()
                .map(render::render)
                .expect("the source must parse"),
        }
    }

    fn group<'a>(comparison: &'a Comparison, name: &str) -> &'a [Difference] {
        comparison
            .groups
            .iter()
            .find(|group| group.name == name)
            .map(|group| group.differences.as_slice())
            .unwrap_or_default()
    }

    #[test]
    fn only_the_shape_of_a_version_identifier_reaches_forgejo() {
        assert_eq!(version_id("A1B2C3D4E5F6"), Some("a1b2c3d4e5f6".to_string()));
        assert_eq!(version_id(" abcdef1 "), Some("abcdef1".to_string()));

        for value in [
            "",
            "abc",
            "../../admin/users",
            "main",
            "abcdefg",
            "abcdef1;rm",
            &"a".repeat(65),
        ] {
            assert_eq!(version_id(value), None, "`{value}` must not be sent on");
        }
    }

    #[test]
    fn a_moment_shows_the_day_and_the_clock() {
        assert_eq!(moment("2026-08-26T09:41:00+02:00"), "2026-08-26 09:41");
        assert_eq!(moment("2026-08-26"), "2026-08-26");
        assert_eq!(moment(""), "");
    }

    #[test]
    fn a_version_without_a_description_still_names_itself() {
        assert_eq!(description("Update Chili\n\nLess salt."), "Update Chili");
        assert_eq!(description("   "), "No description");
    }

    #[test]
    fn the_name_of_a_version_prefers_the_forgejo_account() {
        let mut commit = Commit {
            sha: "abc".to_string(),
            author: Some(ForgejoUser {
                id: 1,
                login: "sam".to_string(),
                full_name: "Sam Cook".to_string(),
                avatar_url: String::new(),
                email: String::new(),
            }),
            commit: CommitDetail {
                message: "Update".to_string(),
                author: Some(CommitIdentity {
                    name: "Somebody Else".to_string(),
                    date: "2026-08-26T09:41:00Z".to_string(),
                }),
            },
        };
        assert_eq!(author(&commit), "Sam Cook");

        // A Version written outside CookLangHub can name somebody Forgejo
        // has no account for. The name Git holds is then the best there is.
        commit.author = None;
        assert_eq!(author(&commit), "Somebody Else");

        commit.commit.author = None;
        assert_eq!(author(&commit), "Somebody");
    }

    #[test]
    fn an_ingredient_that_arrives_is_added_and_one_that_goes_is_removed() {
        let before = side_of("Chili", "Chop the @onion{1} and the @garlic{2}.");
        let after = side_of("Chili", "Chop the @onion{1} and the @leek{1}.");

        let comparison = compare(&before, &after);
        let ingredients = group(&comparison, "Ingredients");

        let added: Vec<&str> = ingredients
            .iter()
            .filter(|d| d.mark == "Added")
            .map(|d| d.subject.as_str())
            .collect();
        let removed: Vec<&str> = ingredients
            .iter()
            .filter(|d| d.mark == "Removed")
            .map(|d| d.subject.as_str())
            .collect();

        assert_eq!(added, vec!["leek"]);
        assert_eq!(removed, vec!["garlic"]);
    }

    #[test]
    fn a_different_amount_of_the_same_thing_is_one_change() {
        let before = side_of("Chili", "Add @salt{1%g}.");
        let after = side_of("Chili", "Add @salt{5%g}.");

        let comparison = compare(&before, &after);
        let ingredients = group(&comparison, "Ingredients");

        assert_eq!(ingredients.len(), 1, "one thing changed, not two");
        assert_eq!(ingredients[0].mark, "Changed");
        assert_eq!(ingredients[0].subject, "salt");
        assert_eq!(ingredients[0].was.as_deref(), Some("1 g"));
        assert_eq!(ingredients[0].now.as_deref(), Some("5 g"));
    }

    #[test]
    fn cookware_is_compared_on_its_own() {
        let before = side_of("Chili", "Fry it in a #pan{}.");
        let after = side_of("Chili", "Fry it in a #pot{}.");

        let comparison = compare(&before, &after);
        let cookware = group(&comparison, "Cookware");

        assert!(
            cookware
                .iter()
                .any(|d| d.mark == "Added" && d.subject == "pot")
        );
        assert!(
            cookware
                .iter()
                .any(|d| d.mark == "Removed" && d.subject == "pan")
        );
    }

    #[test]
    fn a_step_put_in_the_middle_moves_nothing_else() {
        // Pairing by position would call every later step changed, which is
        // exactly the noise a cook cannot read.
        let before = side_of("Chili", "Chop it.\n\nFry it.\n\nServe it.");
        let after = side_of("Chili", "Chop it.\n\nRest it.\n\nFry it.\n\nServe it.");

        let comparison = compare(&before, &after);
        let steps = group(&comparison, "Steps");

        assert_eq!(steps.len(), 1, "only one step is new");
        assert_eq!(steps[0].mark, "Added");
        assert_eq!(steps[0].subject, "Step 2");
        assert_eq!(steps[0].now.as_deref(), Some("Rest it."));
    }

    #[test]
    fn a_step_whose_words_changed_reads_as_one_change() {
        let before = side_of("Chili", "Chop it.\n\nFry it for a minute.");
        let after = side_of("Chili", "Chop it.\n\nFry it for an hour.");

        let comparison = compare(&before, &after);
        let steps = group(&comparison, "Steps");

        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].mark, "Changed");
        assert_eq!(steps[0].subject, "Step 2");
        assert_eq!(steps[0].was.as_deref(), Some("Fry it for a minute."));
        assert_eq!(steps[0].now.as_deref(), Some("Fry it for an hour."));
    }

    #[test]
    fn two_versions_that_hold_the_same_recipe_have_no_changes() {
        let source = "---\ntitle: Chili\n---\n\nChop the @onion{1} in a #pan{}.";
        let comparison = compare(&side_of("Chili", source), &side_of("Chili", source));
        assert!(comparison.is_empty());
    }

    #[test]
    fn a_new_name_is_a_change_of_its_own() {
        let comparison = compare(
            &side_of("Chili", "Chop it."),
            &side_of("Hot Chili", "Chop it."),
        );
        let name = group(&comparison, "Name");

        assert_eq!(name.len(), 1);
        assert_eq!(name[0].was.as_deref(), Some("Chili"));
        assert_eq!(name[0].now.as_deref(), Some("Hot Chili"));
    }

    #[test]
    fn the_comparison_names_no_word_of_the_forge() {
        // The group names and the marks are the words the comparison itself
        // puts on the page, whatever the Recipe holds.
        let before = side_of("Chili", "Chop the @onion{1} in a #pan{}.");
        let after = side_of("Hot Chili", "Chop the @leek{2} in a #pot{}.\n\nServe it.");

        let comparison = compare(&before, &after);
        assert!(!comparison.is_empty());

        for group in &comparison.groups {
            let mut words = group.name.to_string();
            for difference in &group.differences {
                words.push(' ');
                words.push_str(difference.mark);
                words.push(' ');
                words.push_str(&difference.subject);
            }

            assert_eq!(
                says_forge_word(&words),
                None,
                "the comparison must say no word of the forge: {words}"
            );
        }
    }

    #[test]
    fn every_message_a_person_reads_uses_cooking_words() {
        let restore = restore_message("2026-08-26 09:41");

        for message in [
            NO_HISTORY_MESSAGE,
            UNREADABLE_MESSAGE,
            NO_WRITE_MESSAGE,
            SAME_CONTENT_MESSAGE,
            NO_BASE_MESSAGE,
            UNREACHABLE_MESSAGE,
            CONFLICT_MESSAGE,
            restore.as_str(),
        ] {
            assert_eq!(
                says_forge_word(message),
                None,
                "a word of the forge must not reach the person: {message}"
            );
        }
    }

    #[test]
    fn every_mark_carries_a_colour_that_the_stylesheet_holds() {
        // A mark without a pill class would draw as plain text, and the
        // three marks would then look the same.
        for difference in [
            Difference::added("x".to_string(), None),
            Difference::removed("x".to_string(), None),
            Difference::changed("x".to_string(), None, None),
        ] {
            assert!(difference.pill.starts_with("metadata-"));
            assert!(!difference.mark.is_empty());
        }
    }
}
