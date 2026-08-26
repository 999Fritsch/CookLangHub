//! What a person chooses about how a page looks.
//!
//! A choice here lives in a cookie and the server writes the result onto the
//! page, so nothing flashes while the page loads and no script is needed.
//! Nothing is stored in the database and nothing changes in a Recipe.

mod support;

use support::TestApp;

async fn get(app: &TestApp, path: &str, cookie: Option<&str>) -> (reqwest::StatusCode, String) {
    let mut request = support::client().get(app.url(path));
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    let response = request.send().await.expect("cannot reach the page");
    let status = response.status();
    (status, response.text().await.unwrap_or_default())
}

/// The class the page carries on its root element.
fn root_class(body: &str) -> String {
    body.split("<html lang=\"en\" class=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap_or_default()
        .to_string()
}

#[tokio::test]
async fn the_preferences_page_offers_both_choices_and_needs_no_script() {
    let forgejo = support::start_forgejo().await;
    let app = support::start_app(&forgejo.base_url).await;

    let (status, body) = get(&app, "/preferences", None).await;

    assert_eq!(status, 200, "a visitor with no account can set these too");
    assert!(body.contains("Preferences"));
    assert!(body.contains("/preferences/theme"), "the palette choice");
    assert!(
        body.contains("/preferences/facts"),
        "the fact colour choice"
    );

    // The page shows the choice with the pills themselves, so a person can
    // see what they are picking before they pick it.
    assert!(body.contains("metadata-difficulty"));
    assert!(body.contains("metadata-cook"));

    // Every choice is a plain form. No script and no inline handler.
    for script in body.split("<script").skip(1) {
        let runs = !script.starts_with(" src=\"") && !script.contains("type=\"application/json\"");
        assert!(!runs, "the preferences page must need no inline script");
    }
    assert!(!body.contains("onclick="));
}

#[tokio::test]
async fn the_colour_on_recipe_facts_is_off_until_a_person_asks_for_it() {
    let forgejo = support::start_forgejo().await;
    let app = support::start_app(&forgejo.base_url).await;

    // A new visitor sees what CookCLI shows: one grey for every fact.
    let (_, body) = get(&app, "/preferences", None).await;
    assert_eq!(root_class(&body), "", "no choice writes no class");

    let (_, body) = get(
        &app,
        "/preferences",
        Some("cooklanghub_fact_colour=coloured"),
    )
    .await;
    assert_eq!(root_class(&body), "fact-colour");

    let (_, body) = get(&app, "/preferences", Some("cooklanghub_fact_colour=plain")).await;
    assert_eq!(root_class(&body), "");
}

#[tokio::test]
async fn the_two_choices_do_not_stand_in_each_other_s_way() {
    let forgejo = support::start_forgejo().await;
    let app = support::start_app(&forgejo.base_url).await;

    let (_, body) = get(
        &app,
        "/preferences",
        Some("cooklanghub_theme=dark; cooklanghub_fact_colour=coloured"),
    )
    .await;
    assert_eq!(root_class(&body), "dark fact-colour");

    // One choice on its own must not leave a stray space behind, because a
    // test elsewhere reads this attribute exactly.
    let (_, body) = get(&app, "/preferences", Some("cooklanghub_theme=dark")).await;
    assert_eq!(root_class(&body), "dark");
}

#[tokio::test]
async fn a_value_that_nobody_offered_is_ignored() {
    let forgejo = support::start_forgejo().await;
    let app = support::start_app(&forgejo.base_url).await;

    for value in ["rainbow", "", "true", "<script>"] {
        let (status, body) = get(
            &app,
            "/preferences",
            Some(&format!("cooklanghub_fact_colour={value}")),
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(root_class(&body), "", "`{value}` must change nothing");
    }
}

#[tokio::test]
async fn choosing_a_colour_sticks_and_reaches_a_recipe_page() {
    let forgejo = support::start_forgejo().await;
    forgejo.create_user("alex", true);
    forgejo.create_user("sam", false);
    let app = support::start_app(&forgejo.base_url).await;
    app.bootstrap(&forgejo.access_token("alex")).await;
    let session = support::sign_in(&app, &forgejo, "sam").await;

    support::create_recipe(
        &app,
        &session,
        "Chili Sin Carne",
        "---\nservings: 4\ndifficulty: easy\nprep time: 25 minutes\n---\n\nChop the @onion{1}.",
        false,
    )
    .await;

    let chosen = support::client()
        .post(app.url("/preferences/facts"))
        .form(&[("facts", "coloured"), ("return_to", "/preferences")])
        .send()
        .await
        .expect("cannot make the choice");

    assert_eq!(chosen.status(), 303, "a choice returns the person");
    let cookie = support::set_cookie(&chosen, "cooklanghub_fact_colour")
        .expect("the choice must be remembered");
    assert!(cookie.contains("coloured"));
    assert!(cookie.contains("HttpOnly"), "no script needs to read it");

    // The choice reaches the Recipe page, where the facts actually are.
    let (_, body) = get(
        &app,
        "/recipes/sam/chili-sin-carne",
        Some("cooklanghub_fact_colour=coloured"),
    )
    .await;
    assert_eq!(root_class(&body), "fact-colour");
    assert!(
        body.contains("metadata-difficulty"),
        "a difficulty carries the class that CookCLI gives it"
    );
    assert!(body.contains("metadata-prep"));
}

#[tokio::test]
async fn a_choice_cannot_send_a_person_to_another_site() {
    let forgejo = support::start_forgejo().await;
    let app = support::start_app(&forgejo.base_url).await;

    for away in [
        "https://evil.test",
        "//evil.test",
        "/\\evil.test",
        "javascript:alert(1)",
    ] {
        let response = support::client()
            .post(app.url("/preferences/facts"))
            .form(&[("facts", "coloured"), ("return_to", away)])
            .send()
            .await
            .expect("cannot make the choice");

        assert_eq!(response.status(), 303);
        assert_eq!(
            response
                .headers()
                .get("location")
                .and_then(|value| value.to_str().ok()),
            Some("/"),
            "`{away}` must not be followed"
        );
    }
}
