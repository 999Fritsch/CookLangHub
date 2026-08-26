//! Finding Recipes: Mine, Shared with me, and Explore.
//!
//! Every list is built the same way. Forgejo says which Recipes this person
//! may see, and the index says what each card shows. The order is never the
//! other way round: a row in the index is not permission to see anything,
//! and a Recipe that Forgejo does not name never reaches a page.
//!
//! Search and order are query parameters on a plain form. The page works
//! with no script, which keeps it inside the `default-src 'self'` policy and
//! keeps it working for anybody who blocks scripts.

use std::sync::Arc;

use askama::Template;
use axum::Router;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum_extra::extract::CookieJar;
use serde::Deserialize;

use crate::forgejo::Ownership;
use crate::index;
use crate::secret::Secret;
use crate::session::{self, COOKIE_NAME, CurrentUser};
use crate::web::{AppState, Layout, MaybeUser, RecipeCard};

/// How many Recipes one page shows.
const PAGE_SIZE: usize = 60;

/// Shown when Forgejo cannot answer. The list is empty because nothing is
/// known, and not because the person has no Recipes.
const NO_FORGEJO: &str =
    "CookLangHub cannot reach Forgejo now, so this list is not complete. Try again in a moment.";

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/explore", get(explore))
        .route("/admin/index", get(admin_index))
        .route("/admin/index/rebuild", post(rebuild))
}

/// Which list a person is looking at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Area {
    /// The Recipes of this person.
    Mine,
    /// The Recipes of somebody else that this person may work on.
    Shared,
    /// Every public Recipe.
    Explore,
}

impl Area {
    /// The value that the address carries.
    pub fn as_str(&self) -> &'static str {
        match self {
            Area::Mine => "mine",
            Area::Shared => "shared",
            Area::Explore => "explore",
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "shared" => Area::Shared,
            _ => Area::Mine,
        }
    }
}

/// The order that a person picked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sort {
    /// The Recipe that changed last comes first.
    #[default]
    Recent,
    /// The title decides, from A to Z.
    Title,
}

impl Sort {
    pub fn as_str(&self) -> &'static str {
        match self {
            Sort::Recent => "recent",
            Sort::Title => "title",
        }
    }

    /// For a template, which cannot compare two values of this type.
    pub fn is(&self, name: &str) -> bool {
        self.as_str() == name
    }

    fn parse(value: &str) -> Self {
        match value {
            "title" => Sort::Title,
            _ => Sort::Recent,
        }
    }
}

/// What the search form sends.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BrowseQuery {
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub sort: Option<String>,
    #[serde(default)]
    pub area: Option<String>,
}

impl BrowseQuery {
    /// What the person typed, without the spaces around it.
    pub fn words(&self) -> String {
        self.q.as_deref().unwrap_or_default().trim().to_string()
    }

    pub fn sort(&self) -> Sort {
        Sort::parse(self.sort.as_deref().unwrap_or_default())
    }

    pub fn area(&self) -> Area {
        Area::parse(self.area.as_deref().unwrap_or_default())
    }
}

/// The search box and the order control, for one page.
#[derive(Debug, Clone)]
pub struct Controls {
    /// Where the form sends the person.
    pub action: String,
    /// The list that the form must stay on, or nothing for one list only.
    pub area: String,
    pub q: String,
    pub sort: Sort,
}

/// One list that a person can move to.
#[derive(Debug, Clone)]
pub struct Tab {
    pub name: &'static str,
    pub href: String,
    pub active: bool,
}

/// What a list came to.
pub struct Listing {
    pub cards: Vec<RecipeCard>,
    /// A message about the state of the list, when there is one to give.
    pub notice: Option<String>,
    /// What to say when the list is empty.
    pub empty: String,
}

