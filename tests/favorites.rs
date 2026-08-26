//! Acceptance tests for Favorite and Notify me.
//!
//! A Favorite is a Forgejo star and Notify me is a Forgejo watch. Forgejo is
//! authoritative for both, so every test asks Forgejo itself what happened
//! and never trusts the page alone. Where a test needs a change that this
//! application did not make, it makes that change in Forgejo, which is what
//! an outside action looks like from here.

mod support;

use std::time::Duration;

use cooklanghub::secret::Secret;
use cooklanghub::session::COOKIE_NAME;

use support::{Forgejo, TestApp};

/// Everything that a test starts from.
///
/// Two people, because a Favorite of one person must never appear as the
/// Favorite of another. Forgejo gives one access token per person, so the
/// tokens are made once here.
struct Ready {
    forgejo: Forgejo,
    app: TestApp,
    /// The session cookie of Sam.
    sam: String,
    /// The session cookie of Alex, who administers the installation.
    alex: String,
    /// An access token of Sam, for asking Forgejo directly.
    sam_token: Secret<String>,
    /// An access token of Alex, for asking Forgejo directly.
    alex_token: Secret<String>,
}

async fn ready() -> Ready {
    let forgejo = support::start_forgejo().await;
    forgejo.create_user("alex", true);
    forgejo.create_user("sam", false);

    let alex_token = forgejo.access_token("alex");
    let sam_token = forgejo.access_token("sam");

    let app = support::start_app(&forgejo.base_url).await;
    app.bootstrap(&alex_token).await;

    let sam = support::sign_in(&app, &forgejo, "sam").await;
    let alex = support::sign_in(&app, &forgejo, "alex").await;

    Ready {
        forgejo,
        app,
        sam,
        alex,
        sam_token,
        alex_token,
    }
}

/// Read a page, as an anonymous visitor or as the holder of a session.
async fn page(app: &TestApp, path: &str, session: Option<&str>) -> String {
    let mut request = support::client().get(app.url(path));
    if let Some(session) = session {
        request = request.header("cookie", format!("{COOKIE_NAME}={session}"));
    }

    let response = request.send().await.expect("cannot reach the page");
    assert_eq!(response.status(), 200, "GET {path} answered wrongly");
    response.text().await.expect("the page has no body")
}

/// Where a title sits in a page, so that a test can compare two of them.
fn position(body: &str, needle: &str) -> usize {
    body.find(needle)
        .unwrap_or_else(|| panic!("the page does not name `{needle}`"))
}

/// Ask Forgejo whether one person made one repository a Favorite.
///
/// Forgejo answers 204 for a star that is there and 404 for one that is
/// not. This asks Forgejo itself, so no step of the application can hide
/// what really landed there.
async fn is_starred(forgejo: &Forgejo, token: &Secret<String>, path: &str) -> bool {
    let status = reqwest::Client::new()
        .get(format!("{}/api/v1/user/starred/{path}", forgejo.base_url))
        .header("Authorization", format!("token {}", token.expose()))
        .send()
        .await
        .expect("cannot reach the Forgejo API")
        .status();

    assert!(
        status.is_success() || status.as_u16() == 404,
        "Forgejo answered {status} about the Favorite of {path}"
    );
    status.is_success()
}

/// Ask Forgejo whether it notifies one person about one repository.
///
/// Forgejo answers 404 when it holds no subscription, and 200 with
/// `subscribed` false when a person turned one off. Both mean no.
async fn is_watching(forgejo: &Forgejo, token: &Secret<String>, path: &str) -> bool {
    let response = reqwest::Client::new()
        .get(format!(
            "{}/api/v1/repos/{path}/subscription",
            forgejo.base_url
        ))
        .header("Authorization", format!("token {}", token.expose()))
        .send()
        .await
        .expect("cannot reach the Forgejo API");

    let status = response.status();
    if status.as_u16() == 404 {
        return false;
    }
    assert!(
        status.is_success(),
        "Forgejo answered {status} about the notifications for {path}"
    );

    let body: serde_json::Value = response.json().await.expect("the answer is not JSON");
    body["subscribed"].as_bool().unwrap_or(false)
}

