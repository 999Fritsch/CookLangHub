//! Acceptance tests for the profile of one cook.
//!
//! Every test drives the real page against a real Forgejo, because the
//! question this page asks is a permission question and Forgejo is the only
//! authority on it. A mock would answer whatever this application expected,
//! which is exactly the mistake these tests exist to catch.

mod support;

use cooklanghub::secret::Secret;
use cooklanghub::session::COOKIE_NAME;

use support::{Forgejo, TestApp};

/// The address that Forgejo in the container reports itself at.
///
/// The container keeps the Forgejo default `ROOT_URL`, and the test machine
/// reaches the container on a port that the operating system chose. The two
/// therefore differ, exactly as they differ in the bundled stack: this
/// application reaches Forgejo by one address, and a browser by another.
/// Forgejo builds an avatar address out of the browser one, so a test that
/// fetches a picture starts an application configured that way round.
const FORGEJO_ROOT_URL: &str = "http://localhost:3000";

/// Everything that a test starts from.
///
/// Three people, because the answers differ for all three. `sam` owns the
/// Recipes. `jo` is another ordinary cook, and is the person who must never
/// see a private Recipe of Sam. `alex` administers the installation, which
/// Forgejo lets see every profile, so Alex is never the one who tests a
/// hidden profile.
struct Ready {
    forgejo: Forgejo,
    app: TestApp,
    /// The session cookie of Sam.
    sam: String,
    /// The session cookie of Jo.
    jo: String,
    /// An access token of Alex, who administers the installation.
    admin: Secret<String>,
    /// An access token of Sam.
    sam_token: Secret<String>,
}

async fn ready() -> Ready {
    let forgejo = support::start_forgejo().await;
    forgejo.create_user("alex", true);
    forgejo.create_user("sam", false);
    forgejo.create_user("jo", false);

    let admin = forgejo.access_token("alex");
    let sam_token = forgejo.access_token("sam");

    let app = support::start_app(&forgejo.base_url).await;
    app.bootstrap(&admin).await;

    let sam = support::sign_in(&app, &forgejo, "sam").await;
    let jo = support::sign_in(&app, &forgejo, "jo").await;

    Ready {
        forgejo,
        app,
        sam,
        jo,
        admin,
        sam_token,
    }
}

/// Read a page, as an anonymous visitor or as the holder of a session.
async fn get(app: &TestApp, path: &str, session: Option<&str>) -> reqwest::Response {
    let mut request = support::client().get(app.url(path));
    if let Some(session) = session {
        request = request.header("cookie", format!("{COOKIE_NAME}={session}"));
    }

    request.send().await.expect("cannot reach the page")
}

/// Read a page that must answer 200.
async fn page(app: &TestApp, path: &str, session: Option<&str>) -> String {
    let response = get(app, path, session).await;
    let status = response.status();
    let body = response.text().await.expect("the page has no body");
    assert_eq!(status, 200, "GET {path} answered wrongly: {body:.600}");
    body
}

/// Every image address that a page carries.
///
/// The Content Security Policy allows an image from this origin only, so
/// every one of these must be an address of this application.
fn image_sources(body: &str) -> Vec<String> {
    body.match_indices("<img")
        .filter_map(|(at, _)| {
            let tag_end = at + body[at..].find('>')?;
            let tag = &body[at..tag_end];
            let value_at = tag.find("src=\"")? + "src=\"".len();
            let value_end = value_at + tag[value_at..].find('"')?;
            Some(tag[value_at..value_end].to_string())
        })
        .collect()
}

/// Change the visibility of a Forgejo profile, as an administrator does.
///
/// Forgejo owns this setting. The application reads its effect and never
/// keeps a copy of it, so a test moves the setting in Forgejo itself.
async fn set_profile_visibility(
    forgejo: &Forgejo,
    admin: &Secret<String>,
    login: &str,
    visibility: &str,
) {
    let response = support::forgejo_write(
        forgejo,
        admin,
        reqwest::Method::PATCH,
        &format!("/admin/users/{login}"),
        serde_json::json!({
            "login_name": login,
            "source_id": 0,
            "visibility": visibility,
        }),
    )
    .await;

    let status = response.status();
    assert!(
        status.is_success(),
        "cannot set the profile of {login} to {visibility}: {status} {}",
        response.text().await.unwrap_or_default()
    );
}

