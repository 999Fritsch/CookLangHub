//! Acceptance tests for a Cookbook that follows a Recipe.
//!
//! Every test drives the real pages against a real Forgejo, and asks Forgejo
//! and Git what actually landed. A page that reads well proves nothing here:
//! what matters is which Version each Cookbook records, how many Versions it
//! gained, who the author of each one is, and that no Recipe changed at all.
//!
//! Where a test needs a change that this application did not make, it makes
//! that change in Forgejo itself. A Recipe that gains a Version, a Recipe
//! that loses what a Cookbook follows, and an administrator who takes the
//! access of the automation away are all outside events, and that is exactly
//! how they arrive here.

mod support;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use cooklanghub::automation;
use cooklanghub::secret::Secret;
use cooklanghub::session::COOKIE_NAME;

use support::{Forgejo, TestApp};

/// The Forgejo administrator that the bootstrap command uses.
const ADMIN: &str = "alex";
/// The person who owns the Cookbooks and the Recipes in these tests.
const OWNER: &str = "sam";
/// Somebody else, who owns nothing here.
const OTHER: &str = "robin";
/// The ordinary Forgejo account that this installation automates with.
const BOT: &str = "cooklanghub-bot";

/// What a person types into the create form. The application writes the
/// title into the source itself.
const SOURCE: &str = "Add @salt{1%pinch} to the #pan{}.";

/// A Version that somebody pushes outside this application. It carries the
/// whole file, title and all, exactly as a Git client sends it.
const CHANGED: &str = "---\ntitle: Chili\n---\n\nAdd @salt{2%pinch} to the #pan{}.";
const CHANGED_AGAIN: &str =
    "---\ntitle: Chili\n---\n\nAdd @salt{3%pinch} to the #pan{} and wait ~{5%minutes}.";
/// A source that the parser refuses: a timer needs a unit of time.
const BROKEN: &str = "---\ntitle: Chili\n---\n\nAdd @salt{1%pinch}. Wait ~{5%bananas}.";

/// Everything that a test starts from.
struct Ready {
    forgejo: Forgejo,
    app: TestApp,
    /// The session cookie of Sam, who owns the Cookbooks.
    sam: String,
    /// The session cookie of Robin, who owns nothing here.
    robin: String,
    /// An access token of Alex, who administers the installation and can
    /// therefore ask Forgejo about anything a test made.
    admin: Secret<String>,
    /// An access token of Sam, for a change that Sam makes outside this
    /// application.
    sam_token: Secret<String>,
}

/// Start everything, and register the automation account.
///
/// An administrator makes the account in Forgejo and gives this application
/// one access token for it. That is the whole registration: the application
/// asks Forgejo who the token belongs to and records the answer.
async fn ready() -> Ready {
    let forgejo = support::start_forgejo().await;
    forgejo.create_user(ADMIN, true);
    forgejo.create_user(OWNER, false);
    forgejo.create_user(OTHER, false);
    forgejo.create_user(BOT, false);

    let admin = forgejo.access_token(ADMIN);
    let sam_token = forgejo.access_token(OWNER);
    let bot_token = forgejo.access_token(BOT);

    let app = support::start_app(&forgejo.base_url).await;
    app.bootstrap(&admin).await;

    let recorded = automation::record(&app.pool, &app.cipher, &app.forgejo, &bot_token)
        .await
        .expect("cannot register the automation account");
    assert_eq!(
        recorded.login, BOT,
        "Forgejo must say who the credential belongs to"
    );

    let sam = support::sign_in(&app, &forgejo, OWNER).await;
    let robin = support::sign_in(&app, &forgejo, OTHER).await;

    Ready {
        forgejo,
        app,
        sam,
        robin,
        admin,
        sam_token,
    }
}

// -------------------------------------------------------------- the pages

async fn get(app: &TestApp, path: &str, session: Option<&str>) -> reqwest::Response {
    let mut request = support::client().get(app.url(path));
    if let Some(session) = session {
        request = request.header("cookie", format!("{COOKIE_NAME}={session}"));
    }
    request.send().await.expect("cannot reach the page")
}