/// Make a repository a Favorite in Forgejo, without this application.
async fn star_outside(forgejo: &Forgejo, token: &Secret<String>, path: &str) {
    let response = support::forgejo_write(
        forgejo,
        token,
        reqwest::Method::PUT,
        &format!("/user/starred/{path}"),
        serde_json::json!({}),
    )
    .await;
    assert!(
        response.status().is_success(),
        "cannot make {path} a Favorite: {}",
        response.status()
    );
}

/// Take a Favorite away in Forgejo, without this application.
async fn unstar_outside(forgejo: &Forgejo, token: &Secret<String>, path: &str) {
    let response = support::forgejo_write(
        forgejo,
        token,
        reqwest::Method::DELETE,
        &format!("/user/starred/{path}"),
        serde_json::json!({}),
    )
    .await;
    assert!(
        response.status().is_success(),
        "cannot remove the Favorite of {path}: {}",
        response.status()
    );
}

/// Post one of the two controls and give back the address it sends the
/// person to.
async fn press(app: &TestApp, session: &str, path: &str, on: bool) -> String {
    let value = if on { "yes" } else { "no" };
    let response = support::post_fields(app, session, path, &[("on", value)]).await;

    assert_eq!(
        response.status(),
        303,
        "POST {path} must answer with a redirect, so a reload cannot repeat it"
    );

    response
        .headers()
        .get("location")
        .and_then(|value| value.to_str().ok())
        .expect("the answer has no location header")
        .to_string()
}

/// Wait until Forgejo reports a different moment of change.
///
/// Forgejo keeps that moment in whole seconds, so two Recipes made inside
/// one second cannot be told apart by it.
async fn next_second() {
    tokio::time::sleep(Duration::from_millis(1100)).await;
}

// ---------------------------------------------------------------- Favorite

#[tokio::test]
async fn favorite_makes_a_star_in_forgejo_and_removing_it_takes_the_star_away() {
    let Ready {
        forgejo,
        app,
        sam,
        alex,
        sam_token,
        alex_token: _alex_token,
    } = ready().await;

    support::create_recipe(&app, &alex, "Alex Stew", "Cook the @beans{2%cups}.", false).await;

    // Forgejo holds no star yet, so the control offers to make one.
    assert!(!is_starred(&forgejo, &sam_token, "alex/alex-stew").await);
    let before = page(&app, "/recipes/alex/alex-stew", Some(&sam)).await;
    assert!(
        before.contains(">Favorite<"),
        "the Recipe must offer Favorite"
    );
    assert!(!before.contains("Remove Favorite"));

    // Sam makes it a Favorite.
    let back = press(&app, &sam, "/recipes/alex/alex-stew/favorite", true).await;
    assert_eq!(back, "/recipes/alex/alex-stew");

    assert!(
        is_starred(&forgejo, &sam_token, "alex/alex-stew").await,
        "Forgejo must hold the star, because Forgejo is authoritative"
    );

    let after = page(&app, "/recipes/alex/alex-stew", Some(&sam)).await;
    assert!(
        after.contains("Remove Favorite"),
        "the Recipe must now offer to remove the Favorite"
    );

    // Sam removes it again.
    press(&app, &sam, "/recipes/alex/alex-stew/favorite", false).await;

    assert!(
        !is_starred(&forgejo, &sam_token, "alex/alex-stew").await,
        "Forgejo must no longer hold the star"
    );

    let removed = page(&app, "/recipes/alex/alex-stew", Some(&sam)).await;
    assert!(removed.contains(">Favorite<"));
    assert!(!removed.contains("Remove Favorite"));
}

#[tokio::test]
async fn the_favorites_list_holds_the_recipes_that_this_person_made_a_favorite() {
    let Ready {
        forgejo,
        app,
        sam,
        alex,
        sam_token,
        alex_token: _alex_token,
    } = ready().await;

    support::create_recipe(&app, &sam, "Sam Soup", "Warm the @broth{1%l}.", false).await;
    support::create_recipe(&app, &alex, "Alex Stew", "Cook the @beans{2%cups}.", false).await;

    press(&app, &sam, "/recipes/alex/alex-stew/favorite", true).await;
    assert!(is_starred(&forgejo, &sam_token, "alex/alex-stew").await);

    let mine = page(&app, "/", Some(&sam)).await;
    assert!(mine.contains("Mine"), "the three lists must be offered");
    assert!(mine.contains("Shared with me"));
    assert!(mine.contains("Favorites"));

    let favorites = page(&app, "/?area=favorites", Some(&sam)).await;
    assert!(
        favorites.contains("Alex Stew"),
        "Favorites must hold what Sam made a Favorite, got: {favorites:.3000}"
    );
    assert!(
        !favorites.contains("Sam Soup"),
        "Favorites must hold only what Sam made a Favorite"
    );

    // A Favorite belongs to one person. Alex made nothing a Favorite.
    let others = page(&app, "/?area=favorites", Some(&alex)).await;
    assert!(
        !others.contains("Alex Stew"),
        "the Favorite of Sam must never appear as a Favorite of Alex"
    );
    assert!(
        others.contains("You have no Favorite Recipes yet"),
        "an empty list must say why it is empty, got: {others:.3000}"
    );

    // And the Recipe page says the same thing to each of them.
    let for_alex = page(&app, "/recipes/alex/alex-stew", Some(&alex)).await;
    assert!(
        for_alex.contains(">Favorite<"),
        "Alex must be offered Favorite, because Alex made none"
    );
    assert!(!for_alex.contains("Remove Favorite"));
}