/// Build one list of Recipes.
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
    let ownership = match (area, viewer) {
        (Area::Explore, _) => Ownership::Anybody,
        (Area::Mine, Some(user)) => Ownership::OwnedBy(user.forgejo_user_id),
        (Area::Shared, Some(user)) => Ownership::ReachableBy(user.forgejo_user_id),
        // A person who is not signed in owns nothing and shares nothing.
        (_, None) => {
            return Listing {
                cards: Vec::new(),
                notice: None,
                empty: "Sign in to see your Recipes.".to_string(),
            };
        }
    };

    let (mut repositories, truncated) = match index::visible(&state.forgejo, token, ownership).await
    {
        Ok(found) => found,
        Err(error) => {
            tracing::warn!(%error, area = area.as_str(), "cannot ask Forgejo for the Recipes");
            return Listing {
                cards: Vec::new(),
                notice: Some(NO_FORGEJO.to_string()),
                empty: String::new(),
            };
        }
    };

    match area {
        // Explore is the public catalog. A signed-in person sees the same
        // list as a visitor, so a private Recipe never appears there.
        Area::Explore => repositories.retain(|repository| !repository.private),
        // Forgejo answers with what this person owns and what they may work
        // on, and Shared with me is the second part of that.
        Area::Shared => {
            let login = viewer.map(|user| user.login.clone()).unwrap_or_default();
            repositories.retain(|repository| !repository.owner.login.eq_ignore_ascii_case(&login));
        }
        Area::Mine => {}
    }

    // Forgejo answers with the newest change first, and the index gives the
    // title that a person searches for.
    let entries = index::entries(&state.pool, &state.forgejo, token, &repositories).await;

    let words = query.words().to_lowercase();
    let mut found: Vec<index::Indexed> = entries
        .into_iter()
        .filter(|entry| words.is_empty() || entry.title.to_lowercase().contains(&words))
        .collect();

    if query.sort() == Sort::Title {
        found.sort_by_key(|entry| entry.title.to_lowercase());
    }

    let total = found.len();
    found.truncate(PAGE_SIZE);

    let mut notice = None;
    if truncated {
        notice = Some(format!(
            "This installation has more Recipes than one list shows. The first {} come first.",
            index::MAX_REPOSITORIES
        ));
    } else if total > PAGE_SIZE {
        notice = Some(format!(
            "{total} Recipes match. The first {PAGE_SIZE} are here. Search to make the list shorter."
        ));
    }

    Listing {
        cards: found.into_iter().map(card).collect(),
        notice,
        empty: empty_message(area, &words),
    }
}

fn card(entry: index::Indexed) -> RecipeCard {
    RecipeCard {
        owner: entry.owner,
        slug: entry.slug,
        title: entry.title,
        private: entry.private,
        servings: entry.servings,
        tags: entry.tags,
        ingredients: entry.ingredients,
        thumbnail: entry.thumbnail,
    }
}

fn empty_message(area: Area, words: &str) -> String {
    if !words.is_empty() {
        return "No Recipe title contains these words.".to_string();
    }

    match area {
        Area::Mine => "You have no Recipes yet. Select New Recipe to write your first one.",
        Area::Shared => "Nobody shares a Recipe with you yet.",
        Area::Explore => "This installation has no public Recipe yet.",
    }
    .to_string()
}

