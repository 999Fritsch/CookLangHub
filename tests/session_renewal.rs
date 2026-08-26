//! A sign-in outlives the Forgejo access token behind it.
//!
//! Forgejo gives an access token that lives one hour. Before this worked, a
//! person stayed signed in for thirty days while every call to Forgejo had
//! been refused since minute sixty: the lists went empty, the message blamed
//! an outage, and publishing a Version failed too, because the same token is
//! the password Git uses.
//!
//! These tests run against a Forgejo that is told to expire an access token
//! after a few seconds, so the state a person really reaches can be reached
//! in a test as well.

mod support;

use support::TestApp;

/// Short enough to wait out in a test, long enough to sign in and act.
const TOKEN_SECONDS: u32 = 5;

/// The stored credential of a session: token, refresh token, and deadline.
type Stored = (String, Option<String>, Option<i64>);
type StoredRow = (Vec<u8>, Option<Vec<u8>>, Option<i64>);

/// The stored credential of a session, as the application holds it.
async fn stored(app: &TestApp, session: &str) -> Option<Stored> {
    use cooklanghub::crypto::digest;

    let row: Option<StoredRow> = sqlx::query_as(
        "SELECT access_token, refresh_token, access_token_expires_at
         FROM session WHERE id = ?",
    )
    .bind(digest(session))
    .fetch_optional(&app.pool)
    .await
    .expect("cannot read the session store");

    row.map(|(access, refresh, expires_at)| {
        (
            app.cipher.decrypt(&access).expect("cannot read the token"),
            refresh.map(|value| app.cipher.decrypt(&value).expect("cannot read the token")),
            expires_at,
        )
    })
}

async fn page(app: &TestApp, path: &str, session: &str) -> (reqwest::StatusCode, String) {
    let response = support::client()
        .get(app.url(path))
        .header("cookie", format!("cooklanghub_session={session}"))
        .send()
        .await
        .expect("cannot reach the page");
    let status = response.status();
    (status, response.text().await.unwrap_or_default())
}

#[tokio::test]
async fn forgejo_gives_a_refresh_token_and_the_application_keeps_it() {
    let forgejo = support::start_forgejo().await;
    forgejo.create_user("alex", true);
    forgejo.create_user("sam", false);
    let app = support::start_app(&forgejo.base_url).await;
    app.bootstrap(&forgejo.access_token("alex")).await;
    let session = support::sign_in(&app, &forgejo, "sam").await;

    let (_, refresh, expires_at) = stored(&app, &session)
        .await
        .expect("the session must exist");

    // Everything below depends on these two facts, so they are asserted on
    // their own. If Forgejo ever stops giving a refresh token, this test
    // says so plainly instead of a renewal test failing for a hidden reason.
    assert!(
        refresh.is_some(),
        "Forgejo must give a refresh token, or a sign-in cannot be renewed"
    );
    assert!(
        expires_at.is_some(),
        "Forgejo must say how long the token lives, or every request renews"
    );
}

#[tokio::test]
async fn a_spent_credential_is_renewed_and_the_person_keeps_working() {
    let forgejo = support::start_forgejo_with_token_lifetime(Some(TOKEN_SECONDS)).await;
    forgejo.create_user("alex", true);
    forgejo.create_user("sam", false);
    let app = support::start_app(&forgejo.base_url).await;
    app.bootstrap(&forgejo.access_token("alex")).await;
    let session = support::sign_in(&app, &forgejo, "sam").await;

    support::create_recipe(
        &app,
        &session,
        "Chili Sin Carne",
        "---\nservings: 4\n---\n\nChop the @onion{1}.",
        false,
    )
    .await;

    let (first_access, first_refresh, _) = stored(&app, &session).await.expect("a session");

    // Wait past the lifetime, so the stored token is genuinely refused by
    // Forgejo from here on.
    tokio::time::sleep(std::time::Duration::from_secs(u64::from(TOKEN_SECONDS) + 3)).await;

    let (status, body) = page(&app, "/", &session).await;

    assert_eq!(status, 200);
    assert!(
        body.contains("Chili Sin Carne"),
        "the Recipes of a signed-in person must survive a spent token"
    );
    assert!(
        !body.contains("cannot reach Forgejo"),
        "a spent token is not an outage and must not be reported as one"
    );

    let (second_access, second_refresh, second_expires) = stored(&app, &session)
        .await
        .expect("the session must survive");

    assert_ne!(
        first_access, second_access,
        "the stored access token must be the renewed one"
    );
    // Forgejo refuses a refresh token once it has been used, so the new one
    // has to replace it or the next renewal fails.
    assert_ne!(
        first_refresh, second_refresh,
        "the rotated refresh token must be stored"
    );
    assert!(
        second_expires.is_some(),
        "the new deadline must be recorded"
    );
}

