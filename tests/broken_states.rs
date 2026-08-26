//! Acceptance tests for the Recipe states that the interface cannot cook.
//!
//! Git is a supported way to change a Recipe, so every state here is made
//! the way a person makes it: a real Git client pushes to the real Forgejo
//! that the application talks to. Nothing is simulated, because the whole
//! point of these states is that they arrive from outside the application.
//!
//! Two things hide a fault, so both are asserted again and again. Nothing
//! this application does may correct a state on its own, and nothing it does
//! may remove a file that somebody put beside the Recipe.

mod support;

use std::path::{Path, PathBuf};
use std::process::Command;

use cooklanghub::secret::Secret;
use cooklanghub::session::COOKIE_NAME;
use reqwest::Method;
use serde_json::json;

/// The Recipe that most of these tests start from.
const GOOD: &str = "Chop the @onion{1}.

Fry it in a #pan{} for ~{5%minutes}.
";

/// Cooklang that the parser refuses. `bananas` is not a unit of time.
const BROKEN: &str = "---
title: Chili
---

Wait ~{5%bananas}.
";

/// Files that a person keeps beside the Recipe. The application understands
/// none of them, and it may never remove one.
const EXTRAS: [(&str, &str); 4] = [
    ("notes.md", "Grandmother wrote this on a card.\n"),
    (".gitignore", "*.tmp\n"),
    ("menu/sunday.cook", "Serve the @chili{}.\n"),
    ("photos/holiday.txt", "The picture from the holiday.\n"),
];

struct World {
    forgejo: support::Forgejo,
    app: support::TestApp,
    /// The session of `sam`, who owns every Recipe in these tests.
    session: String,
    /// The credential that the test itself asks Forgejo questions with, and
    /// that the Git client in these tests pushes with.
    token: Secret<String>,
}