/// The two lists of the Recipes area, as links that keep the search.
pub fn tabs(query: &BrowseQuery, area: Area) -> Vec<Tab> {
    [(Area::Mine, "Mine"), (Area::Shared, "Shared with me")]
        .into_iter()
        .map(|(tab, name)| Tab {
            name,
            href: address("/", Some(tab), query),
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

/// The credential of the person who is looking, when they have one.
pub async fn viewer_token(state: &AppState, jar: &CookieJar) -> Option<Secret<String>> {
    let cookie = jar.get(COOKIE_NAME)?;
    session::access_token(&state.pool, &state.cipher, cookie.value())
        .await
        .ok()
        .flatten()
}

// ------------------------------------------------------------------ pages

#[derive(Template)]
#[template(path = "explore.html")]
struct ExploreTemplate {
    layout: Layout,
    forgejo_url: String,
    recipes: Vec<RecipeCard>,
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
    let token = viewer_token(&state, &jar).await;

    let listing = listing(&state, user.as_ref(), token.as_ref(), Area::Explore, &query).await;

    respond(ExploreTemplate {
        layout: Layout::new(user.as_ref()).on(&headers, &address("/explore", None, &query)),
        forgejo_url: state.forgejo.public_url().to_string(),
        recipes: listing.cards,
        controls: Controls {
            action: "/explore".to_string(),
            area: String::new(),
            q: query.words(),
            sort: query.sort(),
        },
        notice: listing.notice,
        empty: listing.empty,
    })
}

#[derive(Template)]
#[template(path = "admin_index.html")]
struct AdminIndexTemplate {
    layout: Layout,
    forgejo_url: String,
    /// Whether Forgejo says this person administers the installation.
    administrator: bool,
    signed_in: bool,
    /// How many Recipes the index holds now.
    held: i64,
    /// Set after a rebuild.
    report: Option<String>,
}

async fn admin_index(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(user): MaybeUser,
) -> Response {
    let administrator = is_administrator(&state, &jar).await;
    let held = index::count(&state.pool).await.unwrap_or_default();

    respond(AdminIndexTemplate {
        layout: Layout::new(user.as_ref()).on(&headers, "/admin/index"),
        forgejo_url: state.forgejo.public_url().to_string(),
        administrator,
        signed_in: user.is_some(),
        held,
        report: None,
    })
}

async fn rebuild(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(user): MaybeUser,
) -> Response {
    if !is_administrator(&state, &jar).await {
        return (
            StatusCode::FORBIDDEN,
            "Only an administrator can rebuild the index.",
        )
            .into_response();
    }

    let report = index::reconcile(&state.pool, &state.cipher, &state.forgejo).await;
    let held = index::count(&state.pool).await.unwrap_or_default();

    let message = format!(
        "The index is complete again. Forgejo named {} Recipes, and the index holds {held}.",
        report.scanned
    );

    respond(AdminIndexTemplate {
        layout: Layout::new(user.as_ref()).on(&headers, "/admin/index"),
        forgejo_url: state.forgejo.public_url().to_string(),
        administrator: true,
        signed_in: true,
        held,
        report: Some(message),
    })
}

/// Whether Forgejo says the person behind this session administers it.
///
/// Forgejo decides, the same as it decides every other permission.
async fn is_administrator(state: &AppState, jar: &CookieJar) -> bool {
    let Some(token) = viewer_token(state, jar).await else {
        return false;
    };

    match state.forgejo.is_administrator(&token).await {
        Ok(answer) => answer,
        Err(error) => {
            tracing::warn!(%error, "cannot ask Forgejo who administers it");
            false
        }
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
    fn an_empty_query_gives_the_first_list_and_the_recent_order() {
        let empty = BrowseQuery::default();
        assert_eq!(empty.area(), Area::Mine);
        assert_eq!(empty.sort(), Sort::Recent);
        assert_eq!(empty.words(), "");
    }

    #[test]
    fn an_unknown_order_falls_back_to_recent() {
        assert_eq!(query("", "sideways", "").sort(), Sort::Recent);
        assert_eq!(query("", "title", "").sort(), Sort::Title);
    }

    #[test]
    fn the_search_words_lose_the_spaces_around_them() {
        assert_eq!(query("  chili  ", "", "").words(), "chili");
    }

    #[test]
    fn a_tab_keeps_the_search_and_the_order() {
        let tabs = tabs(&query("chili sin carne", "title", "shared"), Area::Shared);

        assert_eq!(tabs.len(), 2);
        assert!(tabs[1].active, "the shared list is the one being shown");
        assert!(!tabs[0].active);

        assert_eq!(tabs[0].href, "/?area=mine&q=chili%20sin%20carne&sort=title");
        assert_eq!(
            tabs[1].href,
            "/?area=shared&q=chili%20sin%20carne&sort=title"
        );
    }

    #[test]
    fn the_recent_order_needs_no_parameter() {
        let tabs = tabs(&BrowseQuery::default(), Area::Mine);
        assert_eq!(tabs[0].href, "/?area=mine");
    }

    #[test]
    fn a_search_term_cannot_break_out_of_the_address() {
        let address = address("/explore", None, &query("a&b=c #d", "", ""));
        assert_eq!(address, "/explore?q=a%26b%3Dc%20%23d");
        assert!(!address.contains('&') || address.matches('&').count() == 0);
    }

    #[test]
    fn an_empty_list_says_why_it_is_empty() {
        assert!(empty_message(Area::Mine, "").contains("New Recipe"));
        assert!(empty_message(Area::Shared, "").contains("shares"));
        assert!(empty_message(Area::Explore, "").contains("public"));
        // A search that found nothing is a different thing to say.
        assert!(empty_message(Area::Mine, "chili").contains("No Recipe title"));
    }
}
