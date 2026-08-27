//! Acceptance tests for browsing, searching, and exploring Recipes.
//!
//! Every test drives the real pages against a real Forgejo. Where a test
//! needs a change that this application did not make, it makes that change
//! in Forgejo itself, which is what an outside push or an administrator
//! action looks like from here.

mod support;

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use cooklanghub::secret::Secret;
use cooklanghub::session::COOKIE_NAME;

use support::{Forgejo, TestApp};

/// Everything that a test starts from.
///
/// `alex` administers the installation and `sam` does not, which is what
/// lets a test tell an administrator action from an ordinary one. Forgejo
/// gives one access token per person, so the tokens are made once here.
struct Ready {
    forgejo: Forgejo,
    app: TestApp,
    /// The session cookie of Sam.
    sam: String,
    /// An access token of Alex, who administers the installation.
    admin: Secret<String>,
    /// An access token of Sam.
    sam_token: Secret<String>,
}

async fn ready() -> Ready {
    let forgejo = support::start_forgejo().await;
    forgejo.create_user("alex", true);
    forgejo.create_user("sam", false);

    let admin = forgejo.access_token("alex");
    let sam_token = forgejo.access_token("sam");

    let app = support::start_app(&forgejo.base_url).await;
    app.bootstrap(&admin).await;

    let sam = support::sign_in(&app, &forgejo, "sam").await;

    Ready {
        forgejo,
        app,
        sam,
        admin,
        sam_token,
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

/// Send a request to Forgejo directly, the way an outside tool would.
async fn forgejo_send(
    forgejo: &Forgejo,
    token: &Secret<String>,
    method: reqwest::Method,
    path: &str,
    body: serde_json::Value,
) -> reqwest::StatusCode {
    let response = reqwest::Client::new()
        .request(method, format!("{}/api/v1{path}", forgejo.base_url))
        .header("Authorization", format!("token {}", token.expose()))
        .json(&body)
        .send()
        .await
        .expect("cannot reach the Forgejo API");

    response.status()
}

/// Replace the topics of a repository in Forgejo.
async fn set_topics(forgejo: &Forgejo, token: &Secret<String>, path: &str, topics: &[&str]) {
    let status = forgejo_send(
        forgejo,
        token,
        reqwest::Method::PUT,
        &format!("/repos/{path}/topics"),
        serde_json::json!({ "topics": topics }),
    )
    .await;
    assert!(status.is_success(), "cannot set the topics: {status}");
}

/// Write `recipe.cook` in Forgejo, without this application.
///
/// This is what a push from a text editor or from Forgejo itself looks
/// like: the Recipe changes and the application is told nothing.
async fn write_recipe_outside(forgejo: &Forgejo, token: &Secret<String>, path: &str, source: &str) {
    let existing = support::forgejo_api(
        forgejo,
        token,
        &format!("/repos/{path}/contents/recipe.cook"),
    )
    .await;
    let sha = existing["sha"].as_str().expect("the file has no sha");

    let status = forgejo_send(
        forgejo,
        token,
        reqwest::Method::PUT,
        &format!("/repos/{path}/contents/recipe.cook"),
        serde_json::json!({
            "content": BASE64.encode(source),
            "sha": sha,
            "message": "Change the Recipe outside the application",
        }),
    )
    .await;
    assert!(status.is_success(), "cannot write the Recipe: {status}");
}

/// The message that Forgejo sends after a Version arrives.
fn push_message(owner: &str, slug: &str, id: i64) -> String {
    serde_json::json!({
        "ref": "refs/heads/main",
        "repository": {
            "id": id,
            "name": slug,
            "full_name": format!("{owner}/{slug}"),
            "owner": { "login": owner },
        },
    })
    .to_string()
}

/// Wait until Forgejo reports a different moment of change.
///
/// Forgejo keeps that moment in whole seconds, so two Recipes made inside
/// one second cannot be told apart by it.
async fn next_second() {
    tokio::time::sleep(Duration::from_millis(1100)).await;
}

#[tokio::test]
async fn the_recipes_area_separates_mine_from_shared_with_me() {
    let Ready {
        forgejo,
        app,
        sam,
        admin,
        sam_token: _sam_token,
    } = ready().await;
    let alex = support::sign_in(&app, &forgejo, "alex").await;

    support::create_recipe(&app, &sam, "Sam Soup", "Warm the @broth{1%l}.", false).await;
    support::create_recipe(&app, &alex, "Alex Stew", "Cook the @beans{2%cups}.", false).await;

    // Alex lets Sam work on the Stew. Forgejo holds the permission, so this
    // is a Forgejo action and not an application one.
    let status = forgejo_send(
        &forgejo,
        &admin,
        reqwest::Method::PUT,
        "/repos/alex/alex-stew/collaborators/sam",
        serde_json::json!({ "permission": "write" }),
    )
    .await;
    assert!(status.is_success(), "cannot share the Recipe: {status}");

    let mine = page(&app, "/", Some(&sam)).await;
    assert!(
        mine.contains("Sam Soup"),
        "Mine must hold the Recipes of Sam"
    );
    assert!(
        !mine.contains("Alex Stew"),
        "Mine must hold nobody else's Recipes"
    );
    assert!(
        mine.contains("Shared with me"),
        "both lists must be offered"
    );

    let shared = page(&app, "/?area=shared", Some(&sam)).await;
    assert!(
        shared.contains("Alex Stew"),
        "Shared with me must hold what somebody else shared"
    );
    assert!(
        !shared.contains("Sam Soup"),
        "Shared with me must not repeat the Recipes of Sam"
    );
}

#[tokio::test]
async fn explore_shows_the_public_recipes_to_a_visitor_with_no_account() {
    let Ready {
        forgejo,
        app,
        sam,
        admin: _admin,
        sam_token: _sam_token,
    } = ready().await;

    support::create_recipe(&app, &sam, "Public Pie", "Bake the @apples{4}.", false).await;
    support::create_recipe(&app, &sam, "Secret Sauce", "Mix the @cream{1%cup}.", true).await;

    // Nobody is signed in, and no account exists for this visitor.
    let anonymous = page(&app, "/explore", None).await;
    assert!(
        anonymous.contains("Public Pie"),
        "Explore must work without an account"
    );
    assert!(
        !anonymous.contains("Secret Sauce"),
        "a private Recipe must never reach a visitor"
    );

    // Another person who is signed in gets the same public catalog.
    let alex = support::sign_in(&app, &forgejo, "alex").await;
    let other = page(&app, "/explore", Some(&alex)).await;
    assert!(other.contains("Public Pie"));
    assert!(
        !other.contains("Secret Sauce"),
        "signing in must not open a private Recipe of somebody else"
    );

    // The owner sees their own private Recipe in their own area, and still
    // not in the public catalog.
    let mine = page(&app, "/", Some(&sam)).await;
    assert!(mine.contains("Secret Sauce"));
    assert!(
        !page(&app, "/explore", Some(&sam))
            .await
            .contains("Secret Sauce")
    );
}

#[tokio::test]
async fn explore_can_be_ordered_by_recent_or_by_title() {
    let Ready {
        forgejo: _forgejo,
        app,
        sam,
        admin: _admin,
        sam_token: _sam_token,
    } = ready().await;

    support::create_recipe(&app, &sam, "Zucchini Bake", "Bake the @zucchini{2}.", false).await;
    next_second().await;
    support::create_recipe(&app, &sam, "Apple Cake", "Bake the @apples{4}.", false).await;
    next_second().await;
    support::create_recipe(&app, &sam, "Mango Rice", "Cook the @rice{1%cup}.", false).await;

    let recent = page(&app, "/explore?sort=recent", None).await;
    assert!(
        position(&recent, "Mango Rice") < position(&recent, "Apple Cake"),
        "the newest Recipe must come first"
    );
    assert!(position(&recent, "Apple Cake") < position(&recent, "Zucchini Bake"));

    let alphabetical = page(&app, "/explore?sort=title", None).await;
    assert!(
        position(&alphabetical, "Apple Cake") < position(&alphabetical, "Mango Rice"),
        "A comes before M"
    );
    assert!(position(&alphabetical, "Mango Rice") < position(&alphabetical, "Zucchini Bake"));
}

#[tokio::test]
async fn search_finds_a_recipe_by_the_title_that_a_person_sees() {
    let Ready {
        forgejo: _forgejo,
        app,
        sam,
        admin: _admin,
        sam_token: _sam_token,
    } = ready().await;

    // Forgejo knows this Recipe as `chili-sin-carne`. A person knows it as
    // `Chili sin Carne`, and the words with a space in them exist only in
    // the title. Finding it therefore proves that the search reads the
    // title inside recipe.cook and not the repository name.
    support::create_recipe(
        &app,
        &sam,
        "Chili sin Carne",
        "Cook the @beans{2%cups} for ~{30%minutes}.",
        false,
    )
    .await;
    support::create_recipe(&app, &sam, "Apple Cake", "Bake the @apples{4}.", false).await;

    let found = page(&app, "/explore?q=sin%20Carne", None).await;
    assert!(found.contains("Chili sin Carne"), "the title must be found");
    assert!(
        !found.contains("Apple Cake"),
        "a search must leave out what it did not match"
    );

    // The search is not sensitive to case.
    assert!(
        page(&app, "/explore?q=CHILI", None)
            .await
            .contains("Chili sin Carne")
    );

    // A search that matches nothing says so instead of showing everything.
    let nothing = page(&app, "/explore?q=zzzz", None).await;
    assert!(!nothing.contains("Chili sin Carne"));
    assert!(!nothing.contains("Apple Cake"));
    assert!(nothing.contains("No Recipe title contains these words"));

    // The same search works in the Recipes area of a signed-in person.
    let mine = page(&app, "/?q=sin%20Carne", Some(&sam)).await;
    assert!(mine.contains("Chili sin Carne"));
    assert!(!mine.contains("Apple Cake"));
}

#[tokio::test]
async fn a_recipe_card_carries_culinary_information_and_no_git_words() {
    let Ready {
        forgejo: _forgejo,
        app,
        sam,
        admin: _admin,
        sam_token: _sam_token,
    } = ready().await;

    support::create_recipe(
        &app,
        &sam,
        "Green Goddess Salad",
        "---\nservings: 4\ntags: [vegan, quick]\n---\n\nChop the @cabbage{1} and the @onion{2}.",
        false,
    )
    .await;

    let body = page(&app, "/explore", None).await;

    assert!(body.contains("Green Goddess Salad"));
    assert!(body.contains("4 servings"), "a card says who it feeds");
    // CookCLI writes the tag plainly on a card and keeps the `#` for the
    // Recipe page, so the card asserts the word and not the mark.
    assert!(body.contains(">vegan<"), "a card carries the Cooklang tags");
    assert!(body.contains(">quick<"));
    assert!(body.contains("2 ingredients"), "a card says what it needs");
    assert!(body.contains("Owned by sam"));

    // Git holds the Recipe, and a cook never has to know that.
    for word in [
        "commit",
        "branch",
        "repository",
        "pull request",
        "fork",
        "clone",
    ] {
        assert!(
            !body.to_lowercase().contains(word),
            "a card must not say `{word}`"
        );
    }
}

#[tokio::test]
async fn forgejo_holds_one_authenticated_system_webhook() {
    let Ready {
        forgejo,
        app,
        sam: _sam,
        admin,
        sam_token: _sam_token,
    } = ready().await;

    let first = registered_hook_id(&app).await;

    // Running the command again must not add a second webhook.
    app.bootstrap(&admin).await;
    app.bootstrap(&admin).await;

    assert_eq!(
        registered_hook_id(&app).await,
        first,
        "a repeated bootstrap must keep the same webhook"
    );

    let hook = support::forgejo_api(&forgejo, &admin, &format!("/admin/hooks/{first}")).await;

    assert_eq!(
        hook["config"]["url"].as_str().unwrap_or_default(),
        app.webhook_url(),
        "the webhook must point at this application"
    );
    assert_eq!(hook["active"], true, "the webhook must be switched on");

    let events: Vec<String> = hook["events"]
        .as_array()
        .expect("a webhook must name its events")
        .iter()
        .map(|event| event.as_str().unwrap_or_default().to_string())
        .collect();
    assert!(
        events.contains(&"repository".to_string()),
        "Forgejo must report a repository change: {events:?}"
    );
    assert!(
        events.contains(&"push".to_string()),
        "Forgejo must report a Version: {events:?}"
    );

    // Nothing else was made. Forgejo counts every webhook with one series
    // of identifiers, and no other webhook exists in this installation.
    let none = reqwest::Client::new()
        .get(format!(
            "{}/api/v1/admin/hooks/{}",
            forgejo.base_url,
            first + 1
        ))
        .header("Authorization", format!("token {}", admin.expose()))
        .send()
        .await
        .expect("cannot reach the Forgejo API");
    assert_eq!(
        none.status(),
        404,
        "a second bootstrap must not have made a second webhook"
    );
}

/// The webhook that the application recorded when it registered.
///
/// Forgejo 15 answers `GET /api/v1/admin/hooks` with an empty list even
/// after it made one, so the identifier comes from the application.
async fn registered_hook_id(app: &TestApp) -> i64 {
    let row: (i64,) = sqlx::query_as("SELECT forgejo_hook_id FROM webhook WHERE id = 1")
        .fetch_one(&app.pool)
        .await
        .expect("the application recorded no webhook");
    row.0
}

#[tokio::test]
async fn the_webhook_refuses_a_body_that_it_did_not_sign() {
    let Ready {
        forgejo,
        app,
        sam,
        admin: _admin,
        sam_token,
    } = ready().await;
    let token = &sam_token;

    support::create_recipe(
        &app,
        &sam,
        "Chili sin Carne",
        "Cook the @beans{2%cups}.",
        false,
    )
    .await;

    let repository = support::forgejo_api(&forgejo, token, "/repos/sam/chili-sin-carne").await;
    let id = repository["id"].as_i64().expect("a repository has an id");

    // Somebody changes the Recipe outside the application.
    write_recipe_outside(
        &forgejo,
        token,
        "sam/chili-sin-carne",
        "---\ntitle: Chili con Carne\n---\n\nCook the @beans{2%cups}.",
    )
    .await;

    let message = push_message("sam", "chili-sin-carne", id);

    // A message with no signature changes nothing.
    let response = support::client()
        .post(app.webhook_url())
        .header("content-type", "application/json")
        .header("x-forgejo-event", "push")
        .body(message.clone())
        .send()
        .await
        .expect("cannot post the message");
    assert_eq!(
        response.status(),
        401,
        "an unsigned message must be refused"
    );

    // A signature from another secret changes nothing either.
    let wrong = cooklanghub::webhook::sign("another-secret", message.as_bytes());
    let response = app.deliver_signed_webhook("push", &message, &wrong).await;
    assert_eq!(response.status(), 401, "a wrong signature must be refused");

    let held = cooklanghub::index::get(&app.pool, "sam", "chili-sin-carne")
        .await
        .unwrap()
        .expect("the Recipe must be in the index");
    assert_eq!(
        held.title, "Chili sin Carne",
        "a refused message must change nothing"
    );

    // The same message with the right signature is acted on.
    let response = app.deliver_webhook("push", &message).await;
    assert_eq!(response.status(), 202);

    let held = cooklanghub::index::get(&app.pool, "sam", "chili-sin-carne")
        .await
        .unwrap()
        .expect("the Recipe must still be in the index");
    assert_eq!(
        held.title, "Chili con Carne",
        "a signed message must bring the index up to date"
    );
}

#[tokio::test]
async fn the_application_rebuilds_the_whole_index_after_it_is_deleted() {
    let Ready {
        forgejo,
        app,
        sam,
        admin: _admin,
        sam_token: _sam_token,
    } = ready().await;

    support::create_recipe(&app, &sam, "Public Pie", "Bake the @apples{4}.", false).await;
    support::create_recipe(&app, &sam, "Secret Sauce", "Mix the @cream{1%cup}.", true).await;

    assert_eq!(cooklanghub::index::count(&app.pool).await.unwrap(), 2);

    // An administrator deletes the index. Nothing of the Recipes is lost,
    // because Forgejo and Git hold them.
    sqlx::query("DELETE FROM recipe_index")
        .execute(&app.pool)
        .await
        .unwrap();
    assert_eq!(cooklanghub::index::count(&app.pool).await.unwrap(), 0);

    let report = app.reconcile().await;
    assert!(report.written >= 2, "the sweep must write every Recipe");

    let held = cooklanghub::index::all(&app.pool).await.unwrap();
    let titles: Vec<&str> = held.iter().map(|entry| entry.title.as_str()).collect();
    assert!(titles.contains(&"Public Pie"), "got {titles:?}");
    assert!(
        titles.contains(&"Secret Sauce"),
        "a private Recipe belongs to its owner and must come back too: {titles:?}"
    );

    // An administrator can ask for the same thing through the application.
    let alex = support::sign_in(&app, &forgejo, "alex").await;
    let rebuilt = support::client()
        .post(app.url("/admin/index/rebuild"))
        .header("cookie", format!("{COOKIE_NAME}={alex}"))
        .send()
        .await
        .expect("cannot ask for a rebuild");
    assert_eq!(rebuilt.status(), 200);
    assert!(
        rebuilt
            .text()
            .await
            .unwrap_or_default()
            .contains("complete again"),
        "the page must report what happened"
    );

    // Somebody who does not administer the installation cannot.
    let refused = support::client()
        .post(app.url("/admin/index/rebuild"))
        .header("cookie", format!("{COOKIE_NAME}={sam}"))
        .send()
        .await
        .expect("cannot ask for a rebuild");
    assert_eq!(refused.status(), 403);
}

#[tokio::test]
async fn the_reconciliation_repairs_what_a_missed_message_left_behind() {
    let Ready {
        forgejo,
        app,
        sam,
        admin: _admin,
        sam_token,
    } = ready().await;
    let token = &sam_token;

    support::create_recipe(
        &app,
        &sam,
        "Chili sin Carne",
        "Cook the @beans{2%cups}.",
        false,
    )
    .await;

    // Forgejo cannot reach this test server, so every message it sends is
    // lost. That is the outage this test needs.
    write_recipe_outside(
        &forgejo,
        token,
        "sam/chili-sin-carne",
        "---\ntitle: Chili con Carne\nservings: 6\n---\n\nCook the @beans{2%cups} and the @rice{1%cup}.",
    )
    .await;

    let held = cooklanghub::index::get(&app.pool, "sam", "chili-sin-carne")
        .await
        .unwrap()
        .expect("the Recipe must be in the index");
    assert_eq!(
        held.title, "Chili sin Carne",
        "the application must not have learned about the change yet"
    );

    app.reconcile().await;

    let held = cooklanghub::index::get(&app.pool, "sam", "chili-sin-carne")
        .await
        .unwrap()
        .expect("the Recipe must still be in the index");
    assert_eq!(held.title, "Chili con Carne", "the index must be repaired");
    assert_eq!(held.servings.as_deref(), Some("6"));
    assert_eq!(held.ingredients, 2);
}

#[tokio::test]
async fn the_topics_decide_what_the_application_shows() {
    let Ready {
        forgejo,
        app,
        sam,
        admin: _admin,
        sam_token,
    } = ready().await;
    let token = &sam_token;

    support::create_recipe(
        &app,
        &sam,
        "Chili sin Carne",
        "Cook the @beans{2%cups}.",
        false,
    )
    .await;

    // A repository that holds a Recipe file but carries no topic is not a
    // Recipe. The application never guesses from the files.
    let status = forgejo_send(
        &forgejo,
        token,
        reqwest::Method::POST,
        "/user/repos",
        serde_json::json!({ "name": "not-a-recipe", "private": false, "auto_init": true }),
    )
    .await;
    assert!(status.is_success(), "cannot make the repository: {status}");

    let status = forgejo_send(
        &forgejo,
        token,
        reqwest::Method::POST,
        "/repos/sam/not-a-recipe/contents/recipe.cook",
        serde_json::json!({
            "content": BASE64.encode("---\ntitle: Hidden Cake\n---\n\nBake the @apples{4}."),
            "message": "Add a Recipe file to a repository that opted out",
        }),
    )
    .await;
    assert!(status.is_success(), "cannot write the file: {status}");

    let body = page(&app, "/explore", None).await;
    assert!(body.contains("Chili sin Carne"));
    assert!(
        !body.contains("Hidden Cake"),
        "a repository without the topics must stay out"
    );

    // One topic is not enough. Both are the marker.
    set_topics(&forgejo, token, "sam/chili-sin-carne", &["cooklang"]).await;
    let body = page(&app, "/explore", None).await;
    assert!(
        !body.contains("Chili sin Carne"),
        "one topic must not be enough"
    );

    // An administrator takes the topics away in Forgejo, and the Recipe
    // leaves the application.
    set_topics(&forgejo, token, "sam/chili-sin-carne", &[]).await;

    let body = page(&app, "/", Some(&sam)).await;
    assert!(
        !body.contains("Chili sin Carne"),
        "a Recipe without its topics must leave every list"
    );

    app.reconcile().await;
    assert!(
        cooklanghub::index::get(&app.pool, "sam", "chili-sin-carne")
            .await
            .unwrap()
            .is_none(),
        "the reconciliation must take the row out of the index"
    );

    // Putting the topics back brings the Recipe back.
    set_topics(
        &forgejo,
        token,
        "sam/chili-sin-carne",
        &["cooklang", "recipe"],
    )
    .await;
    let body = page(&app, "/", Some(&sam)).await;
    assert!(body.contains("Chili sin Carne"));
}

#[tokio::test]
async fn the_reconciliation_changes_nothing_in_forgejo_and_nothing_in_git() {
    let Ready {
        forgejo,
        app,
        sam,
        admin: _admin,
        sam_token,
    } = ready().await;
    let token = &sam_token;

    support::create_recipe(
        &app,
        &sam,
        "Chili sin Carne",
        "Cook the @beans{2%cups}.",
        false,
    )
    .await;
    support::create_recipe(&app, &sam, "Secret Sauce", "Mix the @cream{1%cup}.", true).await;

    let before = snapshot(&forgejo, token).await;

    app.reconcile().await;
    // A second run must be as harmless as the first.
    app.reconcile().await;

    let after = snapshot(&forgejo, token).await;

    assert_eq!(
        before, after,
        "the reconciliation must read Forgejo and Git, and write to neither"
    );
}

/// Everything about the Recipes of Sam that a write would change.
async fn snapshot(forgejo: &Forgejo, token: &Secret<String>) -> Vec<String> {
    let mut out = Vec::new();

    for slug in ["chili-sin-carne", "secret-sauce"] {
        let repository = support::forgejo_api(forgejo, token, &format!("/repos/sam/{slug}")).await;
        let commits =
            support::forgejo_api(forgejo, token, &format!("/repos/sam/{slug}/commits")).await;
        let topics =
            support::forgejo_api(forgejo, token, &format!("/repos/sam/{slug}/topics")).await;

        out.push(format!(
            "{slug} updated={} private={} empty={} versions={} head={} topics={}",
            repository["updated_at"],
            repository["private"],
            repository["empty"],
            commits.as_array().map(Vec::len).unwrap_or_default(),
            commits[0]["sha"],
            topics["topics"],
        ));
    }

    out
}

/// A Recipe that two sweeps both name is counted once.
///
/// A reconciliation sweeps what every user can read, and then what each
/// signed-in person can read. A public Recipe of a signed-in person is in
/// both answers. The report counted it twice, so the Diagnostics page told
/// an administrator that Forgejo held about twice the Recipes it held.
#[tokio::test]
async fn a_reconciliation_counts_each_recipe_once() {
    let Ready {
        forgejo: _forgejo,
        app,
        sam,
        admin: _admin,
        sam_token: _sam_token,
    } = ready().await;

    // Two public and one private, all of them sam's. The public sweep names
    // the two public ones, and the sweep of sam names all three.
    support::create_recipe(&app, &sam, "Public Pie", "Bake the @apples{4}.", false).await;
    support::create_recipe(&app, &sam, "Open Oats", "Soak the @oats{100%g}.", false).await;
    support::create_recipe(&app, &sam, "Secret Sauce", "Mix the @cream{1%cup}.", true).await;

    let report = app.reconcile().await;

    assert_eq!(
        report.scanned, 3,
        "Forgejo holds three Recipes, and the report must say three: {report:?}"
    );
    assert_eq!(
        report.written, 3,
        "each Recipe is written once, not once for each sweep: {report:?}"
    );
    assert_eq!(
        i64::try_from(report.scanned).unwrap(),
        cooklanghub::index::count(&app.pool).await.unwrap(),
        "the number the sweep names and the number the index holds must agree"
    );
}
