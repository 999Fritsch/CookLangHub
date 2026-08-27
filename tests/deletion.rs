//! Acceptance tests for Archive and for permanent deletion.
//!
//! Every test asks Forgejo what actually happened and not only what the page
//! drew. That matters more here than anywhere else: a deletion cannot be
//! taken back, and a page that said "nothing will break" while Forgejo held
//! something else would be the worst fault this product can have.
//!
//! Three facts about Forgejo shape these tests. Each was measured against a
//! real Forgejo 15 and not read out of a document.
//!
//! 1. Forgejo keeps reporting `push` and `admin` on an archived repository,
//!    so no permission answer carries the archive. The refusal has to be its
//!    own check, and it has to sit over the POST.
//! 2. Forgejo answers 200 with an empty list when the Owner of a Recipe asks
//!    for its Variations and the only one is private and belongs to somebody
//!    else. It gives no sign that it left one out.
//! 3. Forgejo holds a Suggestion inside the Recipe, so it removes every
//!    Suggestion with the Recipe. A Suggestion that came from a copy is the
//!    other way round: when the copy goes, Forgejo closes it and keeps it.

mod support;

use std::collections::HashSet;

use cooklanghub::archive;
use cooklanghub::secret::Secret;
use cooklanghub::session::COOKIE_NAME;
use reqwest::Method;
use serde_json::{Value, json};

const DISH: &str = "---
title: Chili
---

Chop the @onion{1}.

Fry it in a #pan{} until it is soft.

Add @salt{1%g} and @pepper{1%g}.

Wait ~{20%minutes}.

Serve.
";

struct World {
    forgejo: support::Forgejo,
    app: support::TestApp,
    /// The session of `sam`, who owns every Recipe in these tests.
    owner: String,
    /// The session of `kim`, who can read but not write.
    reader: String,
    /// The credential that the test itself asks Forgejo questions with.
    token: Secret<String>,
    /// The credential of `kim`.
    reader_token: Secret<String>,
}

async fn ready() -> World {
    let forgejo = support::start_forgejo().await;
    forgejo.create_user("alex", true);
    forgejo.create_user("sam", false);
    forgejo.create_user("kim", false);
    let admin = forgejo.access_token("alex");

    let app = support::start_app(&forgejo.base_url).await;
    app.bootstrap(&admin).await;

    let owner = support::sign_in(&app, &forgejo, "sam").await;
    let reader = support::sign_in(&app, &forgejo, "kim").await;
    let token = forgejo.access_token("sam");
    let reader_token = forgejo.access_token("kim");

    World {
        forgejo,
        app,
        owner,
        reader,
        token,
        reader_token,
    }
}

fn cookie(session: &str) -> String {
    format!("{COOKIE_NAME}={session}")
}

/// Make a Recipe and give back its slug.
async fn a_recipe(world: &World, title: &str, private: bool) -> String {
    let response = support::create_recipe(&world.app, &world.owner, title, DISH, private).await;
    assert_eq!(response.status(), 303, "the Recipe must be created");

    response
        .headers()
        .get("location")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.rsplit('/').next())
        .expect("the redirect names the Recipe")
        .to_string()
}

/// Open a page, with a session cookie or without one.
async fn open(world: &World, session: Option<&str>, path: &str) -> (reqwest::StatusCode, String) {
    let mut request = support::client().get(world.app.url(path));
    if let Some(session) = session {
        request = request.header("cookie", cookie(session));
    }
    let response = request.send().await.expect("cannot reach the page");
    let status = response.status();
    let body = response.text().await.expect("cannot read the body");
    (status, body)
}

/// Post a form with no field in it, the way a stale page would.
async fn post_empty(world: &World, session: &str, path: &str) -> reqwest::Response {
    support::client()
        .post(world.app.url(path))
        .header("cookie", cookie(session))
        .header("content-type", "application/x-www-form-urlencoded")
        .body("")
        .send()
        .await
        .expect("cannot post the form")
}

/// What Forgejo holds for one repository, or nothing when it holds none.
async fn in_forgejo(world: &World, full_name: &str) -> Option<Value> {
    let response = reqwest::Client::new()
        .get(format!(
            "{}/api/v1/repos/{full_name}",
            world.forgejo.base_url
        ))
        .header("Authorization", format!("token {}", world.token.expose()))
        .send()
        .await
        .expect("cannot reach the Forgejo API");

    if response.status() == 404 {
        return None;
    }
    assert!(
        response.status().is_success(),
        "Forgejo answered {} for {full_name}",
        response.status()
    );
    Some(response.json().await.expect("the answer is not JSON"))
}