async fn ready() -> World {
    let forgejo = support::start_forgejo().await;
    forgejo.create_user("alex", true);
    forgejo.create_user("sam", false);
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

/// Read a page and give back what it says.
async fn page(world: &World, session: Option<&str>, path: &str) -> String {
    read(world, session, path)
        .await
        .text()
        .await
        .expect("cannot read the body")
}

/// Post a form that carries no field, exactly as the page sends it.
async fn post(world: &World, session: Option<&str>, path: &str) -> reqwest::Response {
    let mut request = support::client().post(world.app.url(path));
    if let Some(session) = session {
        request = request.header("cookie", cookie(session));
    }
    request
        .header("content-type", "application/x-www-form-urlencoded")
        .body("")
        .send()
        .await
        .expect("cannot post the form")
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

/// Drop every element from a page, leaving the words a person reads.
fn words(html: &str) -> String {
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

/// The words of the forge, which no page a person reads may say.
const FORGE_WORDS: [&str; 10] = [
    "commit",
    "branch",
    "diff",
    "repository",
    "fork",
    "patch",
    "head",
    "sha",
    "merge",
    "rebase",
];

/// Whether a text says one of the words of the forge.
///
/// Whole words only. `Sharing` is an area of a Recipe, so it must not be
/// read as the identifier that Git uses.
fn says_forge_word(text: &str) -> Option<&'static str> {
    let lower = text.to_lowercase();

    if lower.contains("pull request") {
        return Some("pull request");
    }

    let spoken: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();

    FORGE_WORDS.into_iter().find(|word| spoken.contains(word))
}

/// Assert that a diagnosis reads as a diagnosis has to.
///
/// Every one of them names the state, offers **Open in Forgejo**, promises
/// that nothing was corrected, and uses cooking words only.
fn assert_diagnosis(body: &str, heading: &str) {
    assert!(body.contains(heading), "the page must say `{heading}`");
    assert!(
        body.contains("Open in Forgejo"),
        "every diagnosis carries Open in Forgejo"
    );
    assert!(
        body.contains(cooklanghub::recipe_state::UNTOUCHED_MESSAGE),
        "every diagnosis says that nothing was corrected"
    );
    assert!(
        body.contains("role=\"alert\""),
        "the state must be announced"
    );

    let spoken = words(body);
    assert_eq!(
        says_forge_word(&spoken),
        None,
        "a word of the forge reached the person"
    );
}

// ---------------------------------------------------------------------
// A Git client, the way a person outside this application has one
// ---------------------------------------------------------------------

/// A clone of a Recipe that a person drives with Git.
///
/// Git reads its configuration from a home directory inside the workspace,
/// so nothing on the machine that runs the test can change what happens.
struct GitClient {
    /// Held so that the workspace outlives the test.
    _workspace: tempfile::TempDir,
    home: PathBuf,
    root: PathBuf,
}

fn run(home: &Path, directory: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(directory)
        .args(args)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "Sam Cook")
        .env("GIT_AUTHOR_EMAIL", "sam@example.test")
        .env("GIT_COMMITTER_NAME", "Sam Cook")
        .env("GIT_COMMITTER_EMAIL", "sam@example.test")
        .output()
        .expect("cannot run Git");

    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    String::from_utf8_lossy(&output.stdout).to_string()
}

impl GitClient {
    /// Clone a Recipe, the way a person with a Git client does.
    fn open(world: &World, slug: &str) -> Self {
        let workspace = tempfile::tempdir().expect("cannot make a workspace");
        let home = workspace.path().join("home");
        std::fs::create_dir_all(&home).expect("cannot make the home directory");

        // Forgejo takes a personal access token as the password over HTTP.
        let remote =
            world
                .forgejo
                .base_url
                .replacen("://", &format!("://sam:{}@", world.token.expose()), 1);
        let remote = format!("{remote}/sam/{slug}.git");

        run(
            &home,
            workspace.path(),
            &["clone", "--quiet", &remote, "work"],
        );

        Self {
            root: workspace.path().join("work"),
            home,
            _workspace: workspace,
        }
    }

    fn git(&self, args: &[&str]) -> String {
        run(&self.home, &self.root, args)
    }

    fn write(&self, name: &str, bytes: &[u8]) {
        let file = self.root.join(name);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).expect("cannot make the folder");
        }
        std::fs::write(file, bytes).expect("cannot write the file");
    }

    fn remove(&self, name: &str) {
        std::fs::remove_file(self.root.join(name)).expect("cannot remove the file");
    }

    /// Record everything and send it, the way a direct change arrives.
    fn publish(&self, message: &str) {
        self.git(&["add", "--all"]);
        self.git(&["commit", "--quiet", "--message", message]);
        self.git(&["push", "--quiet", "origin", "HEAD:main"]);
    }
}

// ---------------------------------------------------------------------
// What Forgejo actually holds
// ---------------------------------------------------------------------

/// The bytes of one file of a Recipe, straight out of Forgejo.
async fn stored(world: &World, slug: &str, path: &str) -> (reqwest::StatusCode, Vec<u8>) {
    support::forgejo_raw(
        &world.forgejo,
        &world.token,
        &format!("/sam/{slug}/raw/{path}?ref=main"),
    )
    .await
}

/// How many published Versions the Recipe holds.
async fn versions(world: &World, slug: &str) -> usize {
    support::forgejo_api(
        &world.forgejo,
        &world.token,
        &format!("/repos/sam/{slug}/commits?sha=main&limit=50"),
    )
    .await
    .as_array()
    .map(Vec::len)
    .unwrap_or_default()
}

/// Make the Recipe that a test starts from.
async fn a_recipe(world: &World, title: &str, source: &str) {
    let created = support::create_recipe(&world.app, &world.session, title, source, false).await;
    assert_eq!(created.status(), 303, "the Recipe was not created");
}

/// Put the files that a person keeps beside a Recipe into it.
fn add_extras(client: &GitClient) {
    for (name, content) in EXTRAS {
        client.write(name, content.as_bytes());
    }
    client.publish("Keep my notes beside the Recipe");
}

