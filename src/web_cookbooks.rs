//! Pages for creating, finding, and reading a Cookbook.
//!
//! Every list is built the way the Recipe lists are built. Forgejo says which
//! Cookbooks this person may see, and the index says what each card shows.
//! The order is never the other way round: a row in the index is not
//! permission to see anything, and a Cookbook that Forgejo does not name
//! never reaches a page.
//!
//! Search, order, and the description preview are all plain form actions, so
//! every page here works with no script at all.

use std::sync::Arc;

use askama::Template;
use axum::Router;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum_extra::extract::CookieJar;
use serde::Deserialize;

use crate::cookbook::{self, NewCookbook};
use crate::forgejo::Ownership;
use crate::secret::Secret;
use crate::session::CurrentUser;
use crate::web::{AppState, Layout, MaybeUser};
use crate::web_browse::{BrowseQuery, Controls, Sort, Tab};

/// How many Cookbooks one page shows.
const PAGE_SIZE: usize = 60;

/// Shown when Forgejo cannot answer. The list is empty because nothing is
/// known, and not because the person has no Cookbooks.
const NO_FORGEJO: &str =
    "CookLangHub cannot reach Forgejo now, so this list is not complete. Try again in a moment.";

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/cookbooks", get(list))
        .route("/cookbooks/new", get(new_form).post(create))
        .route("/cookbooks/{owner}/{slug}", get(show))
        .route("/cookbooks/{owner}/{slug}/history", get(history))
        .route(
            "/cookbooks/{owner}/{slug}/recipes",
            get(add_form).post(add_recipe),
        )
        .route(
            "/cookbooks/{owner}/{slug}/recipes/remove",
            post(remove_recipe),
        )
        .route(
            "/cookbooks/{owner}/{slug}/recipes/holding",
            post(set_holding),
        )
        .route("/cookbooks/{owner}/{slug}/sharing", get(sharing_show))
        .route(
            "/cookbooks/{owner}/{slug}/sharing/public",
            get(sharing_public),
        )
        .route(
            "/cookbooks/{owner}/{slug}/sharing/visibility",
            post(sharing_visibility),
        )
        .route(
            "/cookbooks/{owner}/{slug}/sharing/people",
            post(sharing_add_person),
        )
        .route(
            "/cookbooks/{owner}/{slug}/sharing/people/remove",
            post(sharing_remove_person),
        )
        .route("/explore/cookbooks", get(explore))
}

// ------------------------------------------------------------------ lists

/// Which list a person is looking at.
///
/// The Cookbooks area has its own set, because a Cookbook can be a Favorite
/// and the Recipe lists do not offer that yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Area {
    /// The Cookbooks of this person.
    Mine,
    /// The Cookbooks of somebody else that this person may work on.
    Shared,
    /// The Cookbooks that this person made a Favorite.
    Favorites,
    /// Every public Cookbook.
    Explore,
}

impl Area {
    /// The value that the address carries.
    pub fn as_str(&self) -> &'static str {
        match self {
            Area::Mine => "mine",
            Area::Shared => "shared",
            Area::Favorites => "favorites",
            Area::Explore => "explore",
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "shared" => Area::Shared,
            "favorites" => Area::Favorites,
            _ => Area::Mine,
        }
    }
}

/// One Cookbook as a card on a list.
///
/// A card carries cooking information only: what the Cookbook is about and
/// who owns it. How Git holds it is not on the card.
#[derive(Debug, Clone)]
pub struct CookbookCard {
    pub owner: String,
    pub slug: String,
    pub title: String,
    pub private: bool,
    /// The first words of the description, as plain text.
    pub summary: String,
}

/// What a list came to.
pub struct Listing {
    pub cards: Vec<CookbookCard>,
    /// A message about the state of the list, when there is one to give.
    pub notice: Option<String>,
    /// What to say when the list is empty.
    pub empty: String,
}

/// Build one list of Cookbooks.
///
/// `token` is the credential of the person who is looking, and `None` means
/// an anonymous visitor. Forgejo answers with what that credential may see,
/// which is what makes Explore safe without an account.
pub async fn listing(
    state: &AppState,
    viewer: Option<&CurrentUser>,
    token: Option<&Secret<String>>,
    area: Area,
    query: &BrowseQuery,
) -> Listing {
    // A person who is not signed in owns nothing, shares nothing, and has
    // no Favorites.
    if viewer.is_none() && area != Area::Explore {
        return Listing {
            cards: Vec::new(),
            notice: None,
            empty: "Sign in to see your Cookbooks.".to_string(),
        };
    }

    let found = match area {
        Area::Favorites => match token {
            Some(token) => cookbook::favorites(&state.forgejo, token).await,
            None => Ok((Vec::new(), false)),
        },
        _ => {
            let ownership = match (area, viewer) {
                (Area::Explore, _) => Ownership::Anybody,
                (Area::Mine, Some(user)) => Ownership::OwnedBy(user.forgejo_user_id),
                (Area::Shared, Some(user)) => Ownership::ReachableBy(user.forgejo_user_id),
                _ => Ownership::Anybody,
            };
            cookbook::visible(&state.forgejo, token, ownership).await
        }
    };

    let (mut repositories, truncated) = match found {
        Ok(found) => found,
        Err(error) => {
            tracing::warn!(%error, area = area.as_str(), "cannot ask Forgejo for the Cookbooks");
            return Listing {
                cards: Vec::new(),
                notice: Some(NO_FORGEJO.to_string()),
                empty: String::new(),
            };
        }
    };

    match area {
        // Explore is the public catalog. A signed-in person sees the same
        // list as a visitor, so a private Cookbook never appears there.
        Area::Explore => repositories.retain(|repository| !repository.private),
        // Forgejo answers with what this person owns and what they may work
        // on, and Shared with me is the second part of that.
        Area::Shared => {
            let login = viewer.map(|user| user.login.clone()).unwrap_or_default();
            repositories.retain(|repository| !repository.owner.login.eq_ignore_ascii_case(&login));
        }
        Area::Mine | Area::Favorites => {}
    }

    let entries = cookbook::entries(&state.pool, &state.forgejo, token, &repositories).await;

    let words = query.words().to_lowercase();
    let mut candidates: Vec<cookbook::Indexed> = entries
        .into_iter()
        .filter(|entry| words.is_empty() || entry.title.to_lowercase().contains(&words))
        .collect();

    if query.sort() == Sort::Title {
        candidates.sort_by_key(|entry| entry.title.to_lowercase());
    }

    let total = candidates.len();
    candidates.truncate(PAGE_SIZE);

    let mut notice = None;
    if truncated {
        notice = Some(format!(
            "This installation has more Cookbooks than one list shows. The first {} come first.",
            cookbook::MAX_REPOSITORIES
        ));
    } else if total > PAGE_SIZE {
        notice = Some(format!(
            "{total} Cookbooks match. The first {PAGE_SIZE} are here. Search to make the list shorter."
        ));
    }

    Listing {
        cards: candidates.into_iter().map(card).collect(),
        notice,
        empty: empty_message(area, &words),
    }
}

fn card(entry: cookbook::Indexed) -> CookbookCard {
    CookbookCard {
        owner: entry.owner,
        slug: entry.slug,
        title: entry.title,
        private: entry.private,
        summary: entry.summary,
    }
}

fn empty_message(area: Area, words: &str) -> String {
    if !words.is_empty() {
        return "No Cookbook title contains these words.".to_string();
    }

    match area {
        Area::Mine => "You have no Cookbooks yet. Select New Cookbook to make your first one.",
        Area::Shared => "Nobody shares a Cookbook with you yet.",
        Area::Favorites => "You have no Favorite Cookbooks yet.",
        Area::Explore => "This installation has no public Cookbook yet.",
    }
    .to_string()
}

/// The three lists of the Cookbooks area, as links that keep the search.
pub fn tabs(query: &BrowseQuery, area: Area) -> Vec<Tab> {
    [
        (Area::Mine, "Mine"),
        (Area::Shared, "Shared with me"),
        (Area::Favorites, "Favorites"),
    ]
    .into_iter()
    .map(|(tab, name)| Tab {
        name,
        href: address("/cookbooks", Some(tab), query),
        active: tab == area,
    })
    .collect()
}

/// Build an address that keeps the search and the order.
fn address(path: &str, area: Option<Area>, query: &BrowseQuery) -> String {
    let mut parts: Vec<String> = Vec::new();

    if let Some(area) = area {
        parts.push(format!("area={}", encode(area.as_str())));
    }
    let words = query.words();
    if !words.is_empty() {
        parts.push(format!("q={}", encode(&words)));
    }
    if query.sort() != Sort::default() {
        parts.push(format!("sort={}", encode(query.sort().as_str())));
    }

    if parts.is_empty() {
        path.to_string()
    } else {
        format!("{path}?{}", parts.join("&"))
    }
}

