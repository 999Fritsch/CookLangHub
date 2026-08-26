//! Acceptance tests for editing a Recipe and publishing a new Version.
//!
//! Every test drives the real editor against a real Forgejo and a real Git,
//! and then asks Forgejo what actually landed in the Recipe. The cases that
//! hide a fault are the ones about History: exactly one Version for one
//! publication, the source kept byte for byte, a `main` that moved while a
//! person wrote, and a change that Git cannot join.

mod support;

use base64::Engine;
use cooklanghub::secret::Secret;
use cooklanghub::session::COOKIE_NAME;

/// A Recipe that is long enough for two people to change different parts.
const DISH: &str = "Chop the @onion{1}.

Fry it in a #pan{} until it is soft.

Add @salt{1%g} and @pepper{1%g}.

Pour in @water{500%ml}.

Wait ~{20%minutes}.

Serve.
";

/// A signed-in owner and a signed-in Reader against one application.
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

fn cookie(session: &str) -> String {
    format!("{COOKIE_NAME}={session}")
}

/// Open the editor and give back the status and the page.
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

/// Post the editor form.
async fn publish(
    app: &support::TestApp,
    session: &str,
    slug: &str,
    base_version: &str,
    source: &str,
    note: &str,
) -> reqwest::Response {
    support::client()
        .post(app.url(&format!("/recipes/sam/{slug}/edit")))
        .header("cookie", cookie(session))
        .form(&[
            ("base_version", base_version),
            ("source", source),
            ("note", note),
        ])
        .send()
        .await
        .expect("cannot post the editor form")
}

/// What Forgejo actually stores, byte for byte.
async fn stored(world: &World, slug: &str) -> String {
    let bytes = reqwest::Client::new()
        .get(format!(
            "{}/api/v1/repos/sam/{slug}/raw/recipe.cook",
            world.forgejo.base_url
        ))
        .header("Authorization", format!("token {}", world.token.expose()))
        .send()
        .await
        .expect("cannot read the Recipe file")
        .bytes()
        .await
        .expect("cannot read the body");

    String::from_utf8(bytes.to_vec()).expect("the stored file must be UTF-8")
}

