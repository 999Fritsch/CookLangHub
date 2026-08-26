//! Acceptance tests for Discussions on a Recipe.
//!
//! A Discussion is a Forgejo issue. Every test therefore asks Forgejo what
//! happened, and not only the application: a Discussion that the application
//! shows but Forgejo does not hold would mean a second discussion store,
//! which this product must not have.

mod support;

use cooklanghub::secret::Secret;
use cooklanghub::session::COOKIE_NAME;
use reqwest::Method;
use serde_json::json;

const SOURCE: &str = "Chop the @onion{1} and fry it in a #pan{} for ~{5%minutes}.";

/// `A note for a Variation.` in base64, which is what the Forgejo file
/// endpoint expects. It is written out so that the test needs no encoder.
const NOTE: &str = "QSBub3RlIGZvciBhIFZhcmlhdGlvbi4K";

/// A signed-in person, a bootstrapped application, and one Recipe.
async fn ready() -> (support::Forgejo, support::TestApp, String) {
    let forgejo = support::start_forgejo().await;
    forgejo.create_user("alex", true);
    forgejo.create_user("sam", false);
    let token = forgejo.access_token("alex");

    let app = support::start_app(&forgejo.base_url).await;
    app.bootstrap(&token).await;

    let session = support::sign_in(&app, &forgejo, "sam").await;
    let created = support::create_recipe(&app, &session, "Chili", SOURCE, false).await;
    assert_eq!(created.status(), 303, "the Recipe was not created");

    (forgejo, app, session)
}

/// Read a page, with a session cookie or without one.
async fn read(app: &support::TestApp, session: Option<&str>, path: &str) -> reqwest::Response {
    let mut request = support::client().get(app.url(path));
    if let Some(session) = session {
        request = request.header("cookie", format!("{COOKIE_NAME}={session}"));
    }
    request.send().await.expect("cannot reach the page")
}

/// Post a form, with a session cookie or without one.
async fn post(
    app: &support::TestApp,
    session: Option<&str>,
    path: &str,
    fields: &[(&str, &str)],
) -> reqwest::Response {
    let mut request = support::client().post(app.url(path));
    if let Some(session) = session {
        request = request.header("cookie", format!("{COOKIE_NAME}={session}"));
    }
    request
        .form(fields)
        .send()
        .await
        .expect("cannot post the form")
}

async fn text(response: reqwest::Response) -> String {
    response.text().await.expect("cannot read the body")
}

fn location(response: &reqwest::Response) -> String {
    response
        .headers()
        .get("location")
        .and_then(|value| value.to_str().ok())
        .expect("the response has no location header")
        .to_string()
}

/// Start a Discussion the way a person does.
async fn start(
    app: &support::TestApp,
    session: &str,
    title: &str,
    message: &str,
) -> reqwest::Response {
    post(
        app,
        Some(session),
        "/recipes/sam/chili/discussions",
        &[("title", title), ("message", message)],
    )
    .await
}

/// Turn Forgejo Issues off for the Recipe, the way its owner can.
async fn turn_issues_off(forgejo: &support::Forgejo, token: &Secret<String>) {
    let response = support::forgejo_write(
        forgejo,
        token,
        Method::PATCH,
        "/repos/sam/chili",
        json!({ "has_issues": false }),
    )
    .await;
    assert!(
        response.status().is_success(),
        "Forgejo did not turn Issues off: {}",
        response.status()
    );

    let repository = support::forgejo_api(forgejo, token, "/repos/sam/chili").await;
    assert_eq!(
        repository["has_issues"], false,
        "Issues must be off before the test starts"
    );
}

/// Drop every element from a page, leaving the words a person reads.
fn visible(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut inside = false;
    for character in html.chars() {
        match character {
            '<' => inside = true,
            '>' => inside = false,
            _ if !inside => out.push(character),
            _ => {}
        }
    }
    out
}

/// The words of the forge must never reach a cook.
fn assert_cooking_words(html: &str) {
    let words = visible(html).to_lowercase();
    for forge_word in [
        "issue",
        "pull request",
        "branch",
        "commit",
        "repository",
        "fork",
    ] {
        assert!(
            !words.contains(forge_word),
            "the page says `{forge_word}` to a cook"
        );
    }
}

#[tokio::test]
async fn a_recipe_has_a_discussions_area() {
    let (_forgejo, app, session) = ready().await;

    let recipe = text(read(&app, Some(&session), "/recipes/sam/chili").await).await;
    assert!(
        recipe.contains("href=\"/recipes/sam/chili/discussions\""),
        "the Recipe page must lead to its Discussions"
    );

    let response = read(&app, Some(&session), "/recipes/sam/chili/discussions").await;
    assert_eq!(response.status(), 200);

    let page = text(response).await;
    assert!(page.contains("Discussions"));
    assert!(
        page.contains("This Recipe has no Discussion yet."),
        "an area with nothing in it must say so"
    );
    assert!(page.contains("Start a Discussion"));
    // The page carries the Recipe title, so the area belongs to the Recipe.
    assert!(page.contains("Chili"));
    assert_cooking_words(&page);
}