/// Percent-encode a query value. Only the unreserved set of RFC 3986 stays.
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[derive(Template)]
#[template(path = "cookbook_list.html")]
struct ListTemplate {
    layout: Layout,
    forgejo_url: String,
    cookbooks: Vec<CookbookCard>,
    /// Mine, Shared with me, and Favorites.
    tabs: Vec<Tab>,
    controls: Controls,
    notice: Option<String>,
    empty: String,
}

async fn list(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(user): MaybeUser,
    Query(query): Query<BrowseQuery>,
) -> Response {
    let area = Area::parse(query.area.as_deref().unwrap_or_default());
    let token = crate::web::viewer_token(&state, &jar).await;

    let listing = listing(&state, user.as_ref(), token.as_ref(), area, &query).await;

    respond(ListTemplate {
        layout: Layout::new(user.as_ref()).on(&headers, "/cookbooks"),
        forgejo_url: state.forgejo.public_url().to_string(),
        cookbooks: listing.cards,
        tabs: tabs(&query, area),
        controls: Controls {
            action: "/cookbooks".to_string(),
            area: area.as_str().to_string(),
            q: query.words(),
            sort: query.sort(),
        },
        notice: listing.notice,
        empty: listing.empty,
    })
}

#[derive(Template)]
#[template(path = "cookbook_explore.html")]
struct ExploreTemplate {
    layout: Layout,
    forgejo_url: String,
    cookbooks: Vec<CookbookCard>,
    controls: Controls,
    notice: Option<String>,
    empty: String,
}

async fn explore(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(user): MaybeUser,
    Query(query): Query<BrowseQuery>,
) -> Response {
    // Explore needs no account. A visitor who is signed in still gets the
    // public catalog, so what a link shows does not depend on who follows it.
    let token = crate::web::viewer_token(&state, &jar).await;

    let listing = listing(&state, user.as_ref(), token.as_ref(), Area::Explore, &query).await;

    respond(ExploreTemplate {
        layout: Layout::new(user.as_ref())
            .on(&headers, &address("/explore/cookbooks", None, &query)),
        forgejo_url: state.forgejo.public_url().to_string(),
        cookbooks: listing.cards,
        controls: Controls {
            action: "/explore/cookbooks".to_string(),
            area: String::new(),
            q: query.words(),
            sort: query.sort(),
        },
        notice: listing.notice,
        empty: listing.empty,
    })
}

// --------------------------------------------------------- the create form

/// What the create form sends.
///
/// The form carries no file, so it arrives as ordinary form data.
#[derive(Debug, Clone, Default, Deserialize)]
struct CreateForm {
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    visibility: String,
    /// `preview` or `create`. A browser sends the button that was pressed.
    #[serde(default)]
    action: String,
}

impl CreateForm {
    /// Public is the default, so only the exact word makes a Cookbook
    /// private.
    fn private(&self) -> bool {
        self.visibility == "private"
    }

    fn wants_preview(&self) -> bool {
        self.action == "preview"
    }
}

#[derive(Template)]
#[template(path = "cookbook_new.html")]
struct NewTemplate {
    layout: Layout,
    title: String,
    /// The raw Markdown, exactly as the person typed it.
    description: String,
    private: bool,
    /// The description as HTML, when the person asked to see it. The value
    /// is already made safe by [`cookbook::render`].
    preview: Option<String>,
    errors: Vec<String>,
}

async fn new_form(headers: HeaderMap, MaybeUser(user): MaybeUser) -> Response {
    if user.is_none() {
        return Redirect::to("/auth/sign-in").into_response();
    }

    respond(NewTemplate {
        layout: Layout::new(user.as_ref()).on(&headers, "/cookbooks/new"),
        title: String::new(),
        description: String::new(),
        // Public is the default.
        private: false,
        preview: None,
        errors: Vec::new(),
    })
}

async fn create(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Form(form): Form<CreateForm>,
) -> Response {
    let Some(actor) = crate::web_recipes::actor(&state, &jar).await else {
        return Redirect::to("/auth/sign-in").into_response();
    };

    let again = |errors: Vec<String>, preview: Option<String>| {
        respond(NewTemplate {
            layout: Layout::new(current.as_ref()).on(&headers, "/cookbooks/new"),
            title: form.title.clone(),
            description: form.description.clone(),
            private: form.private(),
            preview,
            errors,
        })
    };

    // The preview writes nothing. It shows the person what their Markdown
    // becomes, on the same page, with everything they typed still there.
    if form.wants_preview() {
        return again(Vec::new(), Some(cookbook::render(&form.description)));
    }

    let input = NewCookbook {
        title: form.title.clone(),
        description: form.description.clone(),
        private: form.private(),
        noreply_domain: state.forgejo_noreply_domain.clone(),
    };

    let result = cookbook::create(
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
                "created a Cookbook"
            );

            // Put the new Cookbook in the index at once. Forgejo reports the
            // Version before the topics are set, so the message that follows
            // a creation describes a repository that is not yet a Cookbook.
            // The application made this one and knows better.
            cookbook::refresh(
                &state.pool,
                &state.forgejo,
                Some(&actor.token),
                &created.owner,
                &created.slug,
            )
            .await;

            Redirect::to(&format!("/cookbooks/{}/{}", created.owner, created.slug)).into_response()
        }
        Err(error) => {
            tracing::info!(%error, "a Cookbook was not created");
            again(vec![error.to_string()], None)
        }
    }
}

// -------------------------------------------------------- the Cookbook page

/// One Recipe inside a Cookbook.
///
/// A Cookbook holds its Recipes by reference. Git records which Recipe and
/// which Version, and this list is read from there on every request.
///
/// A Recipe that this person cannot read carries no title, no owner, and no
/// name here, because each of those says what the Recipe is.
pub type CookbookRecipe = cookbook::Held;

#[derive(Template)]
#[template(path = "cookbook_show.html")]
struct ShowTemplate {
    layout: Layout,
    owner: String,
    /// The technical name of the Cookbook, for an action that links to it.
    slug: String,
    title: String,
    private: bool,
    /// The description as HTML. The value is already made safe by
    /// [`cookbook::render`].
    description: String,
    /// Whether the Cookbook says anything about itself at all.
    has_description: bool,
    /// The Recipes of this Cookbook, in alphabetical order by title.
    recipes: Vec<CookbookRecipe>,
    /// Whether this person can add a Recipe and take one out.
    can_change: bool,
    forgejo_url: String,
    /// Which area of the Cookbook this page is, for the shared areas nav.
    area: &'static str,
    /// States that this interface cannot show properly. Each one is named
    /// and none of them is repaired.
    problems: Vec<String>,
    /// Why a Recipe of this Cookbook does not follow, when one does not.
    /// The application names the state and repairs none of it.
    follow_problems: Vec<String>,
    /// Why the last action did not happen, when it did not.
    errors: Vec<String>,
}

/// The Cookbook, as this person may see it.
struct Book {
    repository: crate::forgejo::Repository,
    token: Option<Secret<String>>,
    /// Whether this person can publish a Version of the Cookbook.
    can_change: bool,
}

/// Read the Cookbook that a page is about.
///
/// Forgejo answers whether this person may see it, and whether they may
/// change it. Neither answer comes from the index.
async fn book(
    state: &AppState,
    jar: &CookieJar,
    owner: &str,
    slug: &str,
) -> Result<Book, Response> {
    // A public Cookbook is readable without a session. Forgejo applies the
    // permissions, so a private one needs the credential of somebody who
    // may see it.
    let token = crate::web::viewer_token(state, jar).await;

    let missing =
        || (StatusCode::NOT_FOUND, "This Cookbook is not available.").into_response() as Response;

    let repository = match state
        .forgejo
        .repository_as(token.as_ref(), owner, slug)
        .await
    {
        Ok(repository) => repository,
        Err(error) => {
            tracing::info!(%error, %owner, %slug, "cannot read the Cookbook repository");
            return Err(missing());
        }
    };

    // A repository without the marker is not a Cookbook, whatever it holds.
    // This is what keeps a Recipe out of the Cookbook pages.
    if !cookbook::is_cookbook(&repository) {
        return Err(missing());
    }

    let can_change = match token.as_ref() {
        Some(token) => state
            .forgejo
            .can_write(token, owner, slug)
            .await
            .unwrap_or(false),
        None => false,
    };

    Ok(Book {
        repository,
        token,
        can_change,
    })
}