/// Make a Cookbook in Forgejo, with the topics that mark one.
///
/// Cookbooks arrive with their own ticket. Until then a test makes one the
/// way an outside tool would, so that the profile is held against a real
/// Cookbook repository and not against an assumption.
async fn create_cookbook(forgejo: &Forgejo, token: &Secret<String>, name: &str, private: bool) {
    let response = support::forgejo_write(
        forgejo,
        token,
        reqwest::Method::POST,
        "/user/repos",
        serde_json::json!({
            "name": name,
            "private": private,
            "auto_init": true,
        }),
    )
    .await;
    let status = response.status();
    assert!(status.is_success(), "cannot create the Cookbook: {status}");

    let owner = support::forgejo_api(forgejo, token, "/user").await;
    let owner = owner["login"].as_str().expect("the user has no login");

    let response = support::forgejo_write(
        forgejo,
        token,
        reqwest::Method::PUT,
        &format!("/repos/{owner}/{name}/topics"),
        serde_json::json!({ "topics": ["cooklang", "cookbook"] }),
    )
    .await;
    let status = response.status();
    assert!(status.is_success(), "cannot mark the Cookbook: {status}");
}

#[tokio::test]
async fn a_profile_shows_only_the_recipes_that_forgejo_shows_the_person_looking() {
    let Ready {
        forgejo,
        app,
        sam,
        jo,
        admin: _admin,
        sam_token: _sam_token,
    } = ready().await;

    support::create_recipe(&app, &sam, "Sam Soup", "Warm the @broth{1%l}.", false).await;
    support::create_recipe(&app, &sam, "Sam Secret", "Add the @salt{1%pinch}.", true).await;
    support::create_recipe(&app, &jo, "Jo Stew", "Cook the @beans{2%cups}.", false).await;

    // An anonymous visitor. Public means public, so the page opens without
    // an account and holds the public Recipe only.
    let visitor = page(&app, "/cooks/sam", None).await;
    assert!(
        visitor.contains("Sam Soup"),
        "the public Recipe must be here"
    );
    assert!(
        !visitor.contains("Sam Secret"),
        "a visitor must never see a private Recipe"
    );
    assert!(
        !visitor.contains("Jo Stew"),
        "a profile holds the Recipes of one cook only"
    );
    assert!(
        visitor.contains("Owned by sam"),
        "a card must say who owns the Recipe"
    );
    assert!(visitor.contains("1 Recipe"), "the count must be of one");
    assert!(
        visitor.contains("<title>sam &middot; CookLangHub</title>"),
        "the page must be named after the cook"
    );

    // Another cook, signed in. Forgejo does not give them the private
    // Recipe either, so neither does this page.
    let other = page(&app, "/cooks/sam", Some(&jo)).await;
    assert!(other.contains("Sam Soup"));
    assert!(
        !other.contains("Sam Secret"),
        "the private Recipe of somebody else must never appear"
    );

    // Sam looks at their own profile. Forgejo shows them their private
    // Recipe, so the page does too, and it is marked.
    let own = page(&app, "/cooks/sam", Some(&sam)).await;
    assert!(own.contains("Sam Soup"));
    assert!(
        own.contains("Sam Secret"),
        "a cook must see their own private Recipe"
    );
    assert!(own.contains("Private"), "a private Recipe must be marked");

    // The picture comes from this application. An address on Forgejo would
    // break the Content Security Policy and tell Forgejo who is reading.
    // That the bytes really travel through here is held against a real
    // Forgejo in `the_application_obeys_the_forgejo_profile_visibility_setting`.
    let sources = image_sources(&visitor);
    assert!(
        sources.iter().any(|source| source == "/cooks/sam/avatar"),
        "the profile must carry the picture of the cook: {sources:?}"
    );
    for source in &sources {
        assert!(
            source.starts_with('/'),
            "an image must come from this application, not from `{source}`"
        );
        assert!(
            !source.contains(&forgejo.base_url),
            "an image must never be fetched from Forgejo: `{source}`"
        );
    }

    // The Recipe page is the way to the profile.
    let recipe = page(&app, "/recipes/sam/sam-soup", None).await;
    assert!(
        recipe.contains("href=\"/cooks/sam\""),
        "the owner name on a Recipe must lead to their profile"
    );

    // A cook that Forgejo does not know has no page, and the page that says
    // so offers Forgejo.
    let unknown = get(&app, "/cooks/nobody", None).await;
    assert_eq!(unknown.status(), 404, "an unknown cook has no page");
    let unknown = unknown.text().await.unwrap_or_default();
    assert!(
        unknown.contains("This profile is not available"),
        "the application says what the state is"
    );
    assert!(
        unknown.contains("Open in Forgejo"),
        "the application diagnoses the state and offers Forgejo"
    );

    // The application keeps no second profile record. Nobody signed in as
    // Sam through these reads, so no row anywhere describes Sam, and the
    // database holds no table for a person at all.
    let tables: Vec<String> =
        sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'table'")
            .fetch_all(&app.pool)
            .await
            .expect("cannot read the schema");
    for table in &tables {
        assert!(
            !table.contains("profile"),
            "the application must keep no profile record, but `{table}` is here"
        );
    }

    // The one local table that names a Recipe is a cache. Emptying it must
    // change nothing that a person sees, because Forgejo and Git hold the
    // Recipe and the index only supplies the words on a card.
    sqlx::query("DELETE FROM recipe_index")
        .execute(&app.pool)
        .await
        .expect("cannot empty the index");

    let again = page(&app, "/cooks/sam", None).await;
    assert!(
        again.contains("Sam Soup"),
        "the profile must be rebuildable from Forgejo alone"
    );
}

