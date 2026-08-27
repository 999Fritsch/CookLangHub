//! What the application does while Forgejo does not answer.
//!
//! Forgejo is the authority for identity, permissions, and every repository
//! that carries a Recipe or a Cookbook. When it does not answer, this
//! application knows nothing current. Two rules follow from that, and this
//! module holds both of them in one place so that no page can forget one.
//!
//! 1. **No edit happens.** A change that Forgejo cannot record is not a
//!    change, so a request that would write is refused before it starts.
//! 2. **No cache is shown as current.** The Recipe index and the Cookbook
//!    index are caches. During an outage a page says that CookLangHub
//!    cannot reach Forgejo, and it shows no Recipe from the cache as though
//!    the cache were the truth.
//!
//! The guard is one layer instead of a check in every handler. It runs
//! before a request that writes, so nothing is attempted. After a request
//! that reads it looks only at an answer that already failed: a page that
//! worked costs no extra question, and a page that failed is asked once
//! whether Forgejo is the reason. A 404 with Forgejo running still means
//! "there is no such Recipe", and a 404 while Forgejo is away means
//! "CookLangHub cannot tell", which is a different thing to say.

use std::sync::Arc;

use askama::Template;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Response};

use crate::forgejo::{ForgejoClient, ForgejoError};
use crate::session::{self, COOKIE_NAME};
use crate::web::{AppState, Layout};

/// Shown wherever a list or a page cannot be built because Forgejo is away.
///
/// One sentence says the cause, and one says that the list is not a short
/// list but an unknown one.
pub const MESSAGE: &str = "CookLangHub cannot reach Forgejo now, so this is not available. \
     Forgejo holds the Recipes and the Cookbooks. Wait a moment and try again.";

/// Shown on a list that Forgejo could not answer for.
///
/// The list is empty, and the reason matters: it is empty because nothing is
/// known, and not because the person has nothing. The application keeps a
/// copy of the titles to make a search fast, and that copy can be old, so
/// this says plainly that the copy is not what the list shows.
pub const LIST_MESSAGE: &str = "CookLangHub cannot reach Forgejo now, so this list shows \
     nothing. Forgejo says what you can see. CookLangHub keeps a copy of the titles, and \
     that copy can be old, so this list does not show it. Wait a moment and try again.";

/// The paths that need no Forgejo, and that must keep working during an
/// outage.
///
/// `/health` answers for an orchestrator and must keep its own body and its
/// own status code. The preferences and the sign-out are held by this
/// application alone. The webhook is Forgejo talking to this application,
/// and it carries its own refusal.
const LOCAL_ONLY: [&str; 5] = [
    "/health",
    "/preferences/theme",
    "/preferences/facts",
    "/auth/sign-out",
    crate::webhook::PATH,
];

/// Whether this fault means Forgejo is away, rather than an answer about
/// one thing.
///
/// A 404 is Forgejo saying "there is no such repository", which is a true
/// answer. A 500 is Forgejo failing to answer at all, which is an outage as
/// far as a reader is concerned.
pub fn is_outage(error: &ForgejoError) -> bool {
    match error {
        ForgejoError::Unreachable(_) | ForgejoError::Client(_) => true,
        ForgejoError::Status { status, .. } => *status >= 500,
        ForgejoError::Body(_) => false,
    }
}

/// Whether Forgejo answers now.
///
/// Any answer counts, including a refusal: a Forgejo that refuses a question
/// is running, and the refusal belongs to the question and not to the state
/// of the instance.
pub async fn reachable(forgejo: &ForgejoClient) -> bool {
    match forgejo.version().await {
        Ok(_) => true,
        Err(error) => !is_outage(&error),
    }
}

/// Whether the answer says the handler could not do its work.
///
/// A refusal of a person (401, 403) is a true answer and stays. So does a
/// conflict, which says somebody wrote first.
fn failed(status: StatusCode) -> bool {
    status == StatusCode::NOT_FOUND || status.is_server_error()
}

/// Refuse every edit while Forgejo is away, and never present a cache as
/// current.
pub async fn guard(State(state): State<Arc<AppState>>, request: Request, next: Next) -> Response {
    let path = request.uri().path().to_string();

    if LOCAL_ONLY.contains(&path.as_str()) {
        return next.run(request).await;
    }

    let writes = !matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    );
    let headers = request.headers().clone();

    // A request that would write is stopped before the handler runs, so
    // nothing half-finished can reach Forgejo or Git.
    if writes && !reachable(&state.forgejo).await {
        tracing::warn!(%path, "an edit was refused because Forgejo does not answer");
        return unavailable(&state, &headers, &path).await;
    }

    let response = next.run(request).await;

    if !failed(response.status()) {
        return response;
    }

    // The handler failed. Ask once whether Forgejo is the reason, so that a
    // page never says "there is no such Recipe" when the truth is "nobody
    // can tell just now".
    if reachable(&state.forgejo).await {
        return response;
    }

    tracing::warn!(%path, status = %response.status(),
        "a page could not be built because Forgejo does not answer");
    unavailable(&state, &headers, &path).await
}