/// Draw the Cookbook page.
///
/// `errors` says why the last action did not happen. Every other value on
/// the page is read again here, so the page always shows what Forgejo and
/// Git hold now.
async fn draw(
    state: &AppState,
    headers: &HeaderMap,
    current: Option<&CurrentUser>,
    book: &Book,
    errors: Vec<String>,
) -> Response {
    let owner = book.repository.owner.login.clone();
    let slug = book.repository.name.clone();
    let here = format!("/cookbooks/{owner}/{slug}");

    let bytes = state
        .forgejo
        .raw_file(
            book.token.as_ref(),
            &owner,
            &slug,
            book.repository.branch(),
            cookbook::README_FILE,
        )
        .await;

    let readme = match bytes {
        Ok(bytes) => cookbook::read_readme(&bytes),
        // A Cookbook with no README at all is a state a person can reach by
        // pushing, and one this interface cannot repair. Name it.
        Err(error) => {
            tracing::info!(%error, %owner, %slug, "cannot read the Cookbook README");
            cookbook::Readme {
                title: None,
                description: String::new(),
                problems: vec![cookbook::NO_README_MESSAGE.to_string()],
            }
        }
    };

    let contents =
        cookbook::references(&state.forgejo, book.token.as_ref(), &book.repository).await;
    let recipes =
        cookbook::held_recipes(&state.pool, &state.forgejo, book.token.as_ref(), &contents).await;

    let follow_problems = follow_problems(state, &book.repository, &contents.references).await;

    let description = cookbook::render(&readme.description);

    respond(ShowTemplate {
        layout: Layout::new(current).on(headers, &here),
        title: readme.title.unwrap_or_else(|| book.repository.name.clone()),
        owner,
        slug,
        private: book.repository.private,
        has_description: !description.trim().is_empty(),
        description,
        recipes,
        can_change: book.can_change,
        forgejo_url: state.forgejo.web_url(&book.repository.full_name),
        area: "cookbook",
        problems: readme.problems,
        follow_problems,
        errors,
    })
}

/// Why the Recipes of this Cookbook do not follow, when they do not.
///
/// A Cookbook that follows nothing needs no automation, so it is asked
/// nothing. One that does follow something is asked twice: whether this
/// installation has an automation account at all, and whether Forgejo still
/// lets that account write here. Each answer names the state and repairs
/// none of it, so an administrator who took the access away in Forgejo does
/// not get it back by opening a page.
async fn follow_problems(
    state: &AppState,
    repository: &crate::forgejo::Repository,
    references: &[cookbook::Reference],
) -> Vec<String> {
    if !crate::automation::follows_anything(references) {
        return Vec::new();
    }

    let Some(automation) = crate::automation::of(&state.pool, &state.cipher).await else {
        return vec![crate::automation::NO_CREDENTIAL_MESSAGE.to_string()];
    };

    let allowed = crate::automation::can_write(
        &state.forgejo,
        &automation,
        &repository.owner.login,
        &repository.name,
    )
    .await;

    if allowed {
        Vec::new()
    } else {
        vec![crate::automation::NO_ACCESS_MESSAGE.to_string()]
    }
}

async fn show(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug)): Path<(String, String)>,
) -> Response {
    match book(&state, &jar, &owner, &slug).await {
        Ok(book) => draw(&state, &headers, current.as_ref(), &book, Vec::new()).await,
        Err(refusal) => refusal,
    }
}

// ------------------------------------------------- adding a Recipe to one

/// One Recipe that a person can put into this Cookbook.
#[derive(Debug, Clone)]
pub struct RecipeChoice {
    pub owner: String,
    pub slug: String,
    pub title: String,
    /// `owner/slug`, which is what the form carries.
    pub value: String,
}

/// The choice that a person gets before a private Recipe goes into a
/// Cookbook that other people can reach.
///
/// Nothing is written while this is on the screen. The person selects one of
/// three answers, and each one is a form that posts.
pub struct AddGap {
    /// The Recipe that is about to go into the Cookbook.
    pub recipe: cookbook::Named,
    /// The value that the form carries for the Recipe.
    pub value: String,
    /// The value that the form carries for how the Cookbook holds it.
    pub holding: String,
    /// Whether all users can read this Cookbook.
    pub public: bool,
    /// The people that Forgejo says cannot read the Recipe.
    pub shut: Vec<cookbook::Sharer>,
    /// The people that Forgejo did not answer about.
    pub silent: Vec<cookbook::Sharer>,
}

impl AddGap {
    /// Whether there is a person to offer a grant for.
    ///
    /// A public Cookbook can name a private Recipe to every user, and no
    /// grant covers every user, so the page then explains and offers no
    /// grant.
    pub fn has_people(&self) -> bool {
        !self.shut.is_empty() || !self.silent.is_empty()
    }
}

#[derive(Template)]
#[template(path = "cookbook_add_recipe.html")]
struct AddTemplate {
    layout: Layout,
    owner: String,
    slug: String,
    title: String,
    /// The Recipes that this person can put into this Cookbook.
    choices: Vec<RecipeChoice>,
    /// What the person searched for.
    q: String,
    /// A message about the state of the list, when there is one to give.
    notice: Option<String>,
    /// The access mismatch, while a person decides what to do about it.
    gap: Option<AddGap>,
    errors: Vec<String>,
}

/// Which Recipes a person can put into this Cookbook.
///
/// Forgejo names every Recipe that this person can read, and the Recipes
/// that the Cookbook holds already are taken out. The index only supplies
/// the title.
async fn choices(
    state: &AppState,
    token: &Secret<String>,
    viewer: Option<i64>,
    held: &[cookbook::Reference],
    words: &str,
) -> (Vec<RecipeChoice>, bool) {
    // Two questions, because one does not cover the answer. The first names
    // every public Recipe, which is what makes a Cookbook of somebody
    // else's Recipes possible. The second names what this person owns and
    // what is shared with them, which is where a private Recipe is.
    let mut repositories: Vec<crate::forgejo::Repository> = Vec::new();
    let mut truncated = false;

    let mut scopes = vec![Ownership::Anybody];
    if let Some(id) = viewer {
        scopes.push(Ownership::ReachableBy(id));
    }

    for ownership in scopes {
        match crate::index::visible(&state.forgejo, Some(token), ownership).await {
            Ok((found, cut)) => {
                truncated = truncated || cut;
                for repository in found {
                    if !repositories.iter().any(|held| held.id == repository.id) {
                        repositories.push(repository);
                    }
                }
            }
            Err(error) => {
                tracing::warn!(%error, "cannot ask Forgejo for the Recipes");
            }
        }
    }

    // A Recipe that the Cookbook holds already cannot be added a second
    // time, so it is not offered.
    let taken: Vec<(String, String)> = held
        .iter()
        .filter_map(|reference| cookbook::recipe_named_by(&state.forgejo, &reference.url))
        .collect();

    let repositories: Vec<crate::forgejo::Repository> = repositories
        .into_iter()
        .filter(|repository| {
            !taken.iter().any(|(owner, slug)| {
                owner.eq_ignore_ascii_case(&repository.owner.login)
                    && slug.eq_ignore_ascii_case(&repository.name)
            })
        })
        .collect();

    let entries =
        crate::index::entries(&state.pool, &state.forgejo, Some(token), &repositories).await;

    let words = words.to_lowercase();
    let mut found: Vec<RecipeChoice> = entries
        .into_iter()
        .filter(|entry| words.is_empty() || entry.title.to_lowercase().contains(&words))
        .map(|entry| RecipeChoice {
            value: format!("{}/{}", entry.owner, entry.slug),
            owner: entry.owner,
            slug: entry.slug,
            title: entry.title,
        })
        .collect();

    found.sort_by(|one, two| {
        one.title
            .to_lowercase()
            .cmp(&two.title.to_lowercase())
            .then_with(|| one.owner.to_lowercase().cmp(&two.owner.to_lowercase()))
    });

    (found, truncated)
}

/// What the search on the add page carries.
#[derive(Debug, Clone, Default, Deserialize)]
struct SearchQuery {
    #[serde(default)]
    q: Option<String>,
}

impl SearchQuery {
    fn words(&self) -> String {
        self.q.clone().unwrap_or_default().trim().to_string()
    }
}

/// Draw the page that adds a Recipe.
#[allow(clippy::too_many_arguments)]
async fn draw_add(
    state: &AppState,
    headers: &HeaderMap,
    current: Option<&CurrentUser>,
    book: &Book,
    token: &Secret<String>,
    words: &str,
    errors: Vec<String>,
    gap: Option<AddGap>,
) -> Response {
    let owner = book.repository.owner.login.clone();
    let slug = book.repository.name.clone();

    let contents =
        cookbook::references(&state.forgejo, book.token.as_ref(), &book.repository).await;
    let (choices, truncated) = choices(
        state,
        token,
        current.map(|person| person.forgejo_user_id),
        &contents.references,
        words,
    )
    .await;

    let notice = if truncated {
        Some(format!(
            "This installation has more Recipes than one list shows. The first {} are here. Search to find another one.",
            crate::index::MAX_REPOSITORIES
        ))
    } else {
        None
    };

    let bytes = state
        .forgejo
        .raw_file(
            book.token.as_ref(),
            &owner,
            &slug,
            book.repository.branch(),
            cookbook::README_FILE,
        )
        .await
        .unwrap_or_default();

    respond(AddTemplate {
        layout: Layout::new(current).on(headers, &format!("/cookbooks/{owner}/{slug}/recipes")),
        title: cookbook::read_readme(&bytes)
            .title
            .unwrap_or_else(|| slug.clone()),
        owner,
        slug,
        choices,
        q: words.to_string(),
        notice,
        gap,
        errors,
    })
}