#[tokio::test]
async fn a_favorite_that_forgejo_loses_is_gone_from_the_application_at_once() {
    let Ready {
        forgejo,
        app,
        sam,
        alex,
        sam_token,
        alex_token: _alex_token,
    } = ready().await;

    support::create_recipe(&app, &alex, "Alex Stew", "Cook the @beans{2%cups}.", false).await;

    press(&app, &sam, "/recipes/alex/alex-stew/favorite", true).await;
    assert!(
        page(&app, "/?area=favorites", Some(&sam))
            .await
            .contains("Alex Stew")
    );

    // Somebody removes the star in Forgejo. The application is told nothing.
    unstar_outside(&forgejo, &sam_token, "alex/alex-stew").await;

    let favorites = page(&app, "/?area=favorites", Some(&sam)).await;
    assert!(
        !favorites.contains("Alex Stew"),
        "Forgejo is authoritative, so a star it lost cannot stay on the list"
    );

    let recipe = page(&app, "/recipes/alex/alex-stew", Some(&sam)).await;
    assert!(
        recipe.contains(">Favorite<") && !recipe.contains("Remove Favorite"),
        "the Recipe page must follow Forgejo and not a copy of its own"
    );

    // A star that a person adds in Forgejo counts here at once, for the
    // same reason.
    star_outside(&forgejo, &sam_token, "alex/alex-stew").await;
    assert!(
        page(&app, "/?area=favorites", Some(&sam))
            .await
            .contains("Alex Stew"),
        "a Favorite made in Forgejo must appear here with no other step"
    );
}

#[tokio::test]
async fn the_application_keeps_no_second_copy_of_a_favorite() {
    let Ready {
        forgejo: _forgejo,
        app,
        sam,
        alex,
        sam_token: _sam_token,
        alex_token: _alex_token,
    } = ready().await;

    support::create_recipe(&app, &alex, "Alex Stew", "Cook the @beans{2%cups}.", false).await;
    press(&app, &sam, "/recipes/alex/alex-stew/favorite", true).await;
    press(&app, &sam, "/recipes/alex/alex-stew/notify", true).await;

    // Nothing in the operational database names a star, a Favorite, or a
    // watch. Forgejo holds all three, and this holds none of them.
    let schema: Vec<(String, String)> =
        sqlx::query_as("SELECT name, sql FROM sqlite_master WHERE sql IS NOT NULL")
            .fetch_all(&app.pool)
            .await
            .expect("cannot read the schema of the operational database");

    for (name, sql) in schema {
        let text = format!("{name} {sql}").to_lowercase();
        for word in ["favorit", "starred", "num_stars", "subscription", "watcher"] {
            assert!(
                !text.contains(word),
                "the operational database names `{word}` in `{name}`, which is a second copy of what Forgejo holds"
            );
        }
    }
}

// --------------------------------------------------------------- Notify me

