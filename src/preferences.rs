//! What a person chooses about how a page looks.
//!
//! A choice here belongs to the person reading, not to the Recipe and not to
//! the installation, so it lives in a cookie and the server writes the
//! result onto the page. That is the same shape as the palette in
//! [`crate::theme`], for the same reasons: nothing flashes while the page
//! loads, no script is needed under the Content Security Policy, and a
//! visitor with no account gets the choice too.
//!
//! Nothing here is stored in the database. A choice that a person can remake
//! in one click is not worth a row, a migration, or a read on every page.

use std::sync::Arc;

use askama::Template;
use axum::Form;
use axum::Router;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum_extra::extract::CookieJar;
use axum_extra::extract::cookie::{Cookie, SameSite};
use serde::Deserialize;

use crate::web::{AppState, Layout, MaybeUser};

/// Name of the cookie that holds the choice.
pub const COOKIE_NAME: &str = "cooklanghub_fact_colour";

/// How long the choice lasts.
const LIFETIME_DAYS: i64 = 365;

/// Whether a Recipe fact carries a colour for its kind.
///
/// CookCLI gives a difficulty, a preparation time, and a cooking time each
/// their own colour. Those colours never reach a page, in CookCLI or here,
/// because `.metadata-pill` sits in no cascade layer and beats them. This
/// choice turns them on with rules that are outside the layer as well.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FactColour {
    /// One grey for every fact. What CookCLI shows.
    #[default]
    Plain,
    /// A colour for each kind of fact.
    Coloured,
}

impl FactColour {
    pub fn as_str(&self) -> &'static str {
        match self {
            FactColour::Plain => "plain",
            FactColour::Coloured => "coloured",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "plain" => Some(FactColour::Plain),
            "coloured" => Some(FactColour::Coloured),
            _ => None,
        }
    }

    /// The class that the page carries on its root element.
    pub fn css_class(&self) -> &'static str {
        match self {
            FactColour::Plain => "",
            FactColour::Coloured => "fact-colour",
        }
    }

    pub fn is(&self, name: &str) -> bool {
        self.as_str() == name
    }
}

/// Read the choice out of the request.
pub fn from_headers(headers: &HeaderMap) -> FactColour {
    CookieJar::from_headers(headers)
        .get(COOKIE_NAME)
        .and_then(|cookie| FactColour::parse(cookie.value()))
        .unwrap_or_default()
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/preferences", get(page))
        .route("/preferences/facts", post(choose_facts))
}

#[derive(Template)]
#[template(path = "preferences.html")]
struct PreferencesTemplate {
    layout: Layout,
}

async fn page(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    MaybeUser(user): MaybeUser,
) -> Response {
    let _ = &state;

    let template = PreferencesTemplate {
        layout: Layout::new(user.as_ref()).on(&headers, "/preferences"),
    };

    match template.render() {
        Ok(body) => Html(body).into_response(),
        Err(error) => {
            tracing::error!(%error, "cannot render the preferences page");
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "template error",
            )
                .into_response()
        }
    }
}

#[derive(Debug, Deserialize)]
struct FactColourForm {
    facts: String,
    /// Where the person was, so the answer returns them there.
    #[serde(default)]
    return_to: String,
}

async fn choose_facts(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
    Form(form): Form<FactColourForm>,
) -> Response {
    let choice = FactColour::parse(&form.facts).unwrap_or_default();

    let mut cookie = Cookie::new(COOKIE_NAME, choice.as_str().to_string());
    cookie.set_path("/");
    // A colour choice is not a secret, and no script needs to read it.
    cookie.set_http_only(true);
    cookie.set_secure(state.cookie_secure);
    cookie.set_same_site(SameSite::Lax);
    cookie.set_max_age(time::Duration::days(LIFETIME_DAYS));

    (
        jar.add(cookie),
        Redirect::to(&crate::theme::safe_return(&form.return_to)),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_is_what_a_new_visitor_gets() {
        assert_eq!(FactColour::default(), FactColour::Plain);
        assert_eq!(from_headers(&HeaderMap::new()), FactColour::Plain);
        // Plain writes no class, so the page draws exactly as CookCLI does.
        assert_eq!(FactColour::Plain.css_class(), "");
        assert_eq!(FactColour::Coloured.css_class(), "fact-colour");
    }

    #[test]
    fn a_choice_is_read_back_from_the_cookie() {
        for (value, expected) in [
            ("plain", FactColour::Plain),
            ("coloured", FactColour::Coloured),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert("cookie", format!("{COOKIE_NAME}={value}").parse().unwrap());
            assert_eq!(from_headers(&headers), expected);
        }
    }

    #[test]
    fn a_value_that_is_not_a_choice_falls_back_to_plain() {
        let mut headers = HeaderMap::new();
        headers.insert("cookie", format!("{COOKIE_NAME}=rainbow").parse().unwrap());
        assert_eq!(from_headers(&headers), FactColour::Plain);
    }
}
