//! Acceptance tests for History, Changes, and Restore.
//!
//! Git holds History, so every test asks Forgejo what Git actually holds and
//! not only what the application drew. A page that showed a Version Forgejo
//! does not have would mean a second store, which this product must not
//! have.
//!
//! The cases that hide a fault are the ones about History itself: a restore
//! that adds a Version and removes none, work outside the published branch
//! that must never reach the list, and an anonymous person who may read a
//! public History and no private one.

mod support;

use std::collections::HashSet;

use base64::Engine;
use cooklanghub::secret::Secret;
use cooklanghub::session::COOKIE_NAME;
use reqwest::Method;
use serde_json::json;

/// The first Version of the Recipe under test.
const FIRST: &str = "Chop the @onion{1}.

Fry it in a #pan{}.

Add @salt{1%g} and @pepper{1%g}.
";

/// The second Version. Every kind of difference is in here: an amount that
/// changed, a thing that went, a piece of cookware that was exchanged, and
/// one step more than before.
const SECOND: &str = "Chop the @onion{1}.

Fry it in a #pot{}.

Add @salt{5%g}.

Serve it.
";

struct World {
    forgejo: support::Forgejo,
    app: support::TestApp,
    /// The session of `sam`, who owns every Recipe in these tests.
    session: String,
    /// The credential that the test itself asks Forgejo questions with.
    token: Secret<String>,
}