#[tokio::test]
async fn notify_me_makes_a_watch_in_forgejo() {
    let Ready {
        forgejo,
        app,
        sam,
        alex,
        sam_token,
        alex_token: _alex_token,
    } = ready().await;

    support::create_recipe(&app, &alex, "Alex Stew", "Cook the @beans{2%cups}.", false).await;

    assert!(!is_watching(&forgejo, &sam_token, "alex/alex-stew").await);
    let before = page(&app, "/recipes/alex/alex-stew", Some(&sam)).await;
    assert!(
        before.contains(">Notify me<"),
        "the Recipe must offer Notify me"
    );

    let back = press(&app, &sam, "/recipes/alex/alex-stew/notify", true).await;
    assert_eq!(back, "/recipes/alex/alex-stew");

    assert!(
        is_watching(&forgejo, &sam_token, "alex/alex-stew").await,
        "Forgejo must hold the watch, because Forgejo is authoritative"
    );

    let after = page(&app, "/recipes/alex/alex-stew", Some(&sam)).await;
    assert!(
        after.contains("Do not notify me"),
        "the Recipe must now offer to stop the notifications"
    );

    press(&app, &sam, "/recipes/alex/alex-stew/notify", false).await;

    assert!(
        !is_watching(&forgejo, &sam_token, "alex/alex-stew").await,
        "Forgejo must no longer notify Sam"
    );
    assert!(
        page(&app, "/recipes/alex/alex-stew", Some(&sam))
            .await
            .contains(">Notify me<")
    );
}

#[tokio::test]
async fn notify_me_belongs_to_a_recipe_and_stays_apart_from_a_favorite() {
    let Ready {
        forgejo,
        app,
        sam,
        alex,
        sam_token,
        alex_token: _alex_token,
    } = ready().await;

    support::create_recipe(&app, &alex, "Alex Stew", "Cook the @beans{2%cups}.", false).await;

    // Notify me alone must not make a Favorite.
    press(&app, &sam, "/recipes/alex/alex-stew/notify", true).await;
    assert!(is_watching(&forgejo, &sam_token, "alex/alex-stew").await);
    assert!(
        !is_starred(&forgejo, &sam_token, "alex/alex-stew").await,
        "Notify me is a watch and a Favorite is a star, and one is not the other"
    );
    assert!(
        !page(&app, "/?area=favorites", Some(&sam))
            .await
            .contains("Alex Stew")
    );

    // A Favorite alone must not start the notifications.
    press(&app, &sam, "/recipes/alex/alex-stew/notify", false).await;
    press(&app, &sam, "/recipes/alex/alex-stew/favorite", true).await;
    assert!(is_starred(&forgejo, &sam_token, "alex/alex-stew").await);
    assert!(!is_watching(&forgejo, &sam_token, "alex/alex-stew").await);

    // The Recipe uses the words Notify me. Follow updates belongs to a
    // Cookbook and never appears here.
    let recipe = page(&app, "/recipes/alex/alex-stew", Some(&sam)).await;
    assert!(recipe.contains("Notify me"));
    assert!(
        !recipe.contains("Follow updates"),
        "Follow updates is the Cookbook behaviour and must not reach a Recipe"
    );
    assert!(
        !recipe.contains("Watch") && !recipe.contains("Subscribe"),
        "the page must show cooking words only"
    );
    assert!(
        !recipe.contains("Star") && !recipe.contains("Unstar"),
        "a Favorite is never called a star on a page"
    );
}

// ----------------------------------------------------------------- Explore

#[tokio::test]
async fn explore_can_put_the_most_favorited_first() {
    let Ready {
        forgejo,
        app,
        sam,
        alex: _alex,
        sam_token: _sam_token,
        alex_token,
    } = ready().await;

    support::create_recipe(&app, &sam, "Popular Pie", "Bake the @apples{4}.", false).await;
    next_second().await;
    support::create_recipe(&app, &sam, "Quiet Quiche", "Bake the @eggs{3}.", false).await;

    // Alex makes one of them a Favorite in Forgejo. Forgejo counts them.
    star_outside(&forgejo, &alex_token, "sam/popular-pie").await;

    let controls = page(&app, "/explore", None).await;
    assert!(
        controls.contains("Most favorited"),
        "Explore must offer the Most favorited order"
    );

    let recent = page(&app, "/explore", None).await;
    assert!(
        position(&recent, "Quiet Quiche") < position(&recent, "Popular Pie"),
        "the recent order must put the newest Recipe first"
    );

    let favorited = page(&app, "/explore?sort=favorites", None).await;
    assert!(
        position(&favorited, "Popular Pie") < position(&favorited, "Quiet Quiche"),
        "the Most favorited order must put the Recipe with the most Favorites first, got: {favorited:.3000}"
    );

    // Forgejo does the counting, so a Favorite that it loses changes the
    // order at once.
    unstar_outside(&forgejo, &alex_token, "sam/popular-pie").await;
    star_outside(&forgejo, &alex_token, "sam/quiet-quiche").await;

    let again = page(&app, "/explore?sort=favorites", None).await;
    assert!(
        position(&again, "Quiet Quiche") < position(&again, "Popular Pie"),
        "the order must follow Forgejo, got: {again:.3000}"
    );
}

