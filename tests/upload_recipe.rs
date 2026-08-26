//! Acceptance tests for uploading a `.cook` file and a photo.
//!
//! Every test drives the real form against a real Forgejo, and then asks
//! Forgejo what actually landed there. The bytes that come back out are
//! compared with the bytes that went in, because "the application does not
//! convert the image and does not compress it" is a promise about bytes.

mod support;

use std::time::Duration;

use cooklanghub::secret::Secret;
use cooklanghub::session::COOKIE_NAME;

/// A signed-in person against a bootstrapped application.
async fn ready() -> (support::Forgejo, support::TestApp, String) {
    let forgejo = support::start_forgejo().await;
    forgejo.create_user("alex", true);
    forgejo.create_user("sam", false);
    forgejo.create_user("robin", false);
    let token = forgejo.access_token("alex");

    let app = support::start_app(&forgejo.base_url).await;
    app.bootstrap(&token).await;

    let session = support::sign_in(&app, &forgejo, "sam").await;
    (forgejo, app, session)
}

/// Bytes that are easy to compare and hard to produce by accident.
///
/// A value that repeats would survive a change that a real photo would not,
/// so every byte differs from the one before it.
fn payload(seed: u8, length: usize) -> Vec<u8> {
    (0..length)
        .map(|index| (index as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

fn jpeg(length: usize) -> Vec<u8> {
    let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xE0];
    bytes.extend(payload(7, length.saturating_sub(6)));
    bytes.extend([0xFF, 0xD9]);
    bytes
}

fn png(length: usize) -> Vec<u8> {
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    bytes.extend(payload(19, length.saturating_sub(8)));
    bytes
}

fn webp(length: usize) -> Vec<u8> {
    let mut bytes = Vec::from(*b"RIFF\x1a\x00\x00\x00WEBPVP8 ");
    bytes.extend(payload(41, length.saturating_sub(16)));
    bytes
}

/// The create form, with whatever parts a test needs.
fn create_form(title: &str, mode: &str) -> reqwest::multipart::Form {
    reqwest::multipart::Form::new()
        .text("title", title.to_string())
        .text("mode", mode.to_string())
        .text("visibility", "public")
}

/// The names at the top of a Recipe, as Forgejo reports them.
async fn root_files(
    forgejo: &support::Forgejo,
    token: &Secret<String>,
    owner: &str,
    slug: &str,
) -> Vec<String> {
    let contents =
        support::forgejo_api(forgejo, token, &format!("/repos/{owner}/{slug}/contents")).await;

    let mut names: Vec<String> = contents
        .as_array()
        .expect("contents must be a list")
        .iter()
        .filter(|entry| entry["type"] == "file")
        .map(|entry| entry["name"].as_str().unwrap_or_default().to_string())
        .collect();
    names.sort();
    names
}

/// Wait until the names at the top of a Recipe are the ones expected.
///
/// Forgejo answers from Git, and a push it has just accepted can take a
/// moment to show. The wait belongs here and not in a production path.
async fn wait_for_files(
    forgejo: &support::Forgejo,
    token: &Secret<String>,
    owner: &str,
    slug: &str,
    expected: &[&str],
) -> Vec<String> {
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    let mut last = Vec::new();

    while std::time::Instant::now() < deadline {
        last = root_files(forgejo, token, owner, slug).await;
        if last.iter().map(String::as_str).eq(expected.iter().copied()) {
            return last;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    last
}

/// How many Versions a Recipe has.
async fn versions(
    forgejo: &support::Forgejo,
    token: &Secret<String>,
    owner: &str,
    slug: &str,
) -> Vec<serde_json::Value> {
    support::forgejo_api(forgejo, token, &format!("/repos/{owner}/{slug}/commits"))
        .await
        .as_array()
        .expect("the Versions must be a list")
        .clone()
}

#[tokio::test]
async fn a_recipe_can_start_from_a_cook_file() {
    let (forgejo, app, session) = ready().await;
    let token = forgejo.access_token("sam");

    let source = "Chop the @onion{1} in a #pan{} for ~{8%Min.}.";
    let form = create_form("From A File", "file").part(
        "recipe_file",
        support::file_part("dinner.cook", source.into()),
    );

    let response = support::post_form(&app, &session, "/recipes/new", form).await;

    let status = response.status();
    if status != 303 {
        let body = response.text().await.unwrap_or_default();
        panic!("expected 303, got {status}. body:\n{body:.2000}");
    }

    // The file became the Recipe, and the title field wrote itself into it.
    let (status, stored) =
        support::forgejo_raw(&forgejo, &token, "/sam/from-a-file/raw/recipe.cook").await;
    assert!(status.is_success(), "the Recipe file must be there");

    let stored = String::from_utf8(stored).expect("a Recipe is UTF-8 text");
    assert!(stored.contains("title: From A File"), "got: {stored}");
    assert!(
        stored.contains("@onion{1}"),
        "the file content must survive"
    );

    assert_eq!(
        root_files(&forgejo, &token, "sam", "from-a-file").await,
        vec!["recipe.cook".to_string()],
        "a Recipe with no photo holds only the Recipe"
    );
}

#[tokio::test]
async fn the_two_ways_to_give_a_recipe_are_exclusive() {
    let (forgejo, app, session) = ready().await;
    let token = forgejo.access_token("sam");

    // Both at once. The application refuses rather than deciding, because
    // the person must always know which content it used.
    let both = create_form("Both Sources", "file")
        .text("source", "Typed in the form.")
        .part(
            "recipe_file",
            support::file_part("dinner.cook", "From the file.".into()),
        );

    let response = support::post_form(&app, &session, "/recipes/new", both).await;
    assert_eq!(response.status(), 200, "the form must come back");

    let body = response.text().await.expect("cannot read the body");
    assert!(
        body.contains("cannot be created"),
        "the person must see why"
    );
    assert!(
        body.contains("remove one of them"),
        "the reason must say what to do: {body:.600}"
    );
    // What they typed is still there.
    assert!(body.contains("Typed in the form."));

    // The file mode with no file names what is missing.
    let empty = create_form("No File At All", "file");
    let response = support::post_form(&app, &session, "/recipes/new", empty).await;
    assert_eq!(response.status(), 200);
    let body = response.text().await.expect("cannot read the body");
    assert!(
        body.contains("select a Recipe file"),
        "the reason must name the file: {body:.600}"
    );

    // Nothing reached Forgejo.
    let repos = support::forgejo_api(&forgejo, &token, "/user/repos").await;
    let names: Vec<String> = repos
        .as_array()
        .map(|list| {
            list.iter()
                .map(|repository| repository["name"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        names.is_empty(),
        "a refused form must create nothing: {names:?}"
    );
}

#[tokio::test]
async fn the_create_form_offers_both_ways_and_a_photo() {
    let (_forgejo, app, session) = ready().await;

    let body = support::client()
        .get(app.url("/recipes/new"))
        .header("cookie", format!("{COOKIE_NAME}={session}"))
        .send()
        .await
        .expect("cannot reach the create form")
        .text()
        .await
        .expect("cannot read the body");

    // One form, three inputs, and a radio that says which source applies.
    assert!(body.contains(r#"enctype="multipart/form-data""#));
    assert!(body.contains(r#"name="mode" value="text""#));
    assert!(body.contains(r#"name="mode" value="file""#));
    assert!(body.contains(r#"name="recipe_file""#));
    assert!(body.contains(r#"name="thumbnail""#));
    assert!(body.contains("accept=\"image/jpeg,image/png,image/webp\""));

    // Writing here is what a new form starts on.
    assert!(body.contains(r#"<input type="radio" name="mode" value="text" checked>"#));

    // The script is a served file, because the policy allows no inline one.
    assert!(body.contains(r#"<script src="/static/js/recipe-new.js" defer></script>"#));
    assert!(
        !body.contains("onclick="),
        "the policy allows no inline handler"
    );

    let script = support::client()
        .get(app.url("/static/js/recipe-new.js"))
        .send()
        .await
        .expect("cannot reach the script");
    assert_eq!(script.status(), 200, "the script must be served");
}

#[tokio::test]
async fn a_photo_reaches_the_first_version_exactly_as_it_arrived() {
    let (forgejo, app, session) = ready().await;
    let token = forgejo.access_token("sam");

    let picture = jpeg(200_000);
    let form = create_form("Photographed", "text")
        .text("source", "Toast the @bread{2}.")
        .part(
            "thumbnail",
            support::file_part("photo.jpg", picture.clone()),
        );

    let response = support::post_form(&app, &session, "/recipes/new", form).await;
    assert_eq!(
        response.status(),
        303,
        "creation must redirect to the Recipe"
    );

    // One Version holds the Recipe and the photo together.
    assert_eq!(
        versions(&forgejo, &token, "sam", "photographed")
            .await
            .len(),
        1,
        "a new Recipe must have exactly one Version"
    );
    assert_eq!(
        root_files(&forgejo, &token, "sam", "photographed").await,
        vec!["recipe.cook".to_string(), "recipe.jpg".to_string()]
    );

    // Byte for byte. Nothing was converted and nothing was compressed.
    let (status, stored) =
        support::forgejo_raw(&forgejo, &token, "/sam/photographed/raw/recipe.jpg").await;
    assert!(status.is_success(), "the photo must be there");
    assert_eq!(stored.len(), picture.len(), "the size must not change");
    assert_eq!(stored, picture, "every byte must survive storage");
}

#[tokio::test]
async fn the_bytes_decide_the_name_and_every_format_has_one() {
    let (forgejo, app, session) = ready().await;
    let token = forgejo.access_token("sam");

    // A PNG and a WebP each get their own name.
    for (title, slug, bytes, expected) in [
        ("A Png", "a-png", png(4096), "recipe.png"),
        ("A Webp", "a-webp", webp(4096), "recipe.webp"),
    ] {
        let form = create_form(title, "text")
            .text("source", "Toast it.")
            .part("thumbnail", support::file_part("picture", bytes.clone()));

        let response = support::post_form(&app, &session, "/recipes/new", form).await;
        assert_eq!(response.status(), 303, "{title} must be created");

        assert_eq!(
            root_files(&forgejo, &token, "sam", slug).await,
            vec!["recipe.cook".to_string(), expected.to_string()]
        );

        let (_, stored) =
            support::forgejo_raw(&forgejo, &token, &format!("/sam/{slug}/raw/{expected}")).await;
        assert_eq!(stored, bytes, "{title} must keep every byte");
    }

    // A JPEG that arrives named `.png` is still a JPEG. The name says only
    // what somebody typed, so the bytes decide where it is stored.
    let picture = jpeg(4096);
    let form = create_form("Wrong Name", "text")
        .text("source", "Toast it.")
        .part(
            "thumbnail",
            support::file_part("photo.png", picture.clone()),
        );

    let response = support::post_form(&app, &session, "/recipes/new", form).await;
    assert_eq!(response.status(), 303);

    assert_eq!(
        root_files(&forgejo, &token, "sam", "wrong-name").await,
        vec!["recipe.cook".to_string(), "recipe.jpg".to_string()],
        "the first bytes of the file decide the name"
    );
}

#[tokio::test]
async fn a_photo_of_another_format_removes_the_old_one_in_the_same_version() {
    let (forgejo, app, session) = ready().await;
    let token = forgejo.access_token("sam");

    let first = jpeg(8192);
    let form = create_form("Two Photos", "text")
        .text("source", "Toast it.")
        .part("thumbnail", support::file_part("photo.jpg", first));

    assert_eq!(
        support::post_form(&app, &session, "/recipes/new", form)
            .await
            .status(),
        303
    );
    assert_eq!(
        root_files(&forgejo, &token, "sam", "two-photos").await,
        vec!["recipe.cook".to_string(), "recipe.jpg".to_string()]
    );

    // Now a photo of another format.
    let second = webp(9000);
    let response = support::post_form(
        &app,
        &session,
        "/recipes/sam/two-photos/thumbnail",
        reqwest::multipart::Form::new().part(
            "thumbnail",
            support::file_part("photo.webp", second.clone()),
        ),
    )
    .await;

    let status = response.status();
    if status != 303 {
        let body = response.text().await.unwrap_or_default();
        panic!("expected 303, got {status}. body:\n{body:.2000}");
    }

    // Zero or one photo. The old one is gone.
    let files = wait_for_files(
        &forgejo,
        &token,
        "sam",
        "two-photos",
        &["recipe.cook", "recipe.webp"],
    )
    .await;
    assert_eq!(
        files,
        vec!["recipe.cook".to_string(), "recipe.webp".to_string()],
        "a Recipe holds zero or one photo"
    );

    // Two Versions, and the second one both wrote and removed a file.
    let versions = versions(&forgejo, &token, "sam", "two-photos").await;
    assert_eq!(versions.len(), 2, "the change must add exactly one Version");

    let changed: Vec<String> = versions[0]["files"]
        .as_array()
        .expect("a Version must name the files it changed")
        .iter()
        .map(|file| file["filename"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(
        changed.contains(&"recipe.jpg".to_string()),
        "the same Version must remove the old photo, got {changed:?}"
    );
    assert!(
        changed.contains(&"recipe.webp".to_string()),
        "the same Version must write the new photo, got {changed:?}"
    );

    let (_, stored) =
        support::forgejo_raw(&forgejo, &token, "/sam/two-photos/raw/recipe.webp").await;
    assert_eq!(stored, second, "the new photo must keep every byte");
}

#[tokio::test]
async fn an_upload_that_is_too_large_is_refused_and_the_reason_is_given() {
    let (forgejo, app, session) = ready().await;
    let token = forgejo.access_token("sam");

    // A photo above 5 MB.
    let form = create_form("Huge Photo", "text")
        .text("source", "Toast it.")
        .part(
            "thumbnail",
            support::file_part("photo.jpg", jpeg(5 * 1024 * 1024 + 1024)),
        );
    let body = support::post_form(&app, &session, "/recipes/new", form)
        .await
        .text()
        .await
        .expect("cannot read the body");
    assert!(
        body.contains("larger than 5 MB"),
        "the reason must name the limit: {body:.600}"
    );

    // A Recipe file above 1 MB.
    let form = create_form("Huge File", "file").part(
        "recipe_file",
        support::file_part("dinner.cook", vec![b'a'; 1024 * 1024 + 1024]),
    );
    let body = support::post_form(&app, &session, "/recipes/new", form)
        .await
        .text()
        .await
        .expect("cannot read the body");
    assert!(
        body.contains("larger than 1 MB"),
        "the reason must name the limit: {body:.600}"
    );

    // A file that is not an image at all.
    let form = create_form("Not A Photo", "text")
        .text("source", "Toast it.")
        .part(
            "thumbnail",
            support::file_part("notes.txt", b"just some words".to_vec()),
        );
    let body = support::post_form(&app, &session, "/recipes/new", form)
        .await
        .text()
        .await
        .expect("cannot read the body");
    assert!(
        body.contains("JPEG") && body.contains("WebP"),
        "the reason must name the formats: {body:.600}"
    );

    // A refused upload leaves nothing behind.
    let repos = support::forgejo_api(&forgejo, &token, "/user/repos").await;
    assert_eq!(
        repos.as_array().map(Vec::len),
        Some(0),
        "a refused upload must create no Recipe"
    );
}

#[tokio::test]
async fn a_too_large_photo_on_a_recipe_is_refused_and_nothing_changes() {
    let (forgejo, app, session) = ready().await;
    let token = forgejo.access_token("sam");

    let first = png(4096);
    let form = create_form("Keeps Its Photo", "text")
        .text("source", "Toast it.")
        .part("thumbnail", support::file_part("photo.png", first.clone()));
    assert_eq!(
        support::post_form(&app, &session, "/recipes/new", form)
            .await
            .status(),
        303
    );

    let response = support::post_form(
        &app,
        &session,
        "/recipes/sam/keeps-its-photo/thumbnail",
        reqwest::multipart::Form::new().part(
            "thumbnail",
            support::file_part("photo.jpg", jpeg(5 * 1024 * 1024 + 1024)),
        ),
    )
    .await;

    assert_eq!(
        response.status(),
        200,
        "the refusal is a page, not a redirect"
    );
    let body = response.text().await.expect("cannot read the body");
    assert!(
        body.contains("larger than 5 MB"),
        "the reason must name the limit: {body:.600}"
    );
    assert!(
        body.contains("Back to the Recipe"),
        "the way back must be there"
    );

    // The Recipe is untouched: one Version, and the photo it had.
    assert_eq!(
        versions(&forgejo, &token, "sam", "keeps-its-photo")
            .await
            .len(),
        1,
        "a refused photo must add no Version"
    );
    let (_, stored) =
        support::forgejo_raw(&forgejo, &token, "/sam/keeps-its-photo/raw/recipe.png").await;
    assert_eq!(stored, first, "the photo that was there must stay");
}

#[tokio::test]
async fn the_recipe_page_and_the_card_show_the_photo() {
    let (_forgejo, app, session) = ready().await;

    let picture = webp(20_000);
    let form = create_form("Shown Everywhere", "text")
        .text("source", "Toast the @bread{2}.")
        .part(
            "thumbnail",
            support::file_part("photo.webp", picture.clone()),
        );

    assert_eq!(
        support::post_form(&app, &session, "/recipes/new", form)
            .await
            .status(),
        303
    );

    let address = "/recipes/sam/shown-everywhere/thumbnail";

    // The Recipe page.
    let page = support::client()
        .get(app.url("/recipes/sam/shown-everywhere"))
        .header("cookie", format!("{COOKIE_NAME}={session}"))
        .send()
        .await
        .expect("cannot reach the Recipe page")
        .text()
        .await
        .expect("cannot read the body");
    assert!(
        page.contains(&format!(r#"src="{address}""#)),
        "the Recipe page must show the photo"
    );

    // The card on the list.
    let list = support::client()
        .get(app.url("/"))
        .header("cookie", format!("{COOKIE_NAME}={session}"))
        .send()
        .await
        .expect("cannot reach the list")
        .text()
        .await
        .expect("cannot read the body");
    assert!(
        list.contains(&format!(r#"src="{address}""#)),
        "the card must show the photo"
    );

    // The application serves the bytes itself, so the policy needs no other
    // origin and the browser gets the right type.
    let response = support::client()
        .get(app.url(address))
        .header("cookie", format!("{COOKIE_NAME}={session}"))
        .send()
        .await
        .expect("cannot reach the photo");

    assert_eq!(response.status(), 200);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("image/webp")
    );
    let served = response
        .bytes()
        .await
        .expect("cannot read the photo")
        .to_vec();
    assert_eq!(served, picture, "the served photo must be the stored photo");
}

#[tokio::test]
async fn forgejo_decides_who_can_see_a_photo() {
    let (forgejo, app, session) = ready().await;

    let form = reqwest::multipart::Form::new()
        .text("title", "Secret Photo")
        .text("mode", "text")
        .text("source", "Toast it.")
        .text("visibility", "private")
        .part("thumbnail", support::file_part("photo.jpg", jpeg(4096)));

    assert_eq!(
        support::post_form(&app, &session, "/recipes/new", form)
            .await
            .status(),
        303
    );

    let address = "/recipes/sam/secret-photo/thumbnail";

    // Nobody signed in.
    assert_eq!(
        support::client()
            .get(app.url(address))
            .send()
            .await
            .expect("cannot reach the photo")
            .status(),
        404,
        "a stranger must not read the photo of a private Recipe"
    );

    // Somebody else who is signed in.
    let other = support::sign_in(&app, &forgejo, "robin").await;
    assert_eq!(
        support::client()
            .get(app.url(address))
            .header("cookie", format!("{COOKIE_NAME}={other}"))
            .send()
            .await
            .expect("cannot reach the photo")
            .status(),
        404,
        "Forgejo owns the permission, and this person has none"
    );

    // The owner.
    assert_eq!(
        support::client()
            .get(app.url(address))
            .header("cookie", format!("{COOKIE_NAME}={session}"))
            .send()
            .await
            .expect("cannot reach the photo")
            .status(),
        200
    );

    // And a Recipe with no photo has nothing to serve.
    assert_eq!(
        support::create_recipe(&app, &session, "No Photo", "", false)
            .await
            .status(),
        303
    );
    assert_eq!(
        support::client()
            .get(app.url("/recipes/sam/no-photo/thumbnail"))
            .header("cookie", format!("{COOKIE_NAME}={session}"))
            .send()
            .await
            .expect("cannot reach the photo")
            .status(),
        404
    );
}
