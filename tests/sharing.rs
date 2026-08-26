//! Acceptance tests for sharing a Recipe and changing its visibility.
//!
//! Forgejo is authoritative for visibility and for permissions, so every
//! test asks Forgejo what actually happened. A page that says the right
//! thing while Forgejo says another is a failure.

mod support;

use std::collections::BTreeMap;

use cooklanghub::git::{GitAdapter, GitError, Identity, InitialCommit, SystemGit};
use cooklanghub::secret::Secret;
use cooklanghub::session::COOKIE_NAME;
use cooklanghub::web_sharing::PUBLIC_WARNING;

/// The Owner of every Recipe in these tests.
const OWNER: &str = "sam";
/// A person who gets read access.
const READER: &str = "robin";
/// A person who gets write access.
const EDITOR: &str = "dana";
/// The Forgejo administrator that the bootstrap command uses.
const ADMIN: &str = "alex";

const SOURCE: &str = "Add @salt{1%pinch} to the #pan{}.";

/// Forgejo, the application, the Owner signed in to it, and the credential
/// that asks Forgejo what really happened.
///
/// Forgejo gives one access token per name, so the token is made once here
/// and passed on rather than made again in each test.
async fn ready() -> (support::Forgejo, support::TestApp, String, Secret<String>) {
    let forgejo = support::start_forgejo().await;
    forgejo.create_user(ADMIN, true);
    forgejo.create_user(OWNER, false);
    forgejo.create_user(READER, false);
    forgejo.create_user(EDITOR, false);

    let admin_token = forgejo.access_token(ADMIN);
    let app = support::start_app(&forgejo.base_url).await;
    app.bootstrap(&admin_token).await;

    let session = support::sign_in(&app, &forgejo, OWNER).await;
    (forgejo, app, session, admin_token)
}

/// Make a Recipe and check that it was made.
async fn recipe(app: &support::TestApp, session: &str, title: &str, private: bool) {
    let response = support::create_recipe(app, session, title, SOURCE, private).await;
    assert_eq!(
        response.status(),
        303,
        "the Recipe `{title}` was not created"
    );
}

async fn get(app: &support::TestApp, path: &str, session: Option<&str>) -> reqwest::Response {
    let mut request = support::client().get(app.url(path));
    if let Some(session) = session {
        request = request.header("cookie", format!("{COOKIE_NAME}={session}"));
    }
    request.send().await.expect("cannot reach the page")
}

async fn post(
    app: &support::TestApp,
    path: &str,
    session: Option<&str>,
    form: &[(&str, &str)],
) -> reqwest::Response {
    let mut request = support::client().post(app.url(path)).form(form);
    if let Some(session) = session {
        request = request.header("cookie", format!("{COOKIE_NAME}={session}"));
    }
    request.send().await.expect("cannot post the form")
}

async fn body(response: reqwest::Response) -> String {
    response.text().await.expect("cannot read the body")
}

/// What Forgejo says about the visibility of a Recipe.
async fn is_private(forgejo: &support::Forgejo, admin: &Secret<String>, path: &str) -> bool {
    support::forgejo_api(forgejo, admin, &format!("/repos/{path}")).await["private"]
        .as_bool()
        .expect("Forgejo did not report the visibility")
}

/// What Forgejo says one person may do with a Recipe.
async fn permission(
    forgejo: &support::Forgejo,
    admin: &Secret<String>,
    path: &str,
    login: &str,
) -> String {
    support::forgejo_api(
        forgejo,
        admin,
        &format!("/repos/{path}/collaborators/{login}/permission"),
    )
    .await["permission"]
        .as_str()
        .expect("Forgejo did not report a permission")
        .to_string()
}

/// The logins that Forgejo records on a Recipe.
async fn collaborators(
    forgejo: &support::Forgejo,
    admin: &Secret<String>,
    path: &str,
) -> Vec<String> {
    support::forgejo_api(forgejo, admin, &format!("/repos/{path}/collaborators"))
        .await
        .as_array()
        .expect("Forgejo did not answer with a list")
        .iter()
        .filter_map(|user| user["login"].as_str().map(str::to_string))
        .collect()
}