#[tokio::test]
async fn the_application_obeys_the_forgejo_profile_visibility_setting() {
    let Ready {
        forgejo,
        app,
        sam,
        jo,
        admin,
        sam_token: _sam_token,
    } = ready().await;

    support::create_recipe(&app, &sam, "Sam Soup", "Warm the @broth{1%l}.", false).await;

    // A second application, configured the way the bundled stack is: it
    // reaches Forgejo on one address and a browser reaches it on another.
    // Forgejo builds an avatar address out of the browser one, so this is
    // the application that can really fetch a picture, and it is therefore
    // the one that shows the picture travelling through this server.
    let bundled =
        support::start_app_with_public_forgejo_url(&forgejo.base_url, FORGEJO_ROOT_URL).await;

    let picture = get(&bundled, "/cooks/sam/avatar", None).await;
    assert_eq!(
        picture.status(),
        200,
        "a public profile carries a picture, and this server serves it"
    );
    let content_type = picture
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        content_type.starts_with("image/"),
        "the picture must be an image, not `{content_type}`"
    );
    let bytes = picture.bytes().await.expect("the picture has no body");
    assert!(!bytes.is_empty(), "the picture must carry bytes");

    // A limited profile is for people with an account. Forgejo hides it
    // from a visitor, so this application shows a visitor nothing.
    set_profile_visibility(&forgejo, &admin, "sam", "limited").await;

    // The same application that served the picture a moment ago refuses it
    // now. The setting moved in Forgejo, and nothing moved here.
    let picture = get(&bundled, "/cooks/sam/avatar", None).await;
    assert_eq!(
        picture.status(),
        404,
        "a hidden profile must not give away a picture"
    );

    let refused = get(&app, "/cooks/sam", None).await;
    assert_eq!(
        refused.status(),
        404,
        "a limited profile has no page for a visitor"
    );
    let refused = refused.text().await.unwrap_or_default();
    for leak in ["Sam Soup", "/cooks/sam/avatar", "1 Recipe", "Owned by sam"] {
        assert!(
            !refused.contains(leak),
            "a hidden profile must not give away `{leak}`"
        );
    }
    assert!(
        refused.contains("Open in Forgejo"),
        "the application diagnoses the state and offers Forgejo"
    );

    let picture = get(&app, "/cooks/sam/avatar", None).await;
    assert_eq!(
        picture.status(),
        404,
        "a hidden profile must not give away a picture"
    );

    // The same profile, to somebody with an account. Forgejo shows it, so
    // this application shows it too.
    let allowed = page(&app, "/cooks/sam", Some(&jo)).await;
    assert!(
        allowed.contains("Sam Soup"),
        "a limited profile stays open to a signed-in cook"
    );

    // A private profile is for its owner. Forgejo hides it from every
    // ordinary cook, so this application does the same.
    set_profile_visibility(&forgejo, &admin, "sam", "private").await;

    for (who, session) in [("a visitor", None), ("another cook", Some(jo.as_str()))] {
        let refused = get(&app, "/cooks/sam", session).await;
        assert_eq!(
            refused.status(),
            404,
            "a private profile has no page for {who}"
        );
        let refused = refused.text().await.unwrap_or_default();
        for leak in ["Sam Soup", "/cooks/sam/avatar", "1 Recipe", "Owned by sam"] {
            assert!(
                !refused.contains(leak),
                "a private profile must not give away `{leak}` to {who}"
            );
        }

        let picture = get(&app, "/cooks/sam/avatar", session).await;
        assert_eq!(
            picture.status(),
            404,
            "a private profile must not give {who} a picture"
        );
    }

    // The owner still has their own profile.
    let own = page(&app, "/cooks/sam", Some(&sam)).await;
    assert!(
        own.contains("Sam Soup"),
        "a cook must keep their own profile"
    );
}