/// Every published Version, newest first.
async fn versions(world: &World, slug: &str) -> Vec<String> {
    let commits = support::forgejo_api(
        &world.forgejo,
        &world.token,
        &format!("/repos/sam/{slug}/commits"),
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
async fn the_editor_opens_with_the_stored_source_and_a_preview() {
    let world = ready().await;
    let slug = a_recipe(&world, "Onion Base", DISH).await;

    let (status, body) = open_editor(&world.app, &world.session, &slug).await;
    assert_eq!(status, 200);

    // The Cooklang the person wrote is in the editor.
    assert!(body.contains("@onion{1}"), "got: {body:.800}");
    assert!(body.contains("title: Onion Base"));

    // The rendered Recipe is beside it, from the first byte of the page.
    assert!(body.contains("ingredient-badge"), "the preview must render");
    assert!(body.contains("cookware-badge"));
    assert!(body.contains("timer-badge"));

    // The Version the person starts from travels with the form.
    let base = field(&body, "base_version");
    assert_eq!(base.len(), 40, "a Version identifier, got `{base}`");

    // CodeMirror is served from this host and nothing runs inline, so the
    // Content Security Policy stays `default-src 'self'`.
    assert!(body.contains("<script src=\"/static/js/editor.js\" defer></script>"));
    assert!(!body.contains("onclick="), "no inline handler is allowed");
    assert!(!body.contains("<style"), "no inline style is allowed");
    assert!(
        !body.contains("<script>"),
        "every script must be a served file"
    );

    // The words a person reads are cooking words.
    let lower = body.to_lowercase();
    for word in ["commit", "branch", "pull request", "rebase"] {
        assert!(!lower.contains(word), "the editor must not say `{word}`");
    }
}

#[tokio::test]
async fn a_publication_creates_exactly_one_version_and_uses_the_change_note() {
    let world = ready().await;
    let slug = a_recipe(&world, "One Version", DISH).await;

    let (_, page) = open_editor(&world.app, &world.session, &slug).await;
    let base = field(&page, "base_version");

    let edited = stored(&world, &slug)
        .await
        .replace("@onion{1}", "@onion{2}");

    let response = publish(
        &world.app,
        &world.session,
        &slug,
        &base,
        &edited,
        "More onion",
    )
    .await;

    assert_eq!(
        response.status(),
        303,
        "a publication returns to the Recipe"
    );
    assert_eq!(
        response
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok()),
        Some(format!("/recipes/sam/{slug}").as_str())
    );

    let history = versions(&world, &slug).await;
    assert_eq!(
        history.len(),
        2,
        "one publication makes one Version, got {history:?}"
    );
    assert_eq!(
        history[0], "More onion",
        "the change note is the description"
    );

    assert!(stored(&world, &slug).await.contains("@onion{2}"));
}

#[tokio::test]
async fn an_empty_change_note_gets_a_written_message() {
    let world = ready().await;
    let slug = a_recipe(&world, "Quiet Cook", DISH).await;

    let (_, page) = open_editor(&world.app, &world.session, &slug).await;
    let base = field(&page, "base_version");
    let edited = stored(&world, &slug)
        .await
        .replace("@salt{1%g}", "@salt{2%g}");

    let response = publish(&world.app, &world.session, &slug, &base, &edited, "   ").await;
    assert_eq!(response.status(), 303);

    let history = versions(&world, &slug).await;
    assert_eq!(history.len(), 2);
    assert_eq!(
        history[0], "Update Quiet Cook",
        "an empty note gets a written message, got {history:?}"
    );
}

#[tokio::test]
async fn the_source_is_kept_byte_for_byte_apart_from_the_change() {
    let world = ready().await;

    // Spacing, blank lines, and trailing spaces are the person's own. A
    // reformat would move all of them.
    let odd = "Chop the   @onion{1}.  \n\n\n   Fry it slowly.\n\nServe.";
    let slug = a_recipe(&world, "Kept Exactly", odd).await;

    let before = stored(&world, &slug).await;
    let (_, page) = open_editor(&world.app, &world.session, &slug).await;
    let base = field(&page, "base_version");

    let after = format!("{before}\nEat.\n");
    let response = publish(&world.app, &world.session, &slug, &base, &after, "Eat too").await;
    assert_eq!(response.status(), 303);

    assert_eq!(
        stored(&world, &slug).await,
        after,
        "only what the person typed may differ"
    );
}

#[tokio::test]
async fn the_line_endings_a_browser_adds_do_not_rewrite_the_recipe() {
    // A form sends every line break as CR LF. Storing that would rewrite
    // every line of a file that holds LF, and History would show the whole
    // Recipe as changed.
    let world = ready().await;
    let slug = a_recipe(&world, "Line Endings", "Chop the @onion{1}.\n\nServe.").await;

    let (_, page) = open_editor(&world.app, &world.session, &slug).await;
    let base = field(&page, "base_version");

    let plain = stored(&world, &slug).await.replace("Serve.", "Serve hot.");
    let as_a_browser_sends_it = plain.replace('\n', "\r\n");

    let response = publish(
        &world.app,
        &world.session,
        &slug,
        &base,
        &as_a_browser_sends_it,
        "Hot",
    )
    .await;
    assert_eq!(response.status(), 303);

    let result = stored(&world, &slug).await;
    assert!(!result.contains('\r'), "no carriage return may be stored");
    assert_eq!(result, plain);
}

#[tokio::test]
async fn a_cooklang_error_stops_the_publication() {
    let world = ready().await;
    let slug = a_recipe(&world, "Refused Edit", DISH).await;

    let before = stored(&world, &slug).await;
    let (_, page) = open_editor(&world.app, &world.session, &slug).await;
    let base = field(&page, "base_version");

    let broken = format!("{before}\nWait ~{{5%bananas}}.\n");
    let response = publish(&world.app, &world.session, &slug, &base, &broken, "Oops").await;

    assert_eq!(response.status(), 200, "the editor comes back");
    let body = response.text().await.expect("cannot read the body");
    assert!(body.contains("was not published"), "got: {body:.600}");
    assert!(body.to_lowercase().contains("bananas"));
    // The person keeps what they wrote.
    assert!(body.contains("bananas"));

    assert_eq!(
        versions(&world, &slug).await.len(),
        1,
        "a refused publication makes no Version"
    );
    assert_eq!(stored(&world, &slug).await, before);
}

#[tokio::test]
async fn a_cooklang_warning_does_not_stop_the_publication() {
    let world = ready().await;
    let slug = a_recipe(&world, "Warns A Bit", DISH).await;

    let (_, page) = open_editor(&world.app, &world.session, &slug).await;
    let base = field(&page, "base_version");

    // A value the parser cannot read warns, and the person decides whether
    // it matters.
    let warned = stored(&world, &slug)
        .await
        .replace("title: Warns A Bit", "title: Warns A Bit\nservings: many");
    assert!(
        !cooklanghub::recipe::parse(&warned).warnings.is_empty(),
        "this fixture is meant to warn"
    );
    assert!(cooklanghub::recipe::parse(&warned).is_valid());

    let response = publish(&world.app, &world.session, &slug, &base, &warned, "Many").await;
    assert_eq!(response.status(), 303, "a warning must not stop publishing");

    assert_eq!(versions(&world, &slug).await.len(), 2);
    assert!(stored(&world, &slug).await.contains("servings: many"));
}

#[tokio::test]
async fn a_recipe_that_moved_while_the_person_wrote_is_joined_by_git() {
    let world = ready().await;
    let slug = a_recipe(&world, "Moved Under Me", DISH).await;

    let (_, page) = open_editor(&world.app, &world.session, &slug).await;
    let base = field(&page, "base_version");
    let opened_with = stored(&world, &slug).await;

    // Somebody publishes a change to the end of the Recipe while this
    // person is still writing at the start of it.
    somebody_else_publishes(&world, &slug, &opened_with.replace("Serve.", "Serve warm.")).await;

    let mine = opened_with.replace("@onion{1}", "@onion{4}");
    let response = publish(
        &world.app,
        &world.session,
        &slug,
        &base,
        &mine,
        "More onion",
    )
    .await;
    assert_eq!(
        response.status(),
        303,
        "Git can join changes to different parts"
    );

    let result = stored(&world, &slug).await;
    assert!(result.contains("@onion{4}"), "my change must survive");
    assert!(
        result.contains("Serve warm."),
        "the other change must survive: {result}"
    );

    let history = versions(&world, &slug).await;
    assert_eq!(
        history.len(),
        3,
        "one publication still makes one Version, got {history:?}"
    );
    assert_eq!(history[0], "More onion");
}

#[tokio::test]
async fn a_change_that_cannot_be_joined_leaves_the_published_recipe_alone() {
    let world = ready().await;
    let slug = a_recipe(&world, "Same Line", DISH).await;

    let (_, page) = open_editor(&world.app, &world.session, &slug).await;
    let base = field(&page, "base_version");
    let opened_with = stored(&world, &slug).await;

    // Both people change the same line, in different ways.
    let theirs = opened_with.replace("@onion{1}", "@onion{9}");
    somebody_else_publishes(&world, &slug, &theirs).await;

    let mine = opened_with.replace("@onion{1}", "@onion{2}");
    let response = publish(&world.app, &world.session, &slug, &base, &mine, "Mine").await;

    assert_eq!(response.status(), 200, "the editor comes back");
    let body = response.text().await.expect("cannot read the body");
    assert!(body.contains("was not published"), "got: {body:.600}");
    assert!(
        body.contains("Somebody else published a change"),
        "the person needs a diagnosis: {body:.600}"
    );
    assert!(
        body.contains("Open in Forgejo"),
        "a state the interface cannot handle offers Forgejo"
    );
    // Their text is still in front of them.
    assert!(body.contains("@onion{2}"));

    // The published Recipe is exactly what the other person left.
    assert_eq!(stored(&world, &slug).await, theirs);
    let history = versions(&world, &slug).await;
    assert_eq!(
        history.len(),
        2,
        "a conflict adds no Version, got {history:?}"
    );
}

#[tokio::test]
async fn a_reader_publishes_no_version_and_is_sent_to_a_suggestion() {
    let world = ready().await;
    let slug = a_recipe(&world, "Read Only", DISH).await;
    let before = stored(&world, &slug).await;

    // Kim can read this public Recipe. Forgejo gives Kim no write access.
    let reader = support::sign_in(&world.app, &world.forgejo, "kim").await;
    let suggestion = format!("/recipes/sam/{slug}/suggest");

    // The work of a Reader is not lost and is not published either: it
    // becomes a Suggestion.
    let response = support::client()
        .get(world.app.url(&format!("/recipes/sam/{slug}/edit")))
        .header("cookie", cookie(&reader))
        .send()
        .await
        .expect("cannot reach the editor");
    assert_eq!(response.status(), 303, "a Reader must not reach the editor");
    assert_eq!(
        response
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok()),
        Some(suggestion.as_str()),
        "a Reader must be sent to their Suggestion"
    );

    // The refusal is not only in the page. The publish route asks Forgejo
    // again, so a request that never passed through the editor publishes
    // nothing either.
    let response = publish(
        &world.app,
        &reader,
        &slug,
        "0000000000000000000000000000000000000000",
        "Chop the @onion{99}.",
        "Sneaky",
    )
    .await;
    assert_eq!(response.status(), 303);
    assert_eq!(
        response
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok()),
        Some(suggestion.as_str())
    );

    assert_eq!(versions(&world, &slug).await.len(), 1);
    assert_eq!(stored(&world, &slug).await, before);
}