#[derive(Template)]
#[template(path = "unavailable.html")]
struct UnavailableTemplate {
    layout: Layout,
    forgejo_url: String,
    /// Where **Try again** goes.
    here: String,
}

/// The answer while Forgejo is away.
///
/// A browser gets the page. Anything else — the editor saving a draft, for
/// example — gets the same words as JSON, so that the message a person sees
/// is the same message wherever they are.
async fn unavailable(state: &AppState, headers: &HeaderMap, path: &str) -> Response {
    let status = StatusCode::SERVICE_UNAVAILABLE;

    if !wants_html(headers) {
        return (
            status,
            axum::Json(serde_json::json!({ "version": "", "message": MESSAGE })),
        )
            .into_response();
    }

    // The session store is this application's own, so a person stays signed
    // in during an outage and the page keeps their header.
    let user = match headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| cookie_value(value, COOKIE_NAME))
    {
        Some(token) => session::lookup(&state.pool, &token).await.ok().flatten(),
        None => None,
    };

    let template = UnavailableTemplate {
        layout: Layout::new(user.as_ref()).on(headers, path),
        forgejo_url: state.forgejo.public_url().to_string(),
        here: path.to_string(),
    };

    match template.render() {
        Ok(body) => (status, Html(body)).into_response(),
        Err(error) => {
            tracing::error!(%error, "cannot render the unavailable page");
            (status, MESSAGE).into_response()
        }
    }
}

/// Whether the caller is a browser asking for a page.
fn wants_html(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("text/html"))
}

/// One cookie value out of a `Cookie` header.
fn cookie_value(header: &str, name: &str) -> Option<String> {
    header
        .split(';')
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| key.trim() == name)
        .map(|(_, value)| value.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_forgejo_that_is_away_is_an_outage() {
        assert!(is_outage(&ForgejoError::Unreachable("refused".to_string())));
        assert!(is_outage(&ForgejoError::Status {
            status: 502,
            body: String::new()
        }));
        assert!(is_outage(&ForgejoError::Status {
            status: 500,
            body: String::new()
        }));
    }

    #[test]
    fn an_answer_about_one_thing_is_not_an_outage() {
        // Forgejo answered. What it said is about the question, not about
        // whether Forgejo is running.
        for status in [401, 403, 404, 409, 422] {
            assert!(
                !is_outage(&ForgejoError::Status {
                    status,
                    body: String::new()
                }),
                "{status} must not read as an outage"
            );
        }
        assert!(!is_outage(&ForgejoError::Body("not JSON".to_string())));
    }

    #[test]
    fn only_a_failed_answer_is_looked_at_again() {
        assert!(failed(StatusCode::NOT_FOUND));
        assert!(failed(StatusCode::INTERNAL_SERVER_ERROR));
        assert!(failed(StatusCode::BAD_GATEWAY));

        for status in [
            StatusCode::OK,
            StatusCode::SEE_OTHER,
            StatusCode::UNAUTHORIZED,
            StatusCode::FORBIDDEN,
            StatusCode::CONFLICT,
            StatusCode::BAD_REQUEST,
        ] {
            assert!(!failed(status), "{status} must pass through");
        }
    }

    #[test]
    fn the_pages_that_need_no_forgejo_are_never_guarded() {
        // A person must be able to sign out and to change the palette while
        // Forgejo is away, and an orchestrator must still read /health.
        for path in [
            "/health",
            "/auth/sign-out",
            "/preferences/theme",
            "/preferences/facts",
            crate::webhook::PATH,
        ] {
            assert!(LOCAL_ONLY.contains(&path), "`{path}` needs no Forgejo");
        }

        // Everything that writes to Forgejo or to Git is guarded.
        for path in ["/recipes/new", "/cookbooks/new", "/admin/index/rebuild"] {
            assert!(
                !LOCAL_ONLY.contains(&path),
                "`{path}` must go through the guard"
            );
        }
    }

    #[test]
    fn a_browser_gets_a_page_and_a_script_gets_the_same_words() {
        let mut browser = HeaderMap::new();
        browser.insert(
            header::ACCEPT,
            "text/html,application/xhtml+xml".parse().unwrap(),
        );
        assert!(wants_html(&browser));

        let mut script = HeaderMap::new();
        script.insert(header::ACCEPT, "*/*".parse().unwrap());
        assert!(!wants_html(&script));

        assert!(!wants_html(&HeaderMap::new()));
    }

    #[test]
    fn a_cookie_is_read_out_of_a_header_with_several() {
        assert_eq!(
            cookie_value("a=1; cooklanghub_session=abc; b=2", "cooklanghub_session"),
            Some("abc".to_string())
        );
        assert_eq!(cookie_value("a=1", "cooklanghub_session"), None);
        assert_eq!(cookie_value("", "cooklanghub_session"), None);
    }
}
