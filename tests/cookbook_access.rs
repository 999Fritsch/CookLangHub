//! Acceptance tests for Cookbook access and for a broken reference.
//!
//! Two rules are under test here, and both of them are rules about Forgejo.
//!
//! A Cookbook and a Recipe are two repositories, so access to one is never
//! access to the other. The application shows that mismatch before it
//! happens and offers a Forgejo grant for it. Every test therefore asks
//! Forgejo what actually landed: who is a collaborator, with which access
//! mode, and what the Recipe repository looks like afterwards.
//!
//! A reference that names a Recipe which is gone, or renamed, stays exactly
//! as it is. The tests read `.gitmodules` out of Forgejo before and after,
//! and they count the Versions of the Cookbook, so no silent repair can
//! pass.

mod support;

use std::collections::HashSet;

use cooklanghub::cookbook;
use cooklanghub::secret::Secret;
use cooklanghub::session::COOKIE_NAME;

use support::{Forgejo, TestApp};

/// The Forgejo administrator that the bootstrap command uses.
const ADMIN: &str = "alex";
/// The person who owns the Cookbooks and the Recipes in these tests.
const OWNER: &str = "sam";
/// The person that a Cookbook is shared with.
const FRIEND: &str = "robin";
/// Somebody who shares nothing at all.
const STRANGER: &str = "quinn";

const SOURCE: &str = "Add @salt{1%pinch} to the #pan{}.";

/// A Recipe that everybody can read.
const OPEN_TITLE: &str = "Chili";
const OPEN_SLUG: &str = "chili";

/// A Recipe that only named people can read.
const CLOSED_TITLE: &str = "Secret Sauce";
const CLOSED_SLUG: &str = "secret-sauce";

const BOOK_TITLE: &str = "Sunday Dinners";
const BOOK_SLUG: &str = "sunday-dinners";

/// Everything that a test starts from.
struct Ready {
    forgejo: Forgejo,
    app: TestApp,
    /// The session cookie of Sam, who owns everything here.
    sam: String,
    /// The session cookie of Robin, who is shared with.
    robin: String,
    /// The session cookie of Quinn, who shares nothing.
    quinn: String,
    /// An access token of Alex, who administers the installation and can
    /// therefore ask Forgejo about anything a test made.
    admin: Secret<String>,
}