async fn add_form(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug)): Path<(String, String)>,
    Query(query): Query<SearchQuery>,
) -> Response {
    let book = match book(&state, &jar, &owner, &slug).await {
        Ok(book) => book,
        Err(refusal) => return refusal,
    };

    let Some(token) = book.token.clone().filter(|_| book.can_change) else {
        return refuse_change(current.is_some());
    };

    draw_add(
        &state,
        &headers,
        current.as_ref(),
        &book,
        &token,
        &query.words(),
        Vec::new(),
        None,
    )
    .await
}

/// What the add form sends.
#[derive(Debug, Clone, Default, Deserialize)]
struct AddForm {
    /// `owner/slug` of the Recipe.
    #[serde(default)]
    recipe: String,
    /// `pinned` or `following`. Pinned is the default.
    #[serde(default)]
    holding: String,
    /// `yes` after the person read the access mismatch and decided.
    #[serde(default)]
    confirm: String,
    /// `yes` when the person asked for the Recipe grants as well.
    #[serde(default)]
    grant: String,
}

/// The people who can reach a Cookbook, apart from the person who asks.
///
/// Forgejo records every one of them. The Owner is in the list, because the
/// Owner reads the Cookbook too. The person who is adding the Recipe is not,
/// because Forgejo already showed them the Recipe.
async fn cookbook_sharers(
    state: &AppState,
    token: &Secret<String>,
    repository: &crate::forgejo::Repository,
    asking: &str,
) -> Vec<cookbook::Sharer> {
    let owner = &repository.owner.login;
    let slug = &repository.name;

    let mut people: Vec<cookbook::Sharer> = Vec::new();

    let mut keep = |login: String, name: String| {
        if login.eq_ignore_ascii_case(asking) || crate::web_sharing::is_service_identity(&login) {
            return;
        }
        if people
            .iter()
            .any(|person| person.login.eq_ignore_ascii_case(&login))
        {
            return;
        }
        people.push(cookbook::Sharer { login, name });
    };

    keep(owner.clone(), owner.clone());

    match state.forgejo.list_collaborators(token, owner, slug).await {
        Ok(found) => {
            for user in found {
                keep(user.login.clone(), user.display_name().to_string());
            }
        }
        Err(error) => {
            tracing::warn!(%error, %owner, %slug, "cannot read who shares this Cookbook");
        }
    }

    people.sort_by_key(|person| person.login.to_lowercase());
    people
}

async fn add_recipe(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug)): Path<(String, String)>,
    Form(form): Form<AddForm>,
) -> Response {
    let book = match book(&state, &jar, &owner, &slug).await {
        Ok(book) => book,
        Err(refusal) => return refusal,
    };

    if !book.can_change {
        return refuse_change(current.is_some());
    }

    let Some(actor) = crate::web_recipes::actor(&state, &jar).await else {
        return Redirect::to("/auth/sign-in").into_response();
    };

    let again = |errors: Vec<String>, gap: Option<AddGap>| {
        draw_add(
            &state,
            &headers,
            current.as_ref(),
            &book,
            &actor.token,
            "",
            errors,
            gap,
        )
    };

    let Some((recipe_owner, recipe_slug)) = form.recipe.trim().split_once('/') else {
        return again(vec![cookbook::HoldError::NoRecipe.to_string()], None).await;
    };

    // Forgejo says whether this person can read the Recipe at all. Nothing
    // is added on the word of the form.
    let recipe = match state
        .forgejo
        .repository(&actor.token, recipe_owner, recipe_slug)
        .await
    {
        Ok(repository) if crate::index::is_recipe(&repository) => repository,
        Ok(_) | Err(_) => {
            return again(vec!["That Recipe is not available.".to_string()], None).await;
        }
    };

    let title = recipe_title(&state, &actor.token, &recipe).await;

    // A Cookbook gives no access to the Recipes in it, and it names every
    // one of them. So, before a private Recipe goes in, Forgejo answers for
    // each person who can reach the Cookbook whether they can read it.
    let named = cookbook::Named {
        owner: recipe.owner.login.clone(),
        slug: recipe.name.clone(),
        title: title.clone(),
    };
    let sharers = cookbook_sharers(&state, &actor.token, &book.repository, &actor.user.login).await;
    let people =
        cookbook::people_out_of_reach(&state.forgejo, &actor.token, &recipe, &sharers).await;

    // A public Cookbook names its Recipes to every user. No grant covers
    // every user, so this is said and no grant is offered for it.
    let names_it_widely = recipe.private && !book.repository.private;

    let make_gap = || AddGap {
        recipe: named.clone(),
        value: form.recipe.trim().to_string(),
        holding: cookbook::Holding::parse(&form.holding).as_str().to_string(),
        public: !book.repository.private,
        shut: people.shut.clone(),
        silent: people.silent.clone(),
    };

    // The person reads the mismatch and then decides. Nothing changed yet.
    if (!people.is_empty() || names_it_widely) && form.confirm != "yes" {
        return again(Vec::new(), Some(make_gap())).await;
    }

    if form.grant == "yes" && !people.is_empty() {
        let refusals =
            cookbook::grant_readers(&state.forgejo, &actor.token, &named, &people.each()).await;

        // A grant that Forgejo refused is named, and the Cookbook is left
        // alone. The person can select Add it anyway from the same page.
        if !refusals.is_empty() {
            return again(refusals, Some(make_gap())).await;
        }
    }

    let result = cookbook::add_recipe(
        &state.forgejo,
        state.git.as_ref(),
        &actor.token,
        &actor.user,
        cookbook::AddRecipe {
            cookbook: &book.repository,
            recipe: &recipe,
            holding: cookbook::Holding::parse(&form.holding),
            title: &title,
            noreply_domain: &state.forgejo_noreply_domain,
        },
    )
    .await;

    match result {
        Ok(added) => {
            tracing::info!(
                cookbook = %book.repository.full_name,
                recipe = %recipe.full_name,
                path = %added.path,
                "a Cookbook holds one more Recipe"
            );
            align(&state, &actor.token, &book.repository, &added.references).await;
            Redirect::to(&format!("/cookbooks/{owner}/{slug}")).into_response()
        }
        Err(error) => {
            tracing::info!(%error, "a Recipe was not added to a Cookbook");
            again(vec![reason(&error)], None).await
        }
    }
}

/// Give the automation the access this Cookbook needs, and no other access.
///
/// The automation gets write access to a Cookbook that follows a Recipe,
/// and loses it when the Cookbook follows none. Forgejo decides whether the
/// person who asked may give it: a refusal changes nothing here, because
/// Git already holds what the person asked for, and the Cookbook page then
/// reports that the automation cannot run.
async fn align(
    state: &AppState,
    actor: &Secret<String>,
    repository: &crate::forgejo::Repository,
    references: &[cookbook::Reference],
) {
    let automation = crate::automation::of(&state.pool, &state.cipher).await;

    crate::automation::align(
        &state.forgejo,
        actor,
        repository,
        references,
        automation.as_ref(),
    )
    .await;
}

/// What the remove form sends.
#[derive(Debug, Clone, Default, Deserialize)]
struct RemoveForm {
    /// Where the Recipe sits inside the Cookbook.
    #[serde(default)]
    path: String,
}

async fn remove_recipe(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug)): Path<(String, String)>,
    Form(form): Form<RemoveForm>,
) -> Response {
    let book = match book(&state, &jar, &owner, &slug).await {
        Ok(book) => book,
        Err(refusal) => return refusal,
    };

    if !book.can_change {
        return refuse_change(current.is_some());
    }

    let Some(actor) = crate::web_recipes::actor(&state, &jar).await else {
        return Redirect::to("/auth/sign-in").into_response();
    };

    let path = form.path.trim();

    // History reads better with the Recipe title in it, and the title is
    // known only while the Recipe is still there.
    let contents = cookbook::references(&state.forgejo, Some(&actor.token), &book.repository).await;
    let held =
        cookbook::held_recipes(&state.pool, &state.forgejo, Some(&actor.token), &contents).await;
    let title = held
        .iter()
        .find(|recipe| recipe.available && recipe.path == path)
        .map(|recipe| recipe.title.clone())
        .unwrap_or_else(|| path.to_string());

    let result = cookbook::remove_recipe(
        &state.forgejo,
        state.git.as_ref(),
        &actor.token,
        &actor.user,
        cookbook::RemoveRecipe {
            cookbook: &book.repository,
            path,
            title: &title,
            noreply_domain: &state.forgejo_noreply_domain,
        },
    )
    .await;

    match result {
        Ok(removed) => {
            tracing::info!(
                cookbook = %book.repository.full_name,
                "a Cookbook holds one Recipe less"
            );
            align(&state, &actor.token, &book.repository, &removed.references).await;
            Redirect::to(&format!("/cookbooks/{owner}/{slug}")).into_response()
        }
        Err(error) => {
            tracing::info!(%error, "a Recipe was not taken out of a Cookbook");
            draw(
                &state,
                &headers,
                current.as_ref(),
                &book,
                vec![reason(&error)],
            )
            .await
        }
    }
}