/// Read a page and insist that it answered.
async fn page(app: &TestApp, path: &str, session: Option<&str>) -> String {
    let response = get(app, path, session).await;
    assert_eq!(response.status(), 200, "GET {path} answered wrongly");
    response.text().await.expect("the page has no body")
}

async fn post(
    app: &TestApp,
    path: &str,
    session: Option<&str>,
    fields: &[(&str, &str)],
) -> reqwest::Response {
    let mut request = support::client().post(app.url(path)).form(fields);
    if let Some(session) = session {
        request = request.header("cookie", format!("{COOKIE_NAME}={session}"));
    }
    request.send().await.expect("cannot post the form")
}

/// Make a Recipe, and insist that it was made.
async fn recipe(app: &TestApp, session: &str, title: &str) {
    let response = support::create_recipe(app, session, title, SOURCE, false).await;
    assert_eq!(
        response.status(),
        303,
        "the Recipe `{title}` was not created"
    );
}

/// Make a Cookbook, and insist that it was made.
async fn cookbook(app: &TestApp, session: &str, title: &str) {
    let response = support::create_cookbook(app, session, title, "Some words.", false).await;
    assert_eq!(
        response.status(),
        303,
        "the Cookbook `{title}` was not created"
    );
}

/// Put a Recipe into a Cookbook through the page that a person uses.
async fn add(app: &TestApp, session: &str, book: &str, recipe: &str, holding: &str) {
    let response = post(
        app,
        &format!("/cookbooks/{book}/recipes"),
        Some(session),
        &[("recipe", recipe), ("holding", holding)],
    )
    .await;
    assert_eq!(
        response.status(),
        303,
        "`{recipe}` was not added to `{book}`"
    );
}

/// Change how a Cookbook holds one Recipe.
async fn hold(
    app: &TestApp,
    session: Option<&str>,
    book: &str,
    path: &str,
    holding: &str,
) -> reqwest::Response {
    post(
        app,
        &format!("/cookbooks/{book}/recipes/holding"),
        session,
        &[("path", path), ("holding", holding)],
    )
    .await
}

/// Change how a Cookbook holds one Recipe, and insist that it happened.
async fn hold_ok(app: &TestApp, session: &str, book: &str, path: &str, holding: &str) {
    let response = hold(app, Some(session), book, path, holding).await;
    assert_eq!(
        response.status(),
        303,
        "`{path}` in `{book}` was not changed to `{holding}`"
    );
}

// --------------------------------------------------------- what Forgejo has

/// The Version that `main` points at, as Forgejo reports it.
async fn version(forgejo: &Forgejo, token: &Secret<String>, path: &str) -> String {
    let branch =
        support::forgejo_api(forgejo, token, &format!("/repos/{path}/branches/main")).await;
    branch["commit"]["id"]
        .as_str()
        .expect("Forgejo reported no Version")
        .to_string()
}

/// The Versions of a repository, newest first.
async fn commits(forgejo: &Forgejo, token: &Secret<String>, path: &str) -> Vec<serde_json::Value> {
    support::forgejo_api(forgejo, token, &format!("/repos/{path}/commits"))
        .await
        .as_array()
        .expect("Forgejo reported no History")
        .clone()
}

/// How many Versions a repository has.
async fn versions(forgejo: &Forgejo, token: &Secret<String>, path: &str) -> usize {
    commits(forgejo, token, path).await.len()
}

/// The file names at the top of a repository, as Forgejo reports them.
async fn root_names(forgejo: &Forgejo, token: &Secret<String>, path: &str) -> Vec<String> {
    let contents = support::forgejo_api(forgejo, token, &format!("/repos/{path}/contents")).await;
    let mut names: Vec<String> = contents
        .as_array()
        .expect("contents must be a list")
        .iter()
        .map(|entry| entry["name"].as_str().unwrap_or_default().to_string())
        .collect();
    names.sort();
    names
}