async fn ready() -> Ready {
    let forgejo = support::start_forgejo().await;
    forgejo.create_user(ADMIN, true);
    forgejo.create_user(OWNER, false);
    forgejo.create_user(FRIEND, false);
    forgejo.create_user(STRANGER, false);

    let admin = forgejo.access_token(ADMIN);

    let app = support::start_app(&forgejo.base_url).await;
    app.bootstrap(&admin).await;

    let sam = support::sign_in(&app, &forgejo, OWNER).await;
    let robin = support::sign_in(&app, &forgejo, FRIEND).await;
    let quinn = support::sign_in(&app, &forgejo, STRANGER).await;

    Ready {
        forgejo,
        app,
        sam,
        robin,
        quinn,
        admin,
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

fn location(response: &reqwest::Response) -> String {
    response
        .headers()
        .get("location")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

async fn body(response: reqwest::Response) -> String {
    response.text().await.expect("the answer has no body")
}

// ------------------------------------------------------------- the making

async fn recipe(app: &TestApp, session: &str, title: &str, private: bool) {
    let response = support::create_recipe(app, session, title, SOURCE, private).await;
    assert_eq!(
        response.status(),
        303,
        "the Recipe `{title}` was not created"
    );
}

async fn cookbook(app: &TestApp, session: &str, title: &str, private: bool) {
    let response = support::create_cookbook(app, session, title, "Some words.", private).await;
    assert_eq!(
        response.status(),
        303,
        "the Cookbook `{title}` was not created"
    );
}

/// Put a Recipe into a Cookbook, exactly as a person does.
async fn add(
    app: &TestApp,
    session: &str,
    book: &str,
    recipe: &str,
    extra: &[(&str, &str)],
) -> reqwest::Response {
    let mut fields = vec![("recipe", recipe), ("holding", "pinned")];
    fields.extend_from_slice(extra);
    post(
        app,
        &format!("/cookbooks/{book}/recipes"),
        Some(session),
        &fields,
    )
    .await
}

/// Put a Recipe into a Cookbook and insist that it landed.
async fn add_ok(app: &TestApp, session: &str, book: &str, recipe: &str, extra: &[(&str, &str)]) {
    let response = add(app, session, book, recipe, extra).await;
    assert_eq!(
        response.status(),
        303,
        "`{recipe}` was not added to `{book}`"
    );
    assert_eq!(location(&response), format!("/cookbooks/{book}"));
}

/// Share a Cookbook with a person, exactly as the Owner does.
async fn share(
    app: &TestApp,
    session: &str,
    book: &str,
    login: &str,
    role: &str,
    extra: &[(&str, &str)],
) -> reqwest::Response {
    let mut fields = vec![("login", login), ("role", role)];
    fields.extend_from_slice(extra);
    post(
        app,
        &format!("/cookbooks/{book}/sharing/people"),
        Some(session),
        &fields,
    )
    .await
}

// -------------------------------------------------------- what Forgejo has

/// The people that Forgejo records on a repository.
async fn collaborators(forgejo: &Forgejo, token: &Secret<String>, path: &str) -> Vec<String> {
    let found = support::forgejo_api(forgejo, token, &format!("/repos/{path}/collaborators")).await;
    let mut logins: Vec<String> = found
        .as_array()
        .expect("the collaborators must be a list")
        .iter()
        .map(|user| user["login"].as_str().unwrap_or_default().to_string())
        .collect();
    logins.sort();
    logins
}

/// What Forgejo says one person may do with one repository.
async fn access(forgejo: &Forgejo, token: &Secret<String>, path: &str, login: &str) -> String {
    support::forgejo_api(
        forgejo,
        token,
        &format!("/repos/{path}/collaborators/{login}/permission"),
    )
    .await["permission"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The file that names each Recipe of a Cookbook, as text.
async fn modules(forgejo: &Forgejo, token: &Secret<String>, path: &str) -> String {
    let (status, bytes) = support::forgejo_raw(
        forgejo,
        token,
        &format!("/{path}/raw/{}", cookbook::MODULES_FILE),
    )
    .await;
    assert!(status.is_success(), "`{path}` holds no reference file");
    String::from_utf8_lossy(&bytes).to_string()
}

/// How many Versions a repository has.
async fn versions(forgejo: &Forgejo, token: &Secret<String>, path: &str) -> usize {
    support::forgejo_api(forgejo, token, &format!("/repos/{path}/commits"))
        .await
        .as_array()
        .map(Vec::len)
        .expect("Forgejo reported no History")
}

// ------------------------------------------------------------- the words

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

/// The words a person reads, with every run of spaces made into one.
///
/// A template breaks a sentence over several lines, so a test that looks for
/// a sentence has to put it back together first.
fn spoken(html: &str) -> String {
    visible(html)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The words of the forge must never reach a cook.
///
/// Whole words only. `Sharing` is an area of a Cookbook and must not be read
/// as the identifier that Git uses.
fn assert_cooking_words(html: &str) {
    let words = visible(html).to_lowercase();

    for phrase in ["pull request", "merge request"] {
        assert!(
            !words.contains(phrase),
            "the page says `{phrase}` to a cook"
        );
    }

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
        "submodule",
        "submodules",
        "gitlink",
        "collaborator",
        "collaborators",
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

/// A page that names a closed Recipe says nothing that identifies it.
fn assert_says_nothing_about_the_closed_recipe(body: &str) {
    assert!(
        body.contains(cookbook::UNAVAILABLE_MESSAGE),
        "the entry must stay visible and explain itself: {body:.4000}"
    );
    for secret in [CLOSED_TITLE, CLOSED_SLUG, "/recipes/sam/secret-sauce"] {
        assert!(
            !body.contains(secret),
            "`{secret}` must never reach a person who cannot read the Recipe"
        );
    }
}

// -------------------------------------------- sharing a private Cookbook

#[tokio::test]
async fn sharing_a_private_cookbook_lists_the_recipes_that_the_person_cannot_read() {
    let Ready {
        forgejo,
        app,
        sam,
        robin: _robin,
        quinn: _quinn,
        admin,
    } = ready().await;

    recipe(&app, &sam, OPEN_TITLE, false).await;
    recipe(&app, &sam, CLOSED_TITLE, true).await;
    cookbook(&app, &sam, BOOK_TITLE, true).await;

    let book = format!("{OWNER}/{BOOK_SLUG}");
    add_ok(&app, &sam, &book, &format!("{OWNER}/{OPEN_SLUG}"), &[]).await;
    add_ok(&app, &sam, &book, &format!("{OWNER}/{CLOSED_SLUG}"), &[]).await;

    // The Owner reaches Sharing from the Cookbook itself.
    let shown = page(&app, &format!("/cookbooks/{book}"), Some(&sam)).await;
    assert!(
        shown.contains(&format!("/cookbooks/{book}/sharing")),
        "the Cookbook must lead to its Sharing area"
    );

    let answer = share(&app, &sam, &book, FRIEND, "reader", &[]).await;
    assert_eq!(
        answer.status(),
        200,
        "the access mismatch is a page and not a change"
    );

    let asked = body(answer).await;

    // The Recipe that Robin cannot read is named. The public one is not: it
    // is out of reach of nobody.
    assert!(
        asked.contains(CLOSED_TITLE),
        "the Recipe that Robin cannot read must be listed: {asked:.4000}"
    );
    assert!(
        !asked.contains(OPEN_TITLE),
        "a public Recipe is no mismatch: {asked:.4000}"
    );

    // Three answers, and every one of them is a plain form or a link.
    assert!(asked.contains("Give Reader access and share"));
    assert!(asked.contains("Share it anyway"));
    assert!(asked.contains("Cancel"));
    assert!(
        asked.contains("Access to this Cookbook is not access to its Recipes"),
        "the page must say why the mismatch exists"
    );
    assert_cooking_words(&asked);

    // Nothing happened yet. Forgejo records no change at all.
    assert_eq!(
        collaborators(&forgejo, &admin, &book).await,
        Vec::<String>::new(),
        "the person must not reach the Cookbook before they decide"
    );
    assert_eq!(
        collaborators(&forgejo, &admin, &format!("{OWNER}/{CLOSED_SLUG}")).await,
        Vec::<String>::new(),
        "no Recipe may change before the person decides"
    );
}

#[tokio::test]
async fn the_owner_can_grant_reader_access_and_forgejo_holds_the_grant() {
    let Ready {
        forgejo,
        app,
        sam,
        robin,
        quinn: _quinn,
        admin,
    } = ready().await;

    recipe(&app, &sam, CLOSED_TITLE, true).await;
    cookbook(&app, &sam, BOOK_TITLE, true).await;

    let book = format!("{OWNER}/{BOOK_SLUG}");
    let closed = format!("{OWNER}/{CLOSED_SLUG}");
    add_ok(&app, &sam, &book, &closed, &[]).await;

    let answer = share(
        &app,
        &sam,
        &book,
        FRIEND,
        "reader",
        &[("confirm", "yes"), ("grant", "yes")],
    )
    .await;
    assert_eq!(answer.status(), 303);

    // The grant is an ordinary Forgejo permission on the Recipe itself.
    assert_eq!(collaborators(&forgejo, &admin, &closed).await, vec![FRIEND]);
    assert_eq!(access(&forgejo, &admin, &closed, FRIEND).await, "read");
    assert_eq!(collaborators(&forgejo, &admin, &book).await, vec![FRIEND]);

    // Robin can now open the Recipe, and the Cookbook shows it as a Recipe.
    assert_eq!(
        get(&app, &format!("/recipes/{closed}"), Some(&robin))
            .await
            .status(),
        200
    );
    let seen = page(&app, &format!("/cookbooks/{book}"), Some(&robin)).await;
    assert!(seen.contains(CLOSED_TITLE), "got: {seen:.4000}");
    assert!(!seen.contains(cookbook::UNAVAILABLE_MESSAGE));
}

#[tokio::test]
async fn the_owner_can_share_anyway_and_the_cookbook_gives_no_recipe_access() {
    let Ready {
        forgejo,
        app,
        sam,
        robin,
        quinn: _quinn,
        admin,
    } = ready().await;

    recipe(&app, &sam, CLOSED_TITLE, true).await;
    cookbook(&app, &sam, BOOK_TITLE, true).await;

    let book = format!("{OWNER}/{BOOK_SLUG}");
    let closed = format!("{OWNER}/{CLOSED_SLUG}");
    add_ok(&app, &sam, &book, &closed, &[]).await;

    let answer = share(&app, &sam, &book, FRIEND, "reader", &[("confirm", "yes")]).await;
    assert_eq!(answer.status(), 303);

    // Robin reaches the Cookbook and nothing else. This is the criterion:
    // Cookbook access alone is no Recipe access.
    assert_eq!(collaborators(&forgejo, &admin, &book).await, vec![FRIEND]);
    assert_eq!(
        collaborators(&forgejo, &admin, &closed).await,
        Vec::<String>::new(),
        "sharing a Cookbook must change no Recipe"
    );
    assert_eq!(access(&forgejo, &admin, &closed, FRIEND).await, "none");

    assert_eq!(
        get(&app, &format!("/recipes/{closed}"), Some(&robin))
            .await
            .status(),
        404,
        "Forgejo must keep the Recipe closed"
    );

    // The entry stays visible on the Cookbook, and it says nothing about
    // the Recipe behind it.
    let seen = page(&app, &format!("/cookbooks/{book}"), Some(&robin)).await;
    assert_says_nothing_about_the_closed_recipe(&seen);
    assert!(seen.contains("Open in Forgejo"));
    assert_cooking_words(&seen);
}

#[tokio::test]
async fn only_the_owner_can_share_a_cookbook() {
    let Ready {
        forgejo: _forgejo,
        app,
        sam,
        robin,
        quinn: _quinn,
        admin: _admin,
    } = ready().await;

    cookbook(&app, &sam, BOOK_TITLE, true).await;
    let book = format!("{OWNER}/{BOOK_SLUG}");
    let path = format!("/cookbooks/{book}/sharing");

    // An Editor can change the Cookbook and cannot share it.
    assert_eq!(
        share(&app, &sam, &book, FRIEND, "editor", &[])
            .await
            .status(),
        303
    );

    let refused = get(&app, &path, Some(&robin)).await;
    assert_eq!(refused.status(), 403);
    let refusal = body(refused).await;
    assert!(refusal.contains("Only the Owner can share this Cookbook"));
    assert_cooking_words(&refusal);

    // An Editor cannot act either, and not only cannot see the controls.
    assert_eq!(
        share(
            &app,
            &robin,
            &book,
            STRANGER,
            "reader",
            &[("confirm", "yes")]
        )
        .await
        .status(),
        403
    );

    // A visitor with no account is sent to sign in.
    let anonymous = get(&app, &path, None).await;
    assert_eq!(anonymous.status(), 303);
    assert_eq!(location(&anonymous), "/auth/sign-in");
}

// ------------------------------- adding a private Recipe to a shared one

#[tokio::test]
async fn adding_a_private_recipe_lists_the_people_who_cannot_read_it() {
    let Ready {
        forgejo,
        app,
        sam,
        robin,
        quinn: _quinn,
        admin,
    } = ready().await;

    recipe(&app, &sam, CLOSED_TITLE, true).await;
    cookbook(&app, &sam, BOOK_TITLE, true).await;

    let book = format!("{OWNER}/{BOOK_SLUG}");
    let closed = format!("{OWNER}/{CLOSED_SLUG}");

    // The Cookbook is shared while it holds no Recipe at all, so this step
    // asks nothing.
    assert_eq!(
        share(&app, &sam, &book, FRIEND, "editor", &[])
            .await
            .status(),
        303
    );

    let answer = add(&app, &sam, &book, &closed, &[]).await;
    assert_eq!(
        answer.status(),
        200,
        "the access mismatch is a page and not a change"
    );

    let asked = body(answer).await;
    assert!(
        asked.contains(FRIEND),
        "the person who cannot read the Recipe must be listed: {asked:.4000}"
    );
    assert!(asked.contains("Give Reader access and add the Recipe"));
    assert!(asked.contains("Add it anyway"));
    assert!(asked.contains("Cancel"));
    assert_cooking_words(&asked);

    // Nothing landed. The Cookbook still holds one Version and no Recipe.
    let before = versions(&forgejo, &admin, &book).await;
    let (status, _) = support::forgejo_raw(
        &forgejo,
        &admin,
        &format!("/{book}/raw/{}", cookbook::MODULES_FILE),
    )
    .await;
    assert_eq!(status, 404, "no reference may be written before a decision");

    // Give access, and then it lands.
    add_ok(
        &app,
        &sam,
        &book,
        &closed,
        &[("confirm", "yes"), ("grant", "yes")],
    )
    .await;

    assert_eq!(access(&forgejo, &admin, &closed, FRIEND).await, "read");
    assert!(
        modules(&forgejo, &admin, &book)
            .await
            .contains(&format!("/{closed}.git"))
    );
    assert_eq!(
        versions(&forgejo, &admin, &book).await,
        before + 1,
        "one Version of the Cookbook, and no more"
    );

    // Robin can read the Recipe now, so the entry is an ordinary one.
    let seen = page(&app, &format!("/cookbooks/{book}"), Some(&robin)).await;
    assert!(seen.contains(CLOSED_TITLE), "got: {seen:.4000}");
}

#[tokio::test]
async fn a_person_can_add_a_private_recipe_anyway_and_no_access_changes() {
    let Ready {
        forgejo,
        app,
        sam,
        robin,
        quinn: _quinn,
        admin,
    } = ready().await;

    recipe(&app, &sam, CLOSED_TITLE, true).await;
    cookbook(&app, &sam, BOOK_TITLE, true).await;

    let book = format!("{OWNER}/{BOOK_SLUG}");
    let closed = format!("{OWNER}/{CLOSED_SLUG}");

    assert_eq!(
        share(&app, &sam, &book, FRIEND, "editor", &[])
            .await
            .status(),
        303
    );

    add_ok(&app, &sam, &book, &closed, &[("confirm", "yes")]).await;

    assert_eq!(
        collaborators(&forgejo, &admin, &closed).await,
        Vec::<String>::new(),
        "Add it anyway must change no Recipe"
    );

    // Robin can change the Cookbook and still cannot read the Recipe.
    let seen = page(&app, &format!("/cookbooks/{book}"), Some(&robin)).await;
    assert_says_nothing_about_the_closed_recipe(&seen);
}

// --------------------------------------------- what a stranger can read

#[tokio::test]
async fn a_recipe_that_a_viewer_cannot_read_leaks_no_title_and_no_name() {
    let Ready {
        forgejo: _forgejo,
        app,
        sam,
        robin: _robin,
        quinn,
        admin: _admin,
    } = ready().await;

    recipe(&app, &sam, CLOSED_TITLE, true).await;
    cookbook(&app, &sam, BOOK_TITLE, false).await;

    let book = format!("{OWNER}/{BOOK_SLUG}");
    let closed = format!("{OWNER}/{CLOSED_SLUG}");

    // The Cookbook is public and it holds a private Recipe, so this is
    // exactly the state the criterion is about.
    add_ok(&app, &sam, &book, &closed, &[("confirm", "yes")]).await;

    // A signed-in stranger.
    let seen = page(&app, &format!("/cookbooks/{book}"), Some(&quinn)).await;
    assert_says_nothing_about_the_closed_recipe(&seen);
    assert_cooking_words(&seen);

    // And a visitor with no account at all.
    let anonymous = page(&app, &format!("/cookbooks/{book}"), None).await;
    assert_says_nothing_about_the_closed_recipe(&anonymous);
    assert_cooking_words(&anonymous);

    // The Owner still reads it as an ordinary Recipe.
    let owned = page(&app, &format!("/cookbooks/{book}"), Some(&sam)).await;
    assert!(owned.contains(CLOSED_TITLE), "got: {owned:.4000}");
}

#[tokio::test]
async fn a_public_cookbook_says_that_it_will_name_a_private_recipe() {
    let Ready {
        forgejo,
        app,
        sam,
        robin: _robin,
        quinn: _quinn,
        admin,
    } = ready().await;

    recipe(&app, &sam, CLOSED_TITLE, true).await;
    cookbook(&app, &sam, BOOK_TITLE, false).await;

    let book = format!("{OWNER}/{BOOK_SLUG}");
    let closed = format!("{OWNER}/{CLOSED_SLUG}");

    // Nobody shares this Cookbook by name, and every user can read it, so
    // the warning is about all users and it offers no grant.
    let answer = add(&app, &sam, &book, &closed, &[]).await;
    assert_eq!(answer.status(), 200);

    let asked = body(answer).await;
    assert!(
        asked.contains("All users can read this Cookbook"),
        "got: {asked:.4000}"
    );
    assert!(
        !asked.contains("Give Reader access and add the Recipe"),
        "no grant covers all users"
    );
    assert!(asked.contains("Add it anyway"));
    assert_cooking_words(&asked);

    let (status, _) = support::forgejo_raw(
        &forgejo,
        &admin,
        &format!("/{book}/raw/{}", cookbook::MODULES_FILE),
    )
    .await;
    assert_eq!(status, 404, "no reference may be written before a decision");
}

// ------------------------------------------------------ broken references

#[tokio::test]
async fn a_deleted_recipe_leaves_the_entry_visible_and_the_reference_untouched() {
    let Ready {
        forgejo,
        app,
        sam,
        robin: _robin,
        quinn: _quinn,
        admin,
    } = ready().await;

    recipe(&app, &sam, OPEN_TITLE, false).await;
    cookbook(&app, &sam, BOOK_TITLE, false).await;

    let book = format!("{OWNER}/{BOOK_SLUG}");
    add_ok(&app, &sam, &book, &format!("{OWNER}/{OPEN_SLUG}"), &[]).await;

    let before_file = modules(&forgejo, &admin, &book).await;
    let before_versions = versions(&forgejo, &admin, &book).await;

    // The Recipe goes away in Forgejo, which is a state that a person can
    // reach and that this interface cannot repair.
    let removed = support::forgejo_write(
        &forgejo,
        &admin,
        reqwest::Method::DELETE,
        &format!("/repos/{OWNER}/{OPEN_SLUG}"),
        serde_json::json!({}),
    )
    .await;
    assert!(
        removed.status().is_success(),
        "Forgejo did not remove the Recipe: {}",
        removed.status()
    );

    // Read the page several times. A repair, if there were one, would
    // happen on one of them.
    for _ in 0..3 {
        let seen = page(&app, &format!("/cookbooks/{book}"), Some(&sam)).await;
        assert!(
            seen.contains(cookbook::UNAVAILABLE_MESSAGE),
            "the entry must stay visible and explain itself: {seen:.4000}"
        );
        assert!(seen.contains("Open in Forgejo"));
        assert!(
            !seen.contains(OPEN_TITLE),
            "the entry says nothing about a Recipe that cannot be read"
        );
        assert_cooking_words(&seen);
    }

    assert_eq!(
        modules(&forgejo, &admin, &book).await,
        before_file,
        "the reference must stay exactly as it is"
    );
    assert_eq!(
        versions(&forgejo, &admin, &book).await,
        before_versions,
        "nothing may write a Version of the Cookbook by itself"
    );
}

#[tokio::test]
async fn a_renamed_recipe_never_repairs_the_address_that_a_cookbook_holds() {
    let Ready {
        forgejo,
        app,
        sam,
        robin: _robin,
        quinn: _quinn,
        admin,
    } = ready().await;

    recipe(&app, &sam, OPEN_TITLE, false).await;
    cookbook(&app, &sam, BOOK_TITLE, false).await;

    let book = format!("{OWNER}/{BOOK_SLUG}");
    add_ok(&app, &sam, &book, &format!("{OWNER}/{OPEN_SLUG}"), &[]).await;

    let before_file = modules(&forgejo, &admin, &book).await;
    let before_versions = versions(&forgejo, &admin, &book).await;

    let renamed = support::forgejo_write(
        &forgejo,
        &admin,
        reqwest::Method::PATCH,
        &format!("/repos/{OWNER}/{OPEN_SLUG}"),
        serde_json::json!({ "name": "chili-two" }),
    )
    .await;
    assert!(
        renamed.status().is_success(),
        "Forgejo did not rename the Recipe: {}",
        renamed.status()
    );

    for _ in 0..3 {
        let seen = page(&app, &format!("/cookbooks/{book}"), Some(&sam)).await;
        assert_cooking_words(&seen);
    }

    assert_eq!(
        modules(&forgejo, &admin, &book).await,
        before_file,
        "the address must stay exactly as it is after a rename"
    );
    assert!(
        before_file.contains(&format!("/{OWNER}/{OPEN_SLUG}.git")),
        "got: {before_file}"
    );
    assert_eq!(
        versions(&forgejo, &admin, &book).await,
        before_versions,
        "nothing may write a Version of the Cookbook by itself"
    );
}

// --------------------------------------------------- Public to Private

#[tokio::test]
async fn making_a_recipe_private_lists_the_public_cookbooks_that_it_is_in() {
    let Ready {
        forgejo: _forgejo,
        app,
        sam,
        robin: _robin,
        quinn,
        admin: _admin,
    } = ready().await;

    recipe(&app, &sam, OPEN_TITLE, false).await;
    cookbook(&app, &sam, BOOK_TITLE, false).await;

    let book = format!("{OWNER}/{BOOK_SLUG}");
    add_ok(&app, &sam, &book, &format!("{OWNER}/{OPEN_SLUG}"), &[]).await;

    // The Owner reads which Cookbooks this costs before they decide.
    let sharing = page(
        &app,
        &format!("/recipes/{OWNER}/{OPEN_SLUG}/sharing"),
        Some(&sam),
    )
    .await;
    assert!(
        sharing.contains(BOOK_TITLE),
        "the public Cookbook must be listed: {sharing:.6000}"
    );
    assert!(
        spoken(&sharing).contains("becomes partly unavailable"),
        "the page must say what happens: {sharing:.6000}"
    );
    assert_cooking_words(&sharing);

    // The change stays allowed.
    let changed = post(
        &app,
        &format!("/recipes/{OWNER}/{OPEN_SLUG}/sharing/visibility"),
        Some(&sam),
        &[("visibility", "private")],
    )
    .await;
    assert_eq!(changed.status(), 303);

    // And the Cookbook is now partly unavailable, exactly as it was said.
    let seen = page(&app, &format!("/cookbooks/{book}"), Some(&quinn)).await;
    assert!(
        seen.contains(cookbook::UNAVAILABLE_MESSAGE),
        "got: {seen:.4000}"
    );
    assert!(!seen.contains(OPEN_TITLE));
}
