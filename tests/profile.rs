//! Acceptance tests for the profile of one cook.
//!
//! Every test drives the real page against a real Forgejo, because the
//! question this page asks is a permission question and Forgejo is the only
//! authority on it. A mock would answer whatever this application expected,
//! which is exactly the mistake these tests exist to catch.

mod support;

use std::collections::HashSet;

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
    /// An access token of Jo, for the questions that a test puts to Forgejo
    /// itself with the credential of another ordinary cook.
    jo_token: Secret<String>,
}

async fn ready() -> Ready {
    let forgejo = support::start_forgejo().await;
    forgejo.create_user("alex", true);
    forgejo.create_user("sam", false);
    forgejo.create_user("jo", false);

    let admin = forgejo.access_token("alex");
    let sam_token = forgejo.access_token("sam");
    let jo_token = forgejo.access_token("jo");

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
        jo_token,
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

/// One card of a page, from its link to the end of it.
///
/// Two pages that share a card partial give the same text back, so this is
/// how a test holds them together.
fn card_html(body: &str, href: &str) -> String {
    let needle = format!("<a href=\"{href}\"");
    let at = body
        .find(&needle)
        .unwrap_or_else(|| panic!("the page carries no card for `{href}`"));
    let end = body[at..]
        .find("</a>")
        .unwrap_or_else(|| panic!("the card for `{href}` does not end"));

    // Whitespace differs with the indentation of the page around it, and
    // that is not what a cook sees.
    body[at..at + end]
        .split_whitespace()
        .collect::<Vec<&str>>()
        .join(" ")
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
/// The same check as `tests/history.rs`, because the rule is the same on
/// every page. Whole words only: `Sharing` is an area of a Recipe and must
/// not be read as the identifier that Git uses.
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

/// Ask Forgejo something directly, with a credential or with none at all.
///
/// The whole page rests on what Forgejo answers here, so the tests measure
/// that answer instead of taking it from the documentation. `None` is a
/// visitor, exactly as it is inside the application.
async fn forgejo_asked(
    forgejo: &Forgejo,
    path: &str,
    token: Option<&Secret<String>>,
) -> (u16, String) {
    let mut request = support::client().get(format!("{}/api/v1{path}", forgejo.base_url));
    if let Some(token) = token {
        request = request.header("Authorization", format!("token {}", token.expose()));
    }

    let response = request.send().await.expect("cannot reach the Forgejo API");
    let status = response.status().as_u16();
    (status, response.text().await.unwrap_or_default())
}

/// Give a cook a full name, as an administrator does.
///
/// Forgejo owns the name. A test that must prove the application stores no
/// name uses one that nothing else on the machine can hold.
async fn set_full_name(forgejo: &Forgejo, admin: &Secret<String>, login: &str, name: &str) {
    let response = support::forgejo_write(
        forgejo,
        admin,
        reqwest::Method::PATCH,
        &format!("/admin/users/{login}"),
        serde_json::json!({
            "login_name": login,
            "source_id": 0,
            "full_name": name,
        }),
    )
    .await;

    let status = response.status();
    assert!(
        status.is_success(),
        "cannot name {login}: {status} {}",
        response.text().await.unwrap_or_default()
    );
}

/// Where the operational database holds this text, as `table.column`.
///
/// Every table and every column is read, so a new table cannot quietly
/// become the place where a profile is kept.
async fn stored_in(app: &TestApp, needle: &str) -> Vec<String> {
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
    )
    .fetch_all(&app.pool)
    .await
    .expect("cannot read the schema");

    assert!(!tables.is_empty(), "the schema must have tables to search");

    let mut places: Vec<String> = Vec::new();

    for table in &tables {
        // Every name here comes out of the schema of this same database, so
        // no value from outside reaches the text of a statement. Only the
        // text that is searched for is bound.
        let columns: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT name FROM pragma_table_info('{table}')"
        )))
        .fetch_all(&app.pool)
        .await
        .expect("cannot read the columns");

        for column in &columns {
            let found: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT COUNT(*) FROM \"{table}\" WHERE CAST(\"{column}\" AS TEXT) LIKE ?"
            )))
            .bind(format!("%{needle}%"))
            .fetch_one(&app.pool)
            .await
            .expect("cannot search the column");

            if found > 0 {
                places.push(format!("{table}.{column}"));
            }
        }
    }

    places
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
        jo_token: _jo_token,
    } = ready().await;

    support::create_recipe(&app, &sam, "Sam Soup", "Warm the @broth{1%l}.", false).await;
    support::create_recipe(&app, &sam, "Sam Secret", "Add the @salt{1%pinch}.", true).await;
    support::create_recipe(&app, &jo, "Jo Stew", "Cook the @beans{2%cups}.", false).await;

    // An anonymous visitor: no session cookie at all, not a spent one and
    // not a signed-out one. Public means public, so the page opens without
    // an account and holds the public Recipe only.
    let anonymous = get(&app, "/cooks/sam", None).await;
    assert_eq!(
        anonymous.status(),
        200,
        "a public profile must open with no session at all"
    );
    let visitor = anonymous.text().await.expect("the page has no body");
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
    assert_cooking_words(&visitor);

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
    assert_cooking_words(&own);

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
    assert_cooking_words(&unknown);
}