/// Assert that every extra file is still there, byte for byte.
async fn assert_extras_survived(world: &World, slug: &str, after: &str) {
    for (name, content) in EXTRAS {
        let (status, bytes) = stored(world, slug, name).await;
        assert!(
            status.is_success(),
            "`{name}` is gone after {after}: {status}"
        );
        assert_eq!(
            String::from_utf8_lossy(&bytes),
            content,
            "`{name}` changed after {after}"
        );
    }
}

// ---------------------------------------------------------------------
// A direct change is an ordinary Version
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_direct_change_appears_as_a_new_version() {
    let world = ready().await;
    a_recipe(&world, "Chili", GOOD).await;

    let client = GitClient::open(&world, "chili");
    client.write(
        "recipe.cook",
        b"---\ntitle: Chili\n---\n\nChop the @leek{2} in a #pot{}.\n",
    );
    client.publish("Use a leek instead");

    // The Recipe page shows what Git holds now, and not what the
    // application last wrote.
    let body = page(&world, Some(&world.session), "/recipes/sam/chili").await;
    assert!(body.contains("leek"), "the change must show: {body:.400}");
    assert!(!body.contains("onion"), "the old Recipe must be gone");

    // And History carries it as a Version of its own, with the words the
    // person wrote about it.
    let history = page(&world, Some(&world.session), "/recipes/sam/chili/history").await;
    assert!(
        history.contains("Use a leek instead"),
        "the direct change must appear as a Version"
    );
    assert_eq!(versions(&world, "chili").await, 2);
}

// ---------------------------------------------------------------------
// Invalid Cooklang
// ---------------------------------------------------------------------

#[tokio::test]
async fn invalid_cooklang_shows_as_a_broken_recipe_with_every_recovery_option() {
    let world = ready().await;
    a_recipe(&world, "Chili", GOOD).await;

    let client = GitClient::open(&world, "chili");
    client.write("recipe.cook", BROKEN.as_bytes());
    client.publish("Wait for bananas");

    let body = page(&world, Some(&world.session), "/recipes/sam/chili").await;
    assert_diagnosis(&body, "This Recipe is broken");

    // The raw source, exactly as it is stored.
    assert!(
        body.contains("Wait ~{5%bananas}."),
        "the source must be offered"
    );
    // What the parser found, so the person knows where to look.
    assert!(
        body.contains("bananas"),
        "the diagnosis must name what the parser refused"
    );
    // The last valid Version.
    assert!(
        body.contains("Read the last valid Version"),
        "the last valid Version must be offered"
    );
    // The repair. It publishes a Version, so it is a form and never a link.
    assert!(
        body.contains("Repair this Recipe"),
        "an Editor must be offered a repair"
    );
    assert!(
        body.contains("/restore\""),
        "the repair must be a form that posts"
    );

    // A broken Recipe is not cooked. The page must not pretend otherwise.
    assert!(
        !body.contains("step-number"),
        "a Recipe that cannot be read must not be drawn as one"
    );

    // A person who is not signed in reads the same diagnosis and gets no
    // repair, because Forgejo says they may not write.
    let anonymous = page(&world, None, "/recipes/sam/chili").await;
    assert_diagnosis(&anonymous, "This Recipe is broken");
    assert!(!anonymous.contains("Repair this Recipe"));
    assert!(anonymous.contains("Read the last valid Version"));
}