/// Whether Forgejo holds this repository as archived.
async fn archived(world: &World, full_name: &str) -> bool {
    in_forgejo(world, full_name)
        .await
        .and_then(|value| value["archived"].as_bool())
        .expect("Forgejo must report the archive state")
}

/// How many published Versions a Recipe has, as Forgejo counts them.
async fn versions(world: &World, full_name: &str) -> usize {
    support::forgejo_api(
        &world.forgejo,
        &world.token,
        &format!("/repos/{full_name}/commits?limit=50"),
    )
    .await
    .as_array()
    .map(|found| found.len())
    .unwrap_or_default()
}

/// Every Suggestion that Forgejo holds for a Recipe, in whatever state.
async fn suggestions(world: &World, full_name: &str) -> Vec<Value> {
    support::forgejo_api(
        &world.forgejo,
        &world.token,
        &format!("/repos/{full_name}/pulls?state=all"),
    )
    .await
    .as_array()
    .cloned()
    .unwrap_or_default()
}

/// Make a Suggestion on a Recipe as the reader, and give back its number.
async fn a_suggestion(world: &World, slug: &str) -> i64 {
    let (status, page) = open(
        world,
        Some(&world.reader),
        &format!("/recipes/sam/{slug}/suggest"),
    )
    .await;
    assert_eq!(status, 200, "the editor must open: {page:.400}");

    let base = field(&page, "base_version");
    let saved = support::post_fields(
        &world.app,
        &world.reader,
        &format!("/recipes/sam/{slug}/suggest"),
        &[
            ("source", &DISH.replace("@onion{1}", "@onion{2}")),
            ("base_version", &base),
            ("draft_version", ""),
        ],
    )
    .await;
    assert_eq!(saved.status(), 200, "the Suggestion must be saved");

    let held = suggestions(world, &format!("sam/{slug}")).await;
    assert_eq!(held.len(), 1, "Forgejo must hold exactly one Suggestion");
    held[0]["number"].as_i64().expect("it has a number")
}

/// Make a Variation of a Recipe as the reader, and give back its slug.
async fn a_variation(world: &World, slug: &str) -> String {
    let made = support::post_fields(
        &world.app,
        &world.reader,
        &format!("/recipes/sam/{slug}/variations"),
        &[("version", "")],
    )
    .await;
    assert_eq!(made.status(), 303, "the Variation must be made");

    made.headers()
        .get("location")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.rsplit('/').next())
        .expect("the redirect names the Variation")
        .to_string()
}

/// Make a Cookbook that holds one Recipe, and give back its slug.
async fn a_cookbook_holding(world: &World, title: &str, recipe: &str) -> String {
    let made = support::create_cookbook(&world.app, &world.owner, title, "Warm food.", false).await;
    assert_eq!(made.status(), 303, "the Cookbook must be created");

    let slug = made
        .headers()
        .get("location")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.rsplit('/').next())
        .expect("the redirect names the Cookbook")
        .to_string();

    let added = support::post_fields(
        &world.app,
        &world.owner,
        &format!("/cookbooks/sam/{slug}/recipes"),
        &[
            ("recipe", &format!("sam/{recipe}")),
            ("holding", "pinned"),
            ("confirm", "yes"),
        ],
    )
    .await;
    assert_eq!(added.status(), 303, "the Recipe must go in the Cookbook");

    slug
}