/// The credential that the application holds for a signed-in person.
///
/// Publishing uses this credential, so a test of who can publish must use
/// the same one and not a token made for the test.
async fn app_token(app: &support::TestApp, session: &str) -> Secret<String> {
    cooklanghub::session::access_token(&app.pool, &app.cipher, session)
        .await
        .expect("cannot read the session store")
        .expect("the session holds no credential")
}

/// Publish a Version through the Git adapter, as one person.
///
/// Git owns Recipe content, so this is the real seam. Forgejo answers the
/// push, and its answer is the permission decision.
async fn publish(
    app: &support::TestApp,
    token: &Secret<String>,
    path: &str,
    login: &str,
    version: &str,
) -> Result<String, GitError> {
    let mut files = BTreeMap::new();
    files.insert("recipe.cook".to_string(), SOURCE.as_bytes().to_vec());

    SystemGit
        .create_initial_commit(InitialCommit {
            remote_url: &app.forgejo.git_url(path),
            token,
            identity: &Identity {
                name: login.to_string(),
                email: format!("{login}@example.test"),
            },
            branch: version,
            message: "Another Version",
            files,
        })
        .await
}

#[tokio::test]
async fn an_owner_makes_a_recipe_public_after_a_confirmation_and_private_again() {
    let (forgejo, app, session, admin) = ready().await;
    recipe(&app, &session, "Chili", true).await;

    let path = "sam/chili";
    let sharing = "/recipes/sam/chili/sharing";
    assert!(is_private(&forgejo, &admin, path).await);

    // The page says how the Recipe stands and offers the one next step.
    let page = body(get(&app, sharing, Some(&session)).await).await;
    assert!(page.contains("Private"), "the page must say Private");
    assert!(
        page.contains(&format!("{sharing}/public")),
        "the page must offer the confirmation"
    );

    // A post that skips the confirmation changes nothing. The check is on
    // the server, so an interface is not what holds it.
    let skipped = post(
        &app,
        &format!("{sharing}/visibility"),
        Some(&session),
        &[("visibility", "public")],
    )
    .await;
    assert_eq!(skipped.status(), 200, "a skipped confirmation must not act");
    assert!(
        body(skipped).await.contains(PUBLIC_WARNING),
        "the answer must ask for the confirmation"
    );
    assert!(
        is_private(&forgejo, &admin, path).await,
        "the Recipe became public without a confirmation"
    );

    // The confirmation says what changes, and it names the earlier Versions.
    let confirmation = body(get(&app, &format!("{sharing}/public"), Some(&session)).await).await;
    assert!(
        confirmation.contains(PUBLIC_WARNING),
        "the confirmation must carry the warning"
    );
    assert!(
        confirmation.contains("All users can read this Recipe and its earlier Versions"),
        "the confirmation must name the Recipe and its earlier Versions"
    );

    // With the confirmation, the Recipe becomes public.
    let made_public = post(
        &app,
        &format!("{sharing}/visibility"),
        Some(&session),
        &[("visibility", "public"), ("confirm", "yes")],
    )
    .await;
    assert_eq!(made_public.status(), 303);
    assert!(
        !is_private(&forgejo, &admin, path).await,
        "Forgejo must report the Recipe as public"
    );

    let public_page = body(get(&app, sharing, Some(&session)).await).await;
    assert!(public_page.contains("Public"));

    // And it goes back, with no confirmation needed to take access away.
    let made_private = post(
        &app,
        &format!("{sharing}/visibility"),
        Some(&session),
        &[("visibility", "private")],
    )
    .await;
    assert_eq!(made_private.status(), 303);
    assert!(
        is_private(&forgejo, &admin, path).await,
        "Forgejo must report the Recipe as private again"
    );
}

