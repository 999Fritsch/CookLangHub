//! Acceptance tests for **Update from original**.
//!
//! A Variation is a Forgejo fork and Git holds both Histories, so every test
//! here asks Forgejo what the two Recipes really hold. A page that said a
//! Recipe had changed while Forgejo held something else would mean the
//! application had started to keep a lineage of its own, and this product
//! must not have one.
//!
//! The cases that hide a fault are the ones where nothing may move: the
//! source Recipe must be exactly as it was after any update at all, a
//! Variation must be exactly as it was when Git cannot join the two sides,
//! and an update with nothing to bring must leave no Version behind for a
//! person to read.

mod support;

use std::collections::HashSet;

use cooklanghub::secret::Secret;
use cooklanghub::session::COOKIE_NAME;
use reqwest::Method;
use serde_json::json;

/// The Recipe that every test starts from.
///
/// It has room in it on purpose. Two people who change steps far apart make
/// a change that Git joins by itself, and two people who change the same
/// step make one that Git cannot join.
const FIRST: &str = "Chop the @onion{1}.

Fry it in a #pan{}.

Add @salt{1%pinch}.

Cook it for ~{10%minutes}.

Serve it.
";

/// What the owner of the source Recipe writes next. The first step only.
const SOURCE_CHANGED: &str = "Chop the @onion{2}.

Fry it in a #pan{}.

Add @salt{1%pinch}.

Cook it for ~{10%minutes}.

Serve it.
";

/// What the owner of the Variation writes. The last step only, so Git can
/// put the two changes together.
const VARIATION_CHANGED: &str = "Chop the @onion{1}.

Fry it in a #pan{}.

Add @salt{1%pinch}.

Cook it for ~{10%minutes}.

Serve it with @bread{2}.
";

/// What the owner of the Variation writes on the step the source Recipe
/// also changed. Git cannot decide between the two.
const VARIATION_SAME_STEP: &str = "Chop the @shallot{3}.

Fry it in a #pan{}.

Add @salt{1%pinch}.

Cook it for ~{10%minutes}.

Serve it.
";

struct World {
    forgejo: support::Forgejo,
    app: support::TestApp,
    /// The session of `sam`, who owns the source Recipe.
    sam: String,
    /// The session of `kim`, who owns the Variation.
    kim: String,
    /// The session of `lee`, who can read the Variation and nothing more.
    lee: String,
    /// The credential that the test itself asks Forgejo questions with.
    token: Secret<String>,
}

async fn ready() -> World {
    let forgejo = support::start_forgejo().await;
    forgejo.create_user("alex", true);
    forgejo.create_user("sam", false);
    forgejo.create_user("kim", false);
    forgejo.create_user("lee", false);
    let admin = forgejo.access_token("alex");

    let app = support::start_app(&forgejo.base_url).await;
    app.bootstrap(&admin).await;

    let sam = support::sign_in(&app, &forgejo, "sam").await;
    let kim = support::sign_in(&app, &forgejo, "kim").await;
    let lee = support::sign_in(&app, &forgejo, "lee").await;

    World {
        forgejo,
        app,
        sam,
        kim,
        lee,
        token: admin,
    }
}

fn cookie(session: &str) -> String {
    format!("{COOKIE_NAME}={session}")
}

