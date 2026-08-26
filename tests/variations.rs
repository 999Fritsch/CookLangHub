//! Acceptance tests for Variations.
//!
//! A Variation is a Forgejo fork, so every test asks Forgejo what it really
//! holds and never only what the application drew. A page that showed a
//! Variation which Forgejo does not record would mean a second store, and
//! this product must not have one.
//!
//! The cases that hide a fault are the ones about the source Recipe: it must
//! never change when somebody makes a Variation of it, a person must not be
//! able to copy a Recipe that Forgejo hides from them, and a Variation must
//! stay a whole Recipe when its source is not available any more.

mod support;

use std::collections::HashSet;

use cooklanghub::secret::Secret;
use cooklanghub::session::COOKIE_NAME;
use reqwest::Method;
use serde_json::json;

/// The first Version of the Recipe under test.
const FIRST: &str = "Chop the @onion{1}.

Fry it in a #pan{}.
";

/// The second Version, for the tests about an earlier Version.
const SECOND: &str = "Chop the @onion{1}.

Fry it in a #pot{}.

Serve it.
";

struct World {
    forgejo: support::Forgejo,
    app: support::TestApp,
    /// The session of `sam`, who owns the source Recipe in these tests.
    sam: String,
    /// The session of `kim`, who makes the Variations.
    kim: String,
    /// The session of `lee`, a third cook.
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

/// Post the **Create variation** form, with a session cookie or without one.
async fn make_variation(
    world: &World,
    session: Option<&str>,
    owner: &str,
    slug: &str,
    version: Option<&str>,
) -> reqwest::Response {
    let mut request = support::client().post(
        world
            .app
            .url(&format!("/recipes/{owner}/{slug}/variations")),
    );
    if let Some(session) = session {
        request = request.header("cookie", cookie(session));
    }

    let body = match version {
        Some(version) => format!("version={version}"),
        None => String::new(),
    };

    request
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body)
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

/// The Recipe file as the application stores it.
///
/// The title a cook gave lives in the Cooklang metadata of the file, so the
/// stored bytes hold more than what the person typed.
fn with_title(title: &str, source: &str) -> String {
    format!("---\ntitle: {title}\n---\n\n{source}")
}

/// Make a Recipe, the way a person does.
async fn a_recipe(world: &World, session: &str, title: &str, source: &str, private: bool) {
    let created = support::create_recipe(&world.app, session, title, source, private).await;
    assert_eq!(created.status(), 303, "the Recipe was not created");
}

/// Publish one new Version through the editor, the way a person does.
async fn publish(world: &World, session: &str, owner: &str, slug: &str, source: &str, note: &str) {
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
            ("source", source),
            ("note", note),
        ])
        .send()
        .await
        .expect("cannot post the editor form");

    assert_eq!(published.status(), 303, "the Version was not published");
}

/// Ask Forgejo about a repository. This is the authority, not the page.
async fn repository(world: &World, full_name: &str) -> serde_json::Value {
    support::forgejo_api(&world.forgejo, &world.token, &format!("/repos/{full_name}")).await
}

/// Whether Forgejo has a repository at all, as the administrator sees it.
async fn exists(world: &World, full_name: &str) -> bool {
    let response = support::forgejo_write(
        &world.forgejo,
        &world.token,
        Method::GET,
        &format!("/repos/{full_name}"),
        json!({}),
    )
    .await;
    response.status().is_success()
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
        "rebase",
        "git",
        "checkout",
        "upstream",
    ] {
        assert!(
            !spoken.contains(forge_word),
            "the page says `{forge_word}` to a cook"
        );
    }
}