#[tokio::test]
async fn a_repair_happens_only_when_a_person_asks_for_it() {
    let world = ready().await;
    a_recipe(&world, "Chili", GOOD).await;

    let client = GitClient::open(&world, "chili");
    client.write("recipe.cook", BROKEN.as_bytes());
    client.publish("Wait for bananas");

    let after_the_change = versions(&world, "chili").await;
    assert_eq!(after_the_change, 2);

    // Reading the diagnosis, over and over, corrects nothing.
    for _ in 0..3 {
        let body = page(&world, Some(&world.session), "/recipes/sam/chili").await;
        assert!(body.contains("This Recipe is broken"));
    }

    let (_, held) = stored(&world, "chili", "recipe.cook").await;
    assert_eq!(
        String::from_utf8_lossy(&held),
        BROKEN,
        "reading a broken Recipe must never rewrite it"
    );
    assert_eq!(
        versions(&world, "chili").await,
        after_the_change,
        "reading a broken Recipe must add no Version"
    );

    // Now a person presses the repair. The address of it comes off the
    // page, so the test exercises what the page really offers.
    let body = page(&world, Some(&world.session), "/recipes/sam/chili").await;
    let action = body
        .split("<form method=\"post\" action=\"")
        .find(|part| part.starts_with("/recipes/sam/chili/history/"))
        .and_then(|part| part.split('"').next())
        .expect("the page must carry the repair form");

    let repaired = post(&world, Some(&world.session), action).await;
    assert_eq!(repaired.status(), 303, "the repair must publish a Version");

    // The repair added a Version and removed none. The broken Version is
    // still in History, because History is never rewritten.
    assert_eq!(versions(&world, "chili").await, 3);

    let (_, now) = stored(&world, "chili", "recipe.cook").await;
    assert!(
        String::from_utf8_lossy(&now).contains("onion"),
        "the repair must publish the last valid Recipe"
    );

    let body = page(&world, Some(&world.session), "/recipes/sam/chili").await;
    assert!(
        body.contains("step-number"),
        "the Recipe must be readable again"
    );
    assert!(!body.contains("This Recipe is broken"));
}

// ---------------------------------------------------------------------
// A Recipe with no Recipe file
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_recipe_without_a_recipe_file_gets_a_diagnostic() {
    let world = ready().await;
    a_recipe(&world, "Chili", GOOD).await;

    let client = GitClient::open(&world, "chili");
    add_extras(&client);
    client.remove("recipe.cook");
    client.publish("Take the Recipe file away");

    let body = page(&world, Some(&world.session), "/recipes/sam/chili").await;
    assert_diagnosis(&body, "This Recipe has no Recipe file");
    assert!(
        body.contains("recipe.cook"),
        "the diagnosis must name the file that is missing"
    );

    // There is an earlier Version that can be read, so the two options that
    // depend on one are offered.
    assert!(body.contains("Read the last valid Version"));
    assert!(body.contains("Repair this Recipe"));

    // Nothing was written back.
    let (status, _) = stored(&world, "chili", "recipe.cook").await;
    assert_eq!(
        status, 404,
        "the application must not write a Recipe file on its own"
    );
    assert_eq!(versions(&world, "chili").await, 3);

    // And the files that the person keeps beside the Recipe are untouched.
    assert_extras_survived(&world, "chili", "a diagnosis").await;
}

// ---------------------------------------------------------------------
// A Recipe that is not text
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_recipe_that_is_not_text_gets_a_diagnostic_and_keeps_its_bytes() {
    let world = ready().await;
    a_recipe(&world, "Chili", GOOD).await;

    // Git stores any bytes. These are not UTF-8, so no reader can show them
    // as a Recipe.
    let bytes: Vec<u8> = vec![0xFF, 0xFE, 0x00, 0x41, 0x80, 0x9F, 0x0A];

    let client = GitClient::open(&world, "chili");
    client.write("recipe.cook", &bytes);
    client.publish("Store something that is not text");

    let body = page(&world, Some(&world.session), "/recipes/sam/chili").await;
    assert_diagnosis(&body, "This Recipe is not text");
    assert!(
        body.contains("replacement mark"),
        "the diagnosis must explain what the marks are"
    );
    assert!(
        body.contains("Recipe source"),
        "the source must still be offered, as far as it can be read"
    );
    assert!(body.contains("Read the last valid Version"));

    // Every byte is exactly as it was pushed.
    let (status, held) = stored(&world, "chili", "recipe.cook").await;
    assert!(status.is_success());
    assert_eq!(held, bytes, "the application must change no byte");
    assert_eq!(versions(&world, "chili").await, 2);
}

