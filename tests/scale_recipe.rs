//! Acceptance tests for the view options of a Recipe.
//!
//! A cook scales the servings, converts the units, and runs a timer. Every
//! one of these changes the view. The heart of these tests is the opposite
//! assertion: after all of it, Forgejo holds the same bytes and the same
//! single Version.

mod support;

use base64::Engine;
use cooklanghub::secret::Secret;
use cooklanghub::session::COOKIE_NAME;

/// A Recipe with a serving count, a metric amount, an imperial amount, and
/// a German timer, so that one page exercises every view option.
const RECIPE: &str = "---\nservings: 4\n---\n\n\
     Mix @flour{500%g} and @butter{4%oz} in a #bowl{} and wait ~{8%Min.}.";

const OWNER: &str = "sam";
const SLUG: &str = "sunday-bread";

/// A signed-in person with one Recipe, against a bootstrapped application.
async fn ready() -> (support::Forgejo, support::TestApp, String) {
    let forgejo = support::start_forgejo().await;
    forgejo.create_user("alex", true);
    forgejo.create_user(OWNER, false);
    let token = forgejo.access_token("alex");

    let app = support::start_app(&forgejo.base_url).await;
    app.bootstrap(&token).await;

    let session = support::sign_in(&app, &forgejo, OWNER).await;
    let response = support::create_recipe(&app, &session, "Sunday Bread", RECIPE, false).await;
    assert_eq!(response.status(), 303, "the Recipe must be created");

    (forgejo, app, session)
}

/// Ask for the Recipe page with the given query, and give back the body.
async fn page(app: &support::TestApp, session: &str, query: &str) -> String {
    let response = support::client()
        .get(app.url(&format!("/recipes/{OWNER}/{SLUG}{query}")))
        .header("cookie", format!("{COOKIE_NAME}={session}"))
        .send()
        .await
        .expect("cannot reach the Recipe page");

    assert_eq!(
        response.status(),
        200,
        "`{query}` must still give the Recipe page"
    );

    response.text().await.expect("cannot read the body")
}

/// The exact bytes of `recipe.cook`, and the name Git gives them.
///
/// Forgejo answers with the file in base64, so this is the stored blob and
/// not a rendering of it. Two reads that give the same identifier are the
/// same object in Git.
async fn stored_recipe(forgejo: &support::Forgejo, token: &Secret<String>) -> (Vec<u8>, String) {
    let file = support::forgejo_api(
        forgejo,
        token,
        &format!("/repos/{OWNER}/{SLUG}/contents/recipe.cook"),
    )
    .await;

    let encoded = file["content"]
        .as_str()
        .expect("the file must carry its content")
        .replace(['\n', '\r'], "");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .expect("the content must be base64");
    let sha = file["sha"]
        .as_str()
        .expect("the file must carry its identifier")
        .to_string();

    (bytes, sha)
}

/// How many Versions the Recipe has.
async fn versions(forgejo: &support::Forgejo, token: &Secret<String>) -> usize {
    let commits =
        support::forgejo_api(forgejo, token, &format!("/repos/{OWNER}/{SLUG}/commits")).await;
    commits.as_array().expect("the answer must be a list").len()
}

#[tokio::test]
async fn scaling_converting_and_timing_leave_the_stored_recipe_untouched() {
    let (forgejo, app, session) = ready().await;
    let token = forgejo.access_token(OWNER);

    let (before, sha_before) = stored_recipe(&forgejo, &token).await;
    assert_eq!(versions(&forgejo, &token).await, 1);

    // A cook does everything the page offers.
    let doubled = page(&app, &session, "?servings=8").await;
    let halved = page(&app, &session, "?servings=2").await;
    let metric = page(&app, &session, "?units=metric").await;
    let imperial = page(&app, &session, "?units=imperial").await;
    let both = page(&app, &session, "?servings=8&units=metric").await;

    // The view really did change, or the test below proves nothing.
    assert!(doubled.contains("1 kg"), "8 servings must double the flour");
    assert!(halved.contains("250 g"), "2 servings must halve the flour");
    assert!(
        metric.contains("113.398 g"),
        "metric must turn the ounces into grams"
    );
    assert!(imperial.contains("oz"), "imperial must keep the ounces");
    assert!(both.contains("226.796 g"), "got a different amount");

    // And a timer is on the page, ready to run.
    assert!(doubled.contains("data-timer-seconds=\"480\""));

    // The heart of this ticket. Nothing above wrote anything.
    let (after, sha_after) = stored_recipe(&forgejo, &token).await;
    assert_eq!(
        after, before,
        "the stored recipe.cook must be byte-identical"
    );
    assert_eq!(sha_after, sha_before, "Git must hold the same object");
    assert_eq!(
        versions(&forgejo, &token).await,
        1,
        "a view option must never make a Version"
    );

    // The bytes are the Cooklang the person wrote, with the title in it.
    let text = String::from_utf8(after).expect("the Recipe must be text");
    assert!(text.contains("title: Sunday Bread"));
    assert!(text.contains("@flour{500%g}"), "got `{text}`");
    assert!(
        !text.contains("servings: 8"),
        "the serving count in the file must stay as written"
    );
}

