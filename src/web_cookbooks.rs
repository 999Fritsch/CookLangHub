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
use axum::routing::get;
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
/// A Cookbook holds its Recipes as Git submodules, and the ticket that adds
/// and removes them fills this list from `.gitmodules` and the gitlinks.
/// Until then a Cookbook holds none and the page says so.
#[derive(Debug, Clone)]
pub struct CookbookRecipe {
    pub owner: String,
    pub slug: String,
    pub title: String,
}

#[derive(Template)]
#[template(path = "cookbook_show.html")]
struct ShowTemplate {
    layout: Layout,
    owner: String,
    title: String,
    private: bool,
    /// The description as HTML. The value is already made safe by
    /// [`cookbook::render`].
    description: String,
    /// Whether the Cookbook says anything about itself at all.
    has_description: bool,
    /// The Recipes of this Cookbook, in alphabetical order by title.
    recipes: Vec<CookbookRecipe>,
    forgejo_url: String,
    /// States that this interface cannot show properly. Each one is named
    /// and none of them is repaired.
    problems: Vec<String>,
}

async fn show(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug)): Path<(String, String)>,
) -> Response {
    let here = format!("/cookbooks/{owner}/{slug}");

    // A public Cookbook is readable without a session. Forgejo applies the
    // permissions, so a private one needs the credential of somebody who
    // may see it.
    let token = crate::web::viewer_token(&state, &jar).await;

    let repository = match state
        .forgejo
        .repository_as(token.as_ref(), &owner, &slug)
        .await
    {
        Ok(repository) => repository,
        Err(error) => {
            tracing::info!(%error, %owner, %slug, "cannot read the Cookbook repository");
            return (StatusCode::NOT_FOUND, "This Cookbook is not available.").into_response();
        }
    };

    // A repository without the marker is not a Cookbook, whatever it holds.
    // This is what keeps a Recipe out of the Cookbook pages.
    if !cookbook::is_cookbook(&repository) {
        return (StatusCode::NOT_FOUND, "This Cookbook is not available.").into_response();
    }

    let bytes = state
        .forgejo
        .raw_file(
            token.as_ref(),
            &owner,
            &slug,
            repository.branch(),
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

    let description = cookbook::render(&readme.description);

    respond(ShowTemplate {
        layout: Layout::new(current.as_ref()).on(&headers, &here),
        owner,
        title: readme.title.unwrap_or_else(|| repository.name.clone()),
        private: repository.private,
        has_description: !description.trim().is_empty(),
        description,
        recipes: Vec::new(),
        forgejo_url: state.forgejo.web_url(&repository.full_name),
        problems: readme.problems,
    })
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