async fn ready() -> World {
    let forgejo = support::start_forgejo().await;
    forgejo.create_user("alex", true);
    forgejo.create_user("sam", false);
    forgejo.create_user("kim", false);
    let admin = forgejo.access_token("alex");

    let app = support::start_app(&forgejo.base_url).await;
    app.bootstrap(&admin).await;

    let session = support::sign_in(&app, &forgejo, "sam").await;
    let token = forgejo.access_token("sam");

    World {
        forgejo,
        app,
        session,
        token,
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

/// Post a form, with a session cookie or without one.
async fn post(world: &World, session: Option<&str>, path: &str) -> reqwest::Response {
    let mut request = support::client().post(world.app.url(path));
    if let Some(session) = session {
        request = request.header("cookie", cookie(session));
    }
    // A restore is a form with no field in it, exactly as the page sends it.
    request
        .header("content-type", "application/x-www-form-urlencoded")
        .body("")
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

/// Make the Recipe that every test starts from.
async fn a_recipe(world: &World, title: &str, source: &str, private: bool) {
    let created = support::create_recipe(&world.app, &world.session, title, source, private).await;
    assert_eq!(created.status(), 303, "the Recipe was not created");
}

/// Publish one new Version through the editor, the way a person does.
async fn publish(world: &World, slug: &str, source: &str, note: &str) {
    let page = text(
        read(
            world,
            Some(&world.session),
            &format!("/recipes/sam/{slug}/edit"),
        )
        .await,
    )
    .await;
    let base = field(&page, "base_version");

    let published = support::client()
        .post(world.app.url(&format!("/recipes/sam/{slug}/edit")))
        .header("cookie", cookie(&world.session))
        .form(&[
            ("base_version", base.as_str()),
            ("source", source),
            ("note", note),
        ])
        .send()
        .await
        .expect("cannot post the editor form");

    let status = published.status();
    if status != 303 {
        panic!(
            "the Version was not published: {status} {}",
            text(published).await
        );
    }
}

/// One published Version, as Forgejo reports it.
struct Recorded {
    id: String,
    description: String,
    day: String,
}

/// Every published Version, newest first, straight out of Forgejo.
async fn recorded(world: &World, slug: &str) -> Vec<Recorded> {
    let commits = support::forgejo_api(
        &world.forgejo,
        &world.token,
        &format!("/repos/sam/{slug}/commits?sha=main&limit=50"),
    )
    .await;

    commits
        .as_array()
        .expect("the answer is a list")
        .iter()
        .map(|commit| Recorded {
            id: commit["sha"].as_str().unwrap_or_default().to_string(),
            description: commit["commit"]["message"]
                .as_str()
                .unwrap_or_default()
                .trim()
                .to_string(),
            day: commit["commit"]["author"]["date"]
                .as_str()
                .unwrap_or_default()
                .chars()
                .take(10)
                .collect(),
        })
        .collect()
}

/// The Recipe file of one Version, byte for byte, out of Forgejo.
async fn stored(world: &World, slug: &str, reference: &str) -> String {
    let (status, bytes) = support::forgejo_raw(
        &world.forgejo,
        &world.token,
        &format!("/sam/{slug}/raw/recipe.cook?ref={reference}"),
    )
    .await;

    assert!(status.is_success(), "Forgejo answered {status}");
    String::from_utf8(bytes).expect("the stored file must be UTF-8")
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
///
/// The comparison is held to this most strictly: it is an acceptance
/// criterion that it carries none of them.
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
        "patch",
        "head",
        "sha",
        "merge",
        "rebase",
        "git",
        "checkout",
        "revert",
    ] {
        assert!(
            !spoken.contains(forge_word),
            "the page says `{forge_word}` to a cook"
        );
    }
}

#[tokio::test]
async fn history_lists_published_versions_only_with_author_date_and_description() {
    let world = ready().await;
    a_recipe(&world, "Chili", FIRST, false).await;
    publish(&world, "chili", SECOND, "Less salt").await;

    // Work that lives outside the published branch is not a Version, so it
    // must never reach History.
    let aside = support::forgejo_write(
        &world.forgejo,
        &world.token,
        Method::POST,
        "/repos/sam/chili/contents/notes.md",
        json!({
            "content": base64::engine::general_purpose::STANDARD.encode("A note.\n"),
            "message": "Work in progress",
            "branch": "main",
            "new_branch": "later",
        }),
    )
    .await;
    assert!(
        aside.status().is_success(),
        "the test could not put work outside the published Recipe: {}",
        aside.status()
    );

    let versions = recorded(&world, "chili").await;
    assert_eq!(versions.len(), 2, "two Versions are published");

    let response = read(&world, Some(&world.session), "/recipes/sam/chili/history").await;
    assert_eq!(response.status(), 200);
    let page = text(response).await;

    // Every Version, with the description a person wrote.
    assert!(page.contains("Less salt"), "the newest Version is missing");
    assert!(page.contains("Add Chili"), "the first Version is missing");

    // The author and the date of each Version.
    for version in &versions {
        assert!(
            page.contains(&format!("By sam on {}", version.day)),
            "History must name the author and the date of every Version"
        );
        assert!(
            page.contains(&format!("/recipes/sam/chili/history/{}", version.id)),
            "every Version must be readable on its own"
        );
    }

    // Published Versions only.
    assert!(
        !page.contains("Work in progress"),
        "History must hold published Versions only"
    );
    assert!(page.contains("Published now"));

    // The Recipe page leads here, and the page says no word of the forge.
    let recipe = text(read(&world, Some(&world.session), "/recipes/sam/chili").await).await;
    assert!(recipe.contains("href=\"/recipes/sam/chili/history\""));
    assert_cooking_words(&page);
}

#[tokio::test]
async fn an_older_version_can_be_read_and_two_versions_compared_as_changes() {
    let world = ready().await;
    a_recipe(&world, "Chili", FIRST, false).await;
    publish(&world, "chili", SECOND, "Less salt").await;

    let versions = recorded(&world, "chili").await;
    let (later, earlier) = (&versions[0], &versions[1]);

    // The older Version reads as a Recipe, and it holds what it held then.
    let response = read(
        &world,
        Some(&world.session),
        &format!("/recipes/sam/chili/history/{}", earlier.id),
    )
    .await;
    assert_eq!(response.status(), 200);
    let older = text(response).await;

    assert!(
        older.contains("Ingredients"),
        "the Version reads as a Recipe"
    );
    assert!(
        older.contains("pepper"),
        "the older Version still holds what was taken out later"
    );
    assert!(
        older.contains("pan"),
        "the older Version holds its cookware"
    );
    assert!(
        !older.contains("Serve it."),
        "the older Version must not hold a later step"
    );
    assert!(
        older.contains("Add Chili"),
        "the Version names its description"
    );
    assert_cooking_words(&older);

    // The comparison of the two.
    let response = read(
        &world,
        Some(&world.session),
        &format!(
            "/recipes/sam/chili/changes?from={}&to={}",
            earlier.id, later.id
        ),
    )
    .await;
    assert_eq!(response.status(), 200);
    let changes = text(response).await;

    assert!(
        changes.contains("Changes"),
        "the comparison is named Changes"
    );
    assert!(changes.contains("Ingredients"));
    assert!(changes.contains("Cookware"));
    assert!(changes.contains("Steps"));

    // What a cook needs to see: the amount that changed, the thing that
    // went, the cookware that was exchanged, and the step that is new.
    assert!(changes.contains("Added"));
    assert!(changes.contains("Removed"));
    assert!(changes.contains("Changed"));
    assert!(changes.contains("pepper"), "the ingredient that went");
    assert!(changes.contains("pot"), "the cookware that arrived");
    assert!(changes.contains("pan"), "the cookware that went");
    assert!(changes.contains("1 g"), "the amount as it was");
    assert!(changes.contains("5 g"), "the amount as it is now");
    assert!(changes.contains("Serve it."), "the step that is new");

    // The acceptance criterion: no word of the forge, anywhere.
    assert_cooking_words(&changes);
}

#[tokio::test]
async fn a_restore_adds_a_version_and_removes_none() {
    let world = ready().await;
    a_recipe(&world, "Chili", FIRST, false).await;
    publish(&world, "chili", SECOND, "Less salt").await;

    let before = recorded(&world, "chili").await;
    assert_eq!(before.len(), 2);
    let first = &before[1];
    let held_then = stored(&world, "chili", &first.id).await;

    // A person who may only read cannot add a Version.
    let reader = support::sign_in(&world.app, &world.forgejo, "kim").await;
    let refused = post(
        &world,
        Some(&reader),
        &format!("/recipes/sam/chili/history/{}/restore", first.id),
    )
    .await;
    assert_eq!(refused.status(), 403, "a Reader cannot restore");
    assert_eq!(
        recorded(&world, "chili").await.len(),
        2,
        "a refused restore must add nothing"
    );

    // The owner restores the first Version.
    let restored = post(
        &world,
        Some(&world.session),
        &format!("/recipes/sam/chili/history/{}/restore", first.id),
    )
    .await;
    assert_eq!(restored.status(), 303);
    assert_eq!(location(&restored), "/recipes/sam/chili/history");

    let after = recorded(&world, "chili").await;
    assert_eq!(after.len(), 3, "a restore adds exactly one Version");

    // Every earlier Version is still there. History is never rewritten.
    let kept: HashSet<&str> = after.iter().map(|version| version.id.as_str()).collect();
    for version in &before {
        assert!(
            kept.contains(version.id.as_str()),
            "an earlier Version disappeared from History"
        );
    }

    // The new Version holds the old content.
    assert_eq!(
        stored(&world, "chili", "main").await,
        held_then,
        "the new Version must hold the Recipe of the old Version"
    );
    assert!(
        after[0].description.starts_with("Restore the Version of"),
        "the new Version says what it is: {}",
        after[0].description
    );

    // The Recipe page shows the restored content, and History shows all
    // three Versions.
    let recipe = text(read(&world, Some(&world.session), "/recipes/sam/chili").await).await;
    assert!(recipe.contains("pepper"), "the Recipe is the restored one");

    let page = text(read(&world, Some(&world.session), "/recipes/sam/chili/history").await).await;
    assert!(page.contains("Add Chili"));
    assert!(page.contains("Less salt"));
    assert!(page.contains("Restore the Version of"));
    assert_cooking_words(&page);

    // Restoring the Version that is published changes nothing, and leaves
    // no empty Version behind.
    let again = post(
        &world,
        Some(&world.session),
        &format!("/recipes/sam/chili/history/{}/restore", after[0].id),
    )
    .await;
    assert_eq!(again.status(), 200, "the person stays on the page");
    let said = text(again).await;
    assert!(said.contains("CookLangHub made no new Version."));
    assert_eq!(
        recorded(&world, "chili").await.len(),
        3,
        "a restore that changes nothing adds no Version"
    );
}

#[tokio::test]
async fn an_anonymous_person_reads_a_public_history_and_no_private_one() {
    let world = ready().await;
    a_recipe(&world, "Chili", FIRST, false).await;
    publish(&world, "chili", SECOND, "Less salt").await;
    a_recipe(&world, "Secret Stew", FIRST, true).await;

    let versions = recorded(&world, "chili").await;
    let (later, earlier) = (&versions[0], &versions[1]);

    // A public Recipe: History, one Version, and the comparison.
    let response = read(&world, None, "/recipes/sam/chili/history").await;
    assert_eq!(response.status(), 200, "public History is public");
    let page = text(response).await;
    assert!(page.contains("Add Chili"));
    assert!(page.contains("Less salt"));
    assert_cooking_words(&page);

    let response = read(
        &world,
        None,
        &format!("/recipes/sam/chili/history/{}", earlier.id),
    )
    .await;
    assert_eq!(response.status(), 200);
    let older = text(response).await;
    assert!(older.contains("pepper"));
    assert!(
        !older.contains("Restore this Version"),
        "a person who is not signed in cannot restore"
    );

    let response = read(
        &world,
        None,
        &format!(
            "/recipes/sam/chili/changes?from={}&to={}",
            earlier.id, later.id
        ),
    )
    .await;
    assert_eq!(response.status(), 200);
    assert_cooking_words(&text(response).await);

    // A private Recipe: Forgejo says no, and so does every page here.
    for path in [
        "/recipes/sam/secret-stew/history",
        &format!("/recipes/sam/secret-stew/history/{}", earlier.id),
        &format!(
            "/recipes/sam/secret-stew/changes?from={}&to={}",
            earlier.id, later.id
        ),
    ] {
        let response = read(&world, None, path).await;
        assert_eq!(
            response.status(),
            404,
            "a private History must not answer an anonymous person: {path}"
        );
    }

    // The owner still reads it.
    let response = read(
        &world,
        Some(&world.session),
        "/recipes/sam/secret-stew/history",
    )
    .await;
    assert_eq!(response.status(), 200);
    assert!(text(response).await.contains("Add Secret Stew"));
}