#[tokio::test]
async fn create_variation_makes_a_forgejo_fork_that_is_a_recipe_of_its_own() {
    let world = ready().await;
    a_recipe(&world, &world.sam, "Chili", FIRST, false).await;

    let before = versions(&world, "sam/chili").await;
    let source_file = stored(&world, "sam/chili", "main").await;

    let made = make_variation(&world, Some(&world.kim), "sam", "chili", None).await;
    assert_eq!(made.status(), 303, "the Variation was not made");
    assert_eq!(location(&made), "/recipes/kim/chili");

    // Forgejo is the authority. It has to hold a real fork of the Recipe.
    let variation = repository(&world, "kim/chili").await;
    assert_eq!(variation["fork"], json!(true), "this is not a Forgejo fork");
    assert_eq!(variation["parent"]["full_name"], json!("sam/chili"));
    assert_eq!(variation["owner"]["login"], json!("kim"));
    assert_eq!(variation["private"], json!(false));

    // A Variation is a Recipe: it carries the topics, so it appears in every
    // Recipe list.
    let topics: Vec<String> = variation["topics"]
        .as_array()
        .expect("the answer is a list")
        .iter()
        .map(|topic| topic.as_str().unwrap_or_default().to_string())
        .collect();
    assert!(
        topics.contains(&"cooklang".to_string()),
        "topics: {topics:?}"
    );
    assert!(topics.contains(&"recipe".to_string()), "topics: {topics:?}");

    // The exact content and the exact title.
    assert_eq!(stored(&world, "kim/chili", "main").await, source_file);
    let page = text(read(&world, Some(&world.kim), "/recipes/kim/chili").await).await;
    assert!(page.contains("Chili"), "the Variation lost the title");

    // The shared History, and no Version of its own.
    let copied = versions(&world, "kim/chili").await;
    assert_eq!(copied, before, "the Variation must hold the same Versions");

    // No fork point tag and no lineage file.
    let tags = support::forgejo_api(&world.forgejo, &world.token, "/repos/kim/chili/tags").await;
    assert_eq!(
        tags.as_array().map(Vec::len),
        Some(0),
        "the application must add no tag"
    );
    assert_eq!(files(&world, "kim/chili").await, vec!["recipe.cook"]);

    // The source Recipe is untouched.
    assert_eq!(versions(&world, "sam/chili").await, before);
    assert_eq!(stored(&world, "sam/chili", "main").await, source_file);
    let source = repository(&world, "sam/chili").await;
    assert_eq!(source["private"], json!(false));
    assert_eq!(source["fork"], json!(false));

    // The Variation is in the Recipes of kim, which is the list the index
    // feeds.
    let mine = text(read(&world, Some(&world.kim), "/").await).await;
    assert!(
        mine.contains("/recipes/kim/chili"),
        "the Variation is not in the Recipes of kim"
    );

    // Both are public Recipes, so Explore holds both and neither hides the
    // other.
    let explore = text(read(&world, None, "/explore").await).await;
    assert!(explore.contains("/recipes/kim/chili"));
    assert!(explore.contains("/recipes/sam/chili"));
}

#[tokio::test]
async fn a_variation_of_an_earlier_version_starts_at_that_version() {
    let world = ready().await;
    a_recipe(&world, &world.sam, "Chili", FIRST, false).await;
    publish(
        &world,
        &world.sam,
        "sam",
        "chili",
        &with_title("Chili", SECOND),
        "Serve it",
    )
    .await;

    let published = versions(&world, "sam/chili").await;
    assert_eq!(published.len(), 2, "the Recipe needs two Versions");
    let earlier = published[1].clone();

    // This is the way a person reaches it: they read an earlier Version and
    // the page offers a Variation of what they read.
    let version_page = text(
        read(
            &world,
            Some(&world.kim),
            &format!("/recipes/sam/chili/history/{earlier}"),
        )
        .await,
    )
    .await;
    assert!(
        version_page.contains(&format!("/recipes/sam/chili/variations?from={earlier}")),
        "the Version page offers no Variation of the Version being read"
    );

    let offer = text(
        read(
            &world,
            Some(&world.kim),
            &format!("/recipes/sam/chili/variations?from={earlier}"),
        )
        .await,
    )
    .await;
    assert!(
        offer.contains("This Variation starts at the Version of"),
        "the page does not say which Version the Variation starts at"
    );
    assert_cooking_words(&offer);

    let made = make_variation(&world, Some(&world.kim), "sam", "chili", Some(&earlier)).await;
    assert_eq!(made.status(), 303, "the Variation was not made");

    // The Variation starts exactly there, and holds exactly that Recipe.
    let copied = versions(&world, "kim/chili").await;
    assert_eq!(
        copied,
        vec![earlier.clone()],
        "the Variation starts elsewhere"
    );
    assert_eq!(
        stored(&world, "kim/chili", "main").await,
        with_title("Chili", FIRST)
    );

    // The source Recipe kept both Versions and the newer content.
    assert_eq!(versions(&world, "sam/chili").await, published);
    assert_eq!(
        stored(&world, "sam/chili", "main").await,
        with_title("Chili", SECOND)
    );

    // A Version that this Recipe does not hold changes nothing.
    let refused = make_variation(
        &world,
        Some(&world.lee),
        "sam",
        "chili",
        Some("0123456789abcdef0123456789abcdef01234567"),
    )
    .await;
    assert_eq!(refused.status(), 400);
    assert!(
        !exists(&world, "lee/chili").await,
        "a Variation was made anyway"
    );
}

