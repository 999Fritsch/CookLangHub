//! Acceptance tests for putting a Recipe into a Cookbook and taking it out.
//!
//! Forgejo and Git hold what a Cookbook is made of, so no test is content
//! with a page that reads well. Each one asks Forgejo what actually landed:
//! which file the Cookbook holds, which Version of the Recipe it records,
//! and whether the Recipe repository changed at all.

mod support;

use cooklanghub::cookbook;
use cooklanghub::secret::Secret;
use cooklanghub::session::COOKIE_NAME;

use support::{Forgejo, TestApp};

/// The Forgejo administrator that the bootstrap command uses.
const ADMIN: &str = "alex";
/// The person who owns the Cookbooks in these tests.
const OWNER: &str = "sam";
/// Somebody else, who owns Recipes of their own.
const OTHER: &str = "robin";

const SOURCE: &str = "Add @salt{1%pinch} to the #pan{}.";

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
}

async fn ready() -> Ready {
    let forgejo = support::start_forgejo().await;
    forgejo.create_user(ADMIN, true);
    forgejo.create_user(OWNER, false);
    forgejo.create_user(OTHER, false);

    let admin = forgejo.access_token(ADMIN);

    let app = support::start_app(&forgejo.base_url).await;
    app.bootstrap(&admin).await;

    let sam = support::sign_in(&app, &forgejo, OWNER).await;
    let robin = support::sign_in(&app, &forgejo, OTHER).await;

    Ready {
        forgejo,
        app,
        sam,
        robin,
        admin,
    }
}

// ------------------------------------------------------------- the pages

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

/// Post a form as somebody, or as nobody.
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
async fn recipe(app: &TestApp, session: &str, title: &str, private: bool) {
    let response = support::create_recipe(app, session, title, SOURCE, private).await;
    assert_eq!(
        response.status(),
        303,
        "the Recipe `{title}` was not created"
    );
}

/// Make a Cookbook, and insist that it was made.
async fn cookbook(app: &TestApp, session: &str, title: &str, private: bool) {
    let response = support::create_cookbook(app, session, title, "Some words.", private).await;
    assert_eq!(
        response.status(),
        303,
        "the Cookbook `{title}` was not created"
    );
}

/// Put a Recipe into a Cookbook through the page that a person uses.
///
/// `holding` is left out when it is `None`, which is what a form with no
/// choice at all sends.
async fn add(
    app: &TestApp,
    session: &str,
    book: &str,
    recipe: &str,
    holding: Option<&str>,
) -> reqwest::Response {
    let mut fields = vec![("recipe", recipe)];
    if let Some(holding) = holding {
        fields.push(("holding", holding));
    }
    post(
        app,
        &format!("/cookbooks/{book}/recipes"),
        Some(session),
        &fields,
    )
    .await
}

/// Put a Recipe into a Cookbook and insist that it landed.
async fn add_ok(app: &TestApp, session: &str, book: &str, recipe: &str, holding: Option<&str>) {
    let response = add(app, session, book, recipe, holding).await;
    assert_eq!(
        response.status(),
        303,
        "`{recipe}` was not added to `{book}`"
    );
    assert_eq!(location(&response), format!("/cookbooks/{book}"));
}

/// Take a Recipe out of a Cookbook.
async fn remove(app: &TestApp, session: Option<&str>, book: &str, path: &str) -> reqwest::Response {
    post(
        app,
        &format!("/cookbooks/{book}/recipes/remove"),
        session,
        &[("path", path)],
    )
    .await
}

