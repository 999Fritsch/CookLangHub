//! HTTP surface: server-rendered pages, the health endpoint, and static files.

use std::sync::Arc;

use askama::Template;
use axum::Router;
use axum::extract::{FromRequestParts, State};
use axum::http::request::Parts;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum_extra::extract::CookieJar;
use sqlx::sqlite::SqlitePool;
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;

use crate::crypto::Cipher;
use crate::forgejo::ForgejoClient;
use crate::git::GitAdapter;
use crate::health;
use crate::secret::Secret;
use crate::session::{self, COOKIE_NAME, CurrentUser};

/// Shared state that every handler can read.
#[derive(Debug, Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub forgejo: ForgejoClient,
    /// Git owns Recipe content, and every content operation goes through
    /// this boundary. It is a trait object so that a later ticket can
    /// replace the implementation without touching the Recipe model.
    pub git: Arc<dyn GitAdapter>,
    pub cipher: Cipher,
    /// Whether the session cookie carries the `Secure` attribute.
    pub cookie_secure: bool,
    /// The domain Forgejo uses when a person hides their address.
    pub forgejo_noreply_domain: String,
    pub installation_id: String,
}

/// Content Security Policy for every page.
///
/// `default-src 'self'` stops the browser from loading a script, a style, a
/// font, or an image from another host. A page therefore cannot depend on an
/// external CDN, and cannot leak a page view to one.
const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; img-src 'self' data:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'";

pub fn router(state: AppState, static_dir: &str) -> Router {
    // Both guards below need the same state that the handlers get, so the
    // shared value is made here rather than at the end, and used twice.
    let shared = Arc::new(state);

    // Every page a person sees. These carry the no-store rule below; the
    // files under /static do not, because they are the same for everybody
    // and a browser should keep them.
    let pages = Router::new()
        .route("/", get(index))
        .route("/health", get(health_endpoint))
        .route("/avatar", get(avatar))
        .merge(crate::web_archive::router())
        .merge(crate::auth::router())
        .merge(crate::web_recipes::router())
        .merge(crate::theme::router())
        .merge(crate::preferences::router())
        .merge(crate::web_browse::router())
        .merge(crate::web_cookbooks::router())
        .merge(crate::web_discussions::router())
        .merge(crate::draft::router())
        .merge(crate::web_edit::router())
        .merge(crate::favorite::router())
        .merge(crate::web_history::router())
        .merge(crate::web_profile::router())
        .merge(crate::web_sharing::router())
        .merge(crate::web_suggestions::router())
        .merge(crate::web_variations::router())
        .merge(crate::webhook::router())
        // An archived Recipe is read-only, and one guard over every change
        // is what makes that true. It sits here and not in each handler,
        // because Forgejo keeps reporting write access for an archived
        // repository: no permission answer carries the state, so no handler
        // could learn it from the check it already makes.
        //
        // Both guards are inside the header layers below, so a refusal
        // carries the same headers as a page.
        .layer(axum::middleware::from_fn_with_state(
            shared.clone(),
            crate::archive::read_only,
        ))
        // Forgejo is the authority. While it does not answer, no edit
        // happens and no page presents the local cache as current. One
        // layer holds both rules, so that no handler can forget one.
        //
        // This one is outside the archive guard on purpose. The archive
        // guard has to ask Forgejo what it holds, and while Forgejo is away
        // there is no answer to that question, so the outage is settled
        // first and the archive question is never asked.
        .layer(axum::middleware::from_fn_with_state(
            shared.clone(),
            crate::outage::guard,
        ))
        // A page holds somebody's Recipes, and some of them are private.
        // Without this the browser keeps the page, so after a person signs
        // out the Back button still shows what they were reading. That
        // matters most on a computer that people share.
        //
        // `if_not_present` rather than `overriding`, so a route that has
        // already answered with its own rule keeps it. The photo route and
        // the avatar route both do.
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        ));

    Router::new()
        .merge(pages)
        .nest_service("/static", ServeDir::new(static_dir))
        .layer(SetResponseHeaderLayer::overriding(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(CONTENT_SECURITY_POLICY),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        ))
        .layer(TraceLayer::new_for_http())
        .with_state(shared)
}

/// The signed-in user, or nobody.
///
/// A handler asks for this and gets the session that the cookie names. An
/// absent, unknown, or expired cookie gives `None` rather than an error, so
/// a public page keeps working for a visitor who never signed in.
#[derive(Debug, Clone)]
pub struct MaybeUser(pub Option<CurrentUser>);

