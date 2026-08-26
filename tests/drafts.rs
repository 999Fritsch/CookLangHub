//! Acceptance tests for drafts.
//!
//! Every test drives the real editor against a real Forgejo and a real Git,
//! and then asks Forgejo what actually landed. The cases that hide a fault
//! are the ones about where a draft lives and where it does not: a draft
//! that never reaches the published Recipe, a second tab that must not
//! overwrite the first, a publication that takes the draft away, and a
//! draft that nothing removes on its own.
//!
//! Nothing in these tests keeps state in a browser. Each request builds a
//! new client with no cookie store of its own, so a page that came back
//! with the work of a person can only have got it from Forgejo.

mod support;

use base64::Engine;
use cooklanghub::draft;
use cooklanghub::secret::Secret;
use cooklanghub::session::COOKIE_NAME;

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
    session: String,
    /// The credential that the test itself uses to ask Forgejo questions.
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

/// Make a Recipe and give back its slug.
async fn a_recipe(world: &World, title: &str, source: &str) -> String {
    let response = support::create_recipe(&world.app, &world.session, title, source, false).await;
    assert_eq!(response.status(), 303, "the Recipe must be created");

    response
        .headers()
        .get("location")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.rsplit('/').next())
        .expect("the redirect names the Recipe")
        .to_string()
}

/// Make a Recipe and give back its slug and what Forgejo stores for it.
///
/// Creating a Recipe writes the title a person typed into the source, so
/// the published text is not the constant above it. A test that asks
/// whether the published Recipe moved has to compare against what really
/// landed, or it measures the wrong thing.
async fn a_published_recipe(world: &World, title: &str) -> (String, String) {
    let slug = a_recipe(world, title, DISH).await;
    let source = published(world, &slug).await;
    (slug, source)
}

fn cookie(session: &str) -> String {
    format!("{COOKIE_NAME}={session}")
}

/// Open the editor and give back the status and the page.
///
/// Every call builds a new client, so the page carries nothing that an
/// earlier request left in a browser.
async fn open_editor(
    app: &support::TestApp,
    session: &str,
    slug: &str,
) -> (reqwest::StatusCode, String) {
    let response = support::client()
        .get(app.url(&format!("/recipes/sam/{slug}/edit")))
        .header("cookie", cookie(session))
        .send()
        .await
        .expect("cannot reach the editor");

    let status = response.status();
    let body = response.text().await.expect("cannot read the body");
    (status, body)
}

/// Read the value of a hidden field out of a page.
fn field(html: &str, name: &str) -> String {
    let marker = format!("name=\"{name}\"");
    let start = html
        .find(&marker)
        .unwrap_or_else(|| panic!("the page has no `{name}` field: {html:.600}"));
    let rest = &html[start..];
    let value_at = rest.find("value=\"").expect("the field carries no value") + "value=\"".len();
    let end = rest[value_at..].find('"').expect("the value never ends") + value_at;
    rest[value_at..end].to_string()
}

/// Save a draft, the way the editor does while a person writes.
async fn save(
    app: &support::TestApp,
    session: &str,
    slug: &str,
    base_version: &str,
    draft_version: &str,
    source: &str,
) -> (reqwest::StatusCode, serde_json::Value) {
    let response = support::client()
        .post(app.url(&format!("/recipes/sam/{slug}/draft")))
        .header("cookie", cookie(session))
        .form(&[
            ("source", source),
            ("base_version", base_version),
            ("draft_version", draft_version),
        ])
        .send()
        .await
        .expect("cannot save the draft");

    let status = response.status();
    let body = response.json().await.expect("the answer must be JSON");
    (status, body)
}