// -------------------------------------------------- Pinned and Following

/// What the form that changes Pinned and Following sends.
#[derive(Debug, Clone, Default, Deserialize)]
struct HoldingForm {
    /// Where the Recipe sits inside the Cookbook.
    #[serde(default)]
    path: String,
    /// `pinned` or `following`. Anything else keeps the Version.
    #[serde(default)]
    holding: String,
}

/// Change one Recipe of this Cookbook between Pinned and Following.
///
/// Only the Cookbook changes. Following moves it to the Version the Recipe
/// has now and keeps it moving; Pinned keeps the Version the Cookbook holds.
async fn set_holding(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug)): Path<(String, String)>,
    Form(form): Form<HoldingForm>,
) -> Response {
    let book = match book(&state, &jar, &owner, &slug).await {
        Ok(book) => book,
        Err(refusal) => return refusal,
    };

    if !book.can_change {
        return refuse_change(current.is_some());
    }

    let Some(actor) = crate::web_recipes::actor(&state, &jar).await else {
        return Redirect::to("/auth/sign-in").into_response();
    };

    let path = form.path.trim();
    let holding = cookbook::Holding::parse(&form.holding);

    // History reads better with the Recipe title in it, and the title is a
    // fact about the Recipe rather than about the Cookbook.
    let contents = cookbook::references(&state.forgejo, Some(&actor.token), &book.repository).await;
    let held =
        cookbook::held_recipes(&state.pool, &state.forgejo, Some(&actor.token), &contents).await;
    let title = held
        .iter()
        .find(|recipe| recipe.available && recipe.path == path)
        .map(|recipe| recipe.title.clone())
        .unwrap_or_else(|| path.to_string());

    let result = cookbook::set_holding(
        &state.forgejo,
        state.git.as_ref(),
        &actor.token,
        &actor.user,
        cookbook::SetHolding {
            cookbook: &book.repository,
            path,
            holding,
            title: &title,
            noreply_domain: &state.forgejo_noreply_domain,
        },
    )
    .await;

    match result {
        Ok(switched) => {
            tracing::info!(
                cookbook = %book.repository.full_name,
                %path,
                holding = holding.as_str(),
                "a Cookbook holds a Recipe another way"
            );
            align(&state, &actor.token, &book.repository, &switched.references).await;
            Redirect::to(&format!("/cookbooks/{owner}/{slug}")).into_response()
        }
        Err(error) => {
            tracing::info!(%error, "a Recipe of a Cookbook was not changed");
            draw(
                &state,
                &headers,
                current.as_ref(),
                &book,
                vec![reason(&error)],
            )
            .await
        }
    }
}

// ------------------------------------------------ the History of one Cookbook

/// How many Versions of a Cookbook one page shows.
const HISTORY_SIZE: u32 = 50;

/// One Version of a Cookbook, as a person reads it.
#[derive(Debug, Clone)]
pub struct CookbookVersion {
    /// What was written about the change.
    pub description: String,
    pub author: String,
    pub moment: String,
    /// Whether the automation of this installation made this Version. A
    /// Version that a person made carries the name of that person.
    pub automatic: bool,
}

#[derive(Template)]
#[template(path = "cookbook_history.html")]
struct HistoryTemplate {
    layout: Layout,
    owner: String,
    slug: String,
    title: String,
    /// The Versions of this Cookbook, the newest first.
    versions: Vec<CookbookVersion>,
    forgejo_url: String,
    /// Whether Forgejo says this person can change the Cookbook. The areas
    /// nav offers Sharing only to them.
    can_change: bool,
    /// Which area of the Cookbook this page is, for the shared areas nav.
    area: &'static str,
    /// A message about the state of the list, when there is one to give.
    notice: Option<String>,
}

/// Shown when Forgejo cannot report the History of a Cookbook.
const NO_HISTORY: &str = "CookLangHub cannot read the History of this Cookbook now. Open the Cookbook in Forgejo to read it.";

/// The History of one Cookbook.
///
/// Git holds it and Forgejo reads it out, so nothing here comes from the
/// index. Every Version is shown, and the ones that the automation made
/// carry less weight than the ones that a person made.
async fn history(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug)): Path<(String, String)>,
) -> Response {
    let book = match book(&state, &jar, &owner, &slug).await {
        Ok(book) => book,
        Err(refusal) => return refusal,
    };

    let automation = crate::automation::of(&state.pool, &state.cipher).await;

    let (versions, notice) = match state
        .forgejo
        .list_commits(
            book.token.as_ref(),
            &owner,
            &slug,
            book.repository.branch(),
            HISTORY_SIZE,
        )
        .await
    {
        Ok(commits) => (
            commits
                .iter()
                .map(|commit| version_row(commit, automation.as_ref()))
                .collect(),
            None,
        ),
        Err(error) => {
            tracing::info!(%error, %owner, %slug, "cannot read the History of this Cookbook");
            (Vec::new(), Some(NO_HISTORY.to_string()))
        }
    };

    let bytes = state
        .forgejo
        .raw_file(
            book.token.as_ref(),
            &owner,
            &slug,
            book.repository.branch(),
            cookbook::README_FILE,
        )
        .await
        .unwrap_or_default();

    respond(HistoryTemplate {
        layout: Layout::new(current.as_ref())
            .on(&headers, &format!("/cookbooks/{owner}/{slug}/history")),
        title: cookbook::read_readme(&bytes)
            .title
            .unwrap_or_else(|| slug.clone()),
        owner,
        slug,
        versions,
        forgejo_url: state.forgejo.web_url(&book.repository.full_name),
        can_change: book.can_change,
        area: "history",
        notice,
    })
}

/// One Version of a Cookbook, as the page needs it.
fn version_row(
    commit: &crate::forgejo::Commit,
    automation: Option<&crate::automation::Automation>,
) -> CookbookVersion {
    let written = commit
        .commit
        .author
        .as_ref()
        .map(|identity| identity.name.trim().to_string())
        .unwrap_or_default();

    let account = commit
        .author
        .as_ref()
        .map(|user| user.login.clone())
        .unwrap_or_default();

    // The Forgejo account decides it. A Version made outside this
    // application can name somebody Forgejo has no account for, and then
    // the name that Git holds is the best there is.
    let automatic = automation.is_some_and(|automation| {
        account.eq_ignore_ascii_case(&automation.login)
            || (account.is_empty() && written == automation.name)
    });

    let author = match commit
        .author
        .as_ref()
        .map(|user| user.display_name().trim())
    {
        Some(name) if !name.is_empty() => name.to_string(),
        _ if !written.is_empty() => written,
        _ => "Somebody".to_string(),
    };

    let description = commit
        .commit
        .message
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();

    CookbookVersion {
        description: if description.is_empty() {
            "No description".to_string()
        } else {
            description
        },
        author,
        moment: crate::web_history::moment(
            commit
                .commit
                .author
                .as_ref()
                .map(|identity| identity.date.as_str())
                .unwrap_or_default(),
        ),
        automatic,
    }
}

/// Shown when Forgejo or Git could not do what a person asked.
///
/// The words of a failed command are not something a person can act on, and
/// they name the parts of the machine that this interface hides.
const NOT_NOW: &str =
    "CookLangHub cannot change this Cookbook now. Open the Cookbook in Forgejo to see its state.";

/// What a person reads when an action did not happen.
///
/// A refusal that this application made says what is wrong and what to do.
/// A failure of Forgejo or of Git says neither, so it becomes one sentence
/// and the reason itself goes to the log.
fn reason(error: &cookbook::HoldError) -> String {
    match error {
        cookbook::HoldError::Forgejo(_) | cookbook::HoldError::Git(_) => NOT_NOW.to_string(),
        refusal => refusal.to_string(),
    }
}

/// The title of a Recipe, for the message that History records.
async fn recipe_title(
    state: &AppState,
    token: &Secret<String>,
    repository: &crate::forgejo::Repository,
) -> String {
    let found = crate::index::entries(
        &state.pool,
        &state.forgejo,
        Some(token),
        std::slice::from_ref(repository),
    )
    .await;

    found
        .first()
        .map(|entry| entry.title.clone())
        .unwrap_or_else(|| repository.name.clone())
}

