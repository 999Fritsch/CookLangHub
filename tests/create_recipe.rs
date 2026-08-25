//! Acceptance tests for creating a Recipe.
//!
//! Every test drives the real create form against a real Forgejo, and then
//! asks Forgejo what actually landed there.

mod support;

use cooklanghub::session::COOKIE_NAME;

/// A signed-in person against a bootstrapped application.
async fn ready() -> (support::Forgejo, support::TestApp, String) {
    let forgejo = support::start_forgejo().await;
    forgejo.create_user("alex", true);
    forgejo.create_user("sam", false);
    let token = forgejo.access_token("alex");

    let app = support::start_app(&forgejo.base_url).await;
    app.bootstrap(&token).await;

    let session = support::sign_in(&app, &forgejo, "sam").await;
    (forgejo, app, session)
}

#[tokio::test]
async fn a_recipe_becomes_a_repository_with_one_version() {
    let (forgejo, app, session) = ready().await;
    let token = forgejo.access_token("sam");

    let response = support::create_recipe(
        &app,
        &session,
        "Chili Sin Carne",
        "Chop the @onion{1} in a #pan{} for ~{8%Min.}.",
        false,
    )
    .await;

    assert_eq!(response.status(), 303, "creation must redirect to the Recipe");
    let location = response
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert_eq!(location, "/recipes/sam/chili-sin-carne");

    // The repository exists, is public, and carries both topics.
    let repo = support::forgejo_api(&forgejo, &token, "/repos/sam/chili-sin-carne").await;
    assert_eq!(repo["name"], "chili-sin-carne");
    assert_eq!(repo["private"], false);
    assert_eq!(repo["default_branch"], "main");

    let topics = support::forgejo_api(&forgejo, &token, "/repos/sam/chili-sin-carne/topics").await;
    let topics: Vec<String> = topics["topics"]
        .as_array()
        .expect("topics must be a list")
        .iter()
        .map(|t| t.as_str().unwrap_or_default().to_string())
        .collect();
    assert!(topics.contains(&"cooklang".to_string()), "got {topics:?}");
    assert!(topics.contains(&"recipe".to_string()), "got {topics:?}");

    // Exactly one Version.
    let commits =
        support::forgejo_api(&forgejo, &token, "/repos/sam/chili-sin-carne/commits").await;
    assert_eq!(
        commits.as_array().map(Vec::len),
        Some(1),
        "a new Recipe must have exactly one Version"
    );

    // The Version holds recipe.cook, with the title written into it.
    let contents =
        support::forgejo_api(&forgejo, &token, "/repos/sam/chili-sin-carne/contents").await;
    let names: Vec<String> = contents
        .as_array()
        .expect("contents must be a list")
        .iter()
        .map(|f| f["name"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(names, vec!["recipe.cook".to_string()]);
}

#[tokio::test]
async fn the_title_field_writes_the_cooklang_metadata() {
    let (forgejo, app, session) = ready().await;
    let token = forgejo.access_token("sam");

    support::create_recipe(&app, &session, "Onion Soup", "Chop the @onion{1}.", false).await;

    let raw = reqwest::Client::new()
        .get(format!(
            "{}/api/v1/repos/sam/onion-soup/raw/recipe.cook",
            forgejo.base_url
        ))
        .header("Authorization", format!("token {}", token.expose()))
        .send()
        .await
        .expect("cannot read the Recipe file")
        .text()
        .await
        .expect("cannot read the body");

    assert!(raw.contains("title: Onion Soup"), "got: {raw}");
    assert!(raw.contains("@onion{1}"), "the source must survive");

    // The application keeps no second title: the parser reads it back out
    // of the file itself.
    assert_eq!(
        cooklanghub::recipe::parse(&raw).title.as_deref(),
        Some("Onion Soup")
    );
}

#[tokio::test]
async fn a_title_only_recipe_is_valid() {
    let (forgejo, app, session) = ready().await;
    let token = forgejo.access_token("sam");

    let response = support::create_recipe(&app, &session, "Just A Title", "", false).await;
    let status = response.status();
    if status != 303 {
        let body = response.text().await.unwrap_or_default();
        panic!("expected 303, got {status}. body:
{body:.2000}");
    }

    let repo = support::forgejo_api(&forgejo, &token, "/repos/sam/just-a-title").await;
    assert_eq!(repo["name"], "just-a-title");
}

#[tokio::test]
async fn a_cooklang_error_stops_the_creation_and_is_shown() {
    let (forgejo, app, session) = ready().await;
    let token = forgejo.access_token("sam");

    // A timer needs a unit that the converter knows.
    let response =
        support::create_recipe(&app, &session, "Broken", "Wait ~{5%bananas}.", false).await;

    assert_eq!(response.status(), 200, "the form comes back, it does not redirect");

    let body = response.text().await.expect("cannot read the body");
    assert!(
        body.contains("cannot be created"),
        "the person must see why"
    );
    assert!(body.to_lowercase().contains("bananas"), "got: {body:.400}");
    // What they typed is still there.
    assert!(body.contains("Wait ~{5%bananas}."));

    // Nothing was created in Forgejo.
    let repos = support::forgejo_api(&forgejo, &token, "/user/repos").await;
    let names: Vec<String> = repos
        .as_array()
        .map(|list| {
            list.iter()
                .map(|r| r["name"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        !names.contains(&"broken".to_string()),
        "a refused Recipe must leave nothing behind, got {names:?}"
    );
}

#[tokio::test]
async fn a_cooklang_warning_does_not_stop_the_creation() {
    let (forgejo, app, session) = ready().await;
    let token = forgejo.access_token("sam");

    // A reference to an ingredient that was never introduced warns but does
    // not stop publishing.
    let source = "Add the &missing{1} to the pot.";
    let parsed = cooklanghub::recipe::parse(source);
    assert!(
        !parsed.warnings.is_empty() || parsed.is_valid(),
        "this fixture is meant to warn, not to fail"
    );

    let response = support::create_recipe(&app, &session, "Warns A Bit", source, false).await;
    assert_eq!(response.status(), 303, "a warning must not stop creation");

    let repo = support::forgejo_api(&forgejo, &token, "/repos/sam/warns-a-bit").await;
    assert_eq!(repo["name"], "warns-a-bit");
}

#[tokio::test]
async fn public_is_the_default_and_private_is_honoured() {
    let (forgejo, app, session) = ready().await;
    let token = forgejo.access_token("sam");

    // The form with no visibility field at all must give a public Recipe.
    let response = support::client()
        .post(app.url("/recipes/new"))
        .header("cookie", format!("{COOKIE_NAME}={session}"))
        .form(&[("title", "Default Visibility"), ("source", "")])
        .send()
        .await
        .expect("cannot post the form");
    assert_eq!(response.status(), 303);

    let public = support::forgejo_api(&forgejo, &token, "/repos/sam/default-visibility").await;
    assert_eq!(public["private"], false, "public is the default");

    support::create_recipe(&app, &session, "Secret Dish", "", true).await;
    let private = support::forgejo_api(&forgejo, &token, "/repos/sam/secret-dish").await;
    assert_eq!(private["private"], true);
}

#[tokio::test]
async fn a_name_collision_is_resolved_without_a_user_action() {
    let (forgejo, app, session) = ready().await;
    let token = forgejo.access_token("sam");

    // Two Recipes may share a title. The person never sees the slug.
    for _ in 0..3 {
        let response = support::create_recipe(&app, &session, "Pancakes", "", false).await;
        assert_eq!(response.status(), 303);
    }

    for name in ["pancakes", "pancakes-2", "pancakes-3"] {
        let repo = support::forgejo_api(&forgejo, &token, &format!("/repos/sam/{name}")).await;
        assert_eq!(repo["name"], name);
    }
}

#[tokio::test]
async fn the_version_is_attributed_to_the_signed_in_person() {
    let (forgejo, app, session) = ready().await;
    let token = forgejo.access_token("sam");

    support::create_recipe(&app, &session, "Attributed", "", false).await;

    let commits = support::forgejo_api(&forgejo, &token, "/repos/sam/attributed/commits").await;
    let commit = &commits.as_array().expect("a list")[0];

    let author = &commit["commit"]["author"];
    let committer = &commit["commit"]["committer"];

    // Both sides carry the person, so History reads correctly outside this
    // application too.
    assert_eq!(author["email"], committer["email"]);
    assert_eq!(author["name"], committer["name"]);

    // Forgejo keeps addresses private by default in the bundled deployment,
    // so the address must be the no-reply one and never the real address.
    let email = author["email"].as_str().unwrap_or_default();
    assert!(!email.is_empty(), "the Version needs an author address");
    assert!(
        !email.contains("sam@example.test"),
        "the private address of the person must not reach History, got {email}"
    );
    assert!(
        email.contains("noreply"),
        "expected the Forgejo no-reply address, got {email}"
    );
}

#[tokio::test]
async fn a_source_larger_than_one_megabyte_is_refused() {
    let (_forgejo, app, session) = ready().await;

    let huge = "a".repeat(1024 * 1024 + 100);
    let response = support::create_recipe(&app, &session, "Too Big", &huge, false).await;

    assert_eq!(response.status(), 200, "the form must come back");
    let body = response.text().await.expect("cannot read the body");
    assert!(body.contains("larger than 1 MB"), "got: {body:.400}");
}

#[tokio::test]
async fn the_recipe_page_shows_the_source_and_opens_in_forgejo() {
    let (forgejo, app, session) = ready().await;

    support::create_recipe(
        &app,
        &session,
        "Readable",
        "Chop the @onion{1} in a #pan{}.",
        false,
    )
    .await;

    let body = support::client()
        .get(app.url("/recipes/sam/readable"))
        .header("cookie", format!("{COOKIE_NAME}={session}"))
        .send()
        .await
        .expect("cannot reach the Recipe page")
        .text()
        .await
        .expect("cannot read the body");

    assert!(body.contains("Readable"), "the title must show");
    assert!(body.contains("@onion{1}"), "the source must show");
    assert!(body.contains("Open in Forgejo"));
    assert!(
        body.contains(&format!("{}/sam/readable", forgejo.base_url)),
        "the link must point at the repository"
    );

    // No Git or Forgejo word leaks into the ordinary flow.
    for word in ["commit", "branch", "repository", "pull request"] {
        assert!(
            !body.to_lowercase().contains(word),
            "the page must not say `{word}`"
        );
    }
}

#[tokio::test]
async fn a_private_recipe_is_not_readable_by_a_stranger() {
    let (_forgejo, app, session) = ready().await;

    support::create_recipe(&app, &session, "Secret Dish", "", true).await;

    let anonymous = support::client()
        .get(app.url("/recipes/sam/secret-dish"))
        .send()
        .await
        .expect("cannot reach the Recipe page");

    assert_eq!(
        anonymous.status(),
        404,
        "Forgejo owns the permissions, and a stranger must not see a private Recipe"
    );
}

#[tokio::test]
async fn a_new_recipe_appears_in_the_list_of_the_person() {
    let (_forgejo, app, session) = ready().await;

    support::create_recipe(&app, &session, "Findable Dish", "", false).await;

    let body = support::client()
        .get(app.url("/"))
        .header("cookie", format!("{COOKIE_NAME}={session}"))
        .send()
        .await
        .expect("cannot reach the index page")
        .text()
        .await
        .expect("cannot read the body");

    assert!(
        body.contains("/recipes/sam/findable-dish"),
        "a Recipe must be findable after it is created"
    );

    // The list shows the title a person wrote, not the technical slug.
    assert!(
        body.contains("Findable Dish"),
        "the list must show the Cooklang title"
    );
}

#[tokio::test]
async fn signing_in_is_required_before_creating() {
    let (_forgejo, app, _session) = ready().await;

    let response = support::client()
        .get(app.url("/recipes/new"))
        .send()
        .await
        .expect("cannot reach the create form");

    assert_eq!(response.status(), 303);
    assert_eq!(
        response
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok()),
        Some("/auth/sign-in")
    );
}

#[tokio::test]
async fn umlauts_survive_the_whole_round_trip() {
    // The first Recipes this project was tested with are German. A lost
    // umlaut turns `Äpfel` into a replacement mark, and the person cannot
    // tell whether their Recipe or the application is at fault.
    let (forgejo, app, session) = ready().await;
    let token = forgejo.access_token("sam");

    let source = "@Äpfel{2} schälen. In eine #Schüssel{} geben und mit @Rapsöl{1%TL} pürieren. Straße, Grüße, Müsli.";

    let response =
        support::create_recipe(&app, &session, "Frischer Obstbrei", source, false).await;
    assert_eq!(response.status(), 303);

    // What Forgejo stores must carry the same letters that were written.
    let stored = reqwest::Client::new()
        .get(format!(
            "{}/api/v1/repos/sam/frischer-obstbrei/raw/recipe.cook",
            forgejo.base_url
        ))
        .header("Authorization", format!("token {}", token.expose()))
        .send()
        .await
        .expect("cannot read the Recipe file")
        .bytes()
        .await
        .expect("cannot read the body");

    let stored = String::from_utf8(stored.to_vec()).expect("the stored file must be UTF-8");

    for word in [
        "Äpfel", "schälen", "Schüssel", "Rapsöl", "pürieren", "Straße", "Grüße", "Müsli",
    ] {
        assert!(stored.contains(word), "`{word}` did not survive storage");
    }
    assert!(
        !stored.contains('\u{fffd}'),
        "a replacement mark reached the stored Recipe"
    );

    // And the page must carry them too.
    let page = support::client()
        .get(app.url("/recipes/sam/frischer-obstbrei"))
        .header("cookie", format!("{COOKIE_NAME}={session}"))
        .send()
        .await
        .expect("cannot reach the Recipe page")
        .text()
        .await
        .expect("cannot read the body");

    for word in ["Äpfel", "Schüssel", "Rapsöl"] {
        assert!(page.contains(word), "`{word}` did not survive the page");
    }
    assert!(
        !page.contains('\u{fffd}'),
        "a replacement mark reached the Recipe page"
    );
}

#[tokio::test]
async fn a_title_with_umlauts_keeps_its_letters_and_gets_an_ascii_slug() {
    let (forgejo, app, session) = ready().await;
    let token = forgejo.access_token("sam");

    let response =
        support::create_recipe(&app, &session, "Pfannekuchen für Gäste", "", false).await;
    assert_eq!(response.status(), 303);

    // The slug is technical and stays ASCII. The title keeps its letters.
    let repo = support::forgejo_api(&forgejo, &token, "/repos/sam/pfannekuchen-fuer-gaeste").await;
    assert_eq!(repo["name"], "pfannekuchen-fuer-gaeste");

    let body = support::client()
        .get(app.url("/"))
        .header("cookie", format!("{COOKIE_NAME}={session}"))
        .send()
        .await
        .expect("cannot reach the index page")
        .text()
        .await
        .expect("cannot read the body");

    assert!(
        body.contains("Pfannekuchen für Gäste"),
        "the list must show the title with its own letters"
    );
}
