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
    Router::new()
        .route("/", get(index))
        .route("/health", get(health_endpoint))
        .merge(crate::auth::router())
        .merge(crate::web_recipes::router())
        .merge(crate::theme::router())
        .merge(crate::web_browse::router())
        .merge(crate::web_discussions::router())
        .merge(crate::web_edit::router())
        .merge(crate::web_sharing::router())
        .merge(crate::webhook::router())
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
        .with_state(Arc::new(state))
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
            path: "/".to_string(),
        }
    }

    /// Add what the request itself carries.
    pub fn on(mut self, headers: &axum::http::HeaderMap, path: &str) -> Self {
        self.theme = crate::theme::from_headers(headers);
        self.path = path.to_string();
        self
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