impl FromRequestParts<Arc<AppState>> for MaybeUser {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let jar = CookieJar::from_headers(&parts.headers);

        let Some(cookie) = jar.get(COOKIE_NAME) else {
            return Ok(Self(None));
        };

        match session::lookup(&state.pool, cookie.value()).await {
            Ok(found) => Ok(Self(found)),
            Err(error) => {
                tracing::warn!(%error, "cannot read the session store");
                Ok(Self(None))
            }
        }
    }
}

/// The Forgejo credential of the person who is looking, when they have one.
///
/// Every page that acts as a person goes through here, and the credential is
/// renewed first when it is spent. This is the one place in the application
/// that renews, because Forgejo gives a new refresh token each time and
/// refuses the old one.
///
/// Two paths deliberately do NOT come through here. The photo route serves
/// many images for one page and must not start a renewal per image, and the
/// reconciliation and the webhook read every stored credential at once and
/// must never make an outside event drive a credential operation.
pub async fn viewer_token(state: &AppState, jar: &CookieJar) -> Option<Secret<String>> {
    let cookie = jar.get(session::COOKIE_NAME)?;

    let client = match crate::auth::load_client(&state.pool, &state.cipher).await {
        Ok(Some(client)) => client,
        // No registered client means the bootstrap has not run. Renewal is
        // impossible, so the stored token is the best that can be offered.
        Ok(None) => {
            return session::access_token(&state.pool, &state.cipher, cookie.value())
                .await
                .ok()
                .flatten();
        }
        Err(error) => {
            tracing::warn!(%error, "cannot read the OAuth client");
            return None;
        }
    };

    match session::live_token(
        &state.pool,
        &state.cipher,
        &state.forgejo,
        &client,
        cookie.value(),
    )
    .await
    {
        Ok(found) => found,
        Err(error) => {
            tracing::warn!(%error, "cannot read the credential of this session");
            None
        }
    }
}

/// The values that `base.html` needs on every page.
///
/// Each template carries one of these as `layout`, so a new page gets the
/// header and the footer without repeating their fields.
#[derive(Debug, Clone)]
pub struct Layout {
    pub version: &'static str,
    pub signed_in: bool,
    pub user_name: String,
    pub user_avatar: String,
    /// The palette this person chose.
    pub theme: crate::theme::Theme,
    /// Whether this person asked for a colour on each kind of Recipe fact.
    pub facts: crate::preferences::FactColour,
    /// Where the person is, so the theme control returns them here.
    pub path: String,
}