/// A name that nothing else on the machine can hold.
///
/// Forgejo owns it, the profile shows it, and no cell of the operational
/// database may carry it. That is the whole of "no second profile record",
/// said in a way a test can check.
const SAM_FULL_NAME: &str = "Samantha Peppercorn";

#[tokio::test]
async fn the_application_keeps_no_second_profile_record() {
    let Ready {
        forgejo,
        app,
        sam,
        jo,
        admin,
        sam_token,
        jo_token: _jo_token,
    } = ready().await;

    support::create_recipe(&app, &sam, "Sam Soup", "Warm the @broth{1%l}.", false).await;
    create_cookbook(&forgejo, &sam_token, "weeknight-dinners", false).await;

    // The name is set after Sam signed in, so the session row of Sam still
    // holds the old one. Anything that carries the new name from here on
    // was written by a profile read, which is what this test forbids.
    set_full_name(&forgejo, &admin, "sam", SAM_FULL_NAME).await;

    // Two reads, neither of them by Sam. A visitor has no credential, and Jo
    // has one that is not Sam's.
    let visitor = page(&app, "/cooks/sam", None).await;
    assert!(
        visitor.contains(SAM_FULL_NAME),
        "the profile must show the name that Forgejo holds now"
    );
    let other = page(&app, "/cooks/sam", Some(&jo)).await;
    assert!(other.contains(SAM_FULL_NAME));

    // No table is named for a person.
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

    // The search itself works. The title of a Recipe is in the index, and
    // the search finds it, so an empty answer below means an empty answer
    // and not a search that looks nowhere.
    assert!(
        !stored_in(&app, "Sam Soup").await.is_empty(),
        "the search must find what the index really holds"
    );

    // And no cell of any table holds the name, in any table and any column.
    assert_eq!(
        stored_in(&app, SAM_FULL_NAME).await,
        Vec::<String>::new(),
        "the application must keep no record of this cook"
    );

    // The two local tables that name a Recipe and a Cookbook are caches.
    // Emptying both must change nothing that a person sees, because Forgejo
    // and Git hold them and the index only supplies the words on a card.
    for statement in ["DELETE FROM recipe_index", "DELETE FROM cookbook_index"] {
        sqlx::query(statement)
            .execute(&app.pool)
            .await
            .expect("cannot empty the index");
    }

    let again = page(&app, "/cooks/sam", None).await;
    for fact in [
        SAM_FULL_NAME,
        "Sam Soup",
        "weeknight-dinners",
        "Owned by sam",
    ] {
        assert!(
            again.contains(fact),
            "the profile must be rebuildable from Forgejo alone, but `{fact}` is gone"
        );
    }

    // The rebuild wrote the caches again, and still no name.
    assert_eq!(
        stored_in(&app, SAM_FULL_NAME).await,
        Vec::<String>::new(),
        "a rebuild must not write a record of this cook either"
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
        jo_token,
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

    // What Forgejo itself answers, measured rather than taken from its
    // documentation. The page is built from these two answers and from
    // nothing else, so the setting is in force here because it is in force
    // there.
    //
    // The walk goes public, limited, private, and never back. Forgejo 15
    // answers 200 to a request that sets a profile back to public and keeps
    // the setting it had, so a test that turned round would measure that
    // and not the application.
    let sam_id = {
        let (status, body) = forgejo_asked(&forgejo, "/users/sam", Some(&admin)).await;
        assert_eq!(status, 200, "Forgejo must name Sam to an administrator");
        serde_json::from_str::<serde_json::Value>(&body).expect("the answer is not JSON")["id"]
            .as_i64()
            .expect("the answer carries no identifier")
    };
    let owned = format!("/repos/search?uid={sam_id}&q=cooklang&topic=true&limit=10");

    /// How many repositories Forgejo names in an answer.
    fn named(body: &str) -> usize {
        serde_json::from_str::<serde_json::Value>(body).expect("the answer is not JSON")["data"]
            .as_array()
            .map(|found| found.len())
            .unwrap_or_default()
    }

    /// Assert what Forgejo answers about one cook to one credential.
    ///
    /// Two questions, because the page asks two. `shown` is what Forgejo
    /// does with both of them, and the second one is the surprise: Forgejo
    /// names no repository of a hidden cook either, and it does that for a
    /// public repository as well.
    async fn measure(
        forgejo: &Forgejo,
        owned: &str,
        setting: &str,
        who: &str,
        token: Option<&Secret<String>>,
        shown: bool,
    ) {
        let (status, _) = forgejo_asked(forgejo, "/users/sam", token).await;
        assert_eq!(
            status,
            if shown { 200 } else { 404 },
            "Forgejo answers otherwise about a {setting} profile to {who}"
        );

        let (status, body) = forgejo_asked(forgejo, owned, token).await;
        assert_eq!(status, 200, "the search itself must answer");
        assert_eq!(
            named(&body) > 0,
            shown,
            "Forgejo names the Recipes of a {setting} profile otherwise to {who}"
        );
    }

    // Public. Forgejo shows the profile to everybody, and so does the page.
    measure(&forgejo, &owned, "public", "a visitor", None, true).await;
    measure(
        &forgejo,
        &owned,
        "public",
        "another cook",
        Some(&jo_token),
        true,
    )
    .await;

    let open = page(&app, "/cooks/sam", None).await;
    assert!(
        open.contains("Sam Soup"),
        "a public profile must open to a visitor"
    );

    // A limited profile is for people with an account. Forgejo hides it
    // from a visitor, so this application shows a visitor nothing.
    set_profile_visibility(&forgejo, &admin, "sam", "limited").await;

    measure(&forgejo, &owned, "limited", "a visitor", None, false).await;
    measure(
        &forgejo,
        &owned,
        "limited",
        "another cook",
        Some(&jo_token),
        true,
    )
    .await;

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
    assert!(
        refused.contains("Sign in"),
        "a visitor must read what can change the answer"
    );
    assert_cooking_words(&refused);

    // The setting shows less, and this is the measure of it: the page a
    // visitor gets is shorter than the page they got a moment ago.
    assert!(
        refused.len() < open.len(),
        "a limited profile must show a visitor less than a public one"
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
    assert!(
        allowed.contains("1 Recipe"),
        "a signed-in cook reads the whole of a limited profile"
    );
    assert_cooking_words(&allowed);

    // A private profile is for its owner. Forgejo hides it from every
    // ordinary cook, so this application does the same.
    set_profile_visibility(&forgejo, &admin, "sam", "private").await;

    measure(&forgejo, &owned, "private", "a visitor", None, false).await;
    measure(
        &forgejo,
        &owned,
        "private",
        "another cook",
        Some(&jo_token),
        false,
    )
    .await;

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
        jo_token: _jo_token,
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
    // This test was written while Cookbooks had no page here, so a card led
    // to Forgejo and the page said why. Cookbooks have a page now, and the
    // card must lead to it rather than send a person away.
    assert!(
        visitor.contains("/cookbooks/sam/weeknight-dinners"),
        "a Cookbook card must lead to the Cookbook"
    );
    assert!(
        !visitor.contains("no Cookbook page yet"),
        "there is a Cookbook page now, so nothing should say otherwise"
    );
    assert!(
        !visitor.contains(&format!("{}/sam/weeknight-dinners", forgejo.base_url)),
        "a Cookbook that this application can show must not send a person to Forgejo"
    );

    // Both kinds of card say who owns the thing, so the words are on the
    // page twice: once for the Recipe and once for the Cookbook.
    assert_eq!(
        visitor.matches("Owned by sam").count(),
        2,
        "the Recipe card and the Cookbook card must both say Owned by"
    );

    // One card, one behaviour. The Cookbook card here is the card of every
    // other Cookbook list, so the two are the same markup and cannot drift
    // apart. A third card was written here once, and it kept a message
    // about Forgejo long after the Cookbook page arrived.
    let explore = page(&app, "/explore/cookbooks", None).await;
    let here = card_html(&visitor, "/cookbooks/sam/weeknight-dinners");
    let there = card_html(&explore, "/cookbooks/sam/weeknight-dinners");
    assert_eq!(
        here, there,
        "a Cookbook must look the same on a profile as it does on Explore"
    );

    assert_cooking_words(&visitor);

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
    assert_cooking_words(&own);
}