/// The exact Version that a Cookbook records for one Recipe.
///
/// This reads the tree that Git itself stores, so nothing of this
/// application is between the test and the answer.
async fn recorded(forgejo: &Forgejo, token: &Secret<String>, path: &str, name: &str) -> String {
    let head = version(forgejo, token, path).await;
    let tree =
        support::forgejo_api(forgejo, token, &format!("/repos/{path}/git/trees/{head}")).await;

    let entry = tree["tree"]
        .as_array()
        .expect("the tree must be a list")
        .iter()
        .find(|entry| entry["path"] == name)
        .unwrap_or_else(|| panic!("`{path}` records nothing at `{name}`"))
        .clone();

    assert_eq!(
        entry["mode"], "160000",
        "a Recipe is held by reference and never copied in"
    );

    entry["sha"]
        .as_str()
        .expect("the reference records no Version")
        .to_string()
}

/// The file that names each Recipe of a Cookbook, as text.
async fn modules(forgejo: &Forgejo, token: &Secret<String>, path: &str) -> String {
    let (status, bytes) = support::forgejo_raw(
        forgejo,
        token,
        &format!("/{path}/raw/{}", cooklanghub::cookbook::MODULES_FILE),
    )
    .await;
    assert!(status.is_success(), "`{path}` holds no reference file");
    String::from_utf8_lossy(&bytes).to_string()
}