#[tokio::test]
async fn a_new_discussion_becomes_a_forgejo_issue() {
    let (forgejo, app, session) = ready().await;
    let token = forgejo.access_token("sam");

    let created = start(
        &app,
        &session,
        "How much salt?",
        "The Recipe says a pinch. How much is that?",
    )
    .await;

    assert_eq!(created.status(), 303);
    assert_eq!(location(&created), "/recipes/sam/chili/discussions/1");

    // Forgejo holds it, and it is an issue of the Recipe.
    let issues = support::forgejo_api(&forgejo, &token, "/repos/sam/chili/issues").await;
    let issues = issues.as_array().expect("Forgejo listed no issues");
    assert_eq!(issues.len(), 1, "Forgejo must hold exactly one Discussion");
    assert_eq!(issues[0]["title"], "How much salt?");
    assert_eq!(
        issues[0]["body"],
        "The Recipe says a pinch. How much is that?"
    );
    assert_eq!(issues[0]["state"], "open");
    assert_eq!(
        issues[0]["user"]["login"], "sam",
        "the Discussion belongs to the person who started it"
    );

    // And the application shows what Forgejo holds.
    let list = text(read(&app, Some(&session), "/recipes/sam/chili/discussions").await).await;
    assert!(list.contains("How much salt?"));
    assert!(list.contains("/recipes/sam/chili/discussions/1"));
    assert!(list.contains("Open"), "a new Discussion is open");

    let page = text(read(&app, Some(&session), "/recipes/sam/chili/discussions/1").await).await;
    assert!(page.contains("How much salt?"));
    assert!(page.contains("The Recipe says a pinch."));
    assert!(page.contains("sam"), "the page names who started it");
    assert_cooking_words(&page);
}

#[tokio::test]
async fn a_discussion_needs_a_title() {
    let (forgejo, app, session) = ready().await;
    let token = forgejo.access_token("sam");

    let refused = start(&app, &session, "   ", "A message without a title.").await;

    assert_eq!(refused.status(), 200, "the person stays on the page");
    let page = text(refused).await;
    assert!(page.contains("A Discussion needs a title."));
    // What they wrote is still there.
    assert!(page.contains("A message without a title."));

    let issues = support::forgejo_api(&forgejo, &token, "/repos/sam/chili/issues").await;
    assert_eq!(
        issues.as_array().map(Vec::len),
        Some(0),
        "nothing must reach Forgejo"
    );
}

#[tokio::test]
async fn a_person_writes_a_comment_in_a_discussion() {
    let (forgejo, app, session) = ready().await;
    let token = forgejo.access_token("sam");

    start(&app, &session, "How much salt?", "How much is a pinch?").await;

    let written = post(
        &app,
        Some(&session),
        "/recipes/sam/chili/discussions/1/comments",
        &[("message", "About one gram.")],
    )
    .await;

    assert_eq!(written.status(), 303);
    assert_eq!(location(&written), "/recipes/sam/chili/discussions/1");

    // Forgejo holds the comment.
    let comments =
        support::forgejo_api(&forgejo, &token, "/repos/sam/chili/issues/1/comments").await;
    let comments = comments.as_array().expect("Forgejo listed no comments");
    assert_eq!(comments.len(), 1);
    assert_eq!(comments[0]["body"], "About one gram.");
    assert_eq!(comments[0]["user"]["login"], "sam");

    // And the page shows it.
    let page = text(read(&app, Some(&session), "/recipes/sam/chili/discussions/1").await).await;
    assert!(page.contains("About one gram."));
    assert!(page.contains("wrote on"));
}

#[tokio::test]
async fn an_empty_comment_reaches_nobody() {
    let (forgejo, app, session) = ready().await;
    let token = forgejo.access_token("sam");

    start(&app, &session, "How much salt?", "How much is a pinch?").await;

    let refused = post(
        &app,
        Some(&session),
        "/recipes/sam/chili/discussions/1/comments",
        &[("message", "   ")],
    )
    .await;

    assert_eq!(refused.status(), 200);
    assert!(text(refused).await.contains("A comment needs words."));

    let comments =
        support::forgejo_api(&forgejo, &token, "/repos/sam/chili/issues/1/comments").await;
    assert_eq!(comments.as_array().map(Vec::len), Some(0));
}