#[tokio::test]
async fn the_page_carries_a_serving_and_units_control_that_needs_no_script() {
    let (_forgejo, app, session) = ready().await;

    let body = page(&app, &session, "").await;

    // A plain form, sent by the browser itself.
    assert!(
        body.contains("<form method=\"get\""),
        "the control must be a form"
    );
    assert!(body.contains("name=\"servings\""), "no serving control");
    assert!(body.contains("name=\"units\""), "no units control");
    assert!(body.contains(">Show</button>"), "no way to send the form");

    // Cooking words only.
    for word in ["branch", "commit", "fork", "pull request"] {
        assert!(
            !body.to_lowercase().contains(word),
            "the page must not say `{word}`"
        );
    }

    // The Recipe itself reads as written until somebody asks otherwise.
    assert!(body.contains("500 g"));
    assert!(body.contains("4 oz"));
    assert!(body.contains("4 servings"));
}

#[tokio::test]
async fn the_control_shows_what_the_page_is_showing() {
    let (_forgejo, app, session) = ready().await;

    let scaled = page(&app, &session, "?servings=8&units=metric").await;

    // The number control carries the count on screen, so a second change
    // starts from where the cook is.
    assert!(scaled.contains("value=\"8\""), "the control must show 8");
    assert!(
        scaled.contains("value=\"metric\" selected"),
        "the units control must show metric"
    );
    // The serving pill follows the view.
    assert!(scaled.contains("8 servings"));
    // And there is one step back to the Recipe as written.
    assert!(scaled.contains(">Reset</a>"), "no way back to the Recipe");

    let written = page(&app, &session, "").await;
    assert!(!written.contains(">Reset</a>"), "nothing to reset");
    assert!(written.contains("4 servings"));
}

#[tokio::test]
async fn a_strange_address_gives_the_recipe_and_never_an_error() {
    let (forgejo, app, session) = ready().await;
    let token = forgejo.access_token(OWNER);

    for query in [
        "?servings=0",
        "?servings=-3",
        "?servings=abc",
        "?servings=99999999999999999999",
        "?servings=4.5",
        "?servings=",
        "?servings=8&servings=abc",
        "?units=klingon",
        "?units=",
        "?units=%3Cscript%3E",
        "?servings=nan&units=nan",
    ] {
        let body = page(&app, &session, query).await;
        assert!(
            body.contains("500 g"),
            "`{query}` must give the Recipe as written"
        );
        assert!(
            !body.contains("<script>alert"),
            "`{query}` must never put markup on the page"
        );
    }

    assert_eq!(
        versions(&forgejo, &token).await,
        1,
        "a strange address must never make a Version"
    );
}

#[tokio::test]
async fn a_recipe_without_a_serving_count_says_so_instead_of_guessing() {
    let (_forgejo, app, session) = ready().await;

    support::create_recipe(
        &app,
        &session,
        "No Count",
        "---\nservings: 4-6\n---\n\nMix @flour{500%g}.",
        false,
    )
    .await;

    let body = support::client()
        .get(app.url(&format!("/recipes/{OWNER}/no-count?servings=8")))
        .header("cookie", format!("{COOKIE_NAME}={session}"))
        .send()
        .await
        .expect("cannot reach the Recipe page")
        .text()
        .await
        .expect("cannot read the body");

    assert!(body.contains("500 g"), "the amount must stay as written");
    assert!(
        body.contains("does not give a serving count"),
        "the page must say why it did not scale"
    );
}

#[tokio::test]
async fn a_timer_is_ready_to_run_and_reads_as_words_without_a_script() {
    let (_forgejo, app, session) = ready().await;

    let body = page(&app, &session, "").await;

    // The badge carries the length, which is what the countdown needs.
    assert!(
        body.contains("data-timer-seconds=\"480\""),
        "the timer must carry its length in seconds"
    );
    assert!(
        body.contains("data-timer-label=\"8 Min.\""),
        "the timer must carry the words to return to"
    );
    // And the badge itself is still the words the author wrote.
    assert!(body.contains("timer-badge"));
    assert!(body.contains("8 Min."));

    // The countdown is a served file, because the policy allows no inline
    // script. A file is also what a person can read and block.
    assert!(
        body.contains("<script src=\"/static/js/timer.js\" defer></script>"),
        "the page must load the countdown from a file"
    );
    assert!(!body.contains("onclick="), "no inline handler is allowed");

    let script = support::client()
        .get(app.url("/static/js/timer.js"))
        .send()
        .await
        .expect("cannot reach the countdown");

    assert_eq!(script.status(), 200, "the countdown must be served");
    let content_type = script
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        content_type.contains("javascript"),
        "the countdown must be served as JavaScript, got `{content_type}`"
    );

    let source = script.text().await.expect("cannot read the countdown");
    assert!(
        source.contains("data-timer-seconds"),
        "the countdown must look for the timers on the page"
    );
}

#[tokio::test]
async fn the_policy_that_forbids_an_inline_script_is_still_in_place() {
    let (_forgejo, app, session) = ready().await;

    let response = support::client()
        .get(app.url(&format!("/recipes/{OWNER}/{SLUG}?servings=8")))
        .header("cookie", format!("{COOKIE_NAME}={session}"))
        .send()
        .await
        .expect("cannot reach the Recipe page");

    let policy = response
        .headers()
        .get("content-security-policy")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();

    assert!(policy.contains("default-src 'self'"), "got `{policy}`");
}
