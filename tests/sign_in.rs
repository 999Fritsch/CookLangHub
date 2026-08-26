//! Acceptance tests for sign-in through Forgejo.
//!
//! These run the real OAuth2 flow against a real Forgejo: the harness signs
//! in to the Forgejo web interface, approves the application, and follows
//! the redirect back, exactly as a browser does.

mod support;

use cooklanghub::bootstrap::Outcome;
use cooklanghub::session::COOKIE_NAME;

#[tokio::test]
async fn the_bootstrap_command_registers_one_oauth_client() {
    let forgejo = support::start_forgejo().await;
    forgejo.create_user("alex", true);
    let token = forgejo.access_token("alex");

    let app = support::start_app(&forgejo.base_url).await;
    let outcome = app.bootstrap(&token).await;

    assert!(
        matches!(outcome, Outcome::Created { .. }),
        "the first run must create the application"
    );

    let applications = app
        .forgejo
        .list_oauth_applications(&token)
        .await
        .expect("cannot list the applications");

    let ours: Vec<_> = applications
        .iter()
        .filter(|a| a.name == "CookLangHub")
        .collect();

    assert_eq!(ours.len(), 1);
    assert_eq!(ours[0].redirect_uris, vec![app.redirect_uri()]);
}

#[tokio::test]
async fn a_second_bootstrap_creates_no_duplicate() {
    let forgejo = support::start_forgejo().await;
    forgejo.create_user("alex", true);
    let token = forgejo.access_token("alex");

    let app = support::start_app(&forgejo.base_url).await;

    let first = app.bootstrap(&token).await;
    let second = app.bootstrap(&token).await;
    let third = app.bootstrap(&token).await;

    assert!(matches!(first, Outcome::Created { .. }));
    assert!(matches!(second, Outcome::Reused { .. }));
    assert!(matches!(third, Outcome::Reused { .. }));

    // Forgejo keeps one application, and it keeps the same client id, so
    // nobody has to approve the application again.
    assert_eq!(first.client_id(), second.client_id());
    assert_eq!(second.client_id(), third.client_id());

    let count = app
        .forgejo
        .list_oauth_applications(&token)
        .await
        .expect("cannot list the applications")
        .iter()
        .filter(|a| a.name == "CookLangHub")
        .count();

    assert_eq!(count, 1, "a repeated bootstrap must not add an application");
}

#[tokio::test]
async fn a_user_signs_in_and_the_header_shows_the_name() {
    let forgejo = support::start_forgejo().await;
    forgejo.create_user("alex", true);
    forgejo.create_user("sam", false);
    let token = forgejo.access_token("alex");

    let app = support::start_app(&forgejo.base_url).await;
    app.bootstrap(&token).await;

    // Before signing in, the page offers to sign in.
    let anonymous = reqwest::get(app.url("/"))
        .await
        .expect("cannot reach the index page")
        .text()
        .await
        .expect("cannot read the body");
    assert!(anonymous.contains("/auth/sign-in"));
    assert!(!anonymous.contains("Sign out"));

    let session = support::sign_in(&app, &forgejo, "sam").await;

    let page = support::client()
        .get(app.url("/"))
        .header("cookie", format!("{COOKIE_NAME}={session}"))
        .send()
        .await
        .expect("cannot reach the index page")
        .text()
        .await
        .expect("cannot read the body");

    assert!(page.contains("sam"), "the header must show the user");
    assert!(page.contains("Sign out"));
    assert!(!page.contains("/auth/sign-in"));
}

#[tokio::test]
async fn the_session_cookie_carries_the_required_attributes() {
    let forgejo = support::start_forgejo().await;
    forgejo.create_user("alex", true);
    forgejo.create_user("sam", false);
    let token = forgejo.access_token("alex");

    let app = support::start_app(&forgejo.base_url).await;
    app.bootstrap(&token).await;

    let header = support::sign_in_raw_cookie(&app, &forgejo, "sam").await;

    // HttpOnly keeps the token away from page scripts, Secure keeps it off a
    // plain connection, and SameSite stops a cross-site post from carrying it.
    assert!(header.contains("HttpOnly"), "got `{header}`");
    assert!(header.contains("Secure"), "got `{header}`");
    assert!(header.contains("SameSite=Lax"), "got `{header}`");
    assert!(header.contains("Path=/"), "got `{header}`");
}