#[tokio::test]
async fn a_name_that_is_already_used_is_resolved_without_a_user_action() {
    let world = ready().await;
    a_recipe(&world, &world.sam, "Chili", FIRST, false).await;
    a_recipe(&world, &world.kim, "Chili", SECOND, false).await;

    let made = make_variation(&world, Some(&world.kim), "sam", "chili", None).await;
    assert_eq!(made.status(), 303, "the Variation was not made");
    assert_eq!(
        location(&made),
        "/recipes/kim/chili-2",
        "the application did not step aside from the name that kim uses"
    );

    let variation = repository(&world, "kim/chili-2").await;
    assert_eq!(variation["fork"], json!(true));
    assert_eq!(variation["parent"]["full_name"], json!("sam/chili"));

    // The Recipe that kim already had is untouched.
    assert_eq!(
        stored(&world, "kim/chili", "main").await,
        with_title("Chili", SECOND)
    );
    let own = repository(&world, "kim/chili").await;
    assert_eq!(own["fork"], json!(false));
}

#[tokio::test]
async fn a_variation_of_a_variation_works_and_each_page_names_its_source() {
    let world = ready().await;
    a_recipe(&world, &world.sam, "Chili", FIRST, false).await;

    let first = make_variation(&world, Some(&world.kim), "sam", "chili", None).await;
    assert_eq!(first.status(), 303);

    let second = make_variation(&world, Some(&world.lee), "kim", "chili", None).await;
    assert_eq!(
        second.status(),
        303,
        "a Variation of a Variation was refused"
    );
    assert_eq!(location(&second), "/recipes/lee/chili");

    let deeper = repository(&world, "lee/chili").await;
    assert_eq!(deeper["fork"], json!(true));
    assert_eq!(
        deeper["parent"]["full_name"],
        json!("kim/chili"),
        "the source of the second Variation is the first Variation"
    );

    // The page of the second Variation names the Recipe it came from.
    let page = text(read(&world, Some(&world.lee), "/recipes/lee/chili/variations").await).await;
    assert!(page.contains("This Recipe is a Variation of"));
    assert!(
        page.contains("/recipes/kim/chili"),
        "the page does not link to the source Recipe"
    );
    assert!(page.contains("which kim owns"));
    assert_cooking_words(&page);

    // The page of the source lists what was made from it.
    let listed = text(read(&world, Some(&world.sam), "/recipes/sam/chili/variations").await).await;
    assert!(
        listed.contains("/recipes/kim/chili"),
        "the Variations list does not hold the Variation of kim"
    );
    assert!(listed.contains("Owned by kim"));
    assert_cooking_words(&listed);
}