// ---------------------------------------------------------------------
// A Recipe with no `main`
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_recipe_without_a_main_branch_gets_a_diagnostic_and_no_guess() {
    let world = ready().await;
    a_recipe(&world, "Chili", GOOD).await;

    let client = GitClient::open(&world, "chili");
    client.git(&["push", "--quiet", "origin", "main:kitchen"]);

    // Forgejo refuses to remove the published place while it is the one it
    // points at, so that moves first. Both are ordinary Forgejo operations
    // that a person is allowed to make.
    let moved = support::forgejo_write(
        &world.forgejo,
        &world.token,
        Method::PATCH,
        "/repos/sam/chili",
        json!({ "default_branch": "kitchen" }),
    )
    .await;
    assert!(moved.status().is_success(), "{}", moved.status());

    client.git(&["push", "--quiet", "origin", "--delete", "main"]);

    let body = page(&world, Some(&world.session), "/recipes/sam/chili").await;
    assert_diagnosis(&body, "This Recipe has no published Version");

    // The Recipe still exists somewhere else, and the application must not
    // reach for it. Guessing is exactly the fault this state exists to
    // prevent.
    assert!(
        !body.contains("onion"),
        "the application must not select another Version on its own"
    );
    assert!(
        !body.contains("step-number"),
        "nothing may be drawn as the published Recipe"
    );

    // Nothing was put back.
    let restored = support::forgejo_write(
        &world.forgejo,
        &world.token,
        Method::GET,
        "/repos/sam/chili/branches/main",
        json!({}),
    )
    .await;
    assert_eq!(
        restored.status(),
        404,
        "the application must not create the published place again"
    );
}

// ---------------------------------------------------------------------
// Two photos
// ---------------------------------------------------------------------

/// The smallest bytes that each format is recognised by.
fn jpeg() -> Vec<u8> {
    let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xE0];
    bytes.resize(64, 0x42);
    bytes
}

fn png() -> Vec<u8> {
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    bytes.resize(64, 0x43);
    bytes
}

#[tokio::test]
async fn two_photos_are_an_ambiguous_state_and_nothing_is_selected() {
    let world = ready().await;
    a_recipe(&world, "Chili", GOOD).await;

    let client = GitClient::open(&world, "chili");
    client.write("recipe.jpg", &jpeg());
    client.write("recipe.png", &png());
    client.publish("Two pictures of the same dish");

    let body = page(&world, Some(&world.session), "/recipes/sam/chili").await;

    // The Recipe itself is readable, so it is still cooked. The photo is
    // the part that is ambiguous.
    assert!(
        body.contains("step-number"),
        "the Recipe must still be read"
    );
    assert!(
        body.contains("more than one photo"),
        "the state must be named: {body:.400}"
    );
    assert!(
        body.contains("Open in Forgejo"),
        "the diagnosis must offer the escape hatch"
    );

    // No photo is shown, because no rule may decide which of the two is
    // meant. The form that puts a photo on the Recipe stays, because that
    // is how the owner resolves the state deliberately.
    assert!(
        !body.contains("<img src=\"/recipes/sam/chili/thumbnail\""),
        "the page must select no photo"
    );
    let served = read(&world, Some(&world.session), "/recipes/sam/chili/thumbnail").await;
    assert_eq!(
        served.status(),
        404,
        "an ambiguous photo must not be served"
    );

    // Reading the page several times removes neither of them.
    for _ in 0..3 {
        let _ = page(&world, Some(&world.session), "/recipes/sam/chili").await;
    }

    for name in ["recipe.jpg", "recipe.png"] {
        let (status, _) = stored(&world, "chili", name).await;
        assert!(status.is_success(), "`{name}` must still be there");
    }
    assert_eq!(versions(&world, "chili").await, 2);
}