impl Layout {
    pub fn new(user: Option<&CurrentUser>) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            signed_in: user.is_some(),
            user_name: user.map(|u| u.display_name.clone()).unwrap_or_default(),
            user_avatar: user.map(|u| u.avatar_url.clone()).unwrap_or_default(),
            theme: crate::theme::Theme::default(),
            facts: crate::preferences::FactColour::default(),
            path: "/".to_string(),
        }
    }

    /// Add what the request itself carries.
    pub fn on(mut self, headers: &axum::http::HeaderMap, path: &str) -> Self {
        self.theme = crate::theme::from_headers(headers);
        self.facts = crate::preferences::from_headers(headers);
        self.path = path.to_string();
        self
    }

    /// Whether the navigation should mark an area as the one in use.
    ///
    /// The mark said Recipes on every page that was not Explore, so it said
    /// Recipes while a person stood on Preferences or on New Recipe. It now
    /// answers for the page the person is actually on, and answers for none
    /// of them on a page that belongs to no area.
    pub fn area_is(&self, area: &str) -> bool {
        let path = self.path.as_str();
        match area {
            "explore" => path == "/explore" || path.starts_with("/explore/"),
            "new" => path == "/recipes/new",
            "recipes" => path == "/" || (path.starts_with("/recipes/") && path != "/recipes/new"),
            "cookbooks" => path == "/cookbooks" || path.starts_with("/cookbooks/"),
            // The Suggestions of a person cover every Recipe, so the area
            // is the one address and never a page inside a Recipe.
            "suggestions" => path == crate::web_suggestions::INBOX_HREF,
            _ => false,
        }
    }

    /// The classes the page carries on its root element.
    ///
    /// Built here rather than in the template so that a choice left at its
    /// default adds nothing at all, and the attribute never carries a
    /// stray space.
    pub fn html_class(&self) -> String {
        [self.theme.css_class(), self.facts.css_class()]
            .into_iter()
            .filter(|class| !class.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// One Recipe as a card on a list.
///
/// A card carries culinary information only. What a cook wants to know is
/// what the Recipe is, what it needs, and who wrote it. How Git holds it is
/// not on the card.
#[derive(Debug, Clone)]
pub struct RecipeCard {
    pub owner: String,
    pub slug: String,
    pub title: String,
    pub private: bool,
    /// Whether the card can show a photo. The image itself comes from this
    /// application, so a private Recipe keeps its photo private.
    pub thumbnail: bool,
    /// How many people the Recipe cooks for, when it says.
    pub servings: Option<String>,
    /// The Cooklang tags of the Recipe.
    pub tags: Vec<String>,
    /// How many ingredients the Recipe needs.
    pub ingredients: i64,
}

#[derive(Template)]
#[template(path = "index.html")]
struct IndexTemplate {
    layout: Layout,
    forgejo_url: String,
    recipes: Vec<RecipeCard>,
    /// Mine, and Shared with me.
    tabs: Vec<crate::web_browse::Tab>,
    controls: crate::web_browse::Controls,
    notice: Option<String>,
    empty: String,
}

/// The Recipes area: Mine, and Shared with me.
///
/// Forgejo says which Recipes this person may see, and the index says what
/// each card shows. A visitor who is not signed in gets the welcome instead,
/// and can still follow **Explore**.
async fn index(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    jar: axum_extra::extract::CookieJar,
    MaybeUser(user): MaybeUser,
    axum::extract::Query(query): axum::extract::Query<crate::web_browse::BrowseQuery>,
) -> Response {
    let area = query.area();
    let token = crate::web_browse::viewer_token(&state, &jar).await;

    let listing =
        crate::web_browse::listing(&state, user.as_ref(), token.as_ref(), area, &query).await;

    let template = IndexTemplate {
        layout: Layout::new(user.as_ref()).on(&headers, "/"),
        forgejo_url: state.forgejo.public_url().to_string(),
        recipes: listing.cards,
        tabs: crate::web_browse::tabs(&query, area),
        controls: crate::web_browse::Controls {
            action: "/".to_string(),
            area: area.as_str().to_string(),
            q: query.words(),
            sort: query.sort(),
        },
        notice: listing.notice,
        empty: listing.empty,
    };

    match template.render() {
        Ok(body) => Html(body).into_response(),
        Err(error) => {
            tracing::error!(%error, "cannot render the index template");
            (StatusCode::INTERNAL_SERVER_ERROR, "template error").into_response()
        }
    }
}

/// Report the health of each component.
///
/// The status code lets an orchestrator act without a read of the body:
/// 200 when every component answers, 503 when one does not.
async fn health_endpoint(State(state): State<Arc<AppState>>) -> Response {
    let report = health::report(&state.pool, &state.forgejo).await;

    let status = if report.is_healthy() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    (status, axum::Json(report)).into_response()
}

/// Serve the avatar of the signed-in person from this application.
///
/// Forgejo hosts the image, and the policy of this application allows an
/// image from its own origin only. Fetching it here keeps the rule intact
/// and keeps the browser from asking another host for anything.
///
/// Only an address on the Forgejo this application is configured for is
/// fetched. The address arrives from Forgejo rather than from the person,
/// but a check here means a changed answer still cannot make this server
/// fetch somewhere else.
async fn avatar(State(state): State<Arc<AppState>>, MaybeUser(user): MaybeUser) -> Response {
    let Some(user) = user else {
        return StatusCode::NOT_FOUND.into_response();
    };

    serve_avatar(&state, &user.avatar_url).await
}

/// Fetch one avatar from Forgejo and pass it on from this origin.
///
/// The profile page serves the picture of another person through here, so
/// that one guard and one fetch cover every avatar this application shows.
/// Deciding who may be seen is not this function: the caller asks Forgejo
/// about the person first and comes here only with an address that Forgejo
/// gave it for that viewer.
pub(crate) async fn serve_avatar(state: &AppState, avatar_url: &str) -> Response {
    let Some(address) = avatar_address(
        avatar_url,
        state.forgejo.public_url(),
        state.forgejo.api_url(),
    ) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let answer = match reqwest::Client::new().get(address).send().await {
        Ok(answer) if answer.status().is_success() => answer,
        _ => return StatusCode::NOT_FOUND.into_response(),
    };

    // Only an image is passed on. Forgejo serves one here, and refusing
    // anything else keeps this route from becoming a general way to fetch.
    let content_type = answer
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .filter(|value| value.starts_with("image/"))
        .unwrap_or("image/png")
        .to_string();

    match answer.bytes().await {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, content_type),
                // The picture belongs to one person, so no shared cache
                // keeps it, and the browser rereads it on the next visit.
                (header::CACHE_CONTROL, "private, max-age=300".to_string()),
            ],
            bytes,
        )
            .into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

/// The address to fetch an avatar from, when it may be fetched at all.
///
/// Forgejo reports an avatar address for a browser, so it carries the
/// public address of Forgejo. This server cannot use that address: inside a
/// container `localhost` is the container itself, not Forgejo. The address
/// is therefore checked against the public address and fetched through the
/// one this application talks to.
///
/// Only an address on the Forgejo of this installation passes. The value
/// comes from Forgejo and not from a person, but this server must never be
/// usable to fetch an address that somebody else chose, so the rule is
/// applied rather than assumed.
fn avatar_address(avatar_url: &str, public_url: &str, api_url: &str) -> Option<String> {
    let address = avatar_url.trim();
    let base = public_url.trim_end_matches('/');

    if base.is_empty() || address.len() <= base.len() || !address.starts_with(base) {
        return None;
    }

    let path = &address[base.len()..];

    // The rest must begin a path. Without this check `http://forgejo.test`
    // would also match `http://forgejo.test.evil`.
    if !path.starts_with('/') {
        return None;
    }

    Some(format!("{}{path}", api_url.trim_end_matches('/')))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_navigation_marks_the_area_a_person_is_in() {
        let at = |path: &str| {
            let mut layout = Layout::new(None);
            layout.path = path.to_string();
            layout
        };

        assert!(at("/").area_is("recipes"));
        assert!(at("/recipes/sam/chili").area_is("recipes"));
        assert!(at("/recipes/sam/chili/sharing").area_is("recipes"));
        assert!(at("/explore").area_is("explore"));

        // Cookbooks is its own area, and the Cookbooks of Explore stay
        // inside Explore.
        assert!(at("/cookbooks").area_is("cookbooks"));
        assert!(at("/cookbooks/sam/sunday").area_is("cookbooks"));
        assert!(at("/cookbooks/new").area_is("cookbooks"));
        assert!(!at("/cookbooks").area_is("recipes"));
        assert!(!at("/").area_is("cookbooks"));
        assert!(at("/explore/cookbooks").area_is("explore"));
        assert!(!at("/explore/cookbooks").area_is("cookbooks"));

        // New Recipe is its own place, not a Recipe.
        assert!(at("/recipes/new").area_is("new"));
        assert!(!at("/recipes/new").area_is("recipes"));

        // Preferences belongs to no area, so nothing is marked. The mark
        // used to say Recipes here, which named the wrong place.
        for area in ["recipes", "explore", "new"] {
            assert!(
                !at("/preferences").area_is(area),
                "Preferences must not be marked as `{area}`"
            );
        }
    }

    #[test]
    fn an_avatar_is_checked_in_public_and_fetched_inside() {
        // Forgejo names itself as a browser sees it. The fetch has to go to
        // the address this application reaches it on.
        assert_eq!(
            avatar_address(
                "http://localhost:3000/avatars/abc",
                "http://localhost:3000/",
                "http://forgejo:3000"
            ),
            Some("http://forgejo:3000/avatars/abc".to_string())
        );
        assert_eq!(
            avatar_address(
                "http://forgejo.test/avatars/abc",
                "http://forgejo.test",
                "http://forgejo.test"
            ),
            Some("http://forgejo.test/avatars/abc".to_string())
        );
    }

    #[test]
    fn an_avatar_anywhere_else_is_refused() {
        for address in [
            "http://evil.test/avatars/abc",
            // A host that only begins with the Forgejo address.
            "http://forgejo.test.evil.test/avatars/abc",
            "https://forgejo.test/avatars/abc",
            "file:///etc/passwd",
            "http://169.254.169.254/latest/meta-data/",
            "",
            "   ",
            "http://forgejo.test",
        ] {
            assert_eq!(
                avatar_address(address, "http://forgejo.test", "http://forgejo:3000"),
                None,
                "`{address}` must not be fetched"
            );
        }
    }
}
