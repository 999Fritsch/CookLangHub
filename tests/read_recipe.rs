//! Acceptance tests for reading a rendered Recipe.

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

const GERMAN_RECIPE: &str = "---\nservings: 4\nprep time: 25 minutes\nsource: https://example.test/chili\ntags: [vegan, einfach]\n---\n\nWürfle die @gelbe Zwiebel{1} und brate sie in einer #großen Pfanne{} für ~{8%Min.}.\n\nGib @Öl{2%EL} und @Kidneybohnen{800%g} dazu. Lass alles ~{60%Min.} kochen.";

#[tokio::test]
async fn the_page_shows_ingredients_cookware_timings_and_steps() {
    let (_forgejo, app, session) = ready().await;

    support::create_recipe(&app, &session, "Chili Sin Carne", GERMAN_RECIPE, false).await;

    let body = support::client()
        .get(app.url("/recipes/sam/chili-sin-carne"))
        .header("cookie", format!("{COOKIE_NAME}={session}"))
        .send()
        .await
        .expect("cannot reach the Recipe page")
        .text()
        .await
        .expect("cannot read the body");

    // The cook sees what to gather.
    assert!(body.contains("Ingredients"), "no ingredient list");
    assert!(body.contains("Cookware"), "no cookware list");
    // CookCLI gives the steps no heading of their own: the numbered circle
    // is what marks them.
    assert!(body.contains("step-number"), "no steps");

    for item in ["gelbe Zwiebel", "Öl", "Kidneybohnen", "großen Pfanne"] {
        assert!(body.contains(item), "`{item}` is missing from the page");
    }

    // Amounts and timings appear as written.
    for amount in ["2 EL", "800 g", "8 Min.", "60 Min."] {
        assert!(body.contains(amount), "`{amount}` is missing from the page");
    }

    // Inside a step each entity carries the CookCLI badge class, which is
    // what gives it its color and separates an ingredient from cookware.
    assert!(body.contains("ingredient-badge"), "no ingredient marks");
    assert!(body.contains("cookware-badge"), "no cookware marks");
    assert!(body.contains("timer-badge"), "no timer marks");

    // The gather list reads name then amount, and carries no badge: every
    // row there is already the same kind of thing.
    // The lists carry the CookCLI row: a band across the whole row, the
    // name on the left and the amount on the right.
    assert!(
        body.contains("from-orange-50 to-yellow-50"),
        "the ingredient rows must carry the CookCLI band"
    );
    assert!(
        body.contains("from-green-50 to-blue-50"),
        "the cookware rows must carry the CookCLI band"
    );
    assert!(
        body.contains("text-orange-700 font-semibold"),
        "an amount must stand on its own at the right of the row"
    );

    // The amount sits inside the badge, next to the thing it belongs to,
    // so the eye never leaves the sentence to find it.
    assert!(
        !body.contains("step-needs"),
        "a separate amount line would repeat what the badge already says"
    );
    // The amount sits inside the badge, next to its ingredient, so a
    // reader never leaves the sentence to look it up. This is the one place
    // CookLangHub departs from CookCLI, which puts every amount in a
    // separate line under each step.
    let after = body
        .split("timer-badge\">")
        .nth(1)
        .expect("a timer badge must exist");
    let inside = after.split("</span>").take(2).collect::<Vec<_>>().join(" ");
    let text: String = strip_tags(&inside);
    assert!(
        text.contains("Min."),
        "the amount must be inside the badge, got `{text}`"
    );

    // Metadata a cook cares about, in the pills that CookCLI uses.
    assert!(body.contains("metadata-pill"), "no metadata pills");
    assert!(body.contains("4 servings"), "the serving count must show");
    assert!(body.contains("Prep Time"), "the prep time must show");
    assert!(
        body.contains("#vegan"),
        "tags must show as CookCLI writes them"
    );
}

#[tokio::test]
async fn the_raw_cooklang_is_not_the_first_thing_a_reader_sees() {
    let (_forgejo, app, session) = ready().await;

    support::create_recipe(&app, &session, "Chili Sin Carne", GERMAN_RECIPE, false).await;

    let body = support::client()
        .get(app.url("/recipes/sam/chili-sin-carne"))
        .header("cookie", format!("{COOKIE_NAME}={session}"))
        .send()
        .await
        .expect("cannot reach the Recipe page")
        .text()
        .await
        .expect("cannot read the body");

    // The source stays available, but folded away behind a summary rather
    // than presented as the Recipe.
    assert!(body.contains("<details"), "the source must be folded away");

    let method_at = body
        .find("step-number")
        .expect("the steps must be on the page");
    let source_at = body
        .rfind("<details")
        .expect("the source must still be reachable");
    assert!(
        method_at < source_at,
        "the cooked Recipe must come before the source"
    );
}