/// Save a draft and expect it to be kept. Gives back the draft Version.
async fn save_ok(world: &World, slug: &str, base: &str, held: &str, source: &str) -> String {
    let (status, answer) = save(&world.app, &world.session, slug, base, held, source).await;
    assert_eq!(status, 200, "the draft must be saved: {answer}");
    assert_eq!(answer["message"], draft::SAVED_MESSAGE);

    let version = answer["version"]
        .as_str()
        .expect("the answer names the draft Version")
        .to_string();
    assert_eq!(version.len(), 40, "a Version identifier, got `{version}`");
    version
}

/// Discard the draft, the way the button on the page does.
async fn discard(app: &support::TestApp, session: &str, slug: &str) -> reqwest::Response {
    support::client()
        .post(app.url(&format!("/recipes/sam/{slug}/draft/discard")))
        .header("cookie", cookie(session))
        .send()
        .await
        .expect("cannot discard the draft")
}

/// Publish, the way the editor form does.
async fn publish(
    world: &World,
    slug: &str,
    base_version: &str,
    draft_version: &str,
    source: &str,
    note: &str,
) -> reqwest::Response {
    support::client()
        .post(world.app.url(&format!("/recipes/sam/{slug}/edit")))
        .header("cookie", cookie(&world.session))
        .form(&[
            ("base_version", base_version),
            ("draft_version", draft_version),
            ("source", source),
            ("note", note),
        ])
        .send()
        .await
        .expect("cannot post the editor form")
}