/// Refuse an action to somebody who cannot change this Cookbook.
///
/// Forgejo made this decision, and it is asked again on every request. A
/// visitor with no account is sent to sign in, because signing in can change
/// the answer. Anybody else gets a plain refusal.
fn refuse_change(signed_in: bool) -> Response {
    if !signed_in {
        return Redirect::to("/auth/sign-in").into_response();
    }

    (
        StatusCode::FORBIDDEN,
        "Only a person who can change this Cookbook can add a Recipe or take one out.",
    )
        .into_response()
}

// ----------------------------------------------------- sharing a Cookbook

/// The Forgejo access mode that a Reader gets.
const FORGEJO_READ: &str = "read";
/// The Forgejo access mode that an Editor gets.
const FORGEJO_WRITE: &str = "write";

/// The name for an access mode that this screen does not hand out.
const UNMANAGED_ROLE: &str = "Set in Forgejo";

/// What the confirmation must say before a Cookbook becomes public.
///
/// The second half is the part that surprises people. A Cookbook names each
/// Recipe that it holds, and that list is in the Cookbook. A public Cookbook
/// therefore publishes the name and the address of every Recipe in it, and a
/// private Recipe keeps only its content private.
pub const PUBLIC_COOKBOOK_WARNING: &str = "All users can read this Cookbook and its earlier Versions. This Cookbook also names each Recipe that it holds, and that list becomes public. A private Recipe stays private. Its name in the list becomes public.";

/// What a person reads when Cookbook access is mistaken for Recipe access.
pub const SEPARATE_ACCESS: &str = "Access to this Cookbook is not access to its Recipes. Each Recipe keeps its own Sharing, and Forgejo holds it.";

/// The role that this screen shows for a Forgejo access mode.
fn cookbook_role(permission: &str) -> &'static str {
    match permission {
        FORGEJO_READ => "Reader",
        FORGEJO_WRITE => "Editor",
        _ => UNMANAGED_ROLE,
    }
}

/// Whether this screen can change an access mode.
///
/// Forgejo Administrator and Forgejo Manager are real access that the Owner
/// must see. They are not roles this screen gives or takes.
fn cookbook_manageable(permission: &str) -> bool {
    matches!(permission, FORGEJO_READ | FORGEJO_WRITE)
}

/// One person who can reach a Cookbook.
pub struct Person {
    pub login: String,
    pub name: String,
    pub role: &'static str,
    /// Whether this screen gives the Owner a control for this person.
    pub managed: bool,
}

/// What Forgejo says about one Cookbook and the person who asks about it.
struct Sharing {
    actor: crate::web_recipes::Actor,
    repository: crate::forgejo::Repository,
    is_owner: bool,
}

/// Why a Sharing page or a Sharing action cannot go on.
enum Stop {
    /// Nobody is signed in.
    SignIn,
    /// Forgejo does not show this Cookbook to this person.
    Unknown,
    /// Forgejo says this person does not own the Cookbook.
    NotOwner(Box<Sharing>),
}

/// The choice that a person gets before an access mismatch happens.
///
/// Nothing is written while this is on the screen. The person selects one of
/// three answers, and each one is a form that posts.
pub struct Gap {
    /// The person who is about to reach the Cookbook.
    pub login: String,
    /// The value that the form carries for the role.
    pub role: String,
    /// The role in cooking words.
    pub role_name: &'static str,
    /// The Recipes that Forgejo says this person cannot read.
    pub shut: Vec<cookbook::Named>,
    /// The Recipes that Forgejo did not answer about.
    pub silent: Vec<cookbook::Named>,
}

#[derive(Template)]
#[template(path = "cookbook_sharing.html")]
struct SharingTemplate {
    layout: Layout,
    owner: String,
    slug: String,
    title: String,
    forgejo_url: String,
    /// Whether Forgejo says the person who asks owns this Cookbook.
    is_owner: bool,
    /// Only a person who can change the Cookbook reaches this page, so the
    /// areas nav always offers Sharing here.
    can_change: bool,
    /// Which area of the Cookbook this page is, for the shared areas nav.
    area: &'static str,
    /// Whether Forgejo says that all users can read this Cookbook.
    public: bool,
    /// Where each form posts to, and where a cancel returns to.
    sharing_path: String,
    people: Vec<Person>,
    /// Show the Private to Public confirmation instead of the controls.
    confirming: bool,
    public_warning: &'static str,
    separate_access: &'static str,
    /// The access mismatch, while a person decides what to do about it.
    gap: Option<Gap>,
    errors: Vec<String>,
}

/// Read the Cookbook and ask Forgejo who this person is to it.
///
/// This runs before a page is drawn and again before any form acts, because
/// a check that happens only in the interface is not a check.
async fn sharing_context(
    state: &AppState,
    jar: &CookieJar,
    owner: &str,
    slug: &str,
) -> Result<Sharing, Stop> {
    let Some(actor) = crate::web_recipes::actor(state, jar).await else {
        return Err(Stop::SignIn);
    };

    // Forgejo applies its own permissions here, so a Cookbook that this
    // person may not see never reaches the next line.
    let repository = match state.forgejo.repository(&actor.token, owner, slug).await {
        Ok(repository) if cookbook::is_cookbook(&repository) => repository,
        Ok(_) => return Err(Stop::Unknown),
        Err(error) => {
            tracing::info!(%error, %owner, %slug, "cannot read the Cookbook for its Sharing area");
            return Err(Stop::Unknown);
        }
    };

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

    // A Forgejo Administrator holds the keys to a Cookbook that is not
    // theirs. They are not the Owner, so they go to Forgejo instead.
    let is_owner = repository.owner.login == actor.user.login && holds_the_keys;

    let sharing = Sharing {
        actor,
        repository,
        is_owner,
    };

    if sharing.is_owner {
        Ok(sharing)
    } else {
        Err(Stop::NotOwner(Box::new(sharing)))
    }
}

/// Who can reach this Cookbook, as Forgejo records it now.
///
/// A private Cookbook lists its Readers, because their access is explicit. A
/// public Cookbook lists only the people with more than read access.
async fn sharing_people(state: &AppState, sharing: &Sharing, public: bool) -> Vec<Person> {
    let owner = &sharing.repository.owner.login;
    let slug = &sharing.repository.name;

    let found = match state
        .forgejo
        .list_collaborators(&sharing.actor.token, owner, slug)
        .await
    {
        Ok(found) => found,
        Err(error) => {
            tracing::warn!(%error, %owner, %slug, "cannot read who shares this Cookbook");
            return Vec::new();
        }
    };

    let wanted: Vec<crate::forgejo::ForgejoUser> = found
        .into_iter()
        .filter(|user| !crate::web_sharing::is_service_identity(&user.login))
        .collect();

    let permissions = futures::future::join_all(wanted.iter().map(|user| {
        let forgejo = state.forgejo.clone();
        let token = sharing.actor.token.clone();
        let login = user.login.clone();
        let owner = owner.clone();
        let slug = slug.clone();

        async move {
            forgejo
                .repository_permission(&token, &owner, &slug, &login)
                .await
        }
    }))
    .await;

    let mut people: Vec<Person> = wanted
        .into_iter()
        .zip(permissions)
        .filter_map(|(user, permission)| {
            let permission = permission.ok()?;

            // On a public Cookbook a Reader is nobody special.
            if public && permission.is_read_only() {
                return None;
            }

            Some(Person {
                name: user.display_name().to_string(),
                login: user.login,
                role: cookbook_role(&permission.permission),
                managed: cookbook_manageable(&permission.permission),
            })
        })
        .collect();

    people.sort_by_key(|person| person.login.to_lowercase());
    people
}

/// The Recipes of this Cookbook that the Owner can read.
async fn sharing_recipes(state: &AppState, sharing: &Sharing) -> Vec<cookbook::Held> {
    let contents = cookbook::references(
        &state.forgejo,
        Some(&sharing.actor.token),
        &sharing.repository,
    )
    .await;

    cookbook::held_recipes(
        &state.pool,
        &state.forgejo,
        Some(&sharing.actor.token),
        &contents,
    )
    .await
}

/// The title of a Cookbook, as its README gives it.
async fn cookbook_title(state: &AppState, sharing: &Sharing) -> String {
    let owner = &sharing.repository.owner.login;
    let slug = &sharing.repository.name;

    let bytes = state
        .forgejo
        .raw_file(
            Some(&sharing.actor.token),
            owner,
            slug,
            sharing.repository.branch(),
            cookbook::README_FILE,
        )
        .await
        .unwrap_or_default();

    cookbook::read_readme(&bytes)
        .title
        .unwrap_or_else(|| slug.clone())
}