#[tokio::test]
async fn share_copies_the_normal_recipe_address_and_there_is_no_second_one() {
    let (_forgejo, app, session, _admin) = ready().await;
    recipe(&app, &session, "Open Pot", false).await;

    let page = body(get(&app, "/recipes/sam/open-pot/sharing", Some(&session)).await).await;

    // The address is the Recipe address, whole, and nothing else.
    let address = app.url("/recipes/sam/open-pot");
    assert!(
        page.contains(&address),
        "the page must show the Recipe address, got:\n{page:.2000}"
    );
    assert!(
        !page.to_lowercase().contains("unlisted"),
        "there is no unlisted link"
    );

    // A person who runs no script can still read and select the address.
    assert!(page.contains("id=\"recipe-address\""));
    assert!(page.contains("readonly"));

    // The copy action is a served file, so the policy `default-src 'self'`
    // holds. Nothing is inline.
    assert!(page.contains("/static/js/share.js"));
    assert!(!page.contains("onclick"));
    let script = get(&app, "/static/js/share.js", None).await;
    assert_eq!(script.status(), 200, "the copy action must be served");

    // Sharing needs a Forgejo account. There is no invitation by email.
    assert!(
        !page.contains("type=\"email\""),
        "the screen must ask for no address"
    );

    // Share is reachable from the Recipe itself.
    let recipe_page = body(get(&app, "/recipes/sam/open-pot", Some(&session)).await).await;
    assert!(
        recipe_page.contains("href=\"/recipes/sam/open-pot/sharing\""),
        "the Recipe page must lead to Sharing"
    );
    assert!(
        recipe_page.contains(">Share<"),
        "the Share action must show"
    );
    assert!(
        !recipe_page.contains("aria-disabled=\"true\">Sharing"),
        "the Sharing area must no longer be unavailable"
    );
}

#[tokio::test]
async fn an_owner_adds_a_reader_and_an_editor_and_can_take_the_access_back() {
    let (forgejo, app, session, admin) = ready().await;
    recipe(&app, &session, "Family Pot", true).await;

    let path = "sam/family-pot";
    let sharing = "/recipes/sam/family-pot/sharing";

    let added_reader = post(
        &app,
        &format!("{sharing}/people"),
        Some(&session),
        &[("login", READER), ("role", "reader")],
    )
    .await;
    assert_eq!(added_reader.status(), 303);

    let added_editor = post(
        &app,
        &format!("{sharing}/people"),
        Some(&session),
        &[("login", EDITOR), ("role", "editor")],
    )
    .await;
    assert_eq!(added_editor.status(), 303);

    // Reader is Forgejo Read and Editor is Forgejo Write.
    assert_eq!(permission(&forgejo, &admin, path, READER).await, "read");
    assert_eq!(permission(&forgejo, &admin, path, EDITOR).await, "write");

    // A private Recipe lists its Readers, because their access is explicit.
    let private_page = body(get(&app, sharing, Some(&session)).await).await;
    assert!(private_page.contains(READER), "a Reader must be listed");
    assert!(private_page.contains("Reader"));
    assert!(private_page.contains(EDITOR), "an Editor must be listed");
    assert!(private_page.contains("Editor"));

    // A public Recipe lists only the people with more than read access.
    let made_public = post(
        &app,
        &format!("{sharing}/visibility"),
        Some(&session),
        &[("visibility", "public"), ("confirm", "yes")],
    )
    .await;
    assert_eq!(made_public.status(), 303);

    let public_page = body(get(&app, sharing, Some(&session)).await).await;
    assert!(
        public_page.contains(EDITOR),
        "an Editor must still be listed"
    );
    assert!(
        !public_page.contains(READER),
        "everybody reads a public Recipe, so a Reader says nothing"
    );
    // Forgejo still holds the read access. Only the list leaves it out.
    assert_eq!(permission(&forgejo, &admin, path, READER).await, "read");

    // The Owner takes access back, and Forgejo is what changes.
    let removed = post(
        &app,
        &format!("{sharing}/people/remove"),
        Some(&session),
        &[("login", EDITOR)],
    )
    .await;
    assert_eq!(removed.status(), 303);
    assert!(
        !collaborators(&forgejo, &admin, path)
            .await
            .contains(&EDITOR.to_string()),
        "Forgejo must no longer record the Editor"
    );
}