/// Every branch the Recipe holds in Forgejo.
async fn branches(world: &World, slug: &str) -> Vec<String> {
    let list = support::forgejo_api(
        &world.forgejo,
        &world.token,
        &format!("/repos/sam/{slug}/branches"),
    )
    .await;

    list.as_array()
        .expect("the answer is a list")
        .iter()
        .map(|branch| branch["name"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// The branches that carry a draft.
async fn drafts(world: &World, slug: &str) -> Vec<String> {
    let mut found: Vec<String> = branches(world, slug)
        .await
        .into_iter()
        .filter(|name| name.starts_with("draft/"))
        .collect();
    found.sort();
    found
}

/// What Forgejo stores on a branch or at a Version, byte for byte.
async fn stored_at(world: &World, slug: &str, reference: &str) -> String {
    let (status, bytes) = support::forgejo_raw(
        &world.forgejo,
        &world.token,
        &format!("/sam/{slug}/raw/recipe.cook?ref={reference}"),
    )
    .await;

    assert!(status.is_success(), "cannot read `{reference}`: {status}");
    String::from_utf8(bytes).expect("the stored file must be UTF-8")
}

/// What the published Recipe holds.
async fn published(world: &World, slug: &str) -> String {
    stored_at(world, slug, "main").await
}

/// Every Version on a branch, newest first, by its description.
async fn versions_on(world: &World, slug: &str, branch: &str) -> Vec<String> {
    let commits = support::forgejo_api(
        &world.forgejo,
        &world.token,
        &format!("/repos/sam/{slug}/commits?sha={branch}"),
    )
    .await;

    commits
        .as_array()
        .expect("the answer is a list")
        .iter()
        .map(|commit| {
            commit["commit"]["message"]
                .as_str()
                .unwrap_or_default()
                .trim()
                .to_string()
        })
        .collect()
}

/// History: the Versions a person reads, which live on `main` alone.
async fn history(world: &World, slug: &str) -> Vec<String> {
    versions_on(world, slug, "main").await
}

/// Let another person write to this Recipe, the way sharing does.
async fn share_with(world: &World, slug: &str, login: &str) {
    let response = support::forgejo_write(
        &world.forgejo,
        &world.token,
        reqwest::Method::PUT,
        &format!("/repos/sam/{slug}/collaborators/{login}"),
        serde_json::json!({ "permission": "write" }),
    )
    .await;

    assert!(
        response.status().is_success(),
        "cannot share the Recipe with {login}: {}",
        response.status()
    );
}

/// Publish a change straight through the Forgejo API, as another person
/// would while this person is still writing.
async fn somebody_else_publishes(world: &World, slug: &str, source: &str) {
    let current = support::forgejo_api(
        &world.forgejo,
        &world.token,
        &format!("/repos/sam/{slug}/contents/recipe.cook"),
    )
    .await;
    let sha = current["sha"].as_str().expect("the file has an identifier");

    let response = reqwest::Client::new()
        .put(format!(
            "{}/api/v1/repos/sam/{slug}/contents/recipe.cook",
            world.forgejo.base_url
        ))
        .header("Authorization", format!("token {}", world.token.expose()))
        .json(&serde_json::json!({
            "content": base64::engine::general_purpose::STANDARD.encode(source),
            "sha": sha,
            "message": "Somebody else was faster",
            "branch": "main",
        }))
        .send()
        .await
        .expect("cannot write the file through Forgejo");

    assert!(
        response.status().is_success(),
        "the other change must land: {}",
        response.status()
    );
}

#[tokio::test]
async fn a_draft_lives_in_forgejo_and_opens_again_on_another_device() {
    let world = ready().await;
    let slug = a_recipe(&world, "Kept Work", DISH).await;

    let (_, page) = open_editor(&world.app, &world.session, &slug).await;
    let base = field(&page, "base_version");
    assert_eq!(
        field(&page, "draft_version"),
        "",
        "a Recipe with no draft carries no draft Version"
    );
    assert!(
        !page.contains("Discard draft"),
        "there is nothing to discard yet"
    );

    let written = DISH.replace("@salt{1%g}", "@salt{2%g}");
    let version = save_ok(&world, &slug, &base, "", &written).await;

    // The work is in Forgejo, and it is there under its own name.
    assert_eq!(drafts(&world, &slug).await, vec!["draft/sam".to_string()]);
    assert_eq!(stored_at(&world, &slug, "draft/sam").await, written);

    // Another device: a new client that has never seen this Recipe, with no
    // storage of any kind carried over from the one that wrote the text.
    let (status, again) = open_editor(&world.app, &world.session, &slug).await;
    assert_eq!(status, 200);
    assert!(again.contains("@salt{2%g}"), "got: {again:.800}");
    assert_eq!(field(&again, "draft_version"), version);
    assert!(again.contains(draft::NOTICE_MESSAGE));
    assert!(again.contains("Discard draft"));
    assert!(again.contains(&format!("/recipes/sam/{slug}/draft/discard")));

    // The page still needs no inline script, and it still says nothing that
    // belongs to Git.
    assert!(!again.contains("onclick="), "no inline handler is allowed");
    assert!(!again.contains("<script>"), "no inline script is allowed");
    let lower = again.to_lowercase();
    for word in ["commit", "branch", "pull request", "rebase"] {
        assert!(!lower.contains(word), "the editor must not say `{word}`");
    }
}

#[tokio::test]
async fn a_draft_never_reaches_the_published_recipe_or_its_history() {
    let world = ready().await;
    let (slug, dish) = a_published_recipe(&world, "Quiet Work").await;

    let (_, page) = open_editor(&world.app, &world.session, &slug).await;
    let base = field(&page, "base_version");

    // Several saves, because each one must stay out of History too.
    let first = save_ok(
        &world,
        &slug,
        &base,
        "",
        &dish.replace("Serve.", "Serve with bread."),
    )
    .await;
    save_ok(
        &world,
        &slug,
        &base,
        &first,
        &dish.replace("Serve.", "Serve with butter."),
    )
    .await;

    // The published Recipe is untouched.
    assert_eq!(published(&world, &slug).await, dish);

    // History holds the one Version that creating the Recipe made, and
    // nothing that the writing added.
    let history = history(&world, &slug).await;
    assert_eq!(
        history.len(),
        1,
        "History must hold one Version: {history:?}"
    );
    assert!(
        !history.iter().any(|message| message == "Draft"),
        "a draft must not appear in History: {history:?}"
    );

    // The draft is on a name of its own, and that name is not the one
    // History reads from.
    assert_eq!(drafts(&world, &slug).await, vec!["draft/sam".to_string()]);
}

#[tokio::test]
async fn one_person_has_one_draft_for_one_recipe() {
    let world = ready().await;
    let slug = a_recipe(&world, "One Draft", DISH).await;

    let (_, page) = open_editor(&world.app, &world.session, &slug).await;
    let base = field(&page, "base_version");

    let first = DISH.replace("@onion{1}", "@onion{2}");
    let second = DISH.replace("@onion{1}", "@onion{3}");
    let third = DISH.replace("@onion{1}", "@onion{4}");

    let v1 = save_ok(&world, &slug, &base, "", &first).await;
    let v2 = save_ok(&world, &slug, &base, &v1, &second).await;
    let v3 = save_ok(&world, &slug, &base, &v2, &third).await;

    assert_ne!(v1, v2);
    assert_ne!(v2, v3);

    // One draft, however long the person wrote.
    assert_eq!(drafts(&world, &slug).await, vec!["draft/sam".to_string()]);

    // And one Version in it. The draft is replaced each time rather than
    // added to, so a person who writes all evening leaves one behind.
    let on_draft = versions_on(&world, &slug, "draft/sam").await;
    assert_eq!(
        on_draft.len(),
        2,
        "the draft is one Version on the published one: {on_draft:?}"
    );

    // Only the newest text survives.
    let held = stored_at(&world, &slug, "draft/sam").await;
    assert_eq!(held, third);
    assert!(!held.contains("@onion{2}"));
    assert!(!held.contains("@onion{3}"));
}

#[tokio::test]
async fn a_save_from_a_tab_that_fell_behind_is_refused_and_says_why() {
    let world = ready().await;
    let slug = a_recipe(&world, "Two Tabs", DISH).await;

    // Two tabs, both opened before either of them wrote anything.
    let (_, tab_one) = open_editor(&world.app, &world.session, &slug).await;
    let (_, tab_two) = open_editor(&world.app, &world.session, &slug).await;
    let base = field(&tab_one, "base_version");
    assert_eq!(field(&tab_two, "draft_version"), "");

    let from_one = DISH.replace("@salt{1%g}", "@salt{5%g}");
    let from_two = DISH.replace("@salt{1%g}", "@salt{9%g}");

    let first = save_ok(&world, &slug, &base, "", &from_one).await;

    // The second tab still believes there is no draft, so it is refused.
    let (status, answer) = save(&world.app, &world.session, &slug, &base, "", &from_two).await;
    assert_eq!(status, 409, "a stale save must be refused: {answer}");
    assert_eq!(answer["message"], draft::STALE_MESSAGE);

    // The person is told what happened, why, and what they can do.
    let words = answer["message"]
        .as_str()
        .unwrap_or_default()
        .to_lowercase();
    assert!(words.contains("did not save"));
    assert!(words.contains("different tab"));
    assert!(words.contains("copy your text"));
    for word in ["commit", "branch", "push", "merge", "rebase"] {
        assert!(!words.contains(word), "the refusal must not say `{word}`");
    }

    // Nothing of the first tab was lost.
    assert_eq!(stored_at(&world, &slug, "draft/sam").await, from_one);
    let (_, reopened) = open_editor(&world.app, &world.session, &slug).await;
    assert_eq!(field(&reopened, "draft_version"), first);
    assert!(reopened.contains("@salt{5%g}"));
    assert!(!reopened.contains("@salt{9%g}"));

    // The first tab carries on, because it holds the Version that is there.
    let more = DISH.replace("@salt{1%g}", "@salt{6%g}");
    let second = save_ok(&world, &slug, &base, &first, &more).await;
    assert_ne!(first, second);

    // A save that names the Version before that one is refused as well.
    let (late, answer) = save(&world.app, &world.session, &slug, &base, &first, &from_two).await;
    assert_eq!(late, 409, "a save that fell behind must be refused");
    assert_eq!(answer["message"], draft::STALE_MESSAGE);
    assert_eq!(stored_at(&world, &slug, "draft/sam").await, more);
}

#[tokio::test]
async fn each_person_gets_a_draft_of_their_own() {
    let world = ready().await;
    let (slug, dish) = a_published_recipe(&world, "Two Cooks").await;
    share_with(&world, &slug, "kim").await;

    let kim = support::sign_in(&world.app, &world.forgejo, "kim").await;

    let (_, page) = open_editor(&world.app, &world.session, &slug).await;
    let base = field(&page, "base_version");
    let from_sam = dish.replace("@pepper{1%g}", "@pepper{2%g}");
    save_ok(&world, &slug, &base, "", &from_sam).await;

    let (_, kim_page) = open_editor(&world.app, &kim, &slug).await;
    assert_eq!(
        field(&kim_page, "draft_version"),
        "",
        "the draft of one person must not open for another"
    );
    assert!(kim_page.contains("@pepper{1%g}"));

    let kim_base = field(&kim_page, "base_version");
    let from_kim = dish.replace("@pepper{1%g}", "@pepper{7%g}");
    let (status, answer) = save(&world.app, &kim, &slug, &kim_base, "", &from_kim).await;
    assert_eq!(status, 200, "an Editor can keep a draft: {answer}");

    assert_eq!(
        drafts(&world, &slug).await,
        vec!["draft/kim".to_string(), "draft/sam".to_string()]
    );
    assert_eq!(stored_at(&world, &slug, "draft/sam").await, from_sam);
    assert_eq!(stored_at(&world, &slug, "draft/kim").await, from_kim);

    // And neither of them changed the published Recipe.
    assert_eq!(published(&world, &slug).await, dish);
    assert_eq!(history(&world, &slug).await.len(), 1);
}

#[tokio::test]
async fn a_person_who_cannot_write_cannot_keep_a_draft() {
    let world = ready().await;
    let (slug, dish) = a_published_recipe(&world, "Read Only").await;

    let kim = support::sign_in(&world.app, &world.forgejo, "kim").await;

    let (_, page) = open_editor(&world.app, &world.session, &slug).await;
    let base = field(&page, "base_version");

    let (status, answer) = save(&world.app, &kim, &slug, &base, "", "Chop the @onion{99}.\n").await;

    assert_eq!(status, 403, "a Reader cannot keep a draft: {answer}");
    assert!(drafts(&world, &slug).await.is_empty());
    assert_eq!(published(&world, &slug).await, dish);
}

#[tokio::test]
async fn a_publication_consumes_the_draft() {
    let world = ready().await;
    let slug = a_recipe(&world, "Published Draft", DISH).await;

    let (_, page) = open_editor(&world.app, &world.session, &slug).await;
    let base = field(&page, "base_version");

    let written = DISH.replace("Wait ~{20%minutes}.", "Wait ~{25%minutes}.");
    save_ok(&world, &slug, &base, "", &written).await;

    // The person opens the editor again, and publishes what it shows.
    let (_, ready_page) = open_editor(&world.app, &world.session, &slug).await;
    let response = publish(
        &world,
        &slug,
        &field(&ready_page, "base_version"),
        &field(&ready_page, "draft_version"),
        &written,
        "A longer wait",
    )
    .await;
    assert_eq!(response.status(), 303, "the Version must be published");

    // The work is published, as one Version, with the note the person wrote.
    assert_eq!(published(&world, &slug).await, written);
    let history = history(&world, &slug).await;
    assert_eq!(history.len(), 2, "one new Version: {history:?}");
    assert_eq!(history[0], "A longer wait");

    // The draft is gone.
    assert!(
        drafts(&world, &slug).await.is_empty(),
        "a publication must take the draft away"
    );

    let (_, after) = open_editor(&world.app, &world.session, &slug).await;
    assert_eq!(field(&after, "draft_version"), "");
    assert!(!after.contains("Discard draft"));
}

#[tokio::test]
async fn a_draft_keeps_the_version_it_was_started_from() {
    let world = ready().await;
    let slug = a_recipe(&world, "Moved Recipe", DISH).await;

    let (_, page) = open_editor(&world.app, &world.session, &slug).await;
    let opened_on = field(&page, "base_version");

    let written = DISH.replace("@onion{1}", "@onion{2}");
    save_ok(&world, &slug, &opened_on, "", &written).await;

    // The published Recipe moves while the draft waits.
    somebody_else_publishes(&world, &slug, &DISH.replace("Serve.", "Serve hot.")).await;

    // The editor opens on the draft, and on the Version the draft was
    // started from rather than on the one that is published now.
    let (_, again) = open_editor(&world.app, &world.session, &slug).await;
    assert_eq!(field(&again, "base_version"), opened_on);
    assert!(again.contains("@onion{2}"));

    let response = publish(
        &world,
        &slug,
        &field(&again, "base_version"),
        &field(&again, "draft_version"),
        &written,
        "More onion",
    )
    .await;
    assert_eq!(response.status(), 303);

    // Both changes are in the published Recipe, because Git had the Version
    // that the two of them started from.
    let now = published(&world, &slug).await;
    assert!(now.contains("@onion{2}"), "got: {now}");
    assert!(now.contains("Serve hot."), "got: {now}");
    assert!(drafts(&world, &slug).await.is_empty());
}

#[tokio::test]
async fn a_person_can_discard_a_draft() {
    let world = ready().await;
    let (slug, dish) = a_published_recipe(&world, "Thrown Away").await;

    let (_, page) = open_editor(&world.app, &world.session, &slug).await;
    let base = field(&page, "base_version");
    save_ok(&world, &slug, &base, "", "Chop the @onion{9}.\n").await;
    assert_eq!(drafts(&world, &slug).await.len(), 1);

    let response = discard(&world.app, &world.session, &slug).await;
    assert_eq!(response.status(), 303, "discarding takes the person back");
    assert_eq!(
        response
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok()),
        Some(format!("/recipes/sam/{slug}/edit").as_str())
    );

    assert!(drafts(&world, &slug).await.is_empty());

    // The editor opens on the published Recipe again, and the published
    // Recipe never moved.
    let (status, after) = open_editor(&world.app, &world.session, &slug).await;
    assert_eq!(status, 200);
    assert_eq!(field(&after, "draft_version"), "");
    assert!(after.contains("@onion{1}"));
    assert!(!after.contains("@onion{9}"));
    assert_eq!(published(&world, &slug).await, dish);
    assert_eq!(history(&world, &slug).await.len(), 1);

    // A second discard is not a fault. There is simply nothing to remove.
    assert_eq!(
        discard(&world.app, &world.session, &slug).await.status(),
        303
    );
}

#[tokio::test]
async fn nothing_removes_a_draft_on_its_own() {
    let world = ready().await;
    let slug = a_recipe(&world, "Left Alone", DISH).await;

    let (_, page) = open_editor(&world.app, &world.session, &slug).await;
    let base = field(&page, "base_version");
    let written = DISH.replace("Serve.", "Serve tomorrow.");
    let version = save_ok(&world, &slug, &base, "", &written).await;

    // The application is restarted, and it reads Forgejo again from the
    // start. Neither of those is a moment to remove somebody's work.
    let restarted = support::restart(&world.app).await;
    restarted.reconcile().await;

    assert_eq!(drafts(&world, &slug).await, vec!["draft/sam".to_string()]);
    assert_eq!(stored_at(&world, &slug, "draft/sam").await, written);

    let (status, after) = open_editor(&restarted, &world.session, &slug).await;
    assert_eq!(status, 200);
    assert_eq!(field(&after, "draft_version"), version);
    assert!(after.contains("Serve tomorrow."));
}