#[tokio::test]
async fn the_page_names_the_other_areas_of_a_recipe() {
    let (_forgejo, app, session) = ready().await;

    support::create_recipe(&app, &session, "Areas", "Step one.", false).await;

    let body = support::client()
        .get(app.url("/recipes/sam/areas"))
        .header("cookie", format!("{COOKIE_NAME}={session}"))
        .send()
        .await
        .expect("cannot reach the Recipe page")
        .text()
        .await
        .expect("cannot read the body");

    for area in [
        "History",
        "Suggestions",
        "Discussions",
        "Variations",
        "Sharing",
    ] {
        assert!(body.contains(area), "the page must name `{area}`");
    }
}

#[tokio::test]
async fn an_anonymous_reader_sees_a_public_recipe() {
    let (_forgejo, app, session) = ready().await;

    support::create_recipe(&app, &session, "Open To All", GERMAN_RECIPE, false).await;

    // No cookie at all.
    let response = support::client()
        .get(app.url("/recipes/sam/open-to-all"))
        .send()
        .await
        .expect("cannot reach the Recipe page");

    assert_eq!(response.status(), 200);

    let body = response.text().await.expect("cannot read the body");
    assert!(body.contains("Kidneybohnen"), "the Recipe must be readable");
    assert!(body.contains("step-number"), "the steps must be readable");
    // And they are offered a way in.
    assert!(body.contains("/auth/sign-in"));
}

#[tokio::test]
async fn markup_in_a_recipe_never_becomes_an_element() {
    let (_forgejo, app, session) = ready().await;

    let nasty = "Add @<script>alert(1)</script>{1} and <iframe src=x></iframe> and <form></form>.";
    support::create_recipe(&app, &session, "Nasty", nasty, false).await;

    let body = support::client()
        .get(app.url("/recipes/sam/nasty"))
        .header("cookie", format!("{COOKIE_NAME}={session}"))
        .send()
        .await
        .expect("cannot reach the Recipe page")
        .text()
        .await
        .expect("cannot read the body");

    // The characters are shown, escaped. No element is created.
    assert!(
        body.contains("&lt;script&gt;") || body.contains("&#60;script"),
        "the marks must be escaped"
    );
    assert!(
        !body.contains("<script>alert(1)</script>"),
        "a script element reached the page"
    );
    assert!(!body.contains("<iframe"), "an iframe reached the page");
    assert!(
        !body.contains("<form></form>"),
        "a form from the Recipe reached the page"
    );
}

#[tokio::test]
async fn a_dangerous_address_in_the_metadata_never_becomes_a_link() {
    let (_forgejo, app, session) = ready().await;

    let source = "---\nsource: javascript:alert(1)\n---\n\nStep one.";
    support::create_recipe(&app, &session, "Bad Link", source, false).await;

    let body = support::client()
        .get(app.url("/recipes/sam/bad-link"))
        .header("cookie", format!("{COOKIE_NAME}={session}"))
        .send()
        .await
        .expect("cannot reach the Recipe page")
        .text()
        .await
        .expect("cannot read the body");

    assert!(
        !body.contains("href=\"javascript:"),
        "a javascript address became a link"
    );
    // The value is still visible as text, so nothing is hidden from the person.
    assert!(body.contains("javascript:alert(1)"));
}

#[tokio::test]
async fn an_ordinary_source_address_becomes_a_link() {
    let (_forgejo, app, session) = ready().await;

    support::create_recipe(&app, &session, "Linked", GERMAN_RECIPE, false).await;

    let body = support::client()
        .get(app.url("/recipes/sam/linked"))
        .header("cookie", format!("{COOKIE_NAME}={session}"))
        .send()
        .await
        .expect("cannot reach the Recipe page")
        .text()
        .await
        .expect("cannot read the body");

    assert!(
        body.contains("href=\"https://example.test/chili\""),
        "an http address must be followable"
    );
}

#[tokio::test]
async fn the_page_loads_no_image_from_another_host() {
    let (_forgejo, app, session) = ready().await;

    let source = "---\nimage: https://tracker.example.test/pixel.png\n---\n\nStep one.";
    support::create_recipe(&app, &session, "Pixel", source, false).await;

    let response = support::client()
        .get(app.url("/recipes/sam/pixel"))
        .header("cookie", format!("{COOKIE_NAME}={session}"))
        .send()
        .await
        .expect("cannot reach the Recipe page");

    // The policy stops the browser from fetching it, and the page never
    // asks for it in the first place.
    let policy = response
        .headers()
        .get("content-security-policy")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(policy.contains("img-src 'self' data:"), "got `{policy}`");

    let body = response.text().await.expect("cannot read the body");
    assert!(
        !body.contains("<img src=\"https://tracker.example.test"),
        "the page asked the browser for a remote image"
    );
}

