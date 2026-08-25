//! HTTP surface: server-rendered pages, the health endpoint, and static files.

use std::sync::Arc;

use askama::Template;
use axum::Router;
use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use sqlx::sqlite::SqlitePool;
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;

use crate::forgejo::ForgejoClient;
use crate::health;

/// Shared state that every handler can read.
#[derive(Debug, Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub forgejo: ForgejoClient,
    /// Base URL that a browser uses to reach Forgejo. Every link on a page
    /// uses this value, never the internal API URL.
    pub forgejo_public_url: String,
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

#[derive(Template)]
#[template(path = "index.html")]
struct IndexTemplate {
    forgejo_url: String,
    version: &'static str,
}

async fn index(State(state): State<Arc<AppState>>) -> Response {
    let template = IndexTemplate {
        forgejo_url: state.forgejo_public_url.clone(),
        version: env!("CARGO_PKG_VERSION"),
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
