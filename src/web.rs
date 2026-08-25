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
use crate::health;
use crate::session::{self, COOKIE_NAME, CurrentUser};

/// Shared state that every handler can read.
#[derive(Debug, Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub forgejo: ForgejoClient,
    pub cipher: Cipher,
    /// Whether the session cookie carries the `Secure` attribute.
    pub cookie_secure: bool,
    pub installation_id: String,
}

/// Content Security Policy for every page.
///
/// `default-src 'self'` stops the browser from loading a script, a style, a
/// font, or an image from another host. A page therefore cannot depend on an
/// external CDN, and cannot leak a page view to one.
const CONTENT_SECURITY_POLICY: &str =
    "default-src 'self'; img-src 'self' data:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'";

pub fn router(state: AppState, static_dir: &str) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/health", get(health_endpoint))
        .merge(crate::auth::router())
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
}

impl Layout {
    pub fn new(user: Option<&CurrentUser>) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            signed_in: user.is_some(),
            user_name: user.map(|u| u.display_name.clone()).unwrap_or_default(),
            user_avatar: user.map(|u| u.avatar_url.clone()).unwrap_or_default(),
        }
    }
}

#[derive(Template)]
#[template(path = "index.html")]
struct IndexTemplate {
    layout: Layout,
    forgejo_url: String,
}

async fn index(State(state): State<Arc<AppState>>, MaybeUser(user): MaybeUser) -> Response {
    let template = IndexTemplate {
        layout: Layout::new(user.as_ref()),
        forgejo_url: state.forgejo.public_url().to_string(),
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