#[tokio::test]
async fn publishing_still_works_after_the_credential_was_spent() {
    // The access token is also the password that Git uses to push, so a
    // spent credential breaks publishing as well as reading. This is the
    // half that an interface message alone would never have fixed.
    let forgejo = support::start_forgejo_with_token_lifetime(Some(TOKEN_SECONDS)).await;
    forgejo.create_user("alex", true);
    forgejo.create_user("sam", false);
    let app = support::start_app(&forgejo.base_url).await;
    app.bootstrap(&forgejo.access_token("alex")).await;
    let session = support::sign_in(&app, &forgejo, "sam").await;

    tokio::time::sleep(std::time::Duration::from_secs(u64::from(TOKEN_SECONDS) + 3)).await;

    let response = support::create_recipe(
        &app,
        &session,
        "Grissini",
        "---\nservings: 4\n---\n\nRoll the @dough{500%g}.",
        false,
    )
    .await;

    assert!(
        response.status().is_success() || response.status().is_redirection(),
        "a Recipe must still be publishable after the token was spent, got {}",
        response.status()
    );

    let (status, body) = page(&app, "/", &session).await;
    assert_eq!(status, 200);
    assert!(body.contains("Grissini"), "the new Recipe must be there");
}

#[tokio::test]
async fn a_sign_in_that_forgejo_will_not_renew_ends_instead_of_pretending() {
    let forgejo = support::start_forgejo_with_token_lifetime(Some(TOKEN_SECONDS)).await;
    forgejo.create_user("alex", true);
    forgejo.create_user("sam", false);
    let app = support::start_app(&forgejo.base_url).await;
    app.bootstrap(&forgejo.access_token("alex")).await;
    let session = support::sign_in(&app, &forgejo, "sam").await;

    // Spoil the stored refresh token, the way a withdrawn permission or a
    // closed account would: Forgejo refuses the grant.
    let encrypted = app
        .cipher
        .encrypt("eyJhbGciOiJIUzI1NiJ9.not-a-real-refresh-token.signature")
        .expect("cannot protect the value");
    sqlx::query("UPDATE session SET refresh_token = ?, access_token_expires_at = ? WHERE id = ?")
        .bind(encrypted)
        .bind(cooklanghub::session::now() - 60)
        .bind(cooklanghub::crypto::digest(&session))
        .execute(&app.pool)
        .await
        .expect("cannot write the session store");

    let (status, _) = page(&app, "/", &session).await;
    assert_eq!(status, 200, "the page must answer, not fail");

    // The sign-in is genuinely over, so the row goes. The next request sees
    // a visitor with no account rather than somebody the header calls
    // signed in while nothing works.
    assert!(
        stored(&app, &session).await.is_none(),
        "a sign-in that Forgejo refuses to renew must end"
    );
}

#[tokio::test]
async fn the_refresh_token_never_reaches_a_message_a_person_can_read() {
    let forgejo = support::start_forgejo().await;
    forgejo.create_user("alex", true);
    forgejo.create_user("sam", false);
    let app = support::start_app(&forgejo.base_url).await;
    app.bootstrap(&forgejo.access_token("alex")).await;
    let session = support::sign_in(&app, &forgejo, "sam").await;

    let (access, refresh, _) = stored(&app, &session).await.expect("a session");
    let refresh = refresh.expect("Forgejo must give a refresh token");

    // Both credentials are JWTs, and the redaction knows that shape. This
    // asserts it against the real value rather than against an assumption.
    for credential in [&access, &refresh] {
        let message = format!("git push failed: remote: bad credential {credential}");
        let clean = cooklanghub::forgejo::strip_credentials(&message);
        assert!(
            !clean.contains(credential.as_str()),
            "a credential must not survive in a message"
        );
        assert!(clean.contains("[redacted]"));
    }
}
