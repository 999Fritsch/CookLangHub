//! Finding Recipes: Mine, Shared with me, Favorites, and Explore.
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

use crate::diagnostics;
use crate::favorite;
use crate::forgejo::Ownership;
use crate::index;
use crate::outage;
use crate::secret::Secret;
use crate::session::CurrentUser;

use crate::web::{AppState, Layout, MaybeUser, RecipeCard};

/// How many Recipes one page shows.
const PAGE_SIZE: usize = 60;

/// Shown when Forgejo cannot answer. The list is empty because nothing is
/// known, and not because the person has no Recipes. One message covers
/// every list, so that no page says a softer thing than another.
const NO_FORGEJO: &str = outage::LIST_MESSAGE;

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
    /// The Recipes that this person made a Favorite.
    Favorites,
    /// Every public Recipe.
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

/// The order that a person picked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sort {
    /// The Recipe that changed last comes first.
    #[default]
    Recent,
    /// The title decides, from A to Z.
    Title,
    /// The Recipe that the most people made a Favorite comes first.
    ///
    /// Forgejo counts the Favorites and Forgejo puts them in order, so the
    /// application holds no count of its own.
    Favorites,
}

impl Sort {
    pub fn as_str(&self) -> &'static str {
        match self {
            Sort::Recent => "recent",
            Sort::Title => "title",
            Sort::Favorites => "favorites",
        }
    }

    /// For a template, which cannot compare two values of this type.
    pub fn is(&self, name: &str) -> bool {
        self.as_str() == name
    }

    fn parse(value: &str) -> Self {
        match value {
            "title" => Sort::Title,
            "favorites" => Sort::Favorites,
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
    // A person who is not signed in owns nothing, shares nothing, and has
    // no Favorites.
    if viewer.is_none() && area != Area::Explore {
        return Listing {
            cards: Vec::new(),
            notice: None,
            empty: "Sign in to see your Recipes.".to_string(),
        };
    }

    let ownership = match (area, viewer) {
        (Area::Explore, _) => Ownership::Anybody,
        (Area::Mine, Some(user)) => Ownership::OwnedBy(user.forgejo_user_id),
        (Area::Shared, Some(user)) => Ownership::ReachableBy(user.forgejo_user_id),
        _ => Ownership::Anybody,
    };

    // Forgejo answers all three questions. Which Recipes this person made a
    // Favorite, which Recipes the most people made a Favorite, and which
    // Recipes this credential may see at all.
    let found = match area {
        Area::Favorites => match token {
            Some(token) => favorite::recipes(&state.forgejo, token).await,
            None => Ok((Vec::new(), false)),
        },
        _ if query.sort() == Sort::Favorites => {
            favorite::most_favorited(&state.forgejo, token, ownership).await
        }
        _ => index::visible(&state.forgejo, token, ownership).await,
    };

    let (mut repositories, truncated) = match found {
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
        // Forgejo already answered with exactly the Recipes that this
        // person made a Favorite, and with exactly what they own.
        Area::Mine | Area::Favorites => {}
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
        Area::Favorites => "You have no Favorite Recipes yet.",
        Area::Explore => "This installation has no public Recipe yet.",
    }
    .to_string()
}

/// The three lists of the Recipes area, as links that keep the search.
pub fn tabs(query: &BrowseQuery, area: Area) -> Vec<Tab> {
    [
        (Area::Mine, "Mine"),
        (Area::Shared, "Shared with me"),
        (Area::Favorites, "Favorites"),
    ]
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
    crate::web::viewer_token(state, jar).await
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

/// The Diagnostics page.
///
/// Six subsystems can fail on their own, so the page gives each of them its
/// own state instead of one combined answer. Only an administrator sees the
/// detail: the address is public, and a person who does not administer the
/// installation gets cooking words and nothing internal.
#[derive(Template)]
#[template(path = "admin_index.html")]
struct AdminIndexTemplate {
    layout: Layout,
    forgejo_url: String,
    /// Whether Forgejo says this person administers the installation.
    administrator: bool,
    signed_in: bool,
    /// One card for each subsystem. Empty for anybody but an administrator.
    parts: Vec<diagnostics::Subsystem>,
    /// Set after a reconciliation.
    report: Option<String>,
}

/// Build the page for whoever is looking.
async fn diagnostics_page(
    state: &AppState,
    headers: &HeaderMap,
    admin_token: Option<&Secret<String>>,
    user: Option<&CurrentUser>,
    report: Option<String>,
) -> Response {
    let parts = match admin_token {
        Some(token) => {
            diagnostics::report(&state.pool, &state.cipher, &state.forgejo, token)
                .await
                .subsystems
        }
        // Forgejo says who administers this installation, so while it is
        // away nobody can be shown the detail. The Forgejo card still
        // appears, because the page must say why it is empty, and the
        // address of Forgejo is on every page already.
        None => diagnostics::forgejo_outage(&state.forgejo)
            .await
            .into_iter()
            .collect(),
    };

    respond(AdminIndexTemplate {
        layout: Layout::new(user).on(headers, "/admin/index"),
        forgejo_url: state.forgejo.public_url().to_string(),
        administrator: admin_token.is_some(),
        signed_in: user.is_some(),
        parts,
        report,
    })
}

async fn admin_index(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(user): MaybeUser,
) -> Response {
    let admin_token = administrator_token(&state, &jar).await;

    diagnostics_page(&state, &headers, admin_token.as_ref(), user.as_ref(), None).await
}

/// Start a reconciliation of everything that a sweep can repair.
///
/// This is what the application does when it starts, and it is what brings
/// the installation back after a Forgejo outage. Both indexes are read from
/// Forgejo and Git again, and every Cookbook that follows a Recipe moves to
/// the Version that the Recipe has now. Nothing else is written.
async fn rebuild(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(user): MaybeUser,
) -> Response {
    let Some(admin_token) = administrator_token(&state, &jar).await else {
        return (
            StatusCode::FORBIDDEN,
            "Only an administrator can start a reconciliation.",
        )
            .into_response();
    };

    let recipes = index::reconcile(&state.pool, &state.cipher, &state.forgejo).await;
    let cookbooks = crate::cookbook::reconcile(&state.pool, &state.cipher, &state.forgejo).await;
    let moved = crate::automation::advance(
        &state.pool,
        &state.cipher,
        &state.forgejo,
        state.git.as_ref(),
        &state.forgejo_noreply_domain,
        None,
    )
    .await;

    let held = index::count(&state.pool).await.unwrap_or_default();
    let held_cookbooks = crate::cookbook::count(&state.pool)
        .await
        .unwrap_or_default();

    let message = format!(
        "The indexes are complete again. Forgejo named {} Recipes and {} Cookbooks. \
         The index holds {held} Recipes and {held_cookbooks} Cookbooks. \
         {} Cookbooks moved to a new Version of a Recipe they follow.",
        recipes.scanned, cookbooks.scanned, moved.advanced
    );

    diagnostics_page(
        &state,
        &headers,
        Some(&admin_token),
        user.as_ref(),
        Some(message),
    )
    .await
}

/// The credential of the person behind this session, when Forgejo says they
/// administer the installation.
///
/// Forgejo decides, the same as it decides every other permission. The
/// credential comes back as well, because the Diagnostics page asks Forgejo
/// questions that only an administrator may ask, and it asks them with the
/// credential of the administrator who is already reading the page.
async fn administrator_token(state: &AppState, jar: &CookieJar) -> Option<Secret<String>> {
    let token = viewer_token(state, jar).await?;

    match state.forgejo.is_administrator(&token).await {
        Ok(true) => Some(token),
        Ok(false) => None,
        Err(error) => {
            tracing::warn!(%error, "cannot ask Forgejo who administers it");
            None
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
        assert_eq!(query("", "favorites", "").sort(), Sort::Favorites);
    }

    #[test]
    fn an_unknown_list_falls_back_to_mine() {
        assert_eq!(query("", "", "sideways").area(), Area::Mine);
        assert_eq!(query("", "", "shared").area(), Area::Shared);
        assert_eq!(query("", "", "favorites").area(), Area::Favorites);
    }

    #[test]
    fn the_search_words_lose_the_spaces_around_them() {
        assert_eq!(query("  chili  ", "", "").words(), "chili");
    }

    #[test]
    fn a_tab_keeps_the_search_and_the_order() {
        let tabs = tabs(&query("chili sin carne", "title", "shared"), Area::Shared);

        assert_eq!(tabs.len(), 3);
        assert!(tabs[1].active, "the shared list is the one being shown");
        assert!(!tabs[0].active);
        assert!(!tabs[2].active);

        assert_eq!(tabs[0].href, "/?area=mine&q=chili%20sin%20carne&sort=title");
        assert_eq!(
            tabs[1].href,
            "/?area=shared&q=chili%20sin%20carne&sort=title"
        );
        assert_eq!(tabs[2].name, "Favorites");
        assert_eq!(
            tabs[2].href,
            "/?area=favorites&q=chili%20sin%20carne&sort=title"
        );
    }

    #[test]
    fn the_favorites_list_keeps_the_most_favorited_order() {
        let tabs = tabs(&query("", "favorites", "favorites"), Area::Favorites);

        assert!(tabs[2].active, "the Favorites list is the one being shown");
        assert_eq!(tabs[2].href, "/?area=favorites&sort=favorites");
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
        assert!(empty_message(Area::Favorites, "").contains("Favorite"));
        assert!(empty_message(Area::Explore, "").contains("public"));
        // A search that found nothing is a different thing to say.
        assert!(empty_message(Area::Mine, "chili").contains("No Recipe title"));
    }
}