#[tokio::test]
async fn the_preview_renders_the_source_and_needs_a_session() {
    let world = ready().await;

    let anonymous = support::client()
        .post(world.app.url("/recipes/preview"))
        .form(&[("source", "Chop the @onion{1}.")])
        .send()
        .await
        .expect("cannot reach the preview");
    assert_eq!(anonymous.status(), 401, "the preview needs a session");

    let response = support::client()
        .post(world.app.url("/recipes/preview"))
        .header("cookie", cookie(&world.session))
        .form(&[(
            "source",
            "---\ntitle: Live\n---\n\nChop the @onion{2} in a #pan{}.",
        )])
        .send()
        .await
        .expect("cannot reach the preview");
    assert_eq!(response.status(), 200);

    let body = response.text().await.expect("cannot read the body");
    assert!(body.contains("ingredient-badge"), "got: {body:.600}");
    assert!(body.contains("cookware-badge"));
    assert!(body.contains("onion"));

    // The preview renders text that somebody typed, so markup in it stays
    // text and can never run.
    let dangerous = support::client()
        .post(world.app.url("/recipes/preview"))
        .header("cookie", cookie(&world.session))
        .form(&[(
            "source",
            "---\ntitle: T\n---\n\nAdd @<script>alert(1)</script>{1}.",
        )])
        .send()
        .await
        .expect("cannot reach the preview")
        .text()
        .await
        .expect("cannot read the body");

    assert!(!dangerous.contains("<script>alert(1)</script>"));
    assert!(!dangerous.contains("<script>"), "got: {dangerous:.600}");
    // The template escapes every value, so the marks arrive as characters.
    // Which numeric or named form it writes is the business of the
    // template engine, and both are the same text to a reader.
    assert!(
        dangerous.contains("&#60;script&#62;") || dangerous.contains("&lt;script&gt;"),
        "the marks must arrive escaped: {dangerous:.600}"
    );
}

#[tokio::test]
async fn the_recipe_page_offers_the_editor_and_signing_in_comes_first() {
    let world = ready().await;
    let slug = a_recipe(&world, "Has An Edit Action", DISH).await;

    let page = support::client()
        .get(world.app.url(&format!("/recipes/sam/{slug}")))
        .header("cookie", cookie(&world.session))
        .send()
        .await
        .expect("cannot reach the Recipe page")
        .text()
        .await
        .expect("cannot read the body");

    assert!(
        page.contains(&format!("/recipes/sam/{slug}/edit")),
        "the Recipe page must offer the editor"
    );

    let anonymous = support::client()
        .get(world.app.url(&format!("/recipes/sam/{slug}/edit")))
        .send()
        .await
        .expect("cannot reach the editor");

    assert_eq!(anonymous.status(), 303);
    assert_eq!(
        anonymous
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok()),
        Some("/auth/sign-in")
    );
}
