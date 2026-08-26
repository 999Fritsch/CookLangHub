//! Acceptance tests for creating, finding, and reading a Cookbook.
//!
//! Every test drives the real pages against a real Forgejo, and then asks
//! Forgejo what actually landed there. Forgejo and Git are authoritative, so
//! a page that says the right thing is never enough on its own.

mod support;

use cooklanghub::secret::Secret;
use cooklanghub::session::COOKIE_NAME;

use support::{Forgejo, TestApp};

/// Everything that a test starts from.
///
/// `alex` administers the installation and `sam` does not.
struct Ready {
    forgejo: Forgejo,
    app: TestApp,
    /// The session cookie of Sam.
    sam: String,
    /// An access token of Alex, who administers the installation.
    admin: Secret<String>,
    /// An access token of Sam.
    sam_token: Secret<String>,
}

async fn ready() -> Ready {
    let forgejo = support::start_forgejo().await;
    forgejo.create_user("alex", true);
    forgejo.create_user("sam", false);

    let admin = forgejo.access_token("alex");
    let sam_token = forgejo.access_token("sam");

    let app = support::start_app(&forgejo.base_url).await;
    app.bootstrap(&admin).await;

    let sam = support::sign_in(&app, &forgejo, "sam").await;

    Ready {
        forgejo,
        app,
        sam,
        admin,
        sam_token,
    }
}

/// Read a page, as an anonymous visitor or as the holder of a session.
async fn page(app: &TestApp, path: &str, session: Option<&str>) -> String {
    let response = get(app, path, session).await;
    assert_eq!(response.status(), 200, "GET {path} answered wrongly");
    response.text().await.expect("the page has no body")
}

async fn get(app: &TestApp, path: &str, session: Option<&str>) -> reqwest::Response {
    let mut request = support::client().get(app.url(path));
    if let Some(session) = session {
        request = request.header("cookie", format!("{COOKIE_NAME}={session}"));
    }
    request.send().await.expect("cannot reach the page")
}