#[tokio::test]
async fn a_person_closes_a_discussion_and_opens_it_again() {
    let (forgejo, app, session) = ready().await;
    let token = forgejo.access_token("sam");

    start(&app, &session, "How much salt?", "How much is a pinch?").await;

    let closed = post(
        &app,
        Some(&session),
        "/recipes/sam/chili/discussions/1/state",
        &[("state", "closed")],
    )
    .await;

    assert_eq!(closed.status(), 303);
    assert_eq!(location(&closed), "/recipes/sam/chili/discussions/1");

    let issue = support::forgejo_api(&forgejo, &token, "/repos/sam/chili/issues/1").await;
    assert_eq!(issue["state"], "closed", "Forgejo must hold the new state");

    let page = text(read(&app, Some(&session), "/recipes/sam/chili/discussions/1").await).await;
    assert!(page.contains("Closed"));
    assert!(
        page.contains("Open Discussion again"),
        "a closed Discussion can be opened again"
    );

    let opened = post(
        &app,
        Some(&session),
        "/recipes/sam/chili/discussions/1/state",
        &[("state", "open")],
    )
    .await;

    assert_eq!(opened.status(), 303);

    let issue = support::forgejo_api(&forgejo, &token, "/repos/sam/chili/issues/1").await;
    assert_eq!(issue["state"], "open");

    let page = text(read(&app, Some(&session), "/recipes/sam/chili/discussions/1").await).await;
    assert!(page.contains("Close Discussion"));
}

#[tokio::test]
async fn a_state_that_forgejo_does_not_know_changes_nothing() {
    let (forgejo, app, session) = ready().await;
    let token = forgejo.access_token("sam");

    start(&app, &session, "How much salt?", "How much is a pinch?").await;

    let refused = post(
        &app,
        Some(&session),
        "/recipes/sam/chili/discussions/1/state",
        &[("state", "deleted")],
    )
    .await;

    assert_eq!(refused.status(), 200);

    let issue = support::forgejo_api(&forgejo, &token, "/repos/sam/chili/issues/1").await;
    assert_eq!(issue["state"], "open", "the Discussion must be untouched");
}

#[tokio::test]
async fn the_discussions_area_is_absent_when_forgejo_issues_are_off() {
    let (forgejo, app, session) = ready().await;
    let token = forgejo.access_token("sam");

    turn_issues_off(&forgejo, &token).await;

    // The Recipe page offers no way in.
    let recipe = text(read(&app, Some(&session), "/recipes/sam/chili").await).await;
    assert!(
        !recipe.contains("/recipes/sam/chili/discussions"),
        "the Recipe page must not lead to an area that does not exist"
    );

    // And the area itself is not there.
    for path in [
        "/recipes/sam/chili/discussions",
        "/recipes/sam/chili/discussions/1",
    ] {
        let response = read(&app, Some(&session), path).await;
        assert_eq!(response.status(), 404, "`{path}` must not exist");
        assert!(
            text(response)
                .await
                .contains("This Recipe has no Discussions area.")
        );
    }
}

#[tokio::test]
async fn the_application_never_turns_forgejo_issues_on_again() {
    let (forgejo, app, session) = ready().await;
    let token = forgejo.access_token("sam");

    turn_issues_off(&forgejo, &token).await;

    // Everything a person can do, done against a Recipe that has no
    // Discussions area.
    read(&app, Some(&session), "/recipes/sam/chili").await;
    read(&app, Some(&session), "/recipes/sam/chili/discussions").await;
    read(&app, Some(&session), "/recipes/sam/chili/discussions/1").await;

    let started = start(&app, &session, "Let me in", "Please").await;
    assert_eq!(started.status(), 404);

    let commented = post(
        &app,
        Some(&session),
        "/recipes/sam/chili/discussions/1/comments",
        &[("message", "Please")],
    )
    .await;
    assert_eq!(commented.status(), 404);

    let changed = post(
        &app,
        Some(&session),
        "/recipes/sam/chili/discussions/1/state",
        &[("state", "closed")],
    )
    .await;
    assert_eq!(changed.status(), 404);

    // Forgejo is unchanged. Issues stay off.
    let repository = support::forgejo_api(&forgejo, &token, "/repos/sam/chili").await;
    assert_eq!(
        repository["has_issues"], false,
        "the application turned Forgejo Issues on again"
    );
}

#[tokio::test]
async fn a_discussion_that_does_not_exist_says_so() {
    let (_forgejo, app, session) = ready().await;

    // The Recipe has a Discussions area. This Discussion is not in it, and
    // the page says that and nothing more, because Forgejo decides who can
    // see what.
    let response = read(&app, Some(&session), "/recipes/sam/chili/discussions/404").await;

    assert_eq!(response.status(), 404);
    assert!(
        text(response)
            .await
            .contains("This Discussion is not available.")
    );
}

