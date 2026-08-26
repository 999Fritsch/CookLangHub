//! Acceptance tests for Suggestions.
//!
//! Every test drives the real editor against a real Forgejo and a real Git,
//! and then asks Forgejo what actually landed. The cases that hide a fault
//! are the ones about where a Suggestion lives and where it does not: a
//! Suggestion that Forgejo holds and this application does not, a person
//! without write access who can still propose a change, a published Recipe
//! that does not move at all, and a second save that must reach the same
//! Suggestion rather than make another one.
//!
//! Nothing in these tests keeps state in a browser. Each request builds a
//! new client with no cookie store of its own, so a page that came back
//! with the work of a person can only have got it from Forgejo.

mod support;

use std::collections::HashSet;

use cooklanghub::secret::Secret;
use cooklanghub::session::COOKIE_NAME;
use cooklanghub::suggestion;
use reqwest::Method;
use serde_json::{Value, json};

/// A Recipe long enough for two people to change different parts of it.
const DISH: &str = "---
title: Onion Base
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
    /// The session of `kim`, who can read a Recipe but not write to it.
    reader: String,
    /// The session of `robin`, a second person with read access only.
    other: String,
    /// The credential that the test itself uses to ask Forgejo questions.
    token: Secret<String>,
}

async fn ready() -> World {
    let forgejo = support::start_forgejo().await;
    forgejo.create_user("alex", true);
    forgejo.create_user("sam", false);
    forgejo.create_user("kim", false);
    forgejo.create_user("robin", false);
    let admin = forgejo.access_token("alex");

    let app = support::start_app(&forgejo.base_url).await;
    app.bootstrap(&admin).await;

    let owner = support::sign_in(&app, &forgejo, "sam").await;
    let reader = support::sign_in(&app, &forgejo, "kim").await;
    let other = support::sign_in(&app, &forgejo, "robin").await;
    let token = forgejo.access_token("sam");

    World {
        forgejo,
        app,
        owner,
        reader,
        other,
        token,
    }
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

fn cookie(session: &str) -> String {
    format!("{COOKIE_NAME}={session}")
}

/// Open a page. Every call builds a new client, so the page carries nothing
/// that an earlier request left in a browser.
async fn open(app: &support::TestApp, session: &str, path: &str) -> (reqwest::StatusCode, String) {
    let response = support::client()
        .get(app.url(path))
        .header("cookie", cookie(session))
        .send()
        .await
        .expect("cannot reach the page");

    let status = response.status();
    let body = response.text().await.expect("cannot read the body");
    (status, body)
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

/// The state and the answer of one save while a person writes.
async fn autosave(
    world: &World,
    session: &str,
    slug: &str,
    source: &str,
    base_version: &str,
    draft_version: &str,
) -> (reqwest::StatusCode, Value) {
    let response = support::post_fields(
        &world.app,
        session,
        &format!("/recipes/sam/{slug}/suggest"),
        &[
            ("source", source),
            ("base_version", base_version),
            ("draft_version", draft_version),
        ],
    )
    .await;

    let status = response.status();
    let body: Value = response.json().await.expect("the answer must be JSON");
    (status, body)
}

/// Send the form, the way a browser with no script does.
#[allow(clippy::too_many_arguments)]
async fn submit(
    world: &World,
    session: &str,
    slug: &str,
    source: &str,
    base_version: &str,
    draft_version: &str,
    note: &str,
    action: &str,
) -> reqwest::Response {
    support::post_fields(
        &world.app,
        session,
        &format!("/recipes/sam/{slug}/suggest/save"),
        &[
            ("source", source),
            ("base_version", base_version),
            ("draft_version", draft_version),
            ("note", note),
            ("action", action),
        ],
    )
    .await
}

/// Every Suggestion that Forgejo holds for a Recipe.
async fn suggestions_in_forgejo(world: &World, slug: &str) -> Vec<Value> {
    support::forgejo_api(
        &world.forgejo,
        &world.token,
        &format!("/repos/sam/{slug}/pulls?state=all"),
    )
    .await
    .as_array()
    .cloned()
    .unwrap_or_default()
}

/// The published Versions of a Recipe, as Forgejo reports them.
async fn published_versions(world: &World, slug: &str) -> usize {
    support::forgejo_api(
        &world.forgejo,
        &world.token,
        &format!("/repos/sam/{slug}/commits?limit=50"),
    )
    .await
    .as_array()
    .map(|found| found.len())
    .unwrap_or_default()
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
/// criterion that a Suggestion carries none of them.
fn assert_cooking_words(html: &str) {
    let words = visible(html).to_lowercase();

    for phrase in ["pull request", "merge request", "work in progress"] {
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
        "wip",
        "upstream",
    ] {
        assert!(
            !spoken.contains(forge_word),
            "the page says `{forge_word}` to a cook"
        );
    }
}

/// Open the Suggestion editor of a Reader and read what it carries.
async fn editor(world: &World, session: &str, slug: &str) -> (String, String, String) {
    let (status, page) = open(&world.app, session, &format!("/recipes/sam/{slug}/suggest")).await;
    assert_eq!(status, 200, "the editor must open: {page:.400}");
    assert_cooking_words(&page);

    let base = field(&page, "base_version");
    let draft = field(&page, "draft_version");
    (page, base, draft)
}

#[tokio::test]
async fn a_reader_who_opens_the_editor_gets_a_suggestion_and_not_a_publication() {
    let world = ready().await;
    let slug = a_recipe(&world, "Chili", false).await;

    // Forgejo gives kim read access to a public Recipe and no more.
    let sent = support::client()
        .get(world.app.url(&format!("/recipes/sam/{slug}/edit")))
        .header("cookie", cookie(&world.reader))
        .send()
        .await
        .expect("cannot reach the editor");

    assert_eq!(sent.status(), 303, "a Reader must not reach the editor");
    assert_eq!(
        sent.headers()
            .get("location")
            .and_then(|value| value.to_str().ok()),
        Some(format!("/recipes/sam/{slug}/suggest").as_str()),
        "a Reader must be sent to their Suggestion"
    );

    // The person who owns the Recipe still publishes directly.
    let (status, _) = open(
        &world.app,
        &world.owner,
        &format!("/recipes/sam/{slug}/edit"),
    )
    .await;
    assert_eq!(status, 200, "the owner must still reach the editor");

    // The Suggestion editor opens on the Recipe as it is published now.
    let (page, base, draft) = editor(&world, &world.reader, &slug).await;
    assert!(draft.is_empty(), "there is no Suggestion yet");
    assert_eq!(base.len(), 40, "the page names the published Version");
    assert!(page.contains("Chop the @onion{1}."));
    assert!(page.contains(suggestion::NEW_MESSAGE));
}

#[tokio::test]
async fn forgejo_holds_the_suggestion_that_a_reader_without_write_access_makes() {
    let world = ready().await;
    let slug = a_recipe(&world, "Chili", false).await;
    let published = stored(&world, &slug, "main").await;

    let (_, base, draft) = editor(&world, &world.reader, &slug).await;

    let changed = published.replace("@onion{1}", "@onion{2}");
    let (status, answer) = autosave(&world, &world.reader, &slug, &changed, &base, &draft).await;
    assert_eq!(status, 200, "the save must be taken: {answer}");
    assert_eq!(
        answer["message"].as_str(),
        Some(suggestion::SAVED_MESSAGE),
        "the person must read that the work is safe"
    );
    let version = answer["version"]
        .as_str()
        .expect("the answer names a Version");
    assert_eq!(version.len(), 40);

    // Forgejo holds the Suggestion, and this application holds none of it.
    let held = suggestions_in_forgejo(&world, &slug).await;
    assert_eq!(held.len(), 1, "Forgejo must hold exactly one Suggestion");
    let one = &held[0];
    assert_eq!(one["user"]["login"].as_str(), Some("kim"));
    assert_eq!(one["state"].as_str(), Some("open"));
    assert_eq!(one["merged"].as_bool(), Some(false));
    assert_eq!(
        one["head"]["sha"].as_str(),
        Some(version),
        "Forgejo must hold the Version that the answer named"
    );

    // AGit: Forgejo holds the proposal itself, and nobody copied the
    // Recipe to make it.
    assert_eq!(
        one["head"]["ref"].as_str(),
        Some(format!("refs/pull/{}/head", one["number"]).as_str()),
        "the Suggestion must reach Forgejo through AGit"
    );
    assert_eq!(one["base"]["ref"].as_str(), Some("main"));
    let forks: Value = support::forgejo_api(
        &world.forgejo,
        &world.token,
        &format!("/repos/sam/{slug}/forks"),
    )
    .await;
    assert_eq!(
        forks.as_array().map(|found| found.len()),
        Some(0),
        "a Suggestion must not copy the Recipe"
    );

    // The Suggestion holds the work of the person.
    assert_eq!(stored(&world, &slug, version).await, changed);

    // The published Recipe did not move at all.
    assert_eq!(stored(&world, &slug, "main").await, published);
    assert_eq!(published_versions(&world, &slug).await, 1);
}

#[tokio::test]
async fn every_save_reaches_the_same_suggestion_and_makes_no_second_one() {
    let world = ready().await;
    let slug = a_recipe(&world, "Chili", false).await;
    let published = stored(&world, &slug, "main").await;

    let (_, base, draft) = editor(&world, &world.reader, &slug).await;

    let mut version = draft;
    let mut last = String::new();
    for salt in ["2%g", "3%g", "4%g", "5%g"] {
        last = published.replace("@salt{1%g}", &format!("@salt{{{salt}}}"));
        let (status, answer) = autosave(&world, &world.reader, &slug, &last, &base, &version).await;
        assert_eq!(status, 200, "save `{salt}` must be taken: {answer}");
        version = answer["version"]
            .as_str()
            .expect("the answer names a Version")
            .to_string();
    }

    let held = suggestions_in_forgejo(&world, &slug).await;
    assert_eq!(
        held.len(),
        1,
        "four saves must leave one Suggestion, not four"
    );
    assert_eq!(held[0]["head"]["sha"].as_str(), Some(version.as_str()));
    assert_eq!(stored(&world, &slug, &version).await, last);

    // A second person gets a Suggestion of their own, and the first one is
    // untouched.
    let (_, base, draft) = editor(&world, &world.other, &slug).await;
    let theirs = published.replace("@pepper{1%g}", "@pepper{9%g}");
    let (status, answer) = autosave(&world, &world.other, &slug, &theirs, &base, &draft).await;
    assert_eq!(status, 200, "a second person must get their own: {answer}");

    let held = suggestions_in_forgejo(&world, &slug).await;
    assert_eq!(held.len(), 2, "two people make two Suggestions");
    let logins: HashSet<&str> = held
        .iter()
        .filter_map(|one| one["user"]["login"].as_str())
        .collect();
    assert_eq!(logins, HashSet::from(["kim", "robin"]));

    // Nothing of this reached the published Recipe.
    assert_eq!(stored(&world, &slug, "main").await, published);
    assert_eq!(published_versions(&world, &slug).await, 1);
}

#[tokio::test]
async fn the_two_states_are_the_work_in_progress_convention_of_forgejo() {
    let world = ready().await;
    let slug = a_recipe(&world, "Chili", false).await;
    let published = stored(&world, &slug, "main").await;

    let (_, base, draft) = editor(&world, &world.reader, &slug).await;
    let changed = published.replace("@onion{1}", "@onion{2}");
    let (status, answer) = autosave(&world, &world.reader, &slug, &changed, &base, &draft).await;
    assert_eq!(status, 200, "{answer}");
    let version = answer["version"].as_str().unwrap().to_string();

    // Unfinished. Forgejo marks it with its own prefix, and no page says
    // that prefix to a cook.
    let held = suggestions_in_forgejo(&world, &slug).await;
    let number = held[0]["number"]
        .as_i64()
        .expect("the Suggestion has a number");
    assert!(
        held[0]["title"]
            .as_str()
            .expect("the Suggestion has a title")
            .starts_with("WIP:"),
        "an unfinished Suggestion must carry the Forgejo prefix: {}",
        held[0]["title"]
    );

    let (page, _, _) = editor(&world, &world.reader, &slug).await;
    assert!(page.contains("Editing in progress"));
    assert!(
        page.contains("Ready for review"),
        "the Reader can finish it"
    );

    let (status, one) = open(
        &world.app,
        &world.reader,
        &format!("/recipes/sam/{slug}/suggestions/{number}"),
    )
    .await;
    assert_eq!(status, 200);
    assert!(one.contains("Editing in progress"));
    assert_cooking_words(&one);

    // The Reader marks it ready. This is a plain form, so it works with no
    // script at all.
    let sent = submit(
        &world,
        &world.reader,
        &slug,
        &changed,
        &base,
        &version,
        "More onion, less salt.",
        "ready",
    )
    .await;
    assert_eq!(sent.status(), 303, "the form must redirect");
    assert_eq!(
        sent.headers()
            .get("location")
            .and_then(|value| value.to_str().ok()),
        Some(format!("/recipes/sam/{slug}/suggestions/{number}").as_str())
    );

    // Forgejo holds the state, and it is the title it holds.
    let held = suggestions_in_forgejo(&world, &slug).await;
    assert_eq!(held.len(), 1, "marking it ready must make no second one");
    let title = held[0]["title"]
        .as_str()
        .expect("the Suggestion has a title");
    assert!(
        !title.to_uppercase().starts_with("WIP"),
        "a Suggestion that is ready must not carry the prefix: {title}"
    );
    assert_eq!(held[0]["state"].as_str(), Some("open"));
    assert_eq!(held[0]["body"].as_str(), Some("More onion, less salt."));

    let (status, one) = open(
        &world.app,
        &world.reader,
        &format!("/recipes/sam/{slug}/suggestions/{number}"),
    )
    .await;
    assert_eq!(status, 200);
    assert!(one.contains("Ready for review"));
    assert!(one.contains("More onion, less salt."));
    assert_cooking_words(&one);

    // And still nothing reached the published Recipe.
    assert_eq!(stored(&world, &slug, "main").await, published);
    assert_eq!(published_versions(&world, &slug).await, 1);
}

#[tokio::test]
async fn a_suggestion_lives_in_forgejo_and_survives_a_restart() {
    let world = ready().await;
    let slug = a_recipe(&world, "Chili", false).await;
    let published = stored(&world, &slug, "main").await;

    let (_, base, draft) = editor(&world, &world.reader, &slug).await;
    let changed = published.replace("Serve.", "Serve with bread.");
    let (status, answer) = autosave(&world, &world.reader, &slug, &changed, &base, &draft).await;
    assert_eq!(status, 200, "{answer}");
    let version = answer["version"].as_str().unwrap().to_string();

    // A new server, a new empty memory, and the same operational database.
    // The work is not in either of them: it is in Forgejo.
    let restarted = support::restart(&world.app).await;

    let (status, page) = open(
        &restarted,
        &world.reader,
        &format!("/recipes/sam/{slug}/suggest"),
    )
    .await;
    assert_eq!(status, 200, "the Suggestion must open again");
    assert!(
        page.contains("Serve with bread."),
        "the editor must open on the work of the person"
    );
    assert_eq!(
        field(&page, "draft_version"),
        version,
        "the page must carry the Version that the Suggestion holds"
    );
    assert!(page.contains(suggestion::NOTICE_MESSAGE));
    assert_cooking_words(&page);
}

#[tokio::test]
async fn a_save_from_a_page_that_fell_behind_is_refused_and_changes_nothing() {
    let world = ready().await;
    let slug = a_recipe(&world, "Chili", false).await;
    let published = stored(&world, &slug, "main").await;

    let (_, base, draft) = editor(&world, &world.reader, &slug).await;

    // One tab writes.
    let first = published.replace("@onion{1}", "@onion{2}");
    let (status, answer) = autosave(&world, &world.reader, &slug, &first, &base, &draft).await;
    assert_eq!(status, 200, "{answer}");
    let held = answer["version"].as_str().unwrap().to_string();

    // A second tab writes, still carrying the Version it opened on. The
    // application must refuse it rather than pick a winner.
    let second = published.replace("@onion{1}", "@onion{9}");
    let (status, answer) = autosave(&world, &world.reader, &slug, &second, &base, &draft).await;
    assert_eq!(status, 409, "a save that fell behind must be refused");
    assert_eq!(
        answer["message"].as_str(),
        Some(suggestion::STALE_MESSAGE),
        "the person must read why, and what to do"
    );

    // The Suggestion keeps exactly what the first tab wrote.
    let suggestions = suggestions_in_forgejo(&world, &slug).await;
    assert_eq!(suggestions.len(), 1);
    assert_eq!(suggestions[0]["head"]["sha"].as_str(), Some(held.as_str()));
    assert_eq!(stored(&world, &slug, &held).await, first);
    assert_eq!(stored(&world, &slug, "main").await, published);
}

#[tokio::test]
async fn a_reader_of_a_private_recipe_can_suggest_a_change_to_it() {
    let world = ready().await;
    let slug = a_recipe(&world, "Secret Stew", true).await;

    // Forgejo decides who may read a private Recipe. The owner shares it as
    // a Reader, which is read access and no more.
    let shared = support::post_fields(
        &world.app,
        &world.owner,
        &format!("/recipes/sam/{slug}/sharing/people"),
        &[("login", "kim"), ("role", "reader")],
    )
    .await;
    assert!(
        shared.status().is_success() || shared.status().is_redirection(),
        "the Recipe must be shared: {}",
        shared.status()
    );

    let permission: Value = support::forgejo_api(
        &world.forgejo,
        &world.token,
        &format!("/repos/sam/{slug}/collaborators/kim/permission"),
    )
    .await;
    assert_eq!(
        permission["permission"].as_str(),
        Some("read"),
        "kim must be able to read the Recipe and no more"
    );

    let published = stored(&world, &slug, "main").await;
    let (_, base, draft) = editor(&world, &world.reader, &slug).await;

    let changed = published.replace("@salt{1%g}", "@salt{3%g}");
    let (status, answer) = autosave(&world, &world.reader, &slug, &changed, &base, &draft).await;
    assert_eq!(status, 200, "a Reader must be able to suggest: {answer}");
    let version = answer["version"].as_str().unwrap();

    let held = suggestions_in_forgejo(&world, &slug).await;
    assert_eq!(held.len(), 1);
    assert_eq!(held[0]["user"]["login"].as_str(), Some("kim"));
    assert_eq!(stored(&world, &slug, version).await, changed);

    // Somebody with no access reads nothing of it.
    let (status, _) = open(
        &world.app,
        &world.other,
        &format!("/recipes/sam/{slug}/suggestions"),
    )
    .await;
    assert_eq!(status, 404, "a private Recipe stays private");
}

#[tokio::test]
async fn the_suggestions_area_names_every_suggestion_in_cooking_words() {
    let world = ready().await;
    let slug = a_recipe(&world, "Chili", false).await;
    let published = stored(&world, &slug, "main").await;

    // Empty first.
    let (status, page) = open(
        &world.app,
        &world.owner,
        &format!("/recipes/sam/{slug}/suggestions"),
    )
    .await;
    assert_eq!(status, 200);
    assert!(page.contains("This Recipe has no Suggestion yet."));
    assert_cooking_words(&page);

    // The area is reachable from the Recipe page, and is not greyed out.
    let (status, recipe) = open(&world.app, &world.owner, &format!("/recipes/sam/{slug}")).await;
    assert_eq!(status, 200);
    assert!(
        recipe.contains(&format!("href=\"/recipes/sam/{slug}/suggestions\"")),
        "the Recipe page must offer the Suggestions area"
    );

    // Two people suggest.
    for (session, from, to) in [
        (&world.reader, "@onion{1}", "@onion{2}"),
        (&world.other, "@pepper{1%g}", "@pepper{4%g}"),
    ] {
        let (_, base, draft) = editor(&world, session, &slug).await;
        let changed = published.replace(from, to);
        let (status, answer) = autosave(&world, session, &slug, &changed, &base, &draft).await;
        assert_eq!(status, 200, "{answer}");
    }

    let (status, page) = open(
        &world.app,
        &world.owner,
        &format!("/recipes/sam/{slug}/suggestions"),
    )
    .await;
    assert_eq!(status, 200);
    assert_cooking_words(&page);
    // The name of a Suggestion is the Recipe it is for, and not a name that
    // Git or Forgejo would use.
    assert!(page.contains("Suggestion for Chili"));
    assert!(page.contains("Editing in progress"));
    assert!(page.contains("Made by Kim") || page.contains("Made by kim"));
    assert!(page.contains(&format!("/recipes/sam/{slug}/suggestions/1")));
    assert!(page.contains(&format!("/recipes/sam/{slug}/suggestions/2")));

    // Anybody who can read the Recipe reads the Suggestions of it.
    let (status, page) = open(&world.app, "", &format!("/recipes/sam/{slug}/suggestions")).await;
    assert_eq!(status, 200, "a public Recipe shows its Suggestions");
    assert_cooking_words(&page);
    assert!(page.contains("To suggest a change to this Recipe, sign in."));
}

#[tokio::test]
async fn a_suggestion_that_forgejo_closed_is_not_written_to_again() {
    let world = ready().await;
    let slug = a_recipe(&world, "Chili", false).await;
    let published = stored(&world, &slug, "main").await;

    let (_, base, draft) = editor(&world, &world.reader, &slug).await;
    let changed = published.replace("@onion{1}", "@onion{2}");
    let (status, answer) = autosave(&world, &world.reader, &slug, &changed, &base, &draft).await;
    assert_eq!(status, 200, "{answer}");
    let version = answer["version"].as_str().unwrap().to_string();

    let held = suggestions_in_forgejo(&world, &slug).await;
    let number = held[0]["number"].as_i64().unwrap();

    // The owner declines it in Forgejo itself. This application does not do
    // that yet: ticket #15 builds it.
    let closed = support::forgejo_write(
        &world.forgejo,
        &world.token,
        Method::PATCH,
        &format!("/repos/sam/{slug}/pulls/{number}"),
        json!({ "state": "closed" }),
    )
    .await;
    assert!(
        closed.status().is_success(),
        "the test could not close the Suggestion: {}",
        closed.status()
    );

    // The page of the person still carries the Version of a Suggestion that
    // is closed now. The save is refused, and it is told what happened.
    let again = published.replace("@onion{1}", "@onion{7}");
    let (status, answer) = autosave(&world, &world.reader, &slug, &again, &base, &version).await;
    assert_eq!(status, 409, "a closed Suggestion takes no more work");
    assert_eq!(answer["message"].as_str(), Some(suggestion::GONE_MESSAGE));

    // Nothing was made, and nothing was written over.
    let held = suggestions_in_forgejo(&world, &slug).await;
    assert_eq!(held.len(), 1);
    assert_eq!(held[0]["head"]["sha"].as_str(), Some(version.as_str()));
    assert_eq!(held[0]["state"].as_str(), Some("closed"));

    // The state a person reads says so, and the conversation stays.
    let (status, page) = open(
        &world.app,
        &world.reader,
        &format!("/recipes/sam/{slug}/suggestions/{number}"),
    )
    .await;
    assert_eq!(status, 200);
    assert!(page.contains("Declined"));
    assert_cooking_words(&page);

    // A new Suggestion can still be started, and it is one Suggestion.
    let (_, base, draft) = editor(&world, &world.reader, &slug).await;
    assert!(draft.is_empty(), "a closed Suggestion is not carried on");
    let (status, answer) = autosave(&world, &world.reader, &slug, &again, &base, &draft).await;
    assert_eq!(status, 200, "{answer}");

    let held = suggestions_in_forgejo(&world, &slug).await;
    assert_eq!(held.len(), 2, "the closed one stays, and a new one is made");
    assert_eq!(published_versions(&world, &slug).await, 1);
}

#[tokio::test]
async fn the_form_makes_a_suggestion_with_no_script_at_all() {
    let world = ready().await;
    let slug = a_recipe(&world, "Chili", false).await;
    let published = stored(&world, &slug, "main").await;

    let (page, base, draft) = editor(&world, &world.reader, &slug).await;

    // The editor asks for no script that runs inside the page.
    for script in page.split("<script").skip(1) {
        assert!(
            script.starts_with(" src=\""),
            "the page must carry no inline script"
        );
    }
    assert!(!page.contains("onclick="));
    assert!(!page.contains("onsubmit="));

    let changed = published.replace("Serve.", "Serve hot.");
    let sent = submit(
        &world,
        &world.reader,
        &slug,
        &changed,
        &base,
        &draft,
        "",
        "save",
    )
    .await;
    assert_eq!(sent.status(), 303, "the form must redirect");

    let held = suggestions_in_forgejo(&world, &slug).await;
    assert_eq!(held.len(), 1);
    let number = held[0]["number"].as_i64().unwrap();
    assert_eq!(
        sent.headers()
            .get("location")
            .and_then(|value| value.to_str().ok()),
        Some(format!("/recipes/sam/{slug}/suggestions/{number}").as_str())
    );

    // Saved, and still unfinished: the person pressed Save and not Ready.
    assert!(held[0]["title"].as_str().unwrap().starts_with("WIP:"));
    assert_eq!(
        stored(&world, &slug, held[0]["head"]["sha"].as_str().unwrap()).await,
        changed
    );
    assert_eq!(stored(&world, &slug, "main").await, published);
}
