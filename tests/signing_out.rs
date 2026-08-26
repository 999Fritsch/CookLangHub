//! Signing out, and what a browser keeps afterwards.
//!
//! Two things happen when a person signs out. The session ends here, which
//! this application controls. And the permission stays in Forgejo, which it
//! does not: Forgejo 15 publishes no revocation address and its API has no
//! operation for one grant, so only a person can withdraw it, in Forgejo.
//! The page says so rather than letting a person believe it is gone.
//!
//! The browser must also stop keeping the pages. Without that, the Back
//! button still shows somebody's Recipes after they sign out, which matters
//! most on a computer that people share.

mod support;

use support::TestApp;

async fn ready() -> (support::Forgejo, TestApp, String) {
    let forgejo = support::start_forgejo().await;
    forgejo.create_user("alex", true);
    forgejo.create_user("sam", false);
    let app = support::start_app(&forgejo.base_url).await;
    app.bootstrap(&forgejo.access_token("alex")).await;
    let session = support::sign_in(&app, &forgejo, "sam").await;
    (forgejo, app, session)
}

async fn sign_out(app: &TestApp, session: &str) -> reqwest::Response {
    support::client()
        .post(app.url("/auth/sign-out"))
        .header("cookie", format!("cooklanghub_session={session}"))
        .send()
        .await
        .expect("cannot sign out")
}

#[tokio::test]
async fn signing_out_says_what_forgejo_still_holds() {
    let (forgejo, app, session) = ready().await;

    let response = sign_out(&app, &session).await;
    assert_eq!(response.status(), 200, "the answer is a page, not a jump");
    let body = response.text().await.unwrap_or_default();

    assert!(body.contains("You signed out"));
    assert!(
        body.contains("Forgejo keeps your permission"),
        "a person must not believe the permission went with the sign-in"
    );

    // The way to withdraw it. This application cannot do it, so it shows
    // the page that can.
    assert!(
        body.contains(&format!("{}/user/settings/applications", forgejo.base_url)),
        "the page must name where the permission can be removed"
    );

    // Cooking words, and no Git or OAuth words for a person to decode.
    for word in ["OAuth", "token", "grant", "revoke", "commit", "repository"] {
        assert!(
            !body.contains(word),
            "`{word}` is not a word this page should use"
        );
    }
}

#[tokio::test]
async fn signing_out_really_ends_the_session() {
    let (_forgejo, app, session) = ready().await;

    let response = sign_out(&app, &session).await;

    // The cookie is overwritten with a spent one, so the browser drops it.
    let cookie = support::set_cookie(&response, "cooklanghub_session")
        .expect("the answer must clear the cookie");
    assert!(cookie.contains("cooklanghub_session=;") || cookie.contains("Max-Age=0"));

    // And the row is gone, so the old cookie cannot be replayed.
    let after = support::client()
        .get(app.url("/"))
        .header("cookie", format!("cooklanghub_session={session}"))
        .send()
        .await
        .expect("cannot reach the page")
        .text()
        .await
        .unwrap_or_default();

    assert!(
        after.contains("Sign in"),
        "the old cookie must no longer name a signed-in person"
    );
}

#[tokio::test]
async fn a_browser_does_not_keep_a_page_after_a_person_signs_out() {
    let (_forgejo, app, session) = ready().await;

    support::create_recipe(
        &app,
        &session,
        "Chili Sin Carne",
        "---\nservings: 4\n---\n\nChop the @onion{1}.",
        true,
    )
    .await;

    for path in [
        "/",
        "/explore",
        "/preferences",
        "/recipes/sam/chili-sin-carne",
    ] {
        let response = support::client()
            .get(app.url(path))
            .header("cookie", format!("cooklanghub_session={session}"))
            .send()
            .await
            .expect("cannot reach the page");

        assert_eq!(
            response
                .headers()
                .get("cache-control")
                .and_then(|value| value.to_str().ok()),
            Some("no-store"),
            "`{path}` must not stay in the browser after a sign-out"
        );
    }
}

#[tokio::test]
async fn the_files_that_every_person_shares_stay_in_the_browser() {
    let (_forgejo, app, _session) = ready().await;

    // The stylesheet and the scripts are the same for everybody and hold
    // nothing private. Telling a browser to forget them would cost a
    // download on every page for no gain.
    for path in ["/static/css/styles.css", "/static/js/timer.js"] {
        let response = support::client()
            .get(app.url(path))
            .send()
            .await
            .expect("cannot reach the file");

        assert_eq!(response.status(), 200, "`{path}` must be served");
        assert_ne!(
            response
                .headers()
                .get("cache-control")
                .and_then(|value| value.to_str().ok()),
            Some("no-store"),
            "`{path}` is the same for everybody and must stay in the browser"
        );
    }
}

#[tokio::test]
async fn the_blanket_rule_reaches_a_route_that_sets_none_of_its_own() {
    let (_forgejo, app, session) = ready().await;

    // The avatar cannot be served against the disposable container: Forgejo
    // names itself by its own default address there, while the test reaches
    // it on a mapped port, so the address guard refuses it and the route
    // answers 404. That is the guard working. What this asserts is the
    // blanket rule underneath: a route that sets no rule of its own gets
    // one, whatever it answers.
    //
    // A route that DOES set its own rule keeps it. That is asserted where a
    // photo really exists, in tests/upload_recipe.rs.
    let response = support::client()
        .get(app.url("/avatar"))
        .header("cookie", format!("cooklanghub_session={session}"))
        .send()
        .await
        .expect("cannot reach the avatar");

    assert_eq!(
        response
            .headers()
            .get("cache-control")
            .and_then(|value| value.to_str().ok()),
        Some("no-store")
    );
}

#[tokio::test]
async fn the_menu_on_a_narrow_screen_needs_no_script() {
    let (_forgejo, app, session) = ready().await;

    let body = support::client()
        .get(app.url("/"))
        .header("cookie", format!("cooklanghub_session={session}"))
        .send()
        .await
        .expect("cannot reach the page")
        .text()
        .await
        .unwrap_or_default();

    // CookCLI opens its menu from an `onclick` attribute. The policy here
    // refuses that, so the menu is a `<details>` and opens on its own.
    assert!(
        body.contains("<details class=\"menu"),
        "the menu is a details"
    );
    assert!(!body.contains("onclick="), "no inline handler anywhere");

    // Sign-out changes state, so it stays a form inside the menu and never
    // becomes a link that another site could make a browser follow.
    let menu = body
        .split("<details class=\"menu")
        .nth(1)
        .and_then(|rest| rest.split("</details>").next())
        .expect("the menu must be there");

    assert!(
        menu.contains("<form method=\"post\" action=\"/auth/sign-out\">"),
        "sign-out stays a form"
    );
    for place in ["/explore", "/recipes/new", "/preferences"] {
        assert!(menu.contains(place), "the menu must hold `{place}`");
    }
}