#[tokio::test]
async fn a_discussion_is_about_the_whole_recipe() {
    let (_forgejo, app, session) = ready().await;

    start(&app, &session, "How much salt?", "How much is a pinch?").await;

    let recipe = text(read(&app, Some(&session), "/recipes/sam/chili").await).await;
    assert_eq!(
        recipe.matches("/recipes/sam/chili/discussions").count(),
        1,
        "the Recipe offers one Discussions area, and nothing on one step or one ingredient"
    );

    let page = text(read(&app, Some(&session), "/recipes/sam/chili/discussions/1").await).await;
    for marker in ["step=", "ingredient=", "/steps/", "/ingredients/"] {
        assert!(
            !recipe.contains(marker) && !page.contains(marker),
            "`{marker}` offers a comment on one part of the Recipe"
        );
    }
}

#[tokio::test]
async fn the_words_of_a_discussion_never_become_markup() {
    let (_forgejo, app, session) = ready().await;

    let message = "# Not a heading\n<iframe src=x></iframe>\nA second line.";
    start(&app, &session, "<script>alert(1)</script>", message).await;

    let page = text(read(&app, Some(&session), "/recipes/sam/chili/discussions/1").await).await;

    assert!(
        !page.contains("<script>alert(1)</script>"),
        "a script element reached the page"
    );
    assert!(!page.contains("<iframe"), "an iframe reached the page");
    assert!(
        page.contains("&lt;script&gt;") || page.contains("&#60;script"),
        "the marks must be escaped"
    );

    // Markdown stays as the characters the person wrote. Turning it into
    // HTML would need a sanitiser that this prototype does not have.
    assert!(page.contains("# Not a heading"));
    assert!(!page.contains("<h1>Not a heading"));

    // The line breaks survive without an element of their own.
    assert!(
        page.contains("whitespace-pre-wrap"),
        "the line breaks must show"
    );
    assert!(page.contains("A second line."));
}

#[tokio::test]
async fn an_anonymous_cook_reads_a_public_discussion_and_is_offered_a_way_in() {
    let (_forgejo, app, session) = ready().await;

    start(&app, &session, "How much salt?", "How much is a pinch?").await;

    let response = read(&app, None, "/recipes/sam/chili/discussions/1").await;
    assert_eq!(response.status(), 200);

    let page = text(response).await;
    assert!(page.contains("How much salt?"));
    assert!(
        page.contains("/auth/sign-in"),
        "a visitor is offered a way in"
    );
    assert!(
        !page.contains("discussions/1/comments"),
        "a visitor who is not signed in gets no comment form"
    );
}

#[tokio::test]
async fn a_suggestion_never_appears_as_a_discussion() {
    let (forgejo, app, session) = ready().await;
    let token = forgejo.access_token("sam");

    // Wait until Forgejo finished recording the first Version.
    support::forgejo_api(&forgejo, &token, "/repos/sam/chili/commits").await;

    // A Suggestion is a Forgejo pull request. This application cannot make
    // one yet, so Forgejo makes it directly.
    let file = support::forgejo_write(
        &forgejo,
        &token,
        Method::POST,
        "/repos/sam/chili/contents/notes.md",
        json!({
            "content": NOTE,
            "message": "A note",
            "branch": "main",
            "new_branch": "variation",
        }),
    )
    .await;
    assert!(
        file.status().is_success(),
        "cannot write the file: {}",
        file.status()
    );

    let pull = support::forgejo_write(
        &forgejo,
        &token,
        Method::POST,
        "/repos/sam/chili/pulls",
        json!({ "head": "variation", "base": "main", "title": "Use less salt" }),
    )
    .await;
    assert!(
        pull.status().is_success(),
        "cannot make the pull request: {}",
        pull.status()
    );
    let pull: serde_json::Value = pull.json().await.expect("the answer is not JSON");
    let number = pull["number"]
        .as_i64()
        .expect("the answer carries no number");

    start(&app, &session, "How much salt?", "How much is a pinch?").await;

    let list = text(read(&app, Some(&session), "/recipes/sam/chili/discussions").await).await;
    assert!(list.contains("How much salt?"), "the Discussion is missing");
    assert!(
        !list.contains("Use less salt"),
        "a Suggestion must not appear among the Discussions"
    );

    let response = read(
        &app,
        Some(&session),
        &format!("/recipes/sam/chili/discussions/{number}"),
    )
    .await;
    assert_eq!(
        response.status(),
        404,
        "a Suggestion must not open as a Discussion"
    );
}