#[tokio::test]
async fn the_variations_area_reads_as_a_recipe_area_and_needs_no_account() {
    let world = ready().await;
    a_recipe(&world, &world.sam, "Chili", FIRST, false).await;

    // A public Recipe is readable without an account, so the area is too.
    let anonymous = read(&world, None, "/recipes/sam/chili/variations").await;
    assert_eq!(anonymous.status(), 200);
    let page = text(anonymous).await;

    assert!(page.contains("Variations of this Recipe"));
    assert!(page.contains("Nobody has made a Variation of this Recipe yet."));
    assert!(page.contains("Open in Forgejo"));
    assert!(
        page.contains("/auth/sign-in"),
        "a visitor must be told how to make a Variation"
    );
    assert!(
        !page.contains("Create variation"),
        "a visitor cannot make a Variation"
    );
    assert!(
        page.contains("aria-current=\"page\""),
        "the area is not marked as the one in use"
    );
    assert_cooking_words(&page);

    // The Recipe page offers the area.
    let recipe = text(read(&world, None, "/recipes/sam/chili").await).await;
    assert!(
        recipe.contains("/recipes/sam/chili/variations"),
        "the Recipe page has no Variations area"
    );

    // A person who signs in gets the control.
    let signed_in =
        text(read(&world, Some(&world.kim), "/recipes/sam/chili/variations").await).await;
    assert!(signed_in.contains("Create variation"));
    assert_cooking_words(&signed_in);

    // One Variation for one person of one Recipe. The second attempt says so
    // and makes nothing.
    assert_eq!(
        make_variation(&world, Some(&world.kim), "sam", "chili", None)
            .await
            .status(),
        303
    );
    let again = make_variation(&world, Some(&world.kim), "sam", "chili", None).await;
    assert_eq!(again.status(), 409);
    let refusal = text(again).await;
    assert!(refusal.contains("You have a Variation of this Recipe already."));
    assert_cooking_words(&refusal);
    assert!(
        !exists(&world, "kim/chili-2").await,
        "a second Variation was made"
    );

    // The list holds what was made, for anybody who may read it.
    let listed = text(read(&world, None, "/recipes/sam/chili/variations").await).await;
    assert!(listed.contains("/recipes/kim/chili"));
    assert!(listed.contains("Owned by kim"));

    // A copy that somebody makes in Forgejo itself carries no Recipe topics.
    // The application must not draw it as a Recipe, and must not hide it
    // either: it says how many there are and offers Forgejo.
    let outside = support::forgejo_write(
        &world.forgejo,
        &world.forgejo.access_token("lee"),
        Method::POST,
        "/repos/sam/chili/forks",
        json!({ "name": "chili" }),
    )
    .await;
    assert!(
        outside.status().is_success(),
        "the test could not make a copy in Forgejo: {}",
        outside.status()
    );

    let with_a_stranger = text(read(&world, None, "/recipes/sam/chili/variations").await).await;
    assert!(
        with_a_stranger.contains("Forgejo holds one more copy of this Recipe."),
        "the page hides a state that it cannot draw"
    );
    assert!(
        !with_a_stranger.contains("/recipes/lee/chili"),
        "a copy that is not a Recipe was drawn as one"
    );
    assert_cooking_words(&with_a_stranger);
}

#[tokio::test]
async fn a_variation_inherits_the_visibility_of_the_recipe_it_comes_from() {
    let world = ready().await;
    a_recipe(&world, &world.sam, "Secret", FIRST, true).await;

    // sam shares the private Recipe with kim as a Reader.
    let shared = support::post_fields(
        &world.app,
        &world.sam,
        "/recipes/sam/secret/sharing/people",
        &[("login", "kim"), ("role", "reader")],
    )
    .await;
    assert_eq!(shared.status(), 303, "the Recipe was not shared");

    let made = make_variation(&world, Some(&world.kim), "sam", "secret", None).await;
    assert_eq!(made.status(), 303, "a Reader cannot make a Variation");

    let variation = repository(&world, "kim/secret").await;
    assert_eq!(
        variation["private"],
        json!(true),
        "the Variation must inherit the visibility of its source"
    );

    // The Variation is as private as its source: a person who may read
    // neither reaches neither.
    for path in [
        "/recipes/sam/secret/variations",
        "/recipes/kim/secret/variations",
    ] {
        assert_eq!(
            read(&world, Some(&world.lee), path).await.status(),
            404,
            "`{path}` answered somebody who may not read it"
        );
    }

    // Forgejo hides a private Variation from the Owner of the source too,
    // so the list of sam holds nothing. Forgejo makes that decision on
    // every request, and this application asks it again each time.
    let owner_sees =
        text(read(&world, Some(&world.sam), "/recipes/sam/secret/variations").await).await;
    assert!(
        !owner_sees.contains("/recipes/kim/secret"),
        "a private Variation reached somebody Forgejo hides it from"
    );
    assert!(owner_sees.contains("Nobody has made a Variation of this Recipe yet."));

    let reader_sees =
        text(read(&world, Some(&world.kim), "/recipes/sam/secret/variations").await).await;
    assert!(
        reader_sees.contains("/recipes/kim/secret"),
        "the person who made the Variation must see it"
    );

    // Visibility stays a Forgejo decision about one repository. Forgejo
    // keeps a Variation as private as the Recipe it came from while it
    // records that relationship, and this application does not work around
    // it: it asks, and it shows what Forgejo did.
    let asked = support::post_fields(
        &world.app,
        &world.kim,
        "/recipes/kim/secret/sharing/visibility",
        &[("visibility", "public"), ("confirm", "yes")],
    )
    .await;
    assert_eq!(asked.status(), 303);

    assert_eq!(
        repository(&world, "sam/secret").await["private"],
        json!(true),
        "the source Recipe must keep its own visibility"
    );
    assert_eq!(
        stored(&world, "sam/secret", "main").await,
        with_title("Secret", FIRST),
        "the source Recipe must not change"
    );
}