/// Draw the Sharing area from what Forgejo says right now.
async fn draw_sharing(
    state: &AppState,
    headers: &HeaderMap,
    current: Option<&CurrentUser>,
    sharing: &Sharing,
    errors: Vec<String>,
    confirming: bool,
    gap: Option<Gap>,
) -> Response {
    let owner = sharing.repository.owner.login.clone();
    let slug = sharing.repository.name.clone();
    let sharing_path = format!("/cookbooks/{owner}/{slug}/sharing");
    let public = !sharing.repository.private;

    let people = if sharing.is_owner {
        sharing_people(state, sharing, public).await
    } else {
        Vec::new()
    };

    respond(SharingTemplate {
        layout: Layout::new(current).on(headers, &sharing_path),
        title: cookbook_title(state, sharing).await,
        forgejo_url: state.forgejo.web_url(&sharing.repository.full_name),
        owner,
        slug,
        is_owner: sharing.is_owner,
        can_change: true,
        area: "sharing",
        public,
        sharing_path,
        people,
        confirming,
        public_warning: PUBLIC_COOKBOOK_WARNING,
        separate_access: SEPARATE_ACCESS,
        gap,
        errors,
    })
}

/// Turn a stop into the answer that a person gets.
async fn refuse_sharing(
    state: &AppState,
    headers: &HeaderMap,
    current: Option<&CurrentUser>,
    stop: Stop,
) -> Response {
    match stop {
        Stop::SignIn => Redirect::to("/auth/sign-in").into_response(),
        Stop::Unknown => (StatusCode::NOT_FOUND, "This Cookbook is not available.").into_response(),
        Stop::NotOwner(sharing) => {
            let page =
                draw_sharing(state, headers, current, &sharing, Vec::new(), false, None).await;
            (StatusCode::FORBIDDEN, page).into_response()
        }
    }
}

async fn sharing_show(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug)): Path<(String, String)>,
) -> Response {
    match sharing_context(&state, &jar, &owner, &slug).await {
        Ok(sharing) => {
            draw_sharing(
                &state,
                &headers,
                current.as_ref(),
                &sharing,
                Vec::new(),
                false,
                None,
            )
            .await
        }
        Err(stop) => refuse_sharing(&state, &headers, current.as_ref(), stop).await,
    }
}

/// The step between Private and Public.
///
/// This is a page and not a dialog, because the wording is part of the
/// decision and it must reach a person who runs no scripts.
async fn sharing_public(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug)): Path<(String, String)>,
) -> Response {
    match sharing_context(&state, &jar, &owner, &slug).await {
        Ok(sharing) => {
            draw_sharing(
                &state,
                &headers,
                current.as_ref(),
                &sharing,
                Vec::new(),
                true,
                None,
            )
            .await
        }
        Err(stop) => refuse_sharing(&state, &headers, current.as_ref(), stop).await,
    }
}

/// What the visibility form sends.
#[derive(Debug, Clone, Default, Deserialize)]
struct VisibilityForm {
    #[serde(default)]
    visibility: String,
    /// The confirmation. Public needs it, and the server needs it, not only
    /// the page that asked for it.
    #[serde(default)]
    confirm: String,
}

async fn sharing_visibility(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug)): Path<(String, String)>,
    Form(form): Form<VisibilityForm>,
) -> Response {
    let sharing = match sharing_context(&state, &jar, &owner, &slug).await {
        Ok(sharing) => sharing,
        Err(stop) => return refuse_sharing(&state, &headers, current.as_ref(), stop).await,
    };

    let private = match form.visibility.as_str() {
        "private" => true,
        "public" => {
            // A Cookbook becomes public only after the person confirms. The
            // check lives here, so a post that skips the page changes
            // nothing.
            if form.confirm != "yes" {
                return draw_sharing(
                    &state,
                    &headers,
                    current.as_ref(),
                    &sharing,
                    Vec::new(),
                    true,
                    None,
                )
                .await;
            }
            false
        }
        other => {
            tracing::info!(%other, "a visibility that the application does not know");
            return draw_sharing(
                &state,
                &headers,
                current.as_ref(),
                &sharing,
                vec!["Select Public or Private.".to_string()],
                false,
                None,
            )
            .await;
        }
    };

    match state
        .forgejo
        .set_repository_private(&sharing.actor.token, &owner, &slug, private)
        .await
    {
        Ok(_) => {
            tracing::info!(%owner, %slug, private, "the visibility of a Cookbook changed");
            cookbook::refresh(
                &state.pool,
                &state.forgejo,
                Some(&sharing.actor.token),
                &owner,
                &slug,
            )
            .await;
            Redirect::to(&format!("/cookbooks/{owner}/{slug}/sharing")).into_response()
        }
        Err(error) => {
            tracing::warn!(%error, %owner, %slug, "cannot change the visibility of a Cookbook");
            draw_sharing(
                &state,
                &headers,
                current.as_ref(),
                &sharing,
                vec![format!(
                    "Forgejo did not change who can read this Cookbook: {}. Open the Cookbook in Forgejo to see its state.",
                    forgejo_reason(&error)
                )],
                false,
                None,
            )
            .await
        }
    }
}

/// What the add form of the Sharing area sends.
#[derive(Debug, Clone, Default, Deserialize)]
struct SharingPersonForm {
    #[serde(default)]
    login: String,
    #[serde(default)]
    role: String,
    /// `yes` after the person read the access mismatch and decided.
    #[serde(default)]
    confirm: String,
    /// `yes` when the person asked for the Recipe grants as well.
    #[serde(default)]
    grant: String,
}

async fn sharing_add_person(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug)): Path<(String, String)>,
    Form(form): Form<SharingPersonForm>,
) -> Response {
    let sharing = match sharing_context(&state, &jar, &owner, &slug).await {
        Ok(sharing) => sharing,
        Err(stop) => return refuse_sharing(&state, &headers, current.as_ref(), stop).await,
    };

    let draw = |errors: Vec<String>, gap: Option<Gap>| {
        draw_sharing(
            &state,
            &headers,
            current.as_ref(),
            &sharing,
            errors,
            false,
            gap,
        )
    };

    let typed = form.login.trim().to_string();

    let permission = match form.role.as_str() {
        "reader" => FORGEJO_READ,
        "editor" => FORGEJO_WRITE,
        _ => "",
    };

    let refused: Option<String> = if typed.is_empty() {
        Some("Type the name of the person in Forgejo.".to_string())
    } else if permission.is_empty() {
        Some("Select Reader or Editor.".to_string())
    } else if crate::web_sharing::is_service_identity(&typed) {
        Some(format!(
            "The name `{typed}` belongs to {}. Open the Cookbook in Forgejo to change what it can do.",
            crate::auth::OAUTH_APPLICATION_NAME
        ))
    } else if typed.eq_ignore_ascii_case(&sharing.repository.owner.login) {
        Some(format!("{typed} owns this Cookbook already."))
    } else {
        None
    };

    if let Some(message) = refused {
        return draw(vec![message], None).await;
    }

    // Ask Forgejo about the person first. Forgejo hides a profile that its
    // visibility setting keeps from this person, and it answers 404 for one.
    let found = match state.forgejo.user(&sharing.actor.token, &typed).await {
        Ok(user) => user,
        Err(crate::forgejo::ForgejoError::Status { status: 404, .. }) => {
            return draw(
                vec![format!("Forgejo shows no user with the name `{typed}`.")],
                None,
            )
            .await;
        }
        Err(error) => {
            tracing::warn!(%error, "cannot read the user that a person named");
            return draw(
                vec![format!(
                    "Forgejo did not answer about `{typed}`: {}. Open the Cookbook in Forgejo to add the person there.",
                    forgejo_reason(&error)
                )],
                None,
            )
            .await;
        }
    };

    // Access to a Cookbook is not access to its Recipes. Forgejo answers,
    // for this person and for each private Recipe, whether they can read it.
    let recipes = sharing_recipes(&state, &sharing).await;
    let gap = cookbook::recipes_out_of_reach(
        &state.forgejo,
        &sharing.actor.token,
        &recipes,
        &found.login,
    )
    .await;

    let make_gap = || Gap {
        login: found.login.clone(),
        role: form.role.clone(),
        role_name: cookbook_role(permission),
        shut: gap.shut.clone(),
        silent: gap.silent.clone(),
    };

    // The person reads the mismatch and then decides. Nothing changed yet.
    if !gap.is_empty() && form.confirm != "yes" {
        return draw(Vec::new(), Some(make_gap())).await;
    }

    if form.grant == "yes" {
        let refusals = cookbook::grant_reader(
            &state.forgejo,
            &sharing.actor.token,
            &found.login,
            &gap.each(),
        )
        .await;

        // A grant that Forgejo refused is named, and the Cookbook is left
        // alone. The person can select Share anyway from the same page.
        if !refusals.is_empty() {
            return draw(refusals, Some(make_gap())).await;
        }
    }

    match state
        .forgejo
        .add_collaborator(
            &sharing.actor.token,
            &owner,
            &slug,
            &found.login,
            permission,
        )
        .await
    {
        Ok(()) => {
            tracing::info!(%owner, %slug, login = %found.login, permission, "a person can reach a Cookbook");
            Redirect::to(&format!("/cookbooks/{owner}/{slug}/sharing")).into_response()
        }
        Err(error) => {
            tracing::warn!(%error, %owner, %slug, "cannot share the Cookbook with a person");
            draw(
                vec![format!(
                    "Forgejo did not give `{}` access: {}. Open the Cookbook in Forgejo to add the person there.",
                    found.login,
                    forgejo_reason(&error)
                )],
                None,
            )
            .await
        }
    }
}