/// Read a page, with a session cookie or without one.
async fn read(world: &World, session: Option<&str>, path: &str) -> reqwest::Response {
    let mut request = support::client().get(world.app.url(path));
    if let Some(session) = session {
        request = request.header("cookie", cookie(session));
    }
    request.send().await.expect("cannot reach the page")
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

/// Read the value of a hidden field out of a page.
fn field(html: &str, name: &str) -> String {
    let marker = format!("name=\"{name}\"");
    let start = html
        .find(&marker)
        .unwrap_or_else(|| panic!("the page has no `{name}` field"));
    let rest = &html[start..];
    let value_at = rest.find("value=\"").expect("the field carries no value") + "value=\"".len();
    let end = rest[value_at..].find('"').expect("the value never ends") + value_at;
    rest[value_at..end].to_string()
}

/// Read the address that one link on a page points at.
///
/// The template escapes an address the way HTML needs it, so the escape for
/// `&` comes back here. A browser does the same before it asks for the page.
fn link_after(html: &str, label: &str) -> String {
    let at = html
        .find(label)
        .unwrap_or_else(|| panic!("the page has no `{label}` link"));
    let before = &html[..at];
    let opened = before.rfind("<a ").expect("the label sits in no link");
    let href_at = before[opened..]
        .find("href=\"")
        .expect("the link has no address")
        + opened
        + "href=\"".len();
    let end = before[href_at..].find('"').expect("the address never ends") + href_at;
    before[href_at..end]
        .replace("&amp;", "&")
        .replace("&#38;", "&")
}

/// Make a Recipe, the way a person does.
async fn a_recipe(world: &World, session: &str, title: &str, source: &str) {
    let created = support::create_recipe(&world.app, session, title, source, false).await;
    assert_eq!(created.status(), 303, "the Recipe was not created");
}

/// The Recipe file as the application stores it.
///
/// The title a cook gave lives in the Cooklang metadata of the file, and the
/// editor writes back the whole file. A test that dropped the metadata would
/// make every Version rewrite the top of the file, and then two changes far
/// apart would still meet there.
fn whole(source: &str) -> String {
    format!("---\ntitle: Chili\n---\n\n{source}")
}

/// Publish one new Version through the editor, the way a person does.
async fn publish(world: &World, session: &str, owner: &str, slug: &str, source: &str, note: &str) {
    let source = whole(source);
    let page = text(
        read(
            world,
            Some(session),
            &format!("/recipes/{owner}/{slug}/edit"),
        )
        .await,
    )
    .await;
    let base = field(&page, "base_version");

    let published = support::client()
        .post(world.app.url(&format!("/recipes/{owner}/{slug}/edit")))
        .header("cookie", cookie(session))
        .form(&[
            ("base_version", base.as_str()),
            ("source", source.as_str()),
            ("note", note),
        ])
        .send()
        .await
        .expect("cannot post the editor form");

    assert_eq!(published.status(), 303, "the Version was not published");
}

/// Make a Variation of a Recipe, the way a person does.
async fn make_variation(world: &World, session: &str, owner: &str, slug: &str) -> String {
    let made = support::client()
        .post(
            world
                .app
                .url(&format!("/recipes/{owner}/{slug}/variations")),
        )
        .header("cookie", cookie(session))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(String::new())
        .send()
        .await
        .expect("cannot post the form");

    assert_eq!(made.status(), 303, "the Variation was not made");
    location(&made)
}

/// Press **Update from original**, with a session cookie or without one.
async fn update(
    world: &World,
    session: Option<&str>,
    owner: &str,
    slug: &str,
) -> reqwest::Response {
    let mut request = support::client().post(
        world
            .app
            .url(&format!("/recipes/{owner}/{slug}/variations/update")),
    );
    if let Some(session) = session {
        request = request.header("cookie", cookie(session));
    }

    request
        .header("content-type", "application/x-www-form-urlencoded")
        .body(String::new())
        .send()
        .await
        .expect("cannot post the form")
}

/// Ask Forgejo about a repository. This is the authority, not the page.
async fn repository(world: &World, full_name: &str) -> serde_json::Value {
    support::forgejo_api(&world.forgejo, &world.token, &format!("/repos/{full_name}")).await
}

/// The published Versions of a Recipe, newest first, straight out of Forgejo.
async fn versions(world: &World, full_name: &str) -> Vec<String> {
    let commits = support::forgejo_api(
        &world.forgejo,
        &world.token,
        &format!("/repos/{full_name}/commits?sha=main&limit=50"),
    )
    .await;

    commits
        .as_array()
        .expect("the answer is a list")
        .iter()
        .map(|commit| commit["sha"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// What the newest published Version of a Recipe says about itself.
async fn newest_description(world: &World, full_name: &str) -> String {
    let commits = support::forgejo_api(
        &world.forgejo,
        &world.token,
        &format!("/repos/{full_name}/commits?sha=main&limit=1"),
    )
    .await;

    commits[0]["commit"]["message"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// The Recipe file of one Version, byte for byte, out of Forgejo.
async fn stored(world: &World, full_name: &str, reference: &str) -> String {
    let (status, bytes) = support::forgejo_raw(
        &world.forgejo,
        &world.token,
        &format!("/{full_name}/raw/recipe.cook?ref={reference}"),
    )
    .await;

    assert!(status.is_success(), "Forgejo answered {status}");
    String::from_utf8(bytes).expect("the stored file must be UTF-8")
}

/// The names of the files at the top of a Recipe.
async fn files(world: &World, full_name: &str) -> Vec<String> {
    let entries = support::forgejo_api(
        &world.forgejo,
        &world.token,
        &format!("/repos/{full_name}/contents?ref=main"),
    )
    .await;

    entries
        .as_array()
        .expect("the answer is a list")
        .iter()
        .map(|entry| entry["name"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// Everything Forgejo and Git hold for one Recipe right now.
///
/// A test that says "this Recipe did not change" compares this before and
/// after, which is stronger than reading the page that the application drew.
async fn state(world: &World, full_name: &str) -> (Vec<String>, String) {
    (
        versions(world, full_name).await,
        stored(world, full_name, "main").await,
    )
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

    for phrase in ["pull request", "merge request"] {
        assert!(
            !words.contains(phrase),
            "the page says `{phrase}` to a cook"
        );
    }

    // Whole words only. `Sharing` is an area of a Recipe and must not be
    // read as the identifier that Git uses.
    let spoken: HashSet<&str> = words
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();

    for forge_word in [
        "commit",
        "commits",
        "branch",
        "branches",
        "diff",
        "repository",
        "repo",
        "fork",
        "forks",
        "patch",
        "head",
        "sha",
        "merge",
        "merged",
        "merging",
        "rebase",
        "git",
        "checkout",
        "upstream",
        "pull",
    ] {
        assert!(
            !spoken.contains(forge_word),
            "the page says `{forge_word}` to a cook"
        );
    }
}

/// The Variations page of a Recipe, as one person reads it.
async fn variations_page(world: &World, session: Option<&str>, owner: &str, slug: &str) -> String {
    let page = read(
        world,
        session,
        &format!("/recipes/{owner}/{slug}/variations"),
    )
    .await;
    assert!(page.status().is_success(), "the page did not answer");
    let html = text(page).await;
    assert_cooking_words(&html);
    html
}

// ---------------------------------------------------------------------

#[tokio::test]
async fn the_variation_page_counts_the_newer_versions_of_the_source_recipe() {
    let world = ready().await;
    a_recipe(&world, &world.sam, "Chili", FIRST).await;
    make_variation(&world, &world.kim, "sam", "chili").await;

    // Nothing has moved yet, so there is nothing to bring.
    let quiet = variations_page(&world, Some(&world.kim), "kim", "chili").await;
    assert!(
        quiet.contains("This Recipe holds every Version of the source Recipe"),
        "the page must say that the two Recipes hold the same Versions"
    );
    assert!(
        quiet.contains("Update from original"),
        "the owner of a Variation must be offered the action"
    );

    let before = state(&world, "kim/chili").await;

    // The source Recipe moves on twice.
    publish(
        &world,
        &world.sam,
        "sam",
        "chili",
        SOURCE_CHANGED,
        "Use two onions",
    )
    .await;
    publish(
        &world,
        &world.sam,
        "sam",
        "chili",
        VARIATION_CHANGED,
        "Serve it with bread",
    )
    .await;

    let page = variations_page(&world, Some(&world.kim), "kim", "chili").await;
    assert!(
        page.contains("The source Recipe has 2 newer Versions"),
        "the page must count what the source Recipe holds: {page}"
    );
    assert!(
        page.contains("only when you ask for it"),
        "the page must say that nothing is applied by itself"
    );

    // Nothing was applied. This is the whole of the rule, and the page is
    // not the authority for it: Forgejo is.
    assert_eq!(
        state(&world, "kim/chili").await,
        before,
        "the Variation must not change until a person asks for it"
    );

    // The page offers the comparison that History already draws, and it
    // draws it out of the two Versions that the source Recipe really holds.
    let href = link_after(&page, "See what is different");
    assert!(
        href.starts_with("/recipes/sam/chili/changes?"),
        "the comparison must be the one History draws: {href}"
    );

    let changes = read(&world, Some(&world.kim), &href).await;
    assert!(
        changes.status().is_success(),
        "the comparison did not answer: {} for {href}",
        changes.status()
    );
    let drawn = text(changes).await;
    assert_cooking_words(&drawn);
    assert!(
        drawn.contains("Serve it with bread"),
        "the comparison must show what the source Recipe did: {drawn}"
    );
}

#[tokio::test]
async fn update_from_original_brings_a_clean_change_and_makes_a_version() {
    let world = ready().await;
    a_recipe(&world, &world.sam, "Chili", FIRST).await;
    make_variation(&world, &world.kim, "sam", "chili").await;

    // Each side changes a step of its own, which Git can put together.
    publish(
        &world,
        &world.sam,
        "sam",
        "chili",
        SOURCE_CHANGED,
        "Use two onions",
    )
    .await;
    publish(
        &world,
        &world.kim,
        "kim",
        "chili",
        VARIATION_CHANGED,
        "Serve it with bread",
    )
    .await;

    let source_before = state(&world, "sam/chili").await;
    let ours_before = versions(&world, "kim/chili").await;

    let done = update(&world, Some(&world.kim), "kim", "chili").await;
    assert_eq!(done.status(), 303, "the update did not happen");
    assert_eq!(
        location(&done),
        "/recipes/kim/chili/history",
        "the person must be sent to the Version that the update made"
    );

    // Git joined the two sides, so the Variation holds both changes.
    let joined = stored(&world, "kim/chili", "main").await;
    assert!(
        joined.contains("@onion{2}"),
        "the change of the source Recipe did not arrive: {joined}"
    );
    assert!(
        joined.contains("@bread{2}"),
        "the change of the Variation was lost: {joined}"
    );

    // The update is a Version of its own in the History of the Variation.
    let ours_after = versions(&world, "kim/chili").await;
    assert!(
        ours_after.len() > ours_before.len(),
        "the update must leave a Version in History"
    );
    assert_eq!(
        newest_description(&world, "kim/chili").await,
        "Update from the source Recipe",
        "the newest Version must say what it is"
    );

    // The source Recipe is exactly as it was. Nothing was written to it.
    assert_eq!(
        state(&world, "sam/chili").await,
        source_before,
        "an update must never change the source Recipe"
    );

    // Forgejo still holds the relationship, and the Recipe holds no record
    // of its own. The application keeps no lineage.
    let variation = repository(&world, "kim/chili").await;
    assert_eq!(variation["fork"], json!(true));
    assert_eq!(variation["parent"]["full_name"], json!("sam/chili"));
    assert_eq!(
        files(&world, "kim/chili").await,
        vec!["recipe.cook".to_string()],
        "an update must write no lineage file into the Recipe"
    );

    // The same question answers itself now, with nothing stored.
    let page = variations_page(&world, Some(&world.kim), "kim", "chili").await;
    assert!(
        page.contains("This Recipe holds every Version of the source Recipe"),
        "the page must see that the update happened: {page}"
    );

    // History says it too, in words a cook reads.
    let history = read(&world, Some(&world.kim), "/recipes/kim/chili/history").await;
    let history = text(history).await;
    assert_cooking_words(&history);
    assert!(
        history.contains("Update from the source Recipe"),
        "History must show the Version that the update made"
    );

    // A second update has nothing left to bring, so it makes no Version.
    let after = versions(&world, "kim/chili").await;
    let again = update(&world, Some(&world.kim), "kim", "chili").await;
    assert_eq!(again.status(), 200);
    assert_eq!(
        versions(&world, "kim/chili").await,
        after,
        "a second update must leave no empty Version behind"
    );
}

#[tokio::test]
async fn an_update_that_has_nothing_to_bring_makes_no_version() {
    let world = ready().await;
    a_recipe(&world, &world.sam, "Chili", FIRST).await;
    make_variation(&world, &world.kim, "sam", "chili").await;

    // The Variation goes on ahead. It still holds every Version of the
    // source Recipe, so an update has nothing at all to bring.
    publish(
        &world,
        &world.kim,
        "kim",
        "chili",
        VARIATION_CHANGED,
        "Serve it with bread",
    )
    .await;

    let before = state(&world, "kim/chili").await;

    let answered = update(&world, Some(&world.kim), "kim", "chili").await;
    assert_eq!(answered.status(), 200);
    let page = text(answered).await;
    assert_cooking_words(&page);
    assert!(
        page.contains("CookLangHub made no new Version"),
        "the page must say that nothing was made: {page}"
    );

    assert_eq!(
        state(&world, "kim/chili").await,
        before,
        "an update with nothing to bring must leave no Version behind"
    );
}

#[tokio::test]
async fn a_conflict_leaves_both_recipes_exactly_as_they_were() {
    let world = ready().await;
    a_recipe(&world, &world.sam, "Chili", FIRST).await;
    make_variation(&world, &world.kim, "sam", "chili").await;

    // Both people change the same step, and they write different things.
    // Git cannot decide between them, and this application must not either.
    publish(
        &world,
        &world.sam,
        "sam",
        "chili",
        SOURCE_CHANGED,
        "Use two onions",
    )
    .await;
    publish(
        &world,
        &world.kim,
        "kim",
        "chili",
        VARIATION_SAME_STEP,
        "Use shallots",
    )
    .await;

    let source_before = state(&world, "sam/chili").await;
    let ours_before = state(&world, "kim/chili").await;

    let refused = update(&world, Some(&world.kim), "kim", "chili").await;
    assert_eq!(refused.status(), 409, "a conflict is not a success");

    let page = text(refused).await;
    assert_cooking_words(&page);
    assert!(
        page.contains("cannot join the changes of the source Recipe"),
        "the person must read what happened: {page}"
    );
    assert!(
        page.contains("This Recipe did not change, and the source Recipe did not change"),
        "the diagnosis must say that nothing moved: {page}"
    );
    assert!(
        page.contains("Open in Forgejo"),
        "a state this interface cannot handle must offer Forgejo"
    );

    // The two things that matter, and neither of them is on the page.
    assert_eq!(
        state(&world, "kim/chili").await,
        ours_before,
        "a conflict must leave the published Variation exactly as it was"
    );
    assert_eq!(
        state(&world, "sam/chili").await,
        source_before,
        "a conflict must leave the source Recipe exactly as it was"
    );
}

#[tokio::test]
async fn only_a_person_who_can_change_a_variation_can_update_it() {
    let world = ready().await;
    a_recipe(&world, &world.sam, "Chili", FIRST).await;
    make_variation(&world, &world.kim, "sam", "chili").await;
    publish(
        &world,
        &world.sam,
        "sam",
        "chili",
        SOURCE_CHANGED,
        "Use two onions",
    )
    .await;

    let before = state(&world, "kim/chili").await;

    // A cook who only reads the Variation is told what the source Recipe
    // holds, and is offered no action at all.
    let seen = variations_page(&world, Some(&world.lee), "kim", "chili").await;
    assert!(
        seen.contains("The source Recipe has one newer Version"),
        "a reader must still see that the source Recipe moved on: {seen}"
    );
    assert!(
        !seen.contains("Update from original"),
        "a person who cannot change the Recipe must not be offered the action"
    );

    // The button is not the guard. Forgejo is.
    let refused = update(&world, Some(&world.lee), "kim", "chili").await;
    assert_eq!(refused.status(), 403);
    let page = text(refused).await;
    assert_cooking_words(&page);
    assert!(page.contains("you cannot change it"), "{page}");

    // A visitor with no session is asked to sign in and changes nothing.
    let anonymous = update(&world, None, "kim", "chili").await;
    assert_eq!(anonymous.status(), 303);
    assert_eq!(location(&anonymous), "/auth/sign-in");

    let reading = variations_page(&world, None, "kim", "chili").await;
    assert!(
        !reading.contains("Update from original"),
        "a visitor with no session must not be offered the action"
    );

    assert_eq!(
        state(&world, "kim/chili").await,
        before,
        "a refused update must change nothing"
    );
}

#[tokio::test]
async fn a_recipe_with_no_source_recipe_offers_no_update() {
    let world = ready().await;
    a_recipe(&world, &world.sam, "Chili", FIRST).await;
    make_variation(&world, &world.kim, "sam", "chili").await;

    // A Recipe that came from nowhere has nothing to update from.
    let own = variations_page(&world, Some(&world.sam), "sam", "chili").await;
    assert!(
        !own.contains("Changes in the source Recipe"),
        "a Recipe with no source Recipe must show no such card"
    );

    let refused = update(&world, Some(&world.sam), "sam", "chili").await;
    assert_eq!(refused.status(), 400);
    let page = text(refused).await;
    assert_cooking_words(&page);
    assert!(page.contains("This Recipe is not a Variation"), "{page}");

    // Forgejo holds the relationship, and it stops holding it when the
    // source Recipe is deleted. The application must then say the same,
    // because it keeps no lineage of its own.
    let before = state(&world, "kim/chili").await;
    let deleted = support::forgejo_write(
        &world.forgejo,
        &world.token,
        Method::DELETE,
        "/repos/sam/chili",
        json!({}),
    )
    .await;
    assert!(deleted.status().is_success(), "the Recipe was not deleted");

    let orphan = update(&world, Some(&world.kim), "kim", "chili").await;
    assert!(
        !orphan.status().is_success(),
        "an update with no source Recipe must not happen"
    );
    assert_cooking_words(&text(orphan).await);

    assert_eq!(
        state(&world, "kim/chili").await,
        before,
        "a Variation must hold everything it held when its source Recipe goes"
    );
}