#[tokio::test]
async fn a_person_cannot_make_a_variation_of_a_recipe_they_cannot_see() {
    let world = ready().await;
    a_recipe(&world, &world.sam, "Secret", FIRST, true).await;

    // The area of a Recipe that Forgejo hides is not there at all.
    let area = read(&world, Some(&world.kim), "/recipes/sam/secret/variations").await;
    assert_eq!(area.status(), 404);

    let refused = make_variation(&world, Some(&world.kim), "sam", "secret", None).await;
    assert_eq!(refused.status(), 404, "Forgejo permissions were not obeyed");
    assert!(
        !exists(&world, "kim/secret").await,
        "a Variation of a hidden Recipe was made"
    );

    // A visitor with no account is sent to sign in, and nothing is made.
    let anonymous = make_variation(&world, None, "sam", "secret", None).await;
    assert_eq!(anonymous.status(), 303);
    assert_eq!(location(&anonymous), "/auth/sign-in");

    // The source Recipe is untouched.
    assert_eq!(
        repository(&world, "sam/secret").await["private"],
        json!(true)
    );
    assert_eq!(
        stored(&world, "sam/secret", "main").await,
        with_title("Secret", FIRST)
    );
}

#[tokio::test]
async fn a_variation_stays_usable_when_the_source_recipe_disappears() {
    let world = ready().await;
    a_recipe(&world, &world.sam, "Chili", FIRST, false).await;

    let made = make_variation(&world, Some(&world.kim), "sam", "chili", None).await;
    assert_eq!(made.status(), 303);

    // The source goes out of reach: sam makes his Recipe private.
    let hidden = support::post_fields(
        &world.app,
        &world.sam,
        "/recipes/sam/chili/sharing/visibility",
        &[("visibility", "private")],
    )
    .await;
    assert_eq!(hidden.status(), 303, "the Recipe did not become private");

    let page = text(read(&world, Some(&world.kim), "/recipes/kim/chili/variations").await).await;
    assert!(
        page.contains("the source Recipe is not available"),
        "the lineage does not report the state"
    );
    assert!(
        !page.contains("/recipes/sam/chili"),
        "the page must not link to a Recipe that this person cannot read"
    );
    assert_cooking_words(&page);

    // The Variation itself is whole.
    let recipe = read(&world, Some(&world.kim), "/recipes/kim/chili").await;
    assert_eq!(recipe.status(), 200);
    assert!(text(recipe).await.contains("Chili"));
    assert_eq!(
        stored(&world, "kim/chili", "main").await,
        with_title("Chili", FIRST)
    );

    // The source Recipe is deleted. Forgejo then records no source at all,
    // and the Variation is an ordinary Recipe that still holds everything.
    let removed = support::forgejo_write(
        &world.forgejo,
        &world.token,
        Method::DELETE,
        "/repos/sam/chili",
        json!({}),
    )
    .await;
    assert!(removed.status().is_success(), "the source was not deleted");

    let after = read(&world, Some(&world.kim), "/recipes/kim/chili/variations").await;
    assert_eq!(after.status(), 200, "the Variation must stay usable");
    let after = text(after).await;
    assert!(
        !after.contains("/recipes/sam/chili"),
        "the page must not link to a Recipe that is gone"
    );
    assert_cooking_words(&after);

    let recipe = read(&world, Some(&world.kim), "/recipes/kim/chili").await;
    assert_eq!(recipe.status(), 200);
    assert_eq!(
        stored(&world, "kim/chili", "main").await,
        with_title("Chili", FIRST)
    );
    assert_eq!(versions(&world, "kim/chili").await.len(), 1);

    // Forgejo records no source any more, so the visibility of the Variation
    // is now a decision about this Recipe and nothing else.
    let own = support::post_fields(
        &world.app,
        &world.kim,
        "/recipes/kim/chili/sharing/visibility",
        &[("visibility", "private")],
    )
    .await;
    assert_eq!(own.status(), 303);
    assert_eq!(
        repository(&world, "kim/chili").await["private"],
        json!(true),
        "the Variation must decide its own visibility"
    );
}