#[tokio::test]
async fn reading_a_recipe_never_changes_it() {
    let (forgejo, app, session) = ready().await;
    let token = forgejo.access_token("sam");

    support::create_recipe(&app, &session, "Untouched", GERMAN_RECIPE, false).await;

    let before = reqwest::Client::new()
        .get(format!(
            "{}/api/v1/repos/sam/untouched/raw/recipe.cook",
            forgejo.base_url
        ))
        .header("Authorization", format!("token {}", token.expose()))
        .send()
        .await
        .expect("cannot read the Recipe file")
        .bytes()
        .await
        .expect("cannot read the body");

    // Read the page several times.
    for _ in 0..3 {
        let status = support::client()
            .get(app.url("/recipes/sam/untouched"))
            .send()
            .await
            .expect("cannot reach the Recipe page")
            .status();
        assert_eq!(status, 200);
    }

    let after = reqwest::Client::new()
        .get(format!(
            "{}/api/v1/repos/sam/untouched/raw/recipe.cook",
            forgejo.base_url
        ))
        .header("Authorization", format!("token {}", token.expose()))
        .send()
        .await
        .expect("cannot read the Recipe file")
        .bytes()
        .await
        .expect("cannot read the body");

    assert_eq!(before, after, "reading must not rewrite the stored Recipe");

    // And History still holds exactly the one Version.
    let commits = support::forgejo_api(&forgejo, &token, "/repos/sam/untouched/commits").await;
    assert_eq!(commits.as_array().map(Vec::len), Some(1));
}

#[tokio::test]
async fn a_title_only_recipe_says_so_instead_of_showing_nothing() {
    let (_forgejo, app, session) = ready().await;

    support::create_recipe(&app, &session, "Only A Title", "", false).await;

    let body = support::client()
        .get(app.url("/recipes/sam/only-a-title"))
        .header("cookie", format!("{COOKIE_NAME}={session}"))
        .send()
        .await
        .expect("cannot reach the Recipe page")
        .text()
        .await
        .expect("cannot read the body");

    assert!(body.contains("Only A Title"));
    assert!(
        body.contains("title and nothing else yet"),
        "an empty Recipe must explain itself"
    );
}

#[tokio::test]
async fn a_person_can_choose_light_or_dark_and_it_sticks() {
    let (_forgejo, app, session) = ready().await;
    support::create_recipe(&app, &session, "Themed", GERMAN_RECIPE, false).await;

    // A new visitor gets no attribute at all, so the operating system
    // decides which palette to use.
    let first = support::client()
        .get(app.url("/"))
        .send()
        .await
        .expect("cannot reach the index page")
        .text()
        .await
        .expect("cannot read the body");
    assert!(
        first.contains("<html lang=\"en\" class=\"\">"),
        "the default carries no class, so the system decides"
    );
    assert!(
        first.contains("Appearance"),
        "the control must be on the page"
    );

    // Choosing dark returns the person to where they were.
    let chosen = support::client()
        .post(app.url("/preferences/theme"))
        .form(&[("theme", "dark"), ("return_to", "/recipes/sam/themed")])
        .send()
        .await
        .expect("cannot choose a theme");

    assert_eq!(chosen.status(), 303);
    assert_eq!(
        chosen
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok()),
        Some("/recipes/sam/themed")
    );

    let cookie =
        support::set_cookie(&chosen, "cooklanghub_theme").expect("the choice must be remembered");
    let value = support::cookie_value(&cookie);
    assert_eq!(value, "dark");

    // And the next page carries it in the markup, not in a script.
    let page = support::client()
        .get(app.url("/recipes/sam/themed"))
        .header("cookie", format!("cooklanghub_theme={value}"))
        .send()
        .await
        .expect("cannot reach the Recipe page")
        .text()
        .await
        .expect("cannot read the body");

    assert!(
        page.contains("<html lang=\"en\" class=\"dark\">"),
        "the page must carry the choice as the class CookCLI uses"
    );
    assert!(!page.contains("<script"), "the choice must need no script");
}

#[tokio::test]
async fn a_theme_form_cannot_send_a_person_to_another_site() {
    let (_forgejo, app, _session) = ready().await;

    for hostile in ["https://evil.test", "//evil.test", r"/\evil.test"] {
        let response = support::client()
            .post(app.url("/preferences/theme"))
            .form(&[("theme", "dark"), ("return_to", hostile)])
            .send()
            .await
            .expect("cannot choose a theme");

        assert_eq!(
            response
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok()),
            Some("/"),
            "`{hostile}` must not be followed"
        );
    }
}

/// Drop every element from a fragment, leaving the words a person reads.
fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut inside = false;
    for c in html.chars() {
        match c {
            '<' => inside = true,
            '>' => inside = false,
            _ if !inside => out.push(c),
            _ => {}
        }
    }
    out.trim().to_string()
}