/// Read the value of a hidden field out of a page.
fn field(html: &str, name: &str) -> String {
    let marker = format!("name=\"{name}\"");
    let at = html
        .find(&marker)
        .unwrap_or_else(|| panic!("the page carries no `{name}` field"));
    let tag_start = html[..at].rfind('<').expect("the field sits in a tag");
    let tag_end = at + html[at..].find('>').expect("the tag ends");
    let tag = &html[tag_start..tag_end];

    let value_at = tag.find("value=\"").expect("the field has a value") + "value=\"".len();
    let value_end = tag[value_at..].find('"').expect("the value ends") + value_at;
    tag[value_at..value_end].to_string()
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

/// Put a Recipe or a Cookbook in the archive through the application.
async fn archive_it(world: &World, path: &str, on: bool) -> reqwest::Response {
    support::post_fields(
        &world.app,
        &world.owner,
        &format!("{path}/archive/state"),
        &[("archived", if on { "yes" } else { "no" })],
    )
    .await
}

// ---------------------------------------------------------------- Archive

#[tokio::test]
async fn archive_is_the_forgejo_setting_and_the_owner_can_take_it_back() {
    let world = ready().await;
    let slug = a_recipe(&world, "Chili", false).await;

    assert!(
        !archived(&world, &format!("sam/{slug}")).await,
        "a new Recipe is not archived"
    );

    // The Archive area is one of the areas of a Recipe, and it is reached
    // from the Recipe page like every other one.
    let (status, page) = open(
        &world,
        Some(&world.owner),
        &format!("/recipes/sam/{slug}/archive"),
    )
    .await;
    assert_eq!(status, 200, "the Archive area must open: {page:.400}");
    assert!(page.contains(archive::IN_USE_LABEL));
    assert_cooking_words(&page);

    let done = archive_it(&world, &format!("/recipes/sam/{slug}"), true).await;
    assert_eq!(done.status(), 303, "the change must be taken");

    // Forgejo holds the state. This application holds no copy of it.
    assert!(
        archived(&world, &format!("sam/{slug}")).await,
        "Forgejo must hold the Recipe as archived"
    );

    let (_, page) = open(
        &world,
        Some(&world.owner),
        &format!("/recipes/sam/{slug}/archive"),
    )
    .await;
    assert!(page.contains(archive::ARCHIVED_LABEL));
    assert!(page.contains(archive::READ_ONLY_MESSAGE));
    assert_cooking_words(&page);

    // The Recipe page says the state as well, and it offers no way in to
    // the editor.
    let (status, page) = open(&world, Some(&world.owner), &format!("/recipes/sam/{slug}")).await;
    assert_eq!(status, 200, "an archived Recipe is still readable");
    assert!(page.contains(archive::ARCHIVED_LABEL));
    assert!(
        !page.contains(&format!("/recipes/sam/{slug}/edit\"")),
        "an archived Recipe must offer no way in to the editor"
    );
    assert_cooking_words(&page);

    // And the Owner can take it back out.
    let undone = archive_it(&world, &format!("/recipes/sam/{slug}"), false).await;
    assert_eq!(undone.status(), 303);
    assert!(
        !archived(&world, &format!("sam/{slug}")).await,
        "Forgejo must hold the Recipe as not archived again"
    );

    // A Cookbook archives the same way.
    let book = a_cookbook_holding(&world, "Winter", &slug).await;
    let done = archive_it(&world, &format!("/cookbooks/sam/{book}"), true).await;
    assert_eq!(done.status(), 303);
    assert!(archived(&world, &format!("sam/{book}")).await);

    let (status, page) = open(
        &world,
        Some(&world.owner),
        &format!("/cookbooks/sam/{book}/archive"),
    )
    .await;
    assert_eq!(status, 200);
    assert!(page.contains(archive::ARCHIVED_LABEL));
    assert_cooking_words(&page);
}

#[tokio::test]
async fn an_archived_recipe_refuses_every_change_on_the_post() {
    let world = ready().await;
    let slug = a_recipe(&world, "Chili", false).await;

    // Something to aim the Discussion actions and the restore at.
    let started = support::post_fields(
        &world.app,
        &world.owner,
        &format!("/recipes/sam/{slug}/discussions"),
        &[("title", "How much salt?"), ("body", "A question.")],
    )
    .await;
    assert_eq!(started.status(), 303, "the Discussion must start");

    let version = support::forgejo_api(
        &world.forgejo,
        &world.token,
        &format!("/repos/sam/{slug}/commits?limit=1"),
    )
    .await[0]["sha"]
        .as_str()
        .expect("the Recipe has a Version")
        .to_string();

    let before_versions = versions(&world, &format!("sam/{slug}")).await;
    let before_issues: usize = support::forgejo_api(
        &world.forgejo,
        &world.token,
        &format!("/repos/sam/{slug}/issues?state=all"),
    )
    .await
    .as_array()
    .map(Vec::len)
    .unwrap_or_default();

    archive_it(&world, &format!("/recipes/sam/{slug}"), true).await;

    // Every address under this Recipe that a POST can change something at.
    // Forgejo still reports write access for the person here, so nothing in
    // the permission answer would refuse any of these.
    let changes = [
        format!("/recipes/sam/{slug}/edit"),
        format!("/recipes/sam/{slug}/draft"),
        format!("/recipes/sam/{slug}/draft/discard"),
        format!("/recipes/sam/{slug}/thumbnail"),
        format!("/recipes/sam/{slug}/discussions"),
        format!("/recipes/sam/{slug}/discussions/1/comments"),
        format!("/recipes/sam/{slug}/discussions/1/state"),
        format!("/recipes/sam/{slug}/history/{version}/restore"),
        format!("/recipes/sam/{slug}/suggest"),
        format!("/recipes/sam/{slug}/suggest/save"),
        format!("/recipes/sam/{slug}/variations/update"),
    ];

    for path in &changes {
        let refused = post_empty(&world, &world.owner, path).await;
        assert_eq!(
            refused.status(),
            423,
            "`{path}` must be refused while the Recipe is archived"
        );

        let page = refused.text().await.expect("cannot read the body");
        assert!(
            page.contains(archive::ARCHIVED_MESSAGE),
            "`{path}` must say why: {page:.300}"
        );
        assert_cooking_words(&page);
    }

    // A Reader is refused the same way, and so is the editor before a
    // person types anything into it.
    let refused = post_empty(
        &world,
        &world.reader,
        &format!("/recipes/sam/{slug}/suggest"),
    )
    .await;
    assert_eq!(refused.status(), 423);

    let (status, page) = open(
        &world,
        Some(&world.reader),
        &format!("/recipes/sam/{slug}/suggest"),
    )
    .await;
    assert_eq!(status, 423, "the Suggestion editor must not open");
    assert!(page.contains(archive::ARCHIVED_MESSAGE));

    let (status, _) = open(
        &world,
        Some(&world.owner),
        &format!("/recipes/sam/{slug}/edit"),
    )
    .await;
    assert_eq!(status, 423, "the editor must not open");

    // Nothing moved in Forgejo. This is the assertion that matters: the
    // refusal is real and not only drawn.
    assert_eq!(
        versions(&world, &format!("sam/{slug}")).await,
        before_versions,
        "no Version may be published while the Recipe is archived"
    );
    let after_issues: usize = support::forgejo_api(
        &world.forgejo,
        &world.token,
        &format!("/repos/sam/{slug}/issues?state=all"),
    )
    .await
    .as_array()
    .map(Vec::len)
    .unwrap_or_default();
    assert_eq!(
        after_issues, before_issues,
        "no Discussion and no Suggestion may be started"
    );

    // What Forgejo itself still allows on an archived repository stays.
    // A Favorite and Notify me belong to the person and change nothing
    // about the Recipe.
    let favorite = support::post_fields(
        &world.app,
        &world.reader,
        &format!("/recipes/sam/{slug}/favorite"),
        &[("on", "yes")],
    )
    .await;
    assert_ne!(
        favorite.status(),
        423,
        "a Favorite is not a change to the Recipe"
    );

    // And so does making a Variation, which writes a new Recipe and nothing
    // to this one.
    let made = support::post_fields(
        &world.app,
        &world.reader,
        &format!("/recipes/sam/{slug}/variations"),
        &[("version", "")],
    )
    .await;
    assert_eq!(
        made.status(),
        303,
        "a Variation of an archived Recipe is still allowed"
    );

    // The Owner keeps control of who can read it.
    let shared = support::post_fields(
        &world.app,
        &world.owner,
        &format!("/recipes/sam/{slug}/sharing/visibility"),
        &[("visibility", "private")],
    )
    .await;
    assert_ne!(shared.status(), 423, "Sharing stays open to the Owner");
}

#[tokio::test]
async fn an_archived_cookbook_refuses_every_change_on_the_post() {
    let world = ready().await;
    let slug = a_recipe(&world, "Chili", false).await;
    let book = a_cookbook_holding(&world, "Winter", &slug).await;
    let second = a_recipe(&world, "Soup", false).await;

    archive_it(&world, &format!("/cookbooks/sam/{book}"), true).await;

    for path in [
        format!("/cookbooks/sam/{book}/recipes"),
        format!("/cookbooks/sam/{book}/recipes/remove"),
        format!("/cookbooks/sam/{book}/recipes/holding"),
    ] {
        let refused = post_empty(&world, &world.owner, &path).await;
        assert_eq!(refused.status(), 423, "`{path}` must be refused");

        let page = refused.text().await.expect("cannot read the body");
        assert!(page.contains(archive::ARCHIVED_COOKBOOK_MESSAGE));
        assert_cooking_words(&page);
    }

    // A real add, with every field it needs, is refused exactly the same.
    let refused = support::post_fields(
        &world.app,
        &world.owner,
        &format!("/cookbooks/sam/{book}/recipes"),
        &[
            ("recipe", &format!("sam/{second}")),
            ("holding", "pinned"),
            ("confirm", "yes"),
        ],
    )
    .await;
    assert_eq!(refused.status(), 423);

    // And Forgejo holds one Recipe in the Cookbook, exactly as before.
    let entries = support::forgejo_api(
        &world.forgejo,
        &world.token,
        &format!("/repos/sam/{book}/contents"),
    )
    .await;
    let held = entries
        .as_array()
        .map(|list| {
            list.iter()
                .filter(|one| one["type"].as_str() == Some("submodule"))
                .count()
        })
        .unwrap_or_default();
    assert_eq!(held, 1, "the Cookbook must hold what it held before");
}

// --------------------------------------------------------------- Deletion

#[tokio::test]
async fn a_get_never_deletes_and_a_deletion_needs_an_explicit_action() {
    let world = ready().await;
    let slug = a_recipe(&world, "Chili", false).await;

    // The report is a page that reads. Opening it deletes nothing.
    let (status, page) = open(
        &world,
        Some(&world.owner),
        &format!("/recipes/sam/{slug}/archive/delete"),
    )
    .await;
    assert_eq!(status, 200, "the report must open");
    assert!(page.contains("Delete Chili"));
    assert!(page.contains(archive::DELETE_WARNING));
    assert_cooking_words(&page);
    assert!(
        in_forgejo(&world, &format!("sam/{slug}")).await.is_some(),
        "a GET must never delete the Recipe"
    );

    // Opening it a second time still deletes nothing.
    let (status, _) = open(
        &world,
        Some(&world.owner),
        &format!("/recipes/sam/{slug}/archive/delete"),
    )
    .await;
    assert_eq!(status, 200);
    assert!(in_forgejo(&world, &format!("sam/{slug}")).await.is_some());

    // A post that skips the confirmation deletes nothing either. The check
    // lives on the server, so a form that was built by hand cannot pass it.
    let without = post_empty(
        &world,
        &world.owner,
        &format!("/recipes/sam/{slug}/archive/delete"),
    )
    .await;
    assert_eq!(without.status(), 200, "it draws the report again");
    assert!(
        in_forgejo(&world, &format!("sam/{slug}")).await.is_some(),
        "a post without the confirmation must delete nothing"
    );

    // Nobody but the Owner can delete it.
    let stranger = support::post_fields(
        &world.app,
        &world.reader,
        &format!("/recipes/sam/{slug}/archive/delete"),
        &[("confirm", "yes")],
    )
    .await;
    assert_eq!(stranger.status(), 403, "only the Owner can delete a Recipe");
    assert!(in_forgejo(&world, &format!("sam/{slug}")).await.is_some());

    // The explicit action, and only then.
    let done = support::post_fields(
        &world.app,
        &world.owner,
        &format!("/recipes/sam/{slug}/archive/delete"),
        &[("confirm", "yes")],
    )
    .await;
    assert_eq!(done.status(), 303, "the Recipe must be deleted");
    assert!(
        in_forgejo(&world, &format!("sam/{slug}")).await.is_none(),
        "Forgejo must hold the Recipe no more"
    );
}

#[tokio::test]
async fn the_report_names_the_cookbooks_the_variations_and_the_open_suggestions() {
    let world = ready().await;
    let slug = a_recipe(&world, "Chili", false).await;

    let book = a_cookbook_holding(&world, "Winter Food", &slug).await;
    let variation = a_variation(&world, &slug).await;
    let number = a_suggestion(&world, &slug).await;

    let (status, page) = open(
        &world,
        Some(&world.owner),
        &format!("/recipes/sam/{slug}/archive/delete"),
    )
    .await;
    assert_eq!(status, 200, "the report must open: {page:.400}");

    // The three questions, each answered from Forgejo.
    assert!(
        page.contains(&format!("/cookbooks/sam/{book}")),
        "the report must name the Cookbook that holds the Recipe"
    );
    assert!(page.contains("Winter Food"));
    assert!(
        page.contains(&format!("/recipes/kim/{variation}")),
        "the report must name the Variation"
    );
    assert!(
        page.contains(&format!("/recipes/sam/{slug}/suggestions/{number}")),
        "the report must name the open Suggestion"
    );
    assert!(
        page.contains("kim"),
        "the report must name who suggested it"
    );

    // What each of the three costs, and what the report cannot see.
    assert!(page.contains(archive::COOKBOOKS_MESSAGE));
    assert!(page.contains(archive::VARIATIONS_MESSAGE));
    assert!(page.contains(archive::SUGGESTIONS_MESSAGE));
    assert!(page.contains(archive::PARTIAL_MESSAGE));
    assert_cooking_words(&page);

    // Nothing was touched by reading the report.
    assert!(in_forgejo(&world, &format!("sam/{slug}")).await.is_some());
    assert!(in_forgejo(&world, &format!("sam/{book}")).await.is_some());
    assert!(
        in_forgejo(&world, &format!("kim/{variation}"))
            .await
            .is_some()
    );
    assert_eq!(suggestions(&world, &format!("sam/{slug}")).await.len(), 1);
}

#[tokio::test]
async fn the_report_says_what_forgejo_hides_rather_than_showing_an_empty_list() {
    // The fault this test exists to catch: Forgejo answers 200 with an
    // empty list when the Owner asks for the Variations of their Recipe and
    // the only one is private and belongs to somebody else. It answers 404
    // for a private Cookbook of another person that holds the Recipe.
    // Neither answer says that anything was left out, so an empty list on
    // the page would read as "nothing will break".
    let world = ready().await;
    let slug = a_recipe(&world, "Chili", true).await;

    // kim can read it, so kim can make a Variation of it and put it in a
    // Cookbook. Both are private, because the Recipe is.
    let shared = support::post_fields(
        &world.app,
        &world.owner,
        &format!("/recipes/sam/{slug}/sharing/people"),
        &[("login", "kim"), ("role", "reader")],
    )
    .await;
    assert_eq!(shared.status(), 303, "kim must be able to read the Recipe");

    let made = support::post_fields(
        &world.app,
        &world.reader,
        &format!("/recipes/sam/{slug}/variations"),
        &[("version", "")],
    )
    .await;
    assert_eq!(made.status(), 303, "kim must be able to make a Variation");

    let book =
        support::create_cookbook(&world.app, &world.reader, "Kim Winter", "Warm food.", true).await;
    assert_eq!(book.status(), 303);
    let added = support::post_fields(
        &world.app,
        &world.reader,
        "/cookbooks/kim/kim-winter/recipes",
        &[
            ("recipe", &format!("sam/{slug}")),
            ("holding", "pinned"),
            ("confirm", "yes"),
        ],
    )
    .await;
    assert_eq!(added.status(), 303, "kim must be able to hold the Recipe");

    // Forgejo hides both from sam, and it says nothing about hiding them.
    let hidden_variations: Vec<Value> = support::forgejo_api(
        &world.forgejo,
        &world.token,
        &format!("/repos/sam/{slug}/forks"),
    )
    .await
    .as_array()
    .cloned()
    .unwrap_or_default();
    assert!(
        hidden_variations.is_empty(),
        "this test rests on Forgejo hiding the private Variation from the Owner of the source"
    );
    assert!(
        in_forgejo(&world, "kim/kim-winter").await.is_none(),
        "this test rests on Forgejo hiding the private Cookbook from sam"
    );

    // So the report has to say so, and it must not read as "nothing will
    // break".
    let (status, page) = open(
        &world,
        Some(&world.owner),
        &format!("/recipes/sam/{slug}/archive/delete"),
    )
    .await;
    assert_eq!(status, 200);
    assert!(
        page.contains(archive::PARTIAL_MESSAGE),
        "the report must say that it shows only what Forgejo shows this person"
    );
    assert_cooking_words(&page);

    // kim, who can read the Recipe but does not own it, gets no control at
    // all and cannot reach the report.
    let (status, page) = open(
        &world,
        Some(&world.reader),
        &format!("/recipes/sam/{slug}/archive/delete"),
    )
    .await;
    assert_eq!(status, 403);
    assert!(!page.contains("name=\"confirm\""));
}

#[tokio::test]
async fn deleting_a_recipe_cascades_into_nothing() {
    let world = ready().await;
    let slug = a_recipe(&world, "Chili", false).await;

    let book = a_cookbook_holding(&world, "Winter", &slug).await;
    let variation = a_variation(&world, &slug).await;
    let number = a_suggestion(&world, &slug).await;
    assert!(number > 0);

    let versions_before = versions(&world, &format!("kim/{variation}")).await;
    let source_before = support::forgejo_raw(
        &world.forgejo,
        &world.reader_token,
        &format!("/kim/{variation}/raw/recipe.cook?ref=main"),
    )
    .await
    .1;

    let done = support::post_fields(
        &world.app,
        &world.owner,
        &format!("/recipes/sam/{slug}/archive/delete"),
        &[("confirm", "yes")],
    )
    .await;
    assert_eq!(done.status(), 303);
    assert!(in_forgejo(&world, &format!("sam/{slug}")).await.is_none());

    // The Variation is untouched. Forgejo holds it, and it holds every word
    // and every Version that it held before.
    let kept = in_forgejo(&world, &format!("kim/{variation}"))
        .await
        .expect("the Variation must stay");
    assert_eq!(
        versions(&world, &format!("kim/{variation}")).await,
        versions_before,
        "a deletion must add no Version to a Variation and remove none"
    );
    assert_eq!(
        support::forgejo_raw(
            &world.forgejo,
            &world.reader_token,
            &format!("/kim/{variation}/raw/recipe.cook?ref=main"),
        )
        .await
        .1,
        source_before,
        "a deletion must not change one byte of a Variation"
    );

    // Forgejo stops naming the source, so the Variation is an ordinary
    // Recipe. This application must not invent a source that Forgejo no
    // longer names.
    assert_eq!(
        kept["fork"].as_bool(),
        Some(false),
        "Forgejo must stop recording where the Variation came from"
    );
    assert!(
        kept["parent"].is_null(),
        "Forgejo must name no source any more"
    );

    // And the Variation is still a usable Recipe in the application.
    let (status, page) = open(
        &world,
        Some(&world.reader),
        &format!("/recipes/kim/{variation}"),
    )
    .await;
    assert_eq!(status, 200, "a Variation of a deleted Recipe must open");
    assert!(page.contains("Chop the"), "it must still be cookable");
    assert_cooking_words(&page);

    let (status, page) = open(
        &world,
        Some(&world.reader),
        &format!("/recipes/kim/{variation}/variations"),
    )
    .await;
    assert_eq!(status, 200);
    assert_cooking_words(&page);

    // The Cookbook keeps its entry, and the entry is broken and visible.
    // Nothing about it was repaired and nothing was rewritten.
    let (status, page) = open(
        &world,
        Some(&world.owner),
        &format!("/cookbooks/sam/{book}"),
    )
    .await;
    assert_eq!(status, 200, "the Cookbook must still open");
    assert!(
        page.contains(cooklanghub::cookbook::UNAVAILABLE_MESSAGE),
        "the broken entry must stay visible and say what it is"
    );
    assert!(
        !page.contains("Chili"),
        "a Recipe that nobody can open must leak no title"
    );
    assert_cooking_words(&page);

    let entries = support::forgejo_api(
        &world.forgejo,
        &world.token,
        &format!("/repos/sam/{book}/contents"),
    )
    .await;
    let held = entries
        .as_array()
        .map(|list| {
            list.iter()
                .filter(|one| one["type"].as_str() == Some("submodule"))
                .count()
        })
        .unwrap_or_default();
    assert_eq!(
        held, 1,
        "the Cookbook must still record the Recipe it recorded"
    );

    // Forgejo holds a Suggestion inside the Recipe, so it went with the
    // Recipe. It was not closed and it was not moved anywhere.
    let answer = reqwest::Client::new()
        .get(format!(
            "{}/api/v1/repos/sam/{slug}/pulls?state=all",
            world.forgejo.base_url
        ))
        .header("Authorization", format!("token {}", world.token.expose()))
        .send()
        .await
        .expect("cannot reach the Forgejo API");
    assert_eq!(
        answer.status(),
        404,
        "Forgejo must hold no Suggestion of a Recipe that is gone"
    );
}

#[tokio::test]
async fn a_suggestion_that_came_from_a_copy_is_closed_and_kept_when_the_copy_goes() {
    // The other half of the deletion semantics of Forgejo. A Suggestion
    // lives inside the Recipe it is aimed at, so deleting the copy it came
    // from does not delete it: Forgejo closes it and keeps every word.
    let world = ready().await;
    let slug = a_recipe(&world, "Chili", false).await;
    let variation = a_variation(&world, &slug).await;

    // kim changes the Variation and proposes it from there, which is the
    // one shape of Suggestion that this application does not make itself.
    let file = support::forgejo_api(
        &world.forgejo,
        &world.reader_token,
        &format!("/repos/kim/{variation}/contents/recipe.cook"),
    )
    .await;
    let changed = support::forgejo_write(
        &world.forgejo,
        &world.reader_token,
        Method::PUT,
        &format!("/repos/kim/{variation}/contents/recipe.cook"),
        json!({
            "content": base64(&DISH.replace("@salt{1%g}", "@salt{4%g}")),
            "message": "More salt",
            "branch": "main",
            "sha": file["sha"].as_str().unwrap_or_default(),
        }),
    )
    .await;
    assert!(
        changed.status().is_success(),
        "the test could not change the Variation: {}",
        changed.status()
    );

    let proposed = support::forgejo_write(
        &world.forgejo,
        &world.reader_token,
        Method::POST,
        &format!("/repos/sam/{slug}/pulls"),
        json!({ "head": format!("kim:main"), "base": "main", "title": "More salt" }),
    )
    .await;
    assert!(
        proposed.status().is_success(),
        "the test could not make the Suggestion: {}",
        proposed.status()
    );

    assert_eq!(suggestions(&world, &format!("sam/{slug}")).await.len(), 1);

    // kim deletes the Variation the Suggestion came from.
    let done = support::post_fields(
        &world.app,
        &world.reader,
        &format!("/recipes/kim/{variation}/archive/delete"),
        &[("confirm", "yes")],
    )
    .await;
    assert_eq!(
        done.status(),
        303,
        "kim owns the Variation and can delete it"
    );
    assert!(
        in_forgejo(&world, &format!("kim/{variation}"))
            .await
            .is_none()
    );

    // Forgejo kept the Suggestion and closed it. It was not deleted.
    let held = suggestions(&world, &format!("sam/{slug}")).await;
    assert_eq!(held.len(), 1, "the Suggestion must stay");
    assert_eq!(
        held[0]["state"].as_str(),
        Some("closed"),
        "Forgejo must close a Suggestion whose copy is gone"
    );
    assert_eq!(
        held[0]["merged"].as_bool(),
        Some(false),
        "closing is not accepting"
    );

    // The Suggestions area still opens and still names it.
    let (status, page) = open(
        &world,
        Some(&world.owner),
        &format!("/recipes/sam/{slug}/suggestions"),
    )
    .await;
    assert_eq!(status, 200);
    assert!(page.contains("More salt"));
    assert_cooking_words(&page);

    // And the Recipe itself did not move at all.
    assert_eq!(versions(&world, &format!("sam/{slug}")).await, 1);
}

#[tokio::test]
async fn deleting_a_cookbook_deletes_no_recipe() {
    let world = ready().await;
    let one = a_recipe(&world, "Chili", false).await;
    let two = a_recipe(&world, "Soup", false).await;
    let book = a_cookbook_holding(&world, "Winter", &one).await;

    let added = support::post_fields(
        &world.app,
        &world.owner,
        &format!("/cookbooks/sam/{book}/recipes"),
        &[
            ("recipe", &format!("sam/{two}")),
            ("holding", "pinned"),
            ("confirm", "yes"),
        ],
    )
    .await;
    assert_eq!(added.status(), 303);

    let versions_one = versions(&world, &format!("sam/{one}")).await;
    let versions_two = versions(&world, &format!("sam/{two}")).await;

    // The report names each Recipe and says plainly that each one stays.
    let (status, page) = open(
        &world,
        Some(&world.owner),
        &format!("/cookbooks/sam/{book}/archive/delete"),
    )
    .await;
    assert_eq!(status, 200, "the report must open: {page:.400}");
    assert!(page.contains(archive::COOKBOOK_RECIPES_MESSAGE));
    assert!(page.contains(&format!("/recipes/sam/{one}")));
    assert!(page.contains(&format!("/recipes/sam/{two}")));
    assert_cooking_words(&page);
    assert!(
        in_forgejo(&world, &format!("sam/{book}")).await.is_some(),
        "a GET must never delete the Cookbook"
    );

    let done = support::post_fields(
        &world.app,
        &world.owner,
        &format!("/cookbooks/sam/{book}/archive/delete"),
        &[("confirm", "yes")],
    )
    .await;
    assert_eq!(done.status(), 303);
    assert!(in_forgejo(&world, &format!("sam/{book}")).await.is_none());

    // Both Recipes are exactly as they were. Forgejo says so, and not a
    // page of this application.
    for (slug, before) in [(&one, versions_one), (&two, versions_two)] {
        assert!(
            in_forgejo(&world, &format!("sam/{slug}")).await.is_some(),
            "deleting a Cookbook must delete no Recipe"
        );
        assert_eq!(
            versions(&world, &format!("sam/{slug}")).await,
            before,
            "deleting a Cookbook must add no Version to a Recipe and remove none"
        );

        let (status, page) =
            open(&world, Some(&world.owner), &format!("/recipes/sam/{slug}")).await;
        assert_eq!(status, 200, "the Recipe must still open");
        assert!(page.contains("Chop the"));
    }
}

fn base64(text: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(text)
}