#[tokio::test]
async fn a_profile_lists_the_cookbooks_that_forgejo_shows() {
    let Ready {
        forgejo,
        app,
        sam,
        jo,
        admin: _admin,
        sam_token,
    } = ready().await;

    support::create_recipe(&app, &sam, "Sam Soup", "Warm the @broth{1%l}.", false).await;
    create_cookbook(&forgejo, &sam_token, "weeknight-dinners", false).await;
    create_cookbook(&forgejo, &sam_token, "sam-secret-book", true).await;

    let visitor = page(&app, "/cooks/sam", None).await;

    assert!(
        visitor.contains("weeknight-dinners"),
        "a public Cookbook must be on the profile"
    );
    assert!(
        !visitor.contains("sam-secret-book"),
        "a visitor must never see a private Cookbook"
    );
    assert!(
        visitor.contains("1 Cookbook"),
        "the count must be of the Cookbooks that this person may see"
    );
    assert!(
        visitor.contains("Owned by sam"),
        "a Cookbook card must say who owns it"
    );
    assert!(
        visitor.contains("Sam Soup"),
        "a Cookbook must not push the Recipes off the page"
    );
    assert!(
        visitor.contains("no Cookbook page yet"),
        "the application says plainly that it cannot show a Cookbook yet"
    );
    assert!(
        visitor.contains(&format!("{}/sam/weeknight-dinners", forgejo.base_url)),
        "a Cookbook opens in Forgejo"
    );

    // Another cook sees the same public Cookbook and no private one.
    let other = page(&app, "/cooks/sam", Some(&jo)).await;
    assert!(other.contains("weeknight-dinners"));
    assert!(
        !other.contains("sam-secret-book"),
        "the private Cookbook of somebody else must never appear"
    );

    // Sam sees both, and the private one is marked.
    let own = page(&app, "/cooks/sam", Some(&sam)).await;
    assert!(own.contains("weeknight-dinners"));
    assert!(
        own.contains("sam-secret-book"),
        "a cook must see their own private Cookbook"
    );
    assert!(own.contains("2 Cookbooks"));
}