/// Who Forgejo says may work on a repository.
async fn collaborators(forgejo: &Forgejo, token: &Secret<String>, path: &str) -> Vec<String> {
    support::forgejo_api(forgejo, token, &format!("/repos/{path}/collaborators"))
        .await
        .as_array()
        .expect("the collaborators must be a list")
        .iter()
        .map(|user| user["login"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// The identifier Forgejo gave a repository.
async fn repository_id(forgejo: &Forgejo, token: &Secret<String>, path: &str) -> i64 {
    support::forgejo_api(forgejo, token, &format!("/repos/{path}")).await["id"]
        .as_i64()
        .expect("a repository has an identifier")
}

// ---------------------------------------------------- what somebody else does

/// Publish a new Version of a Recipe outside this application.
///
/// A person can push to their own Recipe with any Git client, and the parser
/// is not asked about what they push. That is exactly the state that a
/// Cookbook which follows the Recipe has to handle.
async fn publish_outside(forgejo: &Forgejo, token: &Secret<String>, path: &str, source: &str) {
    let existing = support::forgejo_api(
        forgejo,
        token,
        &format!("/repos/{path}/contents/recipe.cook"),
    )
    .await;
    let sha = existing["sha"]
        .as_str()
        .expect("the file has no identifier");

    let response = support::forgejo_write(
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

    assert!(
        response.status().is_success(),
        "cannot publish a Version outside the application: {}",
        response.status()
    );
}

/// Tell the application what Forgejo tells it: a Recipe gained a Version.
///
/// A container cannot reach a listener on the loopback address of the host,
/// so no test waits for a real delivery. The body and the signature are the
/// ones Forgejo sends, and the application cannot tell the difference.
async fn report_push(app: &TestApp, owner: &str, slug: &str, id: i64) {
    let message = serde_json::json!({
        "ref": "refs/heads/main",
        "repository": {
            "id": id,
            "name": slug,
            "full_name": format!("{owner}/{slug}"),
            "owner": { "login": owner },
        },
    })
    .to_string();

    let response = app.deliver_webhook("push", &message).await;
    assert_eq!(
        response.status(),
        202,
        "Forgejo must be told that the message was acted on"
    );
}

/// Where a piece of text sits in a page, so that a test can compare two.
fn position(body: &str, needle: &str) -> usize {
    body.find(needle)
        .unwrap_or_else(|| panic!("the page does not hold `{needle}`"))
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
/// Whole words only. `Sharing` is an area of a Recipe and must not be read
/// as the identifier that Git uses, so the page is split into words first.
fn assert_cooking_words(html: &str, page: &str) {
    let words = visible(html).to_lowercase();

    for phrase in ["pull request", "merge request"] {
        assert!(
            !words.contains(phrase),
            "the {page} page says `{phrase}` to a cook"
        );
    }

    let spoken: std::collections::HashSet<&str> = words
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();

    for forge_word in [
        "commit",
        "commits",
        "branch",
        "branches",
        "submodule",
        "submodules",
        "gitlink",
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
            "the {page} page says `{forge_word}` to a cook"
        );
    }
}

// ------------------------------------------------- Pinned and Following

#[tokio::test]
async fn switching_between_pinned_and_following_moves_the_recipe_and_then_keeps_it() {
    let Ready {
        forgejo,
        app,
        sam,
        robin: _robin,
        admin,
        sam_token,
    } = ready().await;

    recipe(&app, &sam, "Chili").await;
    cookbook(&app, &sam, "Sunday Dinners").await;

    let first = version(&forgejo, &admin, "sam/chili").await;
    add(&app, &sam, "sam/sunday-dinners", "sam/chili", "pinned").await;

    let recipe_id = repository_id(&forgejo, &admin, "sam/chili").await;

    // The Recipe moves on. A Pinned Cookbook must not.
    publish_outside(&forgejo, &sam_token, "sam/chili", CHANGED).await;
    let second = version(&forgejo, &admin, "sam/chili").await;
    assert_ne!(first, second, "the Recipe must have a new Version");

    report_push(&app, "sam", "chili", recipe_id).await;

    assert_eq!(
        recorded(&forgejo, &admin, "sam/sunday-dinners", "chili").await,
        first,
        "a Pinned Recipe must never move when its Recipe publishes a Version"
    );
    assert_eq!(
        versions(&forgejo, &admin, "sam/sunday-dinners").await,
        2,
        "a Pinned Recipe must make no Cookbook Version"
    );

    // Following means current and future, so the change moves the Cookbook
    // to the Version the Recipe has now.
    hold_ok(&app, &sam, "sam/sunday-dinners", "chili", "following").await;

    assert_eq!(
        recorded(&forgejo, &admin, "sam/sunday-dinners", "chili").await,
        second,
        "a change to Following must move the Recipe to the Version it has now"
    );
    assert_eq!(
        versions(&forgejo, &admin, "sam/sunday-dinners").await,
        3,
        "the change makes exactly one Cookbook Version"
    );

    let file = modules(&forgejo, &admin, "sam/sunday-dinners").await;
    assert!(
        file.contains("branch = main"),
        "a Cookbook that follows a Recipe names what it follows: {file}"
    );

    let body = page(&app, "/cookbooks/sam/sunday-dinners", Some(&sam)).await;
    assert!(body.contains("Follows updates"), "got: {body:.6000}");

    // The Recipe moves on again, and the Cookbook follows it.
    publish_outside(&forgejo, &sam_token, "sam/chili", BROKEN).await;
    let third = version(&forgejo, &admin, "sam/chili").await;
    report_push(&app, "sam", "chili", recipe_id).await;

    assert_eq!(
        recorded(&forgejo, &admin, "sam/sunday-dinners", "chili").await,
        third,
        "a Following Recipe must move when its Recipe publishes a Version"
    );

    // Pinned means stop where this Cookbook is. The Version it holds is the
    // Version it keeps, and the Recipe is not read for it at all.
    let held = recorded(&forgejo, &admin, "sam/sunday-dinners", "chili").await;
    hold_ok(&app, &sam, "sam/sunday-dinners", "chili", "pinned").await;

    assert_eq!(
        recorded(&forgejo, &admin, "sam/sunday-dinners", "chili").await,
        held,
        "a change to Pinned must keep the Version that the Cookbook holds"
    );

    let file = modules(&forgejo, &admin, "sam/sunday-dinners").await;
    assert!(
        !file.contains("branch"),
        "a Pinned Recipe follows nothing: {file}"
    );

    // And it stays where it is when the Recipe moves again.
    publish_outside(&forgejo, &sam_token, "sam/chili", CHANGED_AGAIN).await;
    report_push(&app, "sam", "chili", recipe_id).await;

    assert_eq!(
        recorded(&forgejo, &admin, "sam/sunday-dinners", "chili").await,
        held,
        "a Pinned Recipe must never move again"
    );

    let body = page(&app, "/cookbooks/sam/sunday-dinners", Some(&sam)).await;
    assert!(body.contains("This version"), "got: {body:.6000}");
}

// ------------------------------------------------------- the automatic move

#[tokio::test]
async fn a_followed_recipe_advances_the_cookbook_and_history_shows_who_did_it() {
    let Ready {
        forgejo,
        app,
        sam,
        robin: _robin,
        admin,
        sam_token,
    } = ready().await;

    recipe(&app, &sam, "Chili").await;
    cookbook(&app, &sam, "Sunday Dinners").await;
    add(&app, &sam, "sam/sunday-dinners", "sam/chili", "following").await;

    let recipe_id = repository_id(&forgejo, &admin, "sam/chili").await;
    let before = version(&forgejo, &admin, "sam/chili").await;
    let recipe_versions = versions(&forgejo, &admin, "sam/chili").await;
    let recipe_files = root_names(&forgejo, &admin, "sam/chili").await;

    publish_outside(&forgejo, &sam_token, "sam/chili", CHANGED).await;
    let after = version(&forgejo, &admin, "sam/chili").await;

    report_push(&app, "sam", "chili", recipe_id).await;

    // The Cookbook moved, and it moved to exactly the new Version.
    assert_eq!(
        recorded(&forgejo, &admin, "sam/sunday-dinners", "chili").await,
        after,
        "the Cookbook must follow the Recipe to its new Version"
    );
    assert_ne!(after, before);

    // Exactly one Cookbook Version. One to make the Cookbook, one to add
    // the Recipe, one for the move.
    assert_eq!(
        versions(&forgejo, &admin, "sam/sunday-dinners").await,
        3,
        "each automatic move makes exactly one Cookbook Version"
    );

    // The Recipe itself was not written to. Not one Version, not one file.
    assert_eq!(version(&forgejo, &admin, "sam/chili").await, after);
    assert_eq!(
        versions(&forgejo, &admin, "sam/chili").await,
        recipe_versions + 1,
        "only the Version that the person published may be there"
    );
    assert_eq!(
        root_names(&forgejo, &admin, "sam/chili").await,
        recipe_files
    );

    // The automation is the author. Nobody has their name on a change they
    // did not make: not Sam, who published the Recipe, and not Alex.
    let newest = commits(&forgejo, &admin, "sam/sunday-dinners").await;
    let newest = newest.first().expect("the Cookbook has no Version").clone();

    assert_eq!(
        newest["commit"]["author"]["name"].as_str(),
        Some(BOT),
        "the automation must be the author of an automatic Version"
    );
    assert_eq!(
        newest["commit"]["author"]["email"].as_str(),
        Some("cooklanghub-bot@noreply.localhost"),
        "the address must be the one Forgejo gives the automation account"
    );
    assert_eq!(
        newest["commit"]["committer"]["name"].as_str(),
        Some(BOT),
        "the automation must have made the Version as well as written it"
    );
    for person in [OWNER, ADMIN, OTHER] {
        assert_ne!(
            newest["commit"]["author"]["name"].as_str(),
            Some(person),
            "`{person}` must not have their name on a change they did not make"
        );
    }

    // The same message again changes nothing. Forgejo repeats a message
    // that failed, and a Cookbook that is already there must not gain a
    // second Version for it.
    report_push(&app, "sam", "chili", recipe_id).await;
    assert_eq!(
        versions(&forgejo, &admin, "sam/sunday-dinners").await,
        3,
        "a repeated message must make no Version"
    );

    // History shows every Version, and gives the automatic one less weight.
    let history = page(&app, "/cookbooks/sam/sunday-dinners/history", Some(&sam)).await;

    assert!(
        history.contains("Update Chili"),
        "History must show the automatic Version: {history:.8000}"
    );
    assert!(
        history.contains("Add Chili"),
        "History must show the Version that a person made"
    );
    assert!(
        history.contains(BOT),
        "History must name who made the automatic Version"
    );
    assert_eq!(
        history.matches("Automatic").count(),
        1,
        "exactly one Version here is automatic"
    );

    // The mark belongs to the automatic Version, which is the newest one.
    assert!(
        position(&history, "Update Chili") < position(&history, "Automatic")
            && position(&history, "Automatic") < position(&history, "Add Chili"),
        "the mark must sit with the Version the automation made"
    );

    // A Version a person made keeps the warm card; an automatic one gets
    // the plain one, so automation stays visible and does not fill the eye.
    assert!(
        history.contains("bg-gray-50 rounded-xl"),
        "an automatic Version must carry less weight: {history:.8000}"
    );
    assert!(
        history.contains("from-gray-50 to-orange-50 rounded-xl"),
        "a Version that a person made must keep the card the rest of the application uses"
    );

    // Anybody who can read a public Cookbook can read its History.
    let visitor = page(&app, "/cookbooks/sam/sunday-dinners/history", None).await;
    assert!(visitor.contains("Update Chili"));
    assert!(visitor.contains("Automatic"));

    // A cook reads cooking words, on both pages and in both states.
    assert_cooking_words(&history, "Cookbook History");
    let body = page(&app, "/cookbooks/sam/sunday-dinners", Some(&sam)).await;
    assert!(body.contains("Follows updates"));
    assert_cooking_words(&body, "Cookbook");
}

#[tokio::test]
async fn a_followed_recipe_that_is_not_valid_cooklang_still_advances() {
    let Ready {
        forgejo,
        app,
        sam,
        robin: _robin,
        admin,
        sam_token,
    } = ready().await;

    recipe(&app, &sam, "Chili").await;
    cookbook(&app, &sam, "Sunday Dinners").await;
    add(&app, &sam, "sam/sunday-dinners", "sam/chili", "following").await;

    let recipe_id = repository_id(&forgejo, &admin, "sam/chili").await;

    // The published Recipe is what a Cookbook follows, and the parser is
    // not asked about it. A person who pushes a source that this
    // application would refuse still gets a Cookbook that follows.
    publish_outside(&forgejo, &sam_token, "sam/chili", BROKEN).await;
    let broken = version(&forgejo, &admin, "sam/chili").await;

    report_push(&app, "sam", "chili", recipe_id).await;

    assert_eq!(
        recorded(&forgejo, &admin, "sam/sunday-dinners", "chili").await,
        broken,
        "Following follows the published Recipe and not the parser"
    );
    assert_eq!(versions(&forgejo, &admin, "sam/sunday-dinners").await, 3);

    // The Recipe itself is diagnosed, exactly as it was before.
    let body = page(&app, "/recipes/sam/chili", Some(&sam)).await;
    assert!(body.contains("This Recipe is broken"), "got: {body:.4000}");

    // And it keeps advancing afterwards.
    publish_outside(&forgejo, &sam_token, "sam/chili", CHANGED).await;
    let repaired = version(&forgejo, &admin, "sam/chili").await;
    report_push(&app, "sam", "chili", recipe_id).await;

    assert_eq!(
        recorded(&forgejo, &admin, "sam/sunday-dinners", "chili").await,
        repaired,
    );
    assert_eq!(versions(&forgejo, &admin, "sam/sunday-dinners").await, 4);
}

// ------------------------------------------------------- what stops it

#[tokio::test]
async fn following_stops_with_a_diagnostic_when_the_recipe_loses_what_it_follows() {
    let Ready {
        forgejo,
        app,
        sam,
        robin: _robin,
        admin,
        sam_token: _sam_token,
    } = ready().await;

    recipe(&app, &sam, "Chili").await;
    cookbook(&app, &sam, "Sunday Dinners").await;
    add(&app, &sam, "sam/sunday-dinners", "sam/chili", "following").await;

    let recipe_id = repository_id(&forgejo, &admin, "sam/chili").await;
    let held = recorded(&forgejo, &admin, "sam/sunday-dinners", "chili").await;

    // Somebody moves the Recipe onto another branch in Forgejo and removes
    // the one the Cookbook follows. Forgejo allows it, and this application
    // never does it.
    for (method, path, body) in [
        (
            reqwest::Method::POST,
            "/repos/sam/chili/branches".to_string(),
            serde_json::json!({ "new_branch_name": "spare" }),
        ),
        (
            reqwest::Method::PATCH,
            "/repos/sam/chili".to_string(),
            serde_json::json!({ "default_branch": "spare" }),
        ),
        (
            reqwest::Method::DELETE,
            "/repos/sam/chili/branches/main".to_string(),
            serde_json::json!({}),
        ),
    ] {
        let response = support::forgejo_write(&forgejo, &admin, method, &path, body).await;
        assert!(
            response.status().is_success(),
            "cannot change the Recipe in Forgejo: {path} answered {}",
            response.status()
        );
    }

    report_push(&app, "sam", "chili", recipe_id).await;

    // Nothing moved, and nothing else was selected.
    assert_eq!(
        recorded(&forgejo, &admin, "sam/sunday-dinners", "chili").await,
        held,
        "the Cookbook keeps the Version it holds"
    );
    assert_eq!(
        versions(&forgejo, &admin, "sam/sunday-dinners").await,
        2,
        "a Recipe with nothing to follow must make no Cookbook Version"
    );

    let file = modules(&forgejo, &admin, "sam/sunday-dinners").await;
    assert!(
        file.contains("branch = main"),
        "the application must not select something else to follow: {file}"
    );
    assert!(
        !file.contains("spare"),
        "the application must not select something else to follow: {file}"
    );

    // The person is told, on the page where they can act on it.
    let body = page(&app, "/cookbooks/sam/sunday-dinners", Some(&sam)).await;
    assert!(
        body.contains(automation::NOTHING_TO_FOLLOW_MESSAGE),
        "the state must be named: {body:.8000}"
    );
    assert!(
        body.contains("Follows updates"),
        "the Recipe is still a Recipe that this Cookbook follows"
    );
    assert!(body.contains("Open in Forgejo"));

    // A diagnostic is read by a cook, so it uses cooking words as well.
    assert_cooking_words(&body, "Cookbook");
}

#[tokio::test]
async fn removing_the_automation_access_stops_it_and_it_is_never_given_again() {
    let Ready {
        forgejo,
        app,
        sam,
        robin: _robin,
        admin,
        sam_token,
    } = ready().await;

    recipe(&app, &sam, "Chili").await;
    cookbook(&app, &sam, "Sunday Dinners").await;
    add(&app, &sam, "sam/sunday-dinners", "sam/chili", "following").await;

    let recipe_id = repository_id(&forgejo, &admin, "sam/chili").await;
    assert!(
        collaborators(&forgejo, &admin, "sam/sunday-dinners")
            .await
            .iter()
            .any(|login| login == BOT),
        "a Cookbook that follows a Recipe gives the automation what it needs"
    );

    // An administrator takes the access away in Forgejo.
    let removed = support::forgejo_write(
        &forgejo,
        &admin,
        reqwest::Method::DELETE,
        &format!("/repos/sam/sunday-dinners/collaborators/{BOT}"),
        serde_json::json!({}),
    )
    .await;
    assert!(
        removed.status().is_success(),
        "the access was not taken away: {}",
        removed.status()
    );

    let held = recorded(&forgejo, &admin, "sam/sunday-dinners", "chili").await;

    publish_outside(&forgejo, &sam_token, "sam/chili", CHANGED).await;
    report_push(&app, "sam", "chili", recipe_id).await;

    // The automation stopped.
    assert_eq!(
        recorded(&forgejo, &admin, "sam/sunday-dinners", "chili").await,
        held,
        "the automation must not write to a Cookbook it may not write to"
    );
    assert_eq!(versions(&forgejo, &admin, "sam/sunday-dinners").await, 2);

    // The person is told, and reading the page gives nothing back.
    let body = page(&app, "/cookbooks/sam/sunday-dinners", Some(&sam)).await;
    assert!(
        body.contains(automation::NO_ACCESS_MESSAGE),
        "the state must be named: {body:.8000}"
    );

    assert!(
        !collaborators(&forgejo, &admin, "sam/sunday-dinners")
            .await
            .iter()
            .any(|login| login == BOT),
        "the application must not give the access again"
    );

    // An installation with no automation account at all says so as well.
    sqlx::query("DELETE FROM automation")
        .execute(&app.pool)
        .await
        .expect("cannot remove the automation credential");

    report_push(&app, "sam", "chili", recipe_id).await;
    assert_eq!(
        versions(&forgejo, &admin, "sam/sunday-dinners").await,
        2,
        "an installation with no automation account writes nothing"
    );

    let body = page(&app, "/cookbooks/sam/sunday-dinners", Some(&sam)).await;
    assert!(
        body.contains(automation::NO_CREDENTIAL_MESSAGE),
        "the state must be named: {body:.8000}"
    );
    assert_cooking_words(&body, "Cookbook");
}

// ------------------------------------------------------------ the access

#[tokio::test]
async fn the_automation_reaches_only_a_cookbook_that_follows_a_recipe() {
    let Ready {
        forgejo,
        app,
        sam,
        robin: _robin,
        admin,
        sam_token: _sam_token,
    } = ready().await;

    recipe(&app, &sam, "Chili").await;
    recipe(&app, &sam, "Toast").await;
    cookbook(&app, &sam, "Sunday Dinners").await;
    cookbook(&app, &sam, "Weeknights").await;

    add(&app, &sam, "sam/sunday-dinners", "sam/chili", "following").await;
    add(&app, &sam, "sam/weeknights", "sam/toast", "pinned").await;

    let holds_the_automation = async |path: &str| {
        collaborators(&forgejo, &admin, path)
            .await
            .iter()
            .any(|login| login == BOT)
    };

    assert!(
        holds_the_automation("sam/sunday-dinners").await,
        "a Cookbook that follows a Recipe needs the automation"
    );
    assert!(
        !holds_the_automation("sam/weeknights").await,
        "a Cookbook that follows nothing must not give the automation anything"
    );

    // The last Recipe stops following, so the access goes with it.
    hold_ok(&app, &sam, "sam/sunday-dinners", "chili", "pinned").await;
    assert!(
        !holds_the_automation("sam/sunday-dinners").await,
        "a Cookbook that follows nothing any more must take the access back"
    );

    // And it comes again when a Recipe follows again.
    hold_ok(&app, &sam, "sam/weeknights", "toast", "following").await;
    assert!(
        holds_the_automation("sam/weeknights").await,
        "a Cookbook that starts to follow gives the automation what it needs"
    );

    // Taking the Recipe out takes the access with it.
    let response = post(
        &app,
        "/cookbooks/sam/weeknights/recipes/remove",
        Some(&sam),
        &[("path", "toast")],
    )
    .await;
    assert_eq!(response.status(), 303);
    assert!(
        !holds_the_automation("sam/weeknights").await,
        "a Cookbook with no Recipes follows nothing"
    );
}

#[tokio::test]
async fn only_a_person_who_can_change_a_cookbook_can_change_how_it_holds_a_recipe() {
    let Ready {
        forgejo,
        app,
        sam,
        robin,
        admin,
        sam_token: _sam_token,
    } = ready().await;

    recipe(&app, &sam, "Chili").await;
    cookbook(&app, &sam, "Sunday Dinners").await;
    add(&app, &sam, "sam/sunday-dinners", "sam/chili", "pinned").await;

    let held = recorded(&forgejo, &admin, "sam/sunday-dinners", "chili").await;

    // Robin can read the Cookbook and can change nothing in it.
    let body = page(&app, "/cookbooks/sam/sunday-dinners", Some(&robin)).await;
    assert!(body.contains("/recipes/sam/chili"));
    assert!(
        !body.contains("recipes/holding"),
        "the page must not offer an action that Forgejo refuses"
    );

    assert_eq!(
        hold(
            &app,
            Some(&robin),
            "sam/sunday-dinners",
            "chili",
            "following"
        )
        .await
        .status(),
        403
    );

    // A visitor with no account is sent to sign in and changes nothing.
    assert_eq!(
        hold(&app, None, "sam/sunday-dinners", "chili", "following")
            .await
            .status(),
        303
    );

    assert_eq!(
        recorded(&forgejo, &admin, "sam/sunday-dinners", "chili").await,
        held,
        "a refused change must write nothing"
    );
    assert_eq!(versions(&forgejo, &admin, "sam/sunday-dinners").await, 2);

    let file = modules(&forgejo, &admin, "sam/sunday-dinners").await;
    assert!(!file.contains("branch"), "got: {file}");

    // The owner is offered the action, and it works.
    let owner = page(&app, "/cookbooks/sam/sunday-dinners", Some(&sam)).await;
    assert!(owner.contains("Follow updates"), "got: {owner:.8000}");
}