// ------------------------------------------------------- shape and refusal

#[tokio::test]
async fn both_controls_are_post_forms_that_work_with_no_script() {
    let Ready {
        forgejo: _forgejo,
        app,
        sam,
        alex,
        sam_token: _sam_token,
        alex_token: _alex_token,
    } = ready().await;

    support::create_recipe(&app, &alex, "Alex Stew", "Cook the @beans{2%cups}.", false).await;

    let recipe = page(&app, "/recipes/alex/alex-stew", Some(&sam)).await;

    for action in [
        "/recipes/alex/alex-stew/favorite",
        "/recipes/alex/alex-stew/notify",
    ] {
        assert!(
            recipe.contains(&format!("<form method=\"post\" action=\"{action}\">")),
            "{action} must be a POST form, because it changes state"
        );
        assert!(
            !recipe.contains(&format!("href=\"{action}\"")),
            "{action} must never be a link"
        );

        // The same address as a page is not offered, so a crawler or a
        // prefetch cannot change anything.
        let refused = support::client()
            .get(app.url(action))
            .header("cookie", format!("{COOKIE_NAME}={sam}"))
            .send()
            .await
            .expect("cannot reach the address");
        assert_eq!(refused.status(), 405, "{action} must answer only to a POST");
    }

    // Nothing on this page needs a script for either control.
    assert!(!recipe.contains("onclick"));
}

#[tokio::test]
async fn a_visitor_who_is_not_signed_in_is_offered_neither_control() {
    let Ready {
        forgejo: _forgejo,
        app,
        sam: _sam,
        alex,
        sam_token: _sam_token,
        alex_token: _alex_token,
    } = ready().await;

    support::create_recipe(&app, &alex, "Alex Stew", "Cook the @beans{2%cups}.", false).await;

    let recipe = page(&app, "/recipes/alex/alex-stew", None).await;
    assert!(
        recipe.contains("Alex Stew"),
        "the Recipe must still be read"
    );
    assert!(
        !recipe.contains("Favorite"),
        "a visitor with no account cannot make a Favorite, so the control must be absent"
    );
    assert!(
        !recipe.contains("Notify me"),
        "a visitor with no account cannot be notified, so the control must be absent"
    );
    assert!(!recipe.contains("/recipes/alex/alex-stew/favorite"));
    assert!(!recipe.contains("/recipes/alex/alex-stew/notify"));

    // The Favorites list needs an account too, and says so.
    let favorites = page(&app, "/?area=favorites", None).await;
    assert!(favorites.contains("Sign in"));

    // A visitor who posts the form anyway is sent to sign in, and Forgejo
    // holds nothing new.
    let response = support::client()
        .post(app.url("/recipes/alex/alex-stew/favorite"))
        .form(&[("on", "yes")])
        .send()
        .await
        .expect("cannot post the form");
    assert_eq!(response.status(), 303);
    assert_eq!(
        response
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok()),
        Some("/auth/sign-in")
    );
}

#[tokio::test]
async fn a_recipe_that_forgejo_refuses_is_diagnosed_and_offered_in_forgejo() {
    let Ready {
        forgejo: _forgejo,
        app,
        sam,
        alex: _alex,
        sam_token: _sam_token,
        alex_token: _alex_token,
    } = ready().await;

    // Forgejo shows Sam no such Recipe, so it refuses the star. The
    // application must say so and hand the person the tool that can act.
    let response = support::post_fields(
        &app,
        &sam,
        "/recipes/alex/no-such-recipe/favorite",
        &[("on", "yes")],
    )
    .await;

    assert_eq!(
        response.status(),
        200,
        "a refusal is a page and not a redirect"
    );

    let body = response.text().await.expect("the answer has no body");
    assert!(
        body.contains("Open in Forgejo"),
        "a state the interface cannot handle must offer Open in Forgejo, got: {body:.2000}"
    );
    assert!(
        body.contains("Forgejo did not make this Recipe a Favorite"),
        "the page must say plainly what did not happen, got: {body:.2000}"
    );
}
