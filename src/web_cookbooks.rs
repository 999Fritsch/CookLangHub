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
        .route(
            "/cookbooks/{owner}/{slug}/recipes",
            get(add_form).post(add_recipe),
        )
        .route(
            "/cookbooks/{owner}/{slug}/recipes/remove",
            post(remove_recipe),
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
    /// States that this interface cannot show properly. Each one is named
    /// and none of them is repaired.
    problems: Vec<String>,
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
        problems: readme.problems,
        errors,
    })
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
async fn draw_add(
    state: &AppState,
    headers: &HeaderMap,
    current: Option<&CurrentUser>,
    book: &Book,
    token: &Secret<String>,
    words: &str,
    errors: Vec<String>,
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

    let again = |errors: Vec<String>| {
        draw_add(
            &state,
            &headers,
            current.as_ref(),
            &book,
            &actor.token,
            "",
            errors,
        )
    };

    let Some((recipe_owner, recipe_slug)) = form.recipe.trim().split_once('/') else {
        return again(vec![cookbook::HoldError::NoRecipe.to_string()]).await;
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
            return again(vec!["That Recipe is not available.".to_string()]).await;
        }
    };

    let title = recipe_title(&state, &actor.token, &recipe).await;

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
            Redirect::to(&format!("/cookbooks/{owner}/{slug}")).into_response()
        }
        Err(error) => {
            tracing::info!(%error, "a Recipe was not added to a Cookbook");
            again(vec![reason(&error)]).await
        }
    }
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
        Ok(_) => {
            tracing::info!(
                cookbook = %book.repository.full_name,
                "a Cookbook holds one Recipe less"
            );
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