#[tokio::test]
async fn an_editor_can_publish_a_version_and_a_reader_cannot() {
    let (forgejo, app, session, _admin) = ready().await;
    recipe(&app, &session, "Shared Pot", true).await;

    let path = "sam/shared-pot";
    let sharing = "/recipes/sam/shared-pot/sharing";

    for (login, role) in [(READER, "reader"), (EDITOR, "editor")] {
        let added = post(
            &app,
            &format!("{sharing}/people"),
            Some(&session),
            &[("login", login), ("role", role)],
        )
        .await;
        assert_eq!(added.status(), 303, "cannot share with {login}");
    }

    // Each person signs in, so the test publishes with the credential that
    // the application itself would use.
    let editor_session = support::sign_in(&app, &forgejo, EDITOR).await;
    let reader_session = support::sign_in(&app, &forgejo, READER).await;

    let editor_token = app_token(&app, &editor_session).await;
    let reader_token = app_token(&app, &reader_session).await;

    let by_editor = publish(&app, &editor_token, path, EDITOR, "version-by-editor").await;
    assert!(
        by_editor.is_ok(),
        "an Editor must be able to publish a Version: {by_editor:?}"
    );

    let by_reader = publish(&app, &reader_token, path, READER, "version-by-reader").await;
    let error = by_reader.expect_err("a Reader must not be able to publish a Version");

    // Forgejo refused it. The application never made that decision.
    let message = error.to_string();
    assert!(
        message.contains("push"),
        "the refusal must come from the publish step: {message}"
    );
}

#[tokio::test]
async fn the_application_obeys_the_profile_visibility_setting_of_forgejo() {
    let (forgejo, app, session, admin) = ready().await;
    forgejo.create_user("quinn", false);
    hide_profile(&forgejo, &admin, "quinn").await;

    recipe(&app, &session, "Quiet Pot", true).await;
    let path = "sam/quiet-pot";
    let sharing = "/recipes/sam/quiet-pot/sharing";

    // Forgejo hides the profile from the Owner.
    let owner_token = app_token(&app, &session).await;
    let seen = reqwest::Client::new()
        .get(format!("{}/api/v1/users/quinn", forgejo.base_url))
        .bearer_auth(owner_token.expose())
        .send()
        .await
        .expect("cannot ask Forgejo about the user");
    assert_eq!(
        seen.status(),
        404,
        "Forgejo must hide a private profile from another user"
    );

    // So the application refuses, and it adds nobody.
    let response = post(
        &app,
        &format!("{sharing}/people"),
        Some(&session),
        &[("login", "quinn"), ("role", "reader")],
    )
    .await;
    assert_eq!(response.status(), 200, "the answer must stay on the page");

    let page = body(response).await;
    assert!(
        page.contains("Forgejo shows no user with the name"),
        "the page must say what Forgejo answered, got:\n{page:.2000}"
    );
    assert!(
        !collaborators(&forgejo, &admin, path)
            .await
            .contains(&"quinn".to_string()),
        "the application reached past the visibility setting of Forgejo"
    );
}

#[tokio::test]
async fn the_service_identities_of_the_application_are_not_listed() {
    let (forgejo, app, session, admin) = ready().await;
    forgejo.create_user("cooklanghub-automation", false);

    recipe(&app, &session, "Watched Pot", true).await;
    let path = "sam/watched-pot";
    let sharing = "/recipes/sam/watched-pot/sharing";

    let added = post(
        &app,
        &format!("{sharing}/people"),
        Some(&session),
        &[("login", READER), ("role", "reader")],
    )
    .await;
    assert_eq!(added.status(), 303);

    // The identity of the application reaches the Recipe the way it would in
    // a running installation: through Forgejo, not through this screen.
    grant(&forgejo, &admin, path, "cooklanghub-automation", "write").await;
    assert!(
        collaborators(&forgejo, &admin, path)
            .await
            .contains(&"cooklanghub-automation".to_string()),
        "Forgejo must record the identity of the application"
    );

    let page = body(get(&app, sharing, Some(&session)).await).await;
    assert!(page.contains(READER), "a person must still be listed");
    assert!(
        !page.contains("cooklanghub-automation"),
        "an identity of the application must stay in Forgejo, got:\n{page:.2000}"
    );

    // The screen refuses to give one access, and it says where to do that.
    let refused = post(
        &app,
        &format!("{sharing}/people"),
        Some(&session),
        &[("login", "cooklanghub-automation"), ("role", "editor")],
    )
    .await;
    assert_eq!(refused.status(), 200);
    assert!(
        body(refused).await.contains("Open the Recipe in Forgejo"),
        "the screen must send the person to Forgejo"
    );
}