// ---------------------------------------------------------------------
// A file above the friendly limit
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_recipe_file_over_the_friendly_limit_gets_a_safe_state() {
    let world = ready().await;
    a_recipe(&world, "Chili", GOOD).await;

    // Two megabytes of Cooklang. The friendly limit is one.
    let huge = format!(
        "---\ntitle: Chili\n---\n\n{}\n",
        "Chop it. ".repeat(240_000)
    );
    assert!(huge.len() > 2 * 1024 * 1024);

    let client = GitClient::open(&world, "chili");
    client.write("recipe.cook", huge.as_bytes());
    client.publish("A very long Recipe");

    let response = read(&world, Some(&world.session), "/recipes/sam/chili").await;
    assert_eq!(response.status(), 200);
    let body = response.text().await.expect("cannot read the body");

    assert_diagnosis(&body, "This Recipe is too large to show");
    assert!(body.contains("larger than 1 MB"));

    // Safe means the page stays a page. A megabyte of Cooklang must not be
    // drawn into it.
    assert!(
        body.len() < 100_000,
        "the file must stay off the page, got {} bytes",
        body.len()
    );
    assert!(!body.contains("Chop it. Chop it. Chop it."));

    // Every byte of the file is still there. The application changed
    // nothing, and it removed nothing.
    let (status, held) = stored(&world, "chili", "recipe.cook").await;
    assert!(status.is_success());
    assert_eq!(held.len(), huge.len(), "the file must keep every byte");
    assert_eq!(versions(&world, "chili").await, 2);
}

// ---------------------------------------------------------------------
// Extra files
// ---------------------------------------------------------------------

#[tokio::test]
async fn extra_files_survive_every_operation() {
    let world = ready().await;
    a_recipe(&world, "Chili", GOOD).await;

    let client = GitClient::open(&world, "chili");
    add_extras(&client);
    assert_extras_survived(&world, "chili", "a direct change").await;

    // One: publishing a Version through the editor.
    let editor = page(&world, Some(&world.session), "/recipes/sam/chili/edit").await;
    let base = field(&editor, "base_version");
    let published = support::client()
        .post(world.app.url("/recipes/sam/chili/edit"))
        .header("cookie", cookie(&world.session))
        .form(&[
            ("base_version", base.as_str()),
            ("source", "---\ntitle: Chili\n---\n\nChop the @leek{2}.\n"),
            ("note", "Use a leek"),
        ])
        .send()
        .await
        .expect("cannot post the editor form");
    assert_eq!(published.status(), 303, "the Version was not published");
    assert_extras_survived(&world, "chili", "an edit").await;

    // Two: putting a photo on the Recipe.
    let form =
        reqwest::multipart::Form::new().part("thumbnail", support::file_part("photo.jpg", jpeg()));
    let stored_photo = support::post_form(
        &world.app,
        &world.session,
        "/recipes/sam/chili/thumbnail",
        form,
    )
    .await;
    assert_eq!(stored_photo.status(), 303, "the photo was not stored");
    assert_extras_survived(&world, "chili", "a photo").await;

    // Three: a repair, after somebody breaks the Recipe from outside.
    let client = GitClient::open(&world, "chili");
    client.write("recipe.cook", BROKEN.as_bytes());
    client.publish("Wait for bananas");
    assert_extras_survived(&world, "chili", "a direct change that broke it").await;

    let body = page(&world, Some(&world.session), "/recipes/sam/chili").await;
    let action = body
        .split("<form method=\"post\" action=\"")
        .find(|part| part.starts_with("/recipes/sam/chili/history/"))
        .and_then(|part| part.split('"').next())
        .expect("the page must carry the repair form");

    let repaired = post(&world, Some(&world.session), action).await;
    assert_eq!(repaired.status(), 303, "the repair must publish a Version");
    assert_extras_survived(&world, "chili", "a repair").await;

    // And the photo that the application itself wrote is still there too,
    // so the repair replaced the Recipe and nothing else.
    let (status, _) = stored(&world, "chili", "recipe.jpg").await;
    assert!(status.is_success(), "the photo must survive a repair");
}