/// Where a redirect sent the browser.
fn location(response: &reqwest::Response) -> String {
    response
        .headers()
        .get("location")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

/// The file names at the top of a repository, as Forgejo reports them.
async fn root_files(forgejo: &Forgejo, token: &Secret<String>, path: &str) -> Vec<String> {
    let contents = support::forgejo_api(forgejo, token, &format!("/repos/{path}/contents")).await;
    contents
        .as_array()
        .expect("contents must be a list")
        .iter()
        .map(|file| file["name"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// The topics of a repository, as Forgejo reports them.
async fn topics(forgejo: &Forgejo, token: &Secret<String>, path: &str) -> Vec<String> {
    let answer = support::forgejo_api(forgejo, token, &format!("/repos/{path}/topics")).await;
    answer["topics"]
        .as_array()
        .expect("topics must be a list")
        .iter()
        .map(|topic| topic.as_str().unwrap_or_default().to_string())
        .collect()
}

/// One file of a repository, as text.
async fn raw(forgejo: &Forgejo, token: &Secret<String>, path: &str) -> String {
    let (status, bytes) = support::forgejo_raw(forgejo, token, path).await;
    assert!(status.is_success(), "GET {path} answered {status}");
    String::from_utf8_lossy(&bytes).to_string()
}

// ---------------------------------------------------------------- creating

#[tokio::test]
async fn a_cookbook_becomes_a_repository_that_holds_one_readme() {
    let Ready {
        forgejo,
        app,
        sam,
        admin: _admin,
        sam_token,
    } = ready().await;

    let response = support::create_cookbook(
        &app,
        &sam,
        "Sunday Dinners",
        "Everything for a **long** evening.\n\n- Slow food\n- Many people\n",
        false,
    )
    .await;

    assert_eq!(response.status(), 303, "creation must redirect to the page");
    assert_eq!(location(&response), "/cookbooks/sam/sunday-dinners");

    // Forgejo holds it. This is the authority, not the page.
    let repository = support::forgejo_api(&forgejo, &sam_token, "/repos/sam/sunday-dinners").await;
    assert_eq!(repository["name"], "sunday-dinners");
    assert_eq!(repository["private"], false);
    assert_eq!(repository["default_branch"], "main");
    assert_eq!(repository["owner"]["login"], "sam");

    // The marker is `cooklang` and `cookbook`, and never `recipe`.
    let topics = topics(&forgejo, &sam_token, "sam/sunday-dinners").await;
    assert!(topics.contains(&"cooklang".to_string()), "got {topics:?}");
    assert!(topics.contains(&"cookbook".to_string()), "got {topics:?}");
    assert!(!topics.contains(&"recipe".to_string()), "got {topics:?}");

    // README.md and nothing else. There is no `cookbook.yaml`, and
    // `.gitmodules` arrives only with the first Recipe.
    let files = root_files(&forgejo, &sam_token, "sam/sunday-dinners").await;
    assert_eq!(files, vec!["README.md".to_string()]);

    // The title is the first heading, and the description follows it.
    let readme = raw(&forgejo, &sam_token, "/sam/sunday-dinners/raw/README.md").await;
    assert!(
        readme.starts_with("# Sunday Dinners\n"),
        "the title must be the first heading, got: {readme}"
    );
    assert!(readme.contains("Everything for a **long** evening."));
    assert!(
        !readme.contains("cookbook.yaml"),
        "no second description format exists"
    );

    // Exactly one Version, written as the person who asked for it.
    let versions =
        support::forgejo_api(&forgejo, &sam_token, "/repos/sam/sunday-dinners/commits").await;
    assert_eq!(
        versions.as_array().map(Vec::len),
        Some(1),
        "a new Cookbook must have exactly one Version"
    );

    // The page shows the description and the Recipes, and nothing else.
    let body = page(&app, "/cookbooks/sam/sunday-dinners", Some(&sam)).await;
    assert!(body.contains("Sunday Dinners"));
    assert!(
        body.contains("<strong>long</strong>"),
        "the description must be rendered Markdown"
    );
    assert!(body.contains("<li>Slow food</li>"));
    assert!(
        body.contains("This Cookbook has no Recipes yet."),
        "an empty Cookbook must say so"
    );
    assert!(body.contains("Open in Forgejo"));
}

#[tokio::test]
async fn the_form_asks_for_a_title_a_description_and_a_visibility_and_public_is_the_default() {
    let Ready {
        forgejo,
        app,
        sam,
        admin: _admin,
        sam_token,
    } = ready().await;

    let form = page(&app, "/cookbooks/new", Some(&sam)).await;
    assert!(form.contains("name=\"title\""), "a title field is needed");
    assert!(
        form.contains("name=\"description\""),
        "a description field is needed"
    );
    assert!(
        form.contains("name=\"visibility\""),
        "a visibility field is needed"
    );
    assert!(
        form.contains("value=\"public\" checked"),
        "Public must be the choice the form arrives with, got: {form:.2000}"
    );

    // A form that names no visibility at all still gives a public Cookbook.
    let response = support::post_fields(
        &app,
        &sam,
        "/cookbooks/new",
        &[("title", "Weeknights"), ("description", "Quick things.")],
    )
    .await;
    assert_eq!(response.status(), 303, "creation must redirect to the page");

    let repository = support::forgejo_api(&forgejo, &sam_token, "/repos/sam/weeknights").await;
    assert_eq!(
        repository["private"], false,
        "Public is the default visibility"
    );

    // And the word private is obeyed when it is given.
    support::create_cookbook(&app, &sam, "Quiet Ones", "Nothing to see.", true).await;
    let hidden = support::forgejo_api(&forgejo, &sam_token, "/repos/sam/quiet-ones").await;
    assert_eq!(hidden["private"], true);
}

// ------------------------------------------------------- Markdown and safety

#[tokio::test]
async fn the_description_is_raw_markdown_with_a_preview_that_is_made_safe() {
    let Ready {
        forgejo,
        app,
        sam,
        admin: _admin,
        sam_token,
    } = ready().await;

    let written = "## Starters\n\nA <script>alert(1)</script> word and a \
                   [link](javascript:alert(1)).";

    // Preview writes nothing and keeps everything the person typed.
    let response = support::post_fields(
        &app,
        &sam,
        "/cookbooks/new",
        &[
            ("title", "Sunday Dinners"),
            ("description", written),
            ("visibility", "public"),
            ("action", "preview"),
        ],
    )
    .await;

    assert_eq!(response.status(), 200, "a preview stays on the form");
    let body = response.text().await.expect("the preview has no body");

    assert!(
        body.contains("<h2>Starters</h2>"),
        "the preview must show the Markdown rendered, got: {body:.3000}"
    );
    assert!(
        !body.contains("<script>alert(1)</script>"),
        "a written tag must never become a tag"
    );
    assert!(
        !body.to_lowercase().contains("href=\"javascript:"),
        "a link must never run a script"
    );
    assert!(
        body.contains("## Starters"),
        "the raw Markdown must still be in the text area"
    );

    // Nothing reached Forgejo.
    let answer = support::forgejo_write(
        &forgejo,
        &sam_token,
        reqwest::Method::GET,
        "/repos/sam/sunday-dinners",
        serde_json::json!({}),
    )
    .await;
    assert_eq!(answer.status(), 404, "a preview must create no repository");

    // The same words on the published page are made safe there too.
    support::create_cookbook(&app, &sam, "Sunday Dinners", written, false).await;
    let page = page(&app, "/cookbooks/sam/sunday-dinners", Some(&sam)).await;

    assert!(page.contains("<h2>Starters</h2>"));
    assert!(!page.contains("<script>alert(1)</script>"));
    assert!(!page.to_lowercase().contains("href=\"javascript:"));

    // And Git holds exactly what the person wrote, unchanged.
    let readme = raw(&forgejo, &sam_token, "/sam/sunday-dinners/raw/README.md").await;
    assert!(
        readme.contains("<script>alert(1)</script>"),
        "the stored Markdown is the words of the person, got: {readme}"
    );
}

#[tokio::test]
async fn a_readme_larger_than_one_megabyte_is_refused() {
    let Ready {
        forgejo,
        app,
        sam,
        admin: _admin,
        sam_token,
    } = ready().await;

    let long = "a".repeat(1024 * 1024 + 1);
    let response = support::create_cookbook(&app, &sam, "Sunday Dinners", &long, false).await;

    assert_eq!(
        response.status(),
        200,
        "the form must come back with a reason"
    );
    let body = response.text().await.expect("the form has no body");
    assert!(
        body.contains("1 MB"),
        "the reason must name the limit, got: {body:.2000}"
    );

    let answer = support::forgejo_write(
        &forgejo,
        &sam_token,
        reqwest::Method::GET,
        "/repos/sam/sunday-dinners",
        serde_json::json!({}),
    )
    .await;
    assert_eq!(
        answer.status(),
        404,
        "a refused Cookbook must leave nothing behind"
    );
}

// ----------------------------------------------------------------- finding

#[tokio::test]
async fn the_cookbooks_area_shows_mine_shared_with_me_and_favorites() {
    let Ready {
        forgejo,
        app,
        sam,
        admin,
        sam_token,
    } = ready().await;
    let alex = support::sign_in(&app, &forgejo, "alex").await;

    support::create_cookbook(&app, &sam, "Sunday Dinners", "Mine.", false).await;
    support::create_cookbook(&app, &alex, "Weeknights", "Alex made this.", false).await;

    // Alex lets Sam work on the Cookbook. Forgejo holds the permission.
    let shared = support::forgejo_write(
        &forgejo,
        &admin,
        reqwest::Method::PUT,
        "/repos/alex/weeknights/collaborators/sam",
        serde_json::json!({ "permission": "write" }),
    )
    .await;
    assert!(
        shared.status().is_success(),
        "cannot share the Cookbook: {}",
        shared.status()
    );

    // Sam makes it a Favorite. A Favorite is a Forgejo star.
    let starred = support::forgejo_write(
        &forgejo,
        &sam_token,
        reqwest::Method::PUT,
        "/user/starred/alex/weeknights",
        serde_json::json!({}),
    )
    .await;
    assert!(
        starred.status().is_success(),
        "cannot make it a Favorite: {}",
        starred.status()
    );

    let mine = page(&app, "/cookbooks", Some(&sam)).await;
    assert!(mine.contains("Mine"), "the three lists must be offered");
    assert!(mine.contains("Shared with me"));
    assert!(mine.contains("Favorites"));
    assert!(mine.contains("Sunday Dinners"));
    assert!(
        !mine.contains("Weeknights"),
        "Mine must hold nobody else's Cookbooks"
    );

    let shared = page(&app, "/cookbooks?area=shared", Some(&sam)).await;
    assert!(
        shared.contains("Weeknights"),
        "Shared with me must hold what somebody else shared"
    );
    assert!(
        !shared.contains("Sunday Dinners"),
        "Shared with me must not repeat the Cookbooks of Sam"
    );

    let favorites = page(&app, "/cookbooks?area=favorites", Some(&sam)).await;
    assert!(
        favorites.contains("Weeknights"),
        "Favorites must hold what Sam starred in Forgejo, got: {favorites:.3000}"
    );
    assert!(
        !favorites.contains("Sunday Dinners"),
        "Favorites must hold only what Sam starred"
    );
}

#[tokio::test]
async fn explore_and_search_include_cookbooks() {
    let Ready {
        // Held so that the Forgejo container outlives the test.
        forgejo: _forgejo,
        app,
        sam,
        admin: _admin,
        sam_token: _sam_token,
    } = ready().await;

    support::create_cookbook(&app, &sam, "Sunday Dinners", "Slow food.", false).await;
    support::create_cookbook(&app, &sam, "Weeknights", "Quick food.", false).await;
    support::create_cookbook(&app, &sam, "Quiet Ones", "Nothing to see.", true).await;

    // Explore keeps Recipes and Cookbooks apart, and names both.
    let recipes = page(&app, "/explore", None).await;
    assert!(
        recipes.contains("/explore/cookbooks"),
        "Explore must offer the Cookbooks as well"
    );

    // Explore needs no account, because Public means public.
    let anonymous = page(&app, "/explore/cookbooks", None).await;
    assert!(anonymous.contains("Sunday Dinners"));
    assert!(anonymous.contains("Weeknights"));
    assert!(
        !anonymous.contains("Quiet Ones"),
        "a private Cookbook must never reach a visitor"
    );

    // Search is by the title that a person sees.
    let found = page(&app, "/explore/cookbooks?q=sunday", None).await;
    assert!(found.contains("Sunday Dinners"));
    assert!(
        !found.contains("Weeknights"),
        "a search must make the list shorter"
    );

    let nothing = page(&app, "/explore/cookbooks?q=zzzz", None).await;
    assert!(nothing.contains("No Cookbook title contains these words."));

    // The owner searches their own area the same way.
    let mine = page(&app, "/cookbooks?area=mine&q=quiet", Some(&sam)).await;
    assert!(mine.contains("Quiet Ones"));
    assert!(!mine.contains("Sunday Dinners"));
}

#[tokio::test]
async fn a_cookbook_never_meets_a_recipe_in_a_list() {
    let Ready {
        forgejo,
        app,
        sam,
        admin: _admin,
        sam_token,
    } = ready().await;

    support::create_recipe(&app, &sam, "Chili Sin Carne", "Chop the @onion{1}.", false).await;
    support::create_cookbook(&app, &sam, "Sunday Dinners", "Slow food.", false).await;

    // The Recipe lists hold the Recipe only.
    for path in ["/", "/?area=mine", "/explore"] {
        let body = page(&app, path, Some(&sam)).await;
        assert!(
            body.contains("Chili Sin Carne"),
            "{path} must hold the Recipe"
        );
        assert!(
            !body.contains("Sunday Dinners"),
            "{path} must not hold a Cookbook"
        );
    }

    // The Cookbook lists hold the Cookbook only.
    for path in ["/cookbooks", "/cookbooks?area=mine", "/explore/cookbooks"] {
        let body = page(&app, path, Some(&sam)).await;
        assert!(
            body.contains("Sunday Dinners"),
            "{path} must hold the Cookbook"
        );
        assert!(
            !body.contains("Chili Sin Carne"),
            "{path} must not hold a Recipe"
        );
    }

    // A Recipe has no Cookbook page, and a Cookbook has no Recipe page.
    let recipe_as_cookbook = get(&app, "/cookbooks/sam/chili-sin-carne", Some(&sam)).await;
    assert_eq!(
        recipe_as_cookbook.status(),
        404,
        "a Recipe must not open as a Cookbook"
    );

    // The two markers are what separate them, and Forgejo holds them.
    let recipe_topics = topics(&forgejo, &sam_token, "sam/chili-sin-carne").await;
    assert!(recipe_topics.contains(&"recipe".to_string()));
    assert!(!recipe_topics.contains(&"cookbook".to_string()));

    let cookbook_topics = topics(&forgejo, &sam_token, "sam/sunday-dinners").await;
    assert!(cookbook_topics.contains(&"cookbook".to_string()));
    assert!(!cookbook_topics.contains(&"recipe".to_string()));

    // Removing the marker in Forgejo takes the Cookbook out of the
    // application. Forgejo is authoritative, so the application follows.
    let stripped = support::forgejo_write(
        &forgejo,
        &sam_token,
        reqwest::Method::PUT,
        "/repos/sam/sunday-dinners/topics",
        serde_json::json!({ "topics": ["cooklang"] }),
    )
    .await;
    assert!(stripped.status().is_success());

    let after = page(&app, "/cookbooks?area=mine", Some(&sam)).await;
    assert!(
        !after.contains("Sunday Dinners"),
        "a repository without the marker must leave the application"
    );
}

// -------------------------------------------------------------- visibility

#[tokio::test]
async fn forgejo_decides_who_sees_a_private_cookbook() {
    let Ready {
        forgejo,
        app,
        sam,
        admin: _admin,
        sam_token: _sam_token,
    } = ready().await;

    // Alex administers the installation, and Forgejo lets an administrator
    // read every repository. That is a Forgejo rule and the application must
    // not fight it, so the other person here is one who administers nothing.
    forgejo.create_user("robin", false);
    let robin = support::sign_in(&app, &forgejo, "robin").await;

    support::create_cookbook(&app, &sam, "Quiet Ones", "Nothing to see.", true).await;

    // The owner reads it.
    let owner = page(&app, "/cookbooks/sam/quiet-ones", Some(&sam)).await;
    assert!(owner.contains("Quiet Ones"));
    assert!(owner.contains("Private"), "the page must say it is private");

    // Nobody else does, whether or not they are signed in.
    for session in [None, Some(robin.as_str())] {
        let response = get(&app, "/cookbooks/sam/quiet-ones", session).await;
        assert_eq!(
            response.status(),
            404,
            "a private Cookbook must answer nobody else"
        );
    }

    // And it never reaches a public list.
    let anonymous = page(&app, "/explore/cookbooks", None).await;
    assert!(!anonymous.contains("Quiet Ones"));

    let other = page(&app, "/explore/cookbooks", Some(&robin)).await;
    assert!(
        !other.contains("Quiet Ones"),
        "signing in must not open a private Cookbook of somebody else"
    );

    let shared = page(&app, "/cookbooks?area=shared", Some(&robin)).await;
    assert!(!shared.contains("Quiet Ones"));

    // An administrator sees it because Forgejo says so, and never because
    // this application decided anything.
    let alex = support::sign_in(&app, &forgejo, "alex").await;
    let administrator = get(&app, "/cookbooks/sam/quiet-ones", Some(&alex)).await;
    assert_eq!(
        administrator.status(),
        200,
        "Forgejo lets an administrator read every repository"
    );
}

// ------------------------------------------------------------- the index

#[tokio::test]
async fn the_cookbook_index_is_rebuildable() {
    let Ready {
        forgejo,
        app,
        sam,
        admin: _admin,
        sam_token,
    } = ready().await;

    support::create_cookbook(&app, &sam, "Sunday Dinners", "Slow food.", false).await;

    let held = cooklanghub::cookbook::all(&app.pool)
        .await
        .expect("cannot read the Cookbook index");
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].title, "Sunday Dinners");

    // Throw the whole cache away. Forgejo and Git still hold everything.
    sqlx::query("DELETE FROM cookbook_index")
        .execute(&app.pool)
        .await
        .expect("cannot empty the Cookbook index");
    assert_eq!(
        cooklanghub::cookbook::count(&app.pool).await.unwrap(),
        0,
        "the cache must be empty now"
    );

    // A list rebuilds what it needs on its own.
    let body = page(&app, "/cookbooks?area=mine", Some(&sam)).await;
    assert!(
        body.contains("Sunday Dinners"),
        "the title must come back from Git"
    );
    assert_eq!(cooklanghub::cookbook::count(&app.pool).await.unwrap(), 1);

    // And the sweep rebuilds the whole cache, reading only.
    sqlx::query("DELETE FROM cookbook_index")
        .execute(&app.pool)
        .await
        .expect("cannot empty the Cookbook index");

    let report = app.reconcile_cookbooks().await;
    assert!(report.scanned >= 1, "the sweep must find the Cookbook");
    assert_eq!(report.failures, 0);

    let rebuilt = cooklanghub::cookbook::get(&app.pool, "sam", "sunday-dinners")
        .await
        .expect("cannot read the Cookbook index")
        .expect("the sweep wrote no row");
    assert_eq!(rebuilt.title, "Sunday Dinners");
    assert_eq!(rebuilt.summary, "Slow food.");

    // Nothing about the repository changed.
    let files = root_files(&forgejo, &sam_token, "sam/sunday-dinners").await;
    assert_eq!(files, vec!["README.md".to_string()]);

    let versions =
        support::forgejo_api(&forgejo, &sam_token, "/repos/sam/sunday-dinners/commits").await;
    assert_eq!(
        versions.as_array().map(Vec::len),
        Some(1),
        "an index rebuild must write no Version"
    );
}

#[tokio::test]
async fn the_title_comes_from_git_and_never_from_the_index() {
    let Ready {
        forgejo,
        app,
        sam,
        admin: _admin,
        sam_token,
    } = ready().await;

    support::create_cookbook(&app, &sam, "Sunday Dinners", "Slow food.", false).await;

    // Somebody edits README.md in Forgejo. The application is told nothing,
    // and Git is still the authority for the title.
    let existing = support::forgejo_api(
        &forgejo,
        &sam_token,
        "/repos/sam/sunday-dinners/contents/README.md",
    )
    .await;
    let sha = existing["sha"].as_str().expect("the file has no sha");

    use base64::Engine;
    let content =
        base64::engine::general_purpose::STANDARD.encode("# Long Evenings\n\nEven slower food.\n");

    let written = support::forgejo_write(
        &forgejo,
        &sam_token,
        reqwest::Method::PUT,
        "/repos/sam/sunday-dinners/contents/README.md",
        serde_json::json!({ "content": content, "sha": sha, "message": "Rename" }),
    )
    .await;
    assert!(
        written.status().is_success(),
        "cannot write the README: {}",
        written.status()
    );

    // The Cookbook page reads Git on every request, so the new title is
    // there at once. The address does not change with it, because the slug
    // is technical and never follows the title.
    let shown = page(&app, "/cookbooks/sam/sunday-dinners", Some(&sam)).await;
    assert!(
        shown.contains("Long Evenings"),
        "the title must come from Git, got: {shown:.3000}"
    );
    assert!(shown.contains("Even slower food."));

    // A list reads the index, which is a cache. The webhook brings it up to
    // date in an installation that has one, and the sweep repairs whatever
    // the webhook missed. Both read Forgejo and Git and write to neither.
    app.reconcile_cookbooks().await;

    let body = page(&app, "/cookbooks?area=mine", Some(&sam)).await;
    assert!(
        body.contains("Long Evenings"),
        "the new title must reach the list, got: {body:.3000}"
    );
    assert!(
        !body.contains("Sunday Dinners"),
        "the old title must be gone"
    );

    let held = cooklanghub::cookbook::get(&app.pool, "sam", "sunday-dinners")
        .await
        .expect("cannot read the Cookbook index")
        .expect("the Cookbook must be in the index");
    assert_eq!(held.title, "Long Evenings");
    assert_eq!(held.summary, "Even slower food.");
}