#[tokio::test]
async fn only_the_owner_can_reach_or_act_on_the_sharing_controls() {
    let (forgejo, app, session, admin) = ready().await;
    recipe(&app, &session, "Locked Pot", true).await;

    let path = "sam/locked-pot";
    let sharing = "/recipes/sam/locked-pot/sharing";

    let added = post(
        &app,
        &format!("{sharing}/people"),
        Some(&session),
        &[("login", EDITOR), ("role", "editor")],
    )
    .await;
    assert_eq!(added.status(), 303);

    // An Editor can read the Recipe and cannot reach one control.
    let editor_session = support::sign_in(&app, &forgejo, EDITOR).await;
    let seen = get(&app, sharing, Some(&editor_session)).await;
    assert_eq!(seen.status(), 403, "only the Owner reaches this screen");

    let page = body(seen).await;
    assert!(
        !page.contains("/sharing/visibility"),
        "no visibility control"
    );
    assert!(!page.contains("/sharing/people"), "no people control");
    assert!(page.contains("Open in Forgejo"), "a way onward must show");

    // And a post from that person changes nothing, whatever the page showed.
    let visibility = post(
        &app,
        &format!("{sharing}/visibility"),
        Some(&editor_session),
        &[("visibility", "public"), ("confirm", "yes")],
    )
    .await;
    assert_eq!(visibility.status(), 403);
    assert!(
        is_private(&forgejo, &admin, path).await,
        "an Editor made the Recipe public"
    );

    let people = post(
        &app,
        &format!("{sharing}/people"),
        Some(&editor_session),
        &[("login", READER), ("role", "editor")],
    )
    .await;
    assert_eq!(people.status(), 403);
    assert!(
        !collaborators(&forgejo, &admin, path)
            .await
            .contains(&READER.to_string()),
        "an Editor shared a Recipe that is not theirs"
    );

    // Somebody with no access learns nothing about the Recipe at all.
    let reader_session = support::sign_in(&app, &forgejo, READER).await;
    let stranger = get(&app, sharing, Some(&reader_session)).await;
    assert_eq!(stranger.status(), 404);

    // A visitor who never signed in is asked to sign in.
    let anonymous = get(&app, sharing, None).await;
    assert_eq!(anonymous.status(), 303);
    assert_eq!(
        anonymous
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok()),
        Some("/auth/sign-in")
    );
}

/// Turn the profile of one person private, as an administrator does.
async fn hide_profile(forgejo: &support::Forgejo, admin: &Secret<String>, login: &str) {
    let response = reqwest::Client::new()
        .patch(format!("{}/api/v1/admin/users/{login}", forgejo.base_url))
        .header("Authorization", format!("token {}", admin.expose()))
        .json(&serde_json::json!({
            "login_name": login,
            "source_id": 0,
            "visibility": "private",
        }))
        .send()
        .await
        .expect("cannot reach the Forgejo API");

    assert!(
        response.status().is_success(),
        "cannot hide the profile of {login}: {}",
        response.status()
    );
}

/// Give one person access through Forgejo, without this application.
async fn grant(
    forgejo: &support::Forgejo,
    admin: &Secret<String>,
    path: &str,
    login: &str,
    permission: &str,
) {
    let response = reqwest::Client::new()
        .put(format!(
            "{}/api/v1/repos/{path}/collaborators/{login}",
            forgejo.base_url
        ))
        .header("Authorization", format!("token {}", admin.expose()))
        .json(&serde_json::json!({ "permission": permission }))
        .send()
        .await
        .expect("cannot reach the Forgejo API");

    assert!(
        response.status().is_success(),
        "cannot give {login} access: {}",
        response.status()
    );
}