fn location(response: &reqwest::Response) -> String {
    response
        .headers()
        .get("location")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

// ------------------------------------------------------- what Forgejo has

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

/// The Version that `main` points at, as Forgejo reports it.
async fn version(forgejo: &Forgejo, token: &Secret<String>, path: &str) -> String {
    let branch =
        support::forgejo_api(forgejo, token, &format!("/repos/{path}/branches/main")).await;
    branch["commit"]["id"]
        .as_str()
        .expect("Forgejo reported no Version")
        .to_string()
}

/// How many Versions a repository has.
async fn versions(forgejo: &Forgejo, token: &Secret<String>, path: &str) -> usize {
    support::forgejo_api(forgejo, token, &format!("/repos/{path}/commits"))
        .await
        .as_array()
        .map(Vec::len)
        .expect("Forgejo reported no History")
}

/// What the published state of a repository holds, straight from Git.
///
/// This reads the tree that Git itself stores, so a reference to another
/// repository comes back with the mode Git gave it and the exact Version it
/// records. Nothing of this application is between the test and the answer.
async fn tree(forgejo: &Forgejo, token: &Secret<String>, path: &str) -> Vec<serde_json::Value> {
    let head = version(forgejo, token, path).await;
    let tree =
        support::forgejo_api(forgejo, token, &format!("/repos/{path}/git/trees/{head}")).await;
    tree["tree"]
        .as_array()
        .expect("the tree must be a list")
        .clone()
}

/// The one reference that a Cookbook records at this name.
async fn reference(
    forgejo: &Forgejo,
    token: &Secret<String>,
    path: &str,
    name: &str,
) -> serde_json::Value {
    tree(forgejo, token, path)
        .await
        .into_iter()
        .find(|entry| entry["path"] == name)
        .unwrap_or_else(|| panic!("`{path}` records nothing at `{name}`"))
}

/// The names that a Cookbook records a Recipe at, in the order Git holds.
async fn reference_names(forgejo: &Forgejo, token: &Secret<String>, path: &str) -> Vec<String> {
    let mut names: Vec<String> = tree(forgejo, token, path)
        .await
        .into_iter()
        .filter(|entry| entry["mode"] == "160000")
        .map(|entry| entry["path"].as_str().unwrap_or_default().to_string())
        .collect();
    names.sort();
    names
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

/// Where a title sits in a page, so that a test can compare two of them.
fn position(body: &str, needle: &str) -> usize {
    body.find(needle)
        .unwrap_or_else(|| panic!("the page does not name `{needle}`"))
}

// ------------------------------------------------------------ adding one

#[tokio::test]
async fn adding_a_recipe_records_a_reference_and_changes_no_recipe() {
    let Ready {
        forgejo,
        app,
        sam,
        robin: _robin,
        admin,
    } = ready().await;

    recipe(&app, &sam, "Chili", false).await;
    cookbook(&app, &sam, "Sunday Dinners", false).await;

    // Exactly what the Recipe is before anything touches the Cookbook.
    let before = version(&forgejo, &admin, "sam/chili").await;
    let before_files = root_names(&forgejo, &admin, "sam/chili").await;
    let before_versions = versions(&forgejo, &admin, "sam/chili").await;

    // The flow explains both ways of holding a Recipe before the person
    // chooses, and it offers the Recipe.
    let form = page(&app, "/cookbooks/sam/sunday-dinners/recipes", Some(&sam)).await;
    assert!(form.contains("Keep this version"), "got: {form:.2000}");
    assert!(form.contains("Follow future updates"), "got: {form:.2000}");
    assert!(
        form.contains("value=\"pinned\" checked"),
        "Keep this version must be the choice a person starts with"
    );
    assert!(
        form.contains("value=\"sam/chili\""),
        "the Recipe is offered"
    );

    // The form carries no choice at all, so the default is what lands.
    add_ok(&app, &sam, "sam/sunday-dinners", "sam/chili", None).await;

    // Forgejo holds the reference. This is the authority, not the page.
    let recorded = reference(&forgejo, &admin, "sam/sunday-dinners", "chili").await;
    assert_eq!(
        recorded["mode"], "160000",
        "a Recipe is held by reference and never copied in"
    );
    assert_eq!(
        recorded["sha"].as_str(),
        Some(before.as_str()),
        "the reference must record the exact Version that was selected"
    );

    // The file that Git reads names the Recipe and its address, and names
    // no branch, which is what Pinned means.
    let file = modules(&forgejo, &admin, "sam/sunday-dinners").await;
    assert!(file.contains("path = chili"), "got: {file}");
    assert!(
        file.contains(&format!("{}/sam/chili.git", forgejo.base_url)),
        "the reference must carry the address of the Recipe: {file}"
    );
    assert!(
        !file.contains("branch"),
        "a Pinned Recipe follows no branch: {file}"
    );

    // The Recipe repository is untouched. Not one Version, not one file.
    assert_eq!(
        version(&forgejo, &admin, "sam/chili").await,
        before,
        "adding a Recipe must not change it"
    );
    assert_eq!(
        versions(&forgejo, &admin, "sam/chili").await,
        before_versions
    );
    assert_eq!(
        root_names(&forgejo, &admin, "sam/chili").await,
        before_files
    );

    // The Cookbook has a History of its own, and this made one Version.
    assert_eq!(
        versions(&forgejo, &admin, "sam/sunday-dinners").await,
        2,
        "the Cookbook must gain exactly one Version"
    );

    // The Cookbook holds the README and the reference file, and nothing
    // else. There is no section metadata and no order.
    assert_eq!(
        root_names(&forgejo, &admin, "sam/sunday-dinners").await,
        vec![
            ".gitmodules".to_string(),
            "README.md".to_string(),
            "chili".to_string()
        ],
    );

    // The page shows the Recipe, and says how the Cookbook holds it.
    let body = page(&app, "/cookbooks/sam/sunday-dinners", Some(&sam)).await;
    assert!(body.contains("/recipes/sam/chili"), "got: {body:.4000}");
    assert!(body.contains("Chili"));
    assert!(body.contains("Owned by sam"));
    assert!(body.contains("This version"));
}

#[tokio::test]
async fn following_a_recipe_names_the_branch_that_the_cookbook_follows() {
    let Ready {
        forgejo,
        app,
        sam,
        robin: _robin,
        admin,
    } = ready().await;

    recipe(&app, &sam, "Chili", false).await;
    cookbook(&app, &sam, "Sunday Dinners", false).await;

    let published = version(&forgejo, &admin, "sam/chili").await;

    add_ok(
        &app,
        &sam,
        "sam/sunday-dinners",
        "sam/chili",
        Some("following"),
    )
    .await;

    let file = modules(&forgejo, &admin, "sam/sunday-dinners").await;
    assert!(
        file.contains("branch = main"),
        "a Cookbook that follows a Recipe names the branch: {file}"
    );

    // Following records the Version too, so the Cookbook stays exactly
    // reproducible between one update and the next.
    let recorded = reference(&forgejo, &admin, "sam/sunday-dinners", "chili").await;
    assert_eq!(recorded["sha"].as_str(), Some(published.as_str()));

    let body = page(&app, "/cookbooks/sam/sunday-dinners", Some(&sam)).await;
    assert!(body.contains("Follows updates"), "got: {body:.4000}");
}

#[tokio::test]
async fn one_cookbook_holds_one_recipe_at_most_once() {
    let Ready {
        forgejo,
        app,
        sam,
        robin: _robin,
        admin,
    } = ready().await;

    recipe(&app, &sam, "Chili", false).await;
    cookbook(&app, &sam, "Sunday Dinners", false).await;

    add_ok(&app, &sam, "sam/sunday-dinners", "sam/chili", None).await;

    // The page no longer offers a Recipe that is in the Cookbook already.
    let form = page(&app, "/cookbooks/sam/sunday-dinners/recipes", Some(&sam)).await;
    assert!(
        !form.contains("value=\"sam/chili\""),
        "a Recipe that is already there must not be offered again"
    );

    // A form that names it anyway is refused, and nothing changes.
    let response = add(&app, &sam, "sam/sunday-dinners", "sam/chili", None).await;
    assert_eq!(response.status(), 200, "the refusal stays on the page");

    let body = response.text().await.expect("the page has no body");
    assert!(
        body.contains("holds that Recipe already"),
        "got: {body:.4000}"
    );

    assert_eq!(
        reference_names(&forgejo, &admin, "sam/sunday-dinners").await,
        vec!["chili".to_string()],
        "the Cookbook must still hold the Recipe exactly once"
    );
    assert_eq!(
        versions(&forgejo, &admin, "sam/sunday-dinners").await,
        2,
        "a refused add must make no Version"
    );
}

#[tokio::test]
async fn recipes_are_listed_by_title_and_a_repeated_title_shows_its_owner() {
    let Ready {
        forgejo,
        app,
        sam,
        robin,
        admin,
    } = ready().await;

    recipe(&app, &sam, "Toast", false).await;
    recipe(&app, &sam, "Apple Cake", false).await;
    recipe(&app, &robin, "Toast", false).await;
    cookbook(&app, &sam, "Sunday Dinners", false).await;

    // Toast first, so that the second Toast has to find another name.
    add_ok(&app, &sam, "sam/sunday-dinners", "sam/toast", None).await;
    add_ok(&app, &sam, "sam/sunday-dinners", "robin/toast", None).await;
    add_ok(&app, &sam, "sam/sunday-dinners", "sam/apple-cake", None).await;

    // The name of the second Toast was chosen without a question.
    assert_eq!(
        reference_names(&forgejo, &admin, "sam/sunday-dinners").await,
        vec![
            "apple-cake".to_string(),
            "toast".to_string(),
            "toast-2".to_string()
        ],
    );

    // The name comes from the Recipe and it stays. Adding a third Recipe
    // leaves the first two exactly where they were.
    let file = modules(&forgejo, &admin, "sam/sunday-dinners").await;
    assert!(
        file.contains(&format!("{}/sam/toast.git", forgejo.base_url)),
        "got: {file}"
    );
    assert!(
        file.contains(&format!("{}/robin/toast.git", forgejo.base_url)),
        "got: {file}"
    );

    let body = page(&app, "/cookbooks/sam/sunday-dinners", Some(&sam)).await;

    // Alphabetical by Recipe title.
    assert!(
        position(&body, "Apple Cake") < position(&body, "/recipes/sam/toast"),
        "the Recipes must be in alphabetical order by title"
    );

    // Two Recipes with the same title each name their owner, so a cook can
    // tell them apart.
    assert!(body.contains("/recipes/sam/toast"), "got: {body:.6000}");
    assert!(body.contains("/recipes/robin/toast"), "got: {body:.6000}");
    assert!(body.contains("Owned by sam"));
    assert!(body.contains("Owned by robin"));
}

// ---------------------------------------------------------- removing one

#[tokio::test]
async fn removing_a_recipe_changes_the_cookbook_only() {
    let Ready {
        forgejo,
        app,
        sam,
        robin: _robin,
        admin,
    } = ready().await;

    recipe(&app, &sam, "Chili", false).await;
    cookbook(&app, &sam, "Sunday Dinners", false).await;
    cookbook(&app, &sam, "Weeknights", false).await;

    // One Recipe, two Cookbooks. This is what a Cookbook is for.
    add_ok(&app, &sam, "sam/sunday-dinners", "sam/chili", None).await;
    add_ok(&app, &sam, "sam/weeknights", "sam/chili", None).await;

    let before = version(&forgejo, &admin, "sam/chili").await;
    let before_files = root_names(&forgejo, &admin, "sam/chili").await;
    let before_versions = versions(&forgejo, &admin, "sam/chili").await;

    let response = remove(&app, Some(&sam), "sam/sunday-dinners", "chili").await;
    assert_eq!(response.status(), 303);
    assert_eq!(location(&response), "/cookbooks/sam/sunday-dinners");

    // The Recipe repository is exactly as it was. Nothing was deleted.
    assert_eq!(version(&forgejo, &admin, "sam/chili").await, before);
    assert_eq!(
        versions(&forgejo, &admin, "sam/chili").await,
        before_versions
    );
    assert_eq!(
        root_names(&forgejo, &admin, "sam/chili").await,
        before_files
    );
    assert!(
        support::forgejo_api(&forgejo, &admin, "/repos/sam/chili").await["name"] == "chili",
        "the Recipe must still exist"
    );

    // The Cookbook that lost it is back to a README and nothing else.
    assert_eq!(
        root_names(&forgejo, &admin, "sam/sunday-dinners").await,
        vec!["README.md".to_string()],
        "the last Recipe takes the reference file with it"
    );
    assert_eq!(
        versions(&forgejo, &admin, "sam/sunday-dinners").await,
        3,
        "the removal makes one Cookbook Version"
    );

    // The other Cookbook is untouched, so a Recipe stays in as many
    // Cookbooks as hold it.
    assert_eq!(
        reference_names(&forgejo, &admin, "sam/weeknights").await,
        vec!["chili".to_string()],
    );

    let empty = page(&app, "/cookbooks/sam/sunday-dinners", Some(&sam)).await;
    assert!(empty.contains("This Cookbook has no Recipes yet."));

    let kept = page(&app, "/cookbooks/sam/weeknights", Some(&sam)).await;
    assert!(kept.contains("/recipes/sam/chili"), "got: {kept:.4000}");
}

// ------------------------------------------------------- what may be seen

#[tokio::test]
async fn a_recipe_that_a_person_cannot_read_gives_nothing_away() {
    let Ready {
        forgejo,
        app,
        sam,
        robin,
        admin,
    } = ready().await;

    recipe(&app, &sam, "Secret Birthday Cake", true).await;
    cookbook(&app, &sam, "Sunday Dinners", false).await;

    // A private Recipe of this person is offered to them, and only to them.
    let form = page(&app, "/cookbooks/sam/sunday-dinners/recipes", Some(&sam)).await;
    assert!(
        form.contains("value=\"sam/secret-birthday-cake\""),
        "a person must be able to add their own private Recipe: {form:.4000}"
    );

    add_ok(
        &app,
        &sam,
        "sam/sunday-dinners",
        "sam/secret-birthday-cake",
        None,
    )
    .await;

    // Forgejo really holds the reference.
    assert_eq!(
        reference_names(&forgejo, &admin, "sam/sunday-dinners").await,
        vec!["secret-birthday-cake".to_string()],
    );

    // The owner sees the Recipe, because Forgejo shows it to them.
    let owner = page(&app, "/cookbooks/sam/sunday-dinners", Some(&sam)).await;
    assert!(owner.contains("Secret Birthday Cake"));

    // Everybody else is told that something is there, and nothing else.
    // The title, the owner, and the name all say what the Recipe is.
    for (who, session) in [("robin", Some(robin.as_str())), ("a visitor", None)] {
        let body = page(&app, "/cookbooks/sam/sunday-dinners", session).await;

        assert!(
            body.contains(cookbook::UNAVAILABLE_MESSAGE),
            "{who} must be told that a Recipe is there: {body:.6000}"
        );
        assert!(
            !body.contains("Secret Birthday Cake"),
            "{who} must not read the title"
        );
        assert!(
            !body.contains("secret-birthday-cake"),
            "{who} must not read the name, which carries the title"
        );
        assert!(
            !body.contains("This Cookbook has no Recipes yet."),
            "{who} must not be told the Cookbook is empty"
        );
    }
}

#[tokio::test]
async fn a_recipe_that_is_gone_stays_visible_and_is_never_repaired() {
    let Ready {
        forgejo,
        app,
        sam,
        robin: _robin,
        admin,
    } = ready().await;

    recipe(&app, &sam, "Chili", false).await;
    recipe(&app, &sam, "Toast", false).await;
    cookbook(&app, &sam, "Sunday Dinners", false).await;

    add_ok(&app, &sam, "sam/sunday-dinners", "sam/chili", None).await;
    add_ok(&app, &sam, "sam/sunday-dinners", "sam/toast", None).await;

    let before = version(&forgejo, &admin, "sam/sunday-dinners").await;

    // Somebody deletes the Recipe outside this application. The Cookbook
    // now names something that is not there.
    let deleted = support::forgejo_write(
        &forgejo,
        &admin,
        reqwest::Method::DELETE,
        "/repos/sam/chili",
        serde_json::json!({}),
    )
    .await;
    assert!(
        deleted.status().is_success(),
        "the Recipe was not deleted: {}",
        deleted.status()
    );

    // The index still holds the row that names the Recipe, so this also
    // proves that no title ever comes from the cache.
    let body = page(&app, "/cookbooks/sam/sunday-dinners", Some(&sam)).await;

    assert!(
        body.contains(cookbook::UNAVAILABLE_MESSAGE),
        "the entry must stay visible and say what is wrong: {body:.6000}"
    );
    assert!(
        !body.contains("/recipes/sam/chili"),
        "a Recipe that is gone must not be offered as a link"
    );
    assert!(
        body.contains("/recipes/sam/toast"),
        "the other Recipes must still be there"
    );
    assert!(body.contains("Open in Forgejo"));

    // Nothing was repaired and nothing was removed. Git holds exactly what
    // it held, so History still says what the Cookbook was made of.
    assert_eq!(
        version(&forgejo, &admin, "sam/sunday-dinners").await,
        before,
        "the application must not rewrite a reference"
    );
    assert_eq!(
        reference_names(&forgejo, &admin, "sam/sunday-dinners").await,
        vec!["chili".to_string(), "toast".to_string()],
    );
}

#[tokio::test]
async fn only_a_person_who_can_change_a_cookbook_can_add_or_remove_a_recipe() {
    let Ready {
        forgejo,
        app,
        sam,
        robin,
        admin,
    } = ready().await;

    recipe(&app, &sam, "Chili", false).await;
    recipe(&app, &robin, "Toast", false).await;
    cookbook(&app, &sam, "Sunday Dinners", false).await;

    add_ok(&app, &sam, "sam/sunday-dinners", "sam/chili", None).await;

    // Robin can read the Cookbook and can change nothing in it.
    let body = page(&app, "/cookbooks/sam/sunday-dinners", Some(&robin)).await;
    assert!(body.contains("/recipes/sam/chili"));
    assert!(
        !body.contains("Add a Recipe"),
        "the page must not offer an action that Forgejo refuses"
    );
    assert!(!body.contains("recipes/remove"));

    assert_eq!(
        get(&app, "/cookbooks/sam/sunday-dinners/recipes", Some(&robin))
            .await
            .status(),
        403
    );
    assert_eq!(
        add(&app, &robin, "sam/sunday-dinners", "robin/toast", None)
            .await
            .status(),
        403
    );
    assert_eq!(
        remove(&app, Some(&robin), "sam/sunday-dinners", "chili")
            .await
            .status(),
        403
    );

    // A visitor with no account is sent to sign in and changes nothing.
    assert_eq!(
        remove(&app, None, "sam/sunday-dinners", "chili")
            .await
            .status(),
        303
    );

    // Forgejo still holds exactly what Sam put there.
    assert_eq!(
        reference_names(&forgejo, &admin, "sam/sunday-dinners").await,
        vec!["chili".to_string()],
    );
    assert_eq!(versions(&forgejo, &admin, "sam/sunday-dinners").await, 2);
}

// -------------------------------------------------------------- the index

#[tokio::test]
async fn the_recipes_of_a_cookbook_never_live_in_the_index() {
    let Ready {
        forgejo,
        app,
        sam,
        robin: _robin,
        admin,
    } = ready().await;

    recipe(&app, &sam, "Chili", false).await;
    cookbook(&app, &sam, "Sunday Dinners", false).await;
    add_ok(&app, &sam, "sam/sunday-dinners", "sam/chili", None).await;

    // Throw the whole cache away. Forgejo and Git hold what a Cookbook is
    // made of, so the page must not need one row of it.
    sqlx::query("DELETE FROM cookbook_index")
        .execute(&app.pool)
        .await
        .expect("cannot empty the Cookbook index");
    sqlx::query("DELETE FROM recipe_index")
        .execute(&app.pool)
        .await
        .expect("cannot empty the Recipe index");

    let empty = page(&app, "/cookbooks/sam/sunday-dinners", Some(&sam)).await;
    assert!(
        empty.contains("/recipes/sam/chili"),
        "the Recipes of a Cookbook come from Git, not from the index: {empty:.4000}"
    );
    assert!(empty.contains("Chili"));

    // Reading Forgejo again writes every row back, and changes nothing in
    // Forgejo and nothing in Git.
    let before = version(&forgejo, &admin, "sam/sunday-dinners").await;
    app.reconcile().await;
    app.reconcile_cookbooks().await;

    assert_eq!(
        version(&forgejo, &admin, "sam/sunday-dinners").await,
        before,
        "a reconciliation must write nothing"
    );
    assert_eq!(
        reference_names(&forgejo, &admin, "sam/sunday-dinners").await,
        vec!["chili".to_string()],
    );

    let rebuilt = page(&app, "/cookbooks/sam/sunday-dinners", Some(&sam)).await;
    assert!(rebuilt.contains("/recipes/sam/chili"));
    assert!(rebuilt.contains("Chili"));

    // The Cookbook index holds the Cookbook and says nothing about which
    // Recipes it has.
    let rows = cookbook::all(&app.pool)
        .await
        .expect("cannot read the Cookbook index");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].title, "Sunday Dinners");
}
