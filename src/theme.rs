//! Light and dark, chosen by the person.
//!
//! The choice lives in a cookie and the server writes it onto the page, so
//! the correct palette is in the first byte of HTML. A choice applied by a
//! script would show the wrong colours until that script ran.
//!
//! The control is a form. It needs no JavaScript, which keeps the page
//! working under the `default-src 'self'` policy and for anybody who
//! blocks scripts.

use std::sync::Arc;

use axum::Form;
use axum::Router;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::post;
use axum_extra::extract::CookieJar;
use axum_extra::extract::cookie::{Cookie, SameSite};
use serde::Deserialize;

use crate::web::AppState;

/// Name of the cookie that holds the choice.
pub const COOKIE_NAME: &str = "cooklanghub_theme";

/// How long the choice lasts.
const LIFETIME_DAYS: i64 = 365;

/// What a person picked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    /// Follow the operating system. This is what a new visitor gets.
    #[default]
    System,
    Light,
    Dark,
}

impl Theme {
    pub fn as_str(&self) -> &'static str {
        match self {
            Theme::System => "system",
            Theme::Light => "light",
            Theme::Dark => "dark",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "system" => Some(Theme::System),
            "light" => Some(Theme::Light),
            "dark" => Some(Theme::Dark),
            _ => None,
        }
    }

    /// The class that the page carries on its root element.
    ///
    /// CookCLI switches its palette with a `dark` class, so this project
    /// uses the same class and can keep its rules unchanged. `System`
    /// writes nothing, which leaves `prefers-color-scheme` in charge.
    pub fn css_class(&self) -> &'static str {
        match self {
            Theme::System => "",
            Theme::Light => "light",
            Theme::Dark => "dark",
        }
    }

    pub fn is(&self, name: &str) -> bool {
        self.as_str() == name
    }
}

/// Read the choice out of the request.
pub fn from_headers(headers: &HeaderMap) -> Theme {
    CookieJar::from_headers(headers)
        .get(COOKIE_NAME)
        .and_then(|cookie| Theme::parse(cookie.value()))
        .unwrap_or_default()
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/preferences/theme", post(choose))
}

#[derive(Debug, Deserialize)]
struct ThemeForm {
    theme: String,
    /// Where the person was, so the answer returns them there.
    #[serde(default)]
    return_to: String,
}

async fn choose(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
    Form(form): Form<ThemeForm>,
) -> Response {
    let theme = Theme::parse(&form.theme).unwrap_or_default();

    let mut cookie = Cookie::new(COOKIE_NAME, theme.as_str().to_string());
    cookie.set_path("/");
    // A colour choice is not a secret, and no script needs to read it.
    cookie.set_http_only(true);
    cookie.set_secure(state.cookie_secure);
    cookie.set_same_site(SameSite::Lax);
    cookie.set_max_age(time::Duration::days(LIFETIME_DAYS));

    (jar.add(cookie), Redirect::to(&safe_return(&form.return_to))).into_response()
}

/// Keep the return address on this site.
///
/// A value that comes from a form can name another site. Following it would
/// let somebody use this application to send a person somewhere else, so
/// only a plain path on this host is accepted.
fn safe_return(value: &str) -> String {
    let trimmed = value.trim();

    let acceptable = trimmed.starts_with('/')
        // `//host` and `/\host` are addresses on another host.
        && !trimmed.starts_with("//")
        && !trimmed.starts_with("/\\")
        && !trimmed.contains(['\r', '\n'])
        && !trimmed.contains(':');

    if acceptable {
        trimmed.to_string()
    } else {
        "/".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_is_what_a_new_visitor_gets() {
        assert_eq!(Theme::default(), Theme::System);
        assert_eq!(from_headers(&HeaderMap::new()), Theme::System);
    }

    #[test]
    fn system_writes_no_class_so_the_operating_system_decides() {
        assert_eq!(Theme::System.css_class(), "");
        assert_eq!(Theme::Light.css_class(), "light");
        assert_eq!(Theme::Dark.css_class(), "dark");
    }

    #[test]
    fn a_choice_is_read_back_from_the_cookie() {
        for (value, expected) in [
            ("light", Theme::Light),
            ("dark", Theme::Dark),
            ("system", Theme::System),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert("cookie", format!("{COOKIE_NAME}={value}").parse().unwrap());
            assert_eq!(from_headers(&headers), expected);
        }
    }

    #[test]
    fn a_value_that_is_not_a_theme_falls_back_to_system() {
        let mut headers = HeaderMap::new();
        headers.insert("cookie", format!("{COOKIE_NAME}=purple").parse().unwrap());
        assert_eq!(from_headers(&headers), Theme::System);
    }

    #[test]
    fn a_return_address_stays_on_this_site() {
        assert_eq!(safe_return("/recipes/sam/chili"), "/recipes/sam/chili");
        assert_eq!(safe_return("/"), "/");
    }

    #[test]
    fn a_return_address_that_leaves_this_site_is_refused() {
        for value in [
            "https://evil.test",
            "//evil.test",
            "/\\evil.test",
            "javascript:alert(1)",
            "/x\r\nLocation: https://evil.test",
            "",
            "recipes/sam/chili",
        ] {
            assert_eq!(safe_return(value), "/", "`{value}` must not be followed");
        }
    }
}