/// What the remove form of the Sharing area sends.
#[derive(Debug, Clone, Default, Deserialize)]
struct SharingRemoveForm {
    #[serde(default)]
    login: String,
}

async fn sharing_remove_person(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug)): Path<(String, String)>,
    Form(form): Form<SharingRemoveForm>,
) -> Response {
    let sharing = match sharing_context(&state, &jar, &owner, &slug).await {
        Ok(sharing) => sharing,
        Err(stop) => return refuse_sharing(&state, &headers, current.as_ref(), stop).await,
    };

    let login = form.login.trim().to_string();
    if login.is_empty() {
        return draw_sharing(
            &state,
            &headers,
            current.as_ref(),
            &sharing,
            vec!["Select the person to remove.".to_string()],
            false,
            None,
        )
        .await;
    }

    match state
        .forgejo
        .remove_collaborator(&sharing.actor.token, &owner, &slug, &login)
        .await
    {
        Ok(()) => {
            tracing::info!(%owner, %slug, %login, "a person cannot reach a Cookbook any more");
            Redirect::to(&format!("/cookbooks/{owner}/{slug}/sharing")).into_response()
        }
        Err(error) => {
            tracing::warn!(%error, %owner, %slug, "cannot remove a person from a Cookbook");
            draw_sharing(
                &state,
                &headers,
                current.as_ref(),
                &sharing,
                vec![format!(
                    "Forgejo did not remove `{login}`: {}. Open the Cookbook in Forgejo to remove the person there.",
                    forgejo_reason(&error)
                )],
                false,
                None,
            )
            .await
        }
    }
}

/// A short sentence about a Forgejo failure, for a person to read.
///
/// The whole body of a Forgejo answer belongs in the log and not on a page.
fn forgejo_reason(error: &crate::forgejo::ForgejoError) -> String {
    match error {
        crate::forgejo::ForgejoError::Status { status, .. } => format!("it answered {status}"),
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

    fn query(q: &str, sort: &str, area: &str) -> BrowseQuery {
        BrowseQuery {
            q: Some(q.to_string()),
            sort: Some(sort.to_string()),
            area: Some(area.to_string()),
        }
    }

    #[test]
    fn the_cookbooks_area_offers_three_lists() {
        let tabs = tabs(&BrowseQuery::default(), Area::Mine);

        assert_eq!(tabs.len(), 3);
        assert_eq!(tabs[0].name, "Mine");
        assert_eq!(tabs[1].name, "Shared with me");
        assert_eq!(tabs[2].name, "Favorites");
        assert!(tabs[0].active);
    }

    #[test]
    fn a_tab_keeps_the_search_and_the_order() {
        let tabs = tabs(
            &query("sunday dinners", "title", "favorites"),
            Area::Favorites,
        );

        assert!(tabs[2].active);
        assert_eq!(
            tabs[0].href,
            "/cookbooks?area=mine&q=sunday%20dinners&sort=title"
        );
        assert_eq!(
            tabs[2].href,
            "/cookbooks?area=favorites&q=sunday%20dinners&sort=title"
        );
    }

    #[test]
    fn a_refusal_says_what_is_wrong_and_a_failure_offers_forgejo() {
        // A refusal that this application made is something a person can
        // act on, so they read it as it is.
        assert_eq!(
            reason(&cookbook::HoldError::AlreadyHeld),
            cookbook::HoldError::AlreadyHeld.to_string()
        );

        // What a failed command printed is not. It names the parts of the
        // machine that this interface hides, so it never reaches the page.
        let failure = reason(&cookbook::HoldError::Git(crate::git::GitError::Command {
            command: "push".to_string(),
            message: "! [rejected] main -> main".to_string(),
        }));

        assert_eq!(failure, NOT_NOW);
        for word in ["push", "rejected", "main", "git"] {
            assert!(
                !failure.to_lowercase().contains(word),
                "`{word}` must not reach the person: {failure}"
            );
        }
    }

    #[test]
    fn an_empty_query_gives_the_first_list() {
        assert_eq!(Area::parse(""), Area::Mine);
        assert_eq!(Area::parse("sideways"), Area::Mine);
        assert_eq!(Area::parse("shared"), Area::Shared);
        assert_eq!(Area::parse("favorites"), Area::Favorites);
    }

    #[test]
    fn a_search_term_cannot_break_out_of_the_address() {
        let address = address("/cookbooks", None, &query("a&b=c #d", "", ""));
        assert_eq!(address, "/cookbooks?q=a%26b%3Dc%20%23d");
    }

    #[test]
    fn an_empty_list_says_why_it_is_empty() {
        assert!(empty_message(Area::Mine, "").contains("New Cookbook"));
        assert!(empty_message(Area::Shared, "").contains("shares"));
        assert!(empty_message(Area::Favorites, "").contains("Favorite"));
        assert!(empty_message(Area::Explore, "").contains("public"));
        assert!(empty_message(Area::Mine, "sunday").contains("No Cookbook title"));
    }

    #[test]
    fn reader_is_forgejo_read_and_editor_is_forgejo_write() {
        // The Cookbook screen hands out the same two roles as the Recipe
        // screen, and it maps them onto the same Forgejo access modes.
        assert_eq!(cookbook_role(FORGEJO_READ), "Reader");
        assert_eq!(cookbook_role(FORGEJO_WRITE), "Editor");
        assert!(cookbook_manageable(FORGEJO_READ));
        assert!(cookbook_manageable(FORGEJO_WRITE));

        // Administrator and Manager stay in Forgejo. A person who holds one
        // is still listed, because the Owner must see who has access.
        assert_eq!(cookbook_role("admin"), UNMANAGED_ROLE);
        assert_eq!(cookbook_role("owner"), UNMANAGED_ROLE);
        assert!(!cookbook_manageable("admin"));
        assert!(!cookbook_manageable("owner"));
    }

    #[test]
    fn the_confirmation_says_that_a_public_cookbook_names_its_recipes() {
        // A Cookbook names each Recipe that it holds, so a public Cookbook
        // publishes that list. A person must know it before, and not after.
        assert!(PUBLIC_COOKBOOK_WARNING.contains("All users can read"));
        assert!(PUBLIC_COOKBOOK_WARNING.contains("earlier Versions"));
        assert!(PUBLIC_COOKBOOK_WARNING.contains("names each Recipe"));
        assert!(SEPARATE_ACCESS.contains("not access to its Recipes"));
    }

    #[test]
    fn the_sharing_messages_use_cooking_words() {
        for message in [PUBLIC_COOKBOOK_WARNING, SEPARATE_ACCESS] {
            for word in [
                "submodule",
                "repository",
                "branch",
                "commit",
                "collaborator",
                "permission",
                "fork",
                "pull request",
            ] {
                assert!(
                    !message.to_lowercase().contains(word),
                    "`{word}` must not reach the person: {message}"
                );
            }
        }
    }

    #[test]
    fn a_warning_about_all_users_offers_no_grant() {
        // No grant covers every user, so a public Cookbook that names a
        // private Recipe gets the words and not the button.
        let gap = AddGap {
            recipe: cookbook::Named {
                owner: "sam".to_string(),
                slug: "secret".to_string(),
                title: "Secret Sauce".to_string(),
            },
            value: "sam/secret".to_string(),
            holding: "pinned".to_string(),
            public: true,
            shut: Vec::new(),
            silent: Vec::new(),
        };

        assert!(!gap.has_people());

        let named = AddGap {
            shut: vec![cookbook::Sharer {
                login: "robin".to_string(),
                name: "Robin".to_string(),
            }],
            ..gap
        };

        assert!(named.has_people());
    }

    #[test]
    fn public_is_the_default_visibility() {
        // A form with no visibility at all, and one that names the default,
        // must both give a public Cookbook.
        assert!(!CreateForm::default().private());
        assert!(
            !CreateForm {
                visibility: "public".to_string(),
                ..CreateForm::default()
            }
            .private()
        );
        assert!(
            CreateForm {
                visibility: "private".to_string(),
                ..CreateForm::default()
            }
            .private()
        );
    }
}