#[tokio::test]
async fn sign_out_ends_the_session_and_the_old_cookie_stops_working() {
    let forgejo = support::start_forgejo().await;
    forgejo.create_user("alex", true);
    forgejo.create_user("sam", false);
    let token = forgejo.access_token("alex");

    let app = support::start_app(&forgejo.base_url).await;
    app.bootstrap(&token).await;

    let session = support::sign_in(&app, &forgejo, "sam").await;

    let signed_out = support::client()
        .post(app.url("/auth/sign-out"))
        .header("cookie", format!("{COOKIE_NAME}={session}"))
        .send()
        .await
        .expect("cannot sign out");

    // Signing out answers with a page rather than a jump, because this
    // application cannot end the permission that Forgejo holds and has to
    // say so. See tests/signing_out.rs.
    assert_eq!(signed_out.status(), 200);

    // The same cookie must not work again.
    let page = support::client()
        .get(app.url("/"))
        .header("cookie", format!("{COOKIE_NAME}={session}"))
        .send()
        .await
        .expect("cannot reach the index page")
        .text()
        .await
        .expect("cannot read the body");

    assert!(
        page.contains("/auth/sign-in"),
        "the old cookie must no longer sign the user in"
    );
    assert!(!page.contains("Sign out"));
}

#[tokio::test]
async fn the_session_survives_a_restart_of_the_application() {
    let forgejo = support::start_forgejo().await;
    forgejo.create_user("alex", true);
    forgejo.create_user("sam", false);
    let token = forgejo.access_token("alex");

    let app = support::start_app(&forgejo.base_url).await;
    app.bootstrap(&token).await;
    let session = support::sign_in(&app, &forgejo, "sam").await;

    // A restart is a new server over the same operational database.
    let restarted = support::restart(&app).await;

    let page = support::client()
        .get(restarted.url("/"))
        .header("cookie", format!("{COOKIE_NAME}={session}"))
        .send()
        .await
        .expect("cannot reach the index page")
        .text()
        .await
        .expect("cannot read the body");

    assert!(
        page.contains("sam"),
        "the session must survive a restart of the application"
    );
}

#[tokio::test]
async fn a_forged_callback_is_refused() {
    let forgejo = support::start_forgejo().await;
    forgejo.create_user("alex", true);
    let token = forgejo.access_token("alex");

    let app = support::start_app(&forgejo.base_url).await;
    app.bootstrap(&token).await;

    let response = support::client()
        .get(app.url("/auth/callback?code=made-up&state=made-up"))
        .send()
        .await
        .expect("cannot reach the callback");

    assert_eq!(
        response.status(),
        400,
        "a callback that this application did not start must be refused"
    );
    assert!(support::set_cookie(&response, COOKIE_NAME).is_none());
}

#[tokio::test]
async fn a_state_cannot_be_used_twice() {
    let forgejo = support::start_forgejo().await;
    forgejo.create_user("alex", true);
    forgejo.create_user("sam", false);
    let token = forgejo.access_token("alex");

    let app = support::start_app(&forgejo.base_url).await;
    app.bootstrap(&token).await;

    let callback = support::authorized_callback_url(&app, &forgejo, "sam").await;

    let first = support::client()
        .get(&callback)
        .send()
        .await
        .expect("cannot reach the callback");
    assert_eq!(first.status(), 303, "the first use must succeed");

    let second = support::client()
        .get(&callback)
        .send()
        .await
        .expect("cannot reach the callback");
    assert_eq!(
        second.status(),
        400,
        "a replayed callback must be refused because the state is used once"
    );
}

#[tokio::test]
async fn no_forgejo_token_reaches_the_browser() {
    let forgejo = support::start_forgejo().await;
    forgejo.create_user("alex", true);
    forgejo.create_user("sam", false);
    let token = forgejo.access_token("alex");

    let app = support::start_app(&forgejo.base_url).await;
    app.bootstrap(&token).await;
    let session = support::sign_in(&app, &forgejo, "sam").await;

    // The application holds a real Forgejo token for this session.
    let held = cooklanghub::session::access_token(&app.pool, &app.cipher, &session)
        .await
        .expect("cannot read the stored token")
        .expect("the session has no stored token");
    let held = held.expose();

    // The stored value is a working credential. Its exact shape is a Forgejo
    // decision, so the test proves that it works rather than how it looks.
    let who = app
        .forgejo
        .current_user(&cooklanghub::secret::Secret::new(held.clone()))
        .await
        .expect("the stored token must work against Forgejo");
    assert_eq!(who.login, "sam");

    for path in ["/", "/health"] {
        let body = support::client()
            .get(app.url(path))
            .header("cookie", format!("{COOKIE_NAME}={session}"))
            .send()
            .await
            .expect("cannot reach the page")
            .text()
            .await
            .expect("cannot read the body");

        assert!(
            !body.contains(held.as_str()),
            "the access token appeared in the answer of {path}"
        );
        assert!(
            !body.contains("gto_"),
            "a token-shaped value reached {path}"
        );
        assert!(
            !body.contains("eyJ"),
            "a value shaped like a JWT reached {path}"
        );
    }
}

#[tokio::test]
async fn sign_in_is_refused_before_the_administrator_bootstraps() {
    let forgejo = support::start_forgejo().await;
    let app = support::start_app(&forgejo.base_url).await;

    let response = support::client()
        .get(app.url("/auth/sign-in"))
        .send()
        .await
        .expect("cannot reach the sign-in route");

    assert_eq!(
        response.status(),
        503,
        "without a registered client the application must say so"
    );
}
