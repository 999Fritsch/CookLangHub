//! Acceptance tests for diagnostics, outage behavior, and backup.
//!
//! The outage tests stop a real Forgejo container. A port where nothing
//! listens gives an error of the same shape, and it never exercises the
//! recovery, because nothing can come back on it. So the container that the
//! application is already talking to is the one that goes away, and the
//! same running application is the one that has to cope and then recover.

mod support;

use cooklanghub::session::COOKIE_NAME;
use serde_json::Value;

/// A Recipe that the parser accepts.
const GOOD: &str = "Chop the @onion{1}.\n\nFry it in a #pan{} for ~{5%minutes}.\n";

/// The names of the six subsystems, exactly as the page writes them.
const SUBSYSTEMS: [&str; 6] = [
    "The application",
    "Forgejo",
    "The webhook",
    "The reconciliation",
    "The automation",
    "The parser",
];

fn cookie(session: &str) -> String {
    format!("{COOKIE_NAME}={session}")
}

/// Read a page the way a browser does.
///
/// The `Accept` header matters: a browser asks for HTML and gets a page,
/// and the editor saving a draft asks for anything and gets the same words
/// as JSON.
async fn read(app: &support::TestApp, session: Option<&str>, path: &str) -> reqwest::Response {
    let mut request = support::client()
        .get(app.url(path))
        .header("accept", "text/html,application/xhtml+xml");

    if let Some(session) = session {
        request = request.header("cookie", cookie(session));
    }

    request.send().await.expect("cannot reach the page")
}

async fn page(app: &support::TestApp, session: Option<&str>, path: &str) -> String {
    read(app, session, path)
        .await
        .text()
        .await
        .expect("cannot read the body")
}

// -------------------------------------------------------------- the page

#[tokio::test]
async fn the_diagnostics_page_reports_every_subsystem_separately() {
    let forgejo = support::start_forgejo().await;
    forgejo.create_user("alex", true);
    let admin = forgejo.access_token("alex");

    let app = support::start_app(&forgejo.base_url).await;
    app.bootstrap(&admin).await;
    let alex = support::sign_in(&app, &forgejo, "alex").await;

    // A sweep has run, so the page has something to report about it.
    app.reconcile().await;
    app.reconcile_cookbooks().await;

    let body = page(&app, Some(&alex), "/admin/index").await;

    for name in SUBSYSTEMS {
        assert!(
            body.contains(name),
            "the page must report `{name}` on its own"
        );
    }

    // Each subsystem carries a state that a person can read at a glance.
    assert!(
        body.matches("metadata-pill").count() >= SUBSYSTEMS.len(),
        "every subsystem must carry its own badge"
    );

    // Forgejo answers, so the page names the release and the tested one.
    assert!(body.contains("Forgejo 15."), "got: {body:.4000}");
    assert!(body.contains("Tested release"));

    // The reconciliation reports what it last did, not only how large the
    // index is now.
    assert!(body.contains("Recipe index, last run"));
    assert!(body.contains("Cookbook index, last run"));

    // The parser proves itself rather than claiming to work.
    assert!(body.contains("Self-check"), "got: {body:.4000}");

    // An installation with no Cookbook that follows a Recipe needs no
    // automation account, and that is not a fault.
    assert!(body.contains("Not in use"));
}

#[tokio::test]
async fn a_person_who_does_not_administer_the_installation_reads_no_detail() {
    let forgejo = support::start_forgejo().await;
    forgejo.create_user("alex", true);
    forgejo.create_user("sam", false);
    let admin = forgejo.access_token("alex");

    let app = support::start_app(&forgejo.base_url).await;
    app.bootstrap(&admin).await;
    let sam = support::sign_in(&app, &forgejo, "sam").await;

    for session in [None, Some(sam.as_str())] {
        let body = page(&app, session, "/admin/index").await;

        assert!(
            body.contains("Only an administrator"),
            "the page must say who can read it: {body:.2000}"
        );

        for name in ["The webhook", "The reconciliation", "The automation"] {
            assert!(
                !body.contains(name),
                "`{name}` must not reach somebody who does not administer this"
            );
        }
        assert!(
            !body.contains("Start a reconciliation"),
            "only an administrator can start one"
        );
    }

    // And the button itself is refused, not only hidden.
    let refused = support::client()
        .post(app.url("/admin/index/rebuild"))
        .header("cookie", cookie(&sam))
        .send()
        .await
        .expect("cannot post");
    assert_eq!(refused.status(), 403);
}

#[tokio::test]
async fn an_administrator_starts_a_reconciliation_that_repairs_both_indexes() {
    let forgejo = support::start_forgejo().await;
    forgejo.create_user("alex", true);
    forgejo.create_user("sam", false);
    let admin = forgejo.access_token("alex");

    let app = support::start_app(&forgejo.base_url).await;
    app.bootstrap(&admin).await;

    let sam = support::sign_in(&app, &forgejo, "sam").await;
    let alex = support::sign_in(&app, &forgejo, "alex").await;

    support::create_recipe(&app, &sam, "Chili sin Carne", GOOD, false).await;
    support::create_cookbook(&app, &sam, "Sunday", "What we cook on Sunday", false).await;

    // Both indexes lose everything, the way a deleted database does.
    sqlx::query("DELETE FROM recipe_index")
        .execute(&app.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM cookbook_index")
        .execute(&app.pool)
        .await
        .unwrap();

    let rebuilt = support::client()
        .post(app.url("/admin/index/rebuild"))
        .header("cookie", cookie(&alex))
        .header("accept", "text/html")
        .send()
        .await
        .expect("cannot ask for a reconciliation");
    assert_eq!(rebuilt.status(), 200);

    let body = rebuilt.text().await.unwrap_or_default();
    assert!(
        body.contains("complete again"),
        "the page must report what happened: {body:.2000}"
    );

    assert!(
        cooklanghub::index::count(&app.pool).await.unwrap() >= 1,
        "the Recipe index must hold the Recipe again"
    );
    assert!(
        cooklanghub::cookbook::count(&app.pool).await.unwrap() >= 1,
        "the Cookbook index must hold the Cookbook again"
    );

    // The page then reports the run it just made, not a run that never
    // happened.
    let after = page(&app, Some(&alex), "/admin/index").await;
    assert!(
        !after.contains("it has not run yet"),
        "a finished run must be reported: {after:.4000}"
    );
}

#[tokio::test]
async fn the_page_reports_that_forgejo_reaches_this_application() {
    let forgejo = support::start_forgejo().await;
    forgejo.create_user("alex", true);
    let admin = forgejo.access_token("alex");

    let app = support::start_app(&forgejo.base_url).await;
    app.bootstrap(&admin).await;
    let alex = support::sign_in(&app, &forgejo, "alex").await;

    // A container cannot reach a listener on the loopback address of the
    // host, so no real delivery arrives here. The message this test posts
    // carries the signature that Forgejo would put on it, and the handler
    // therefore treats it exactly as a real one.
    let before = page(&app, Some(&alex), "/admin/index").await;
    assert!(
        before.contains("no message has arrived yet"),
        "a webhook that has never posted is the common fault of a new \
         installation, and the page must say so: {before:.4000}"
    );
    assert!(
        before.contains("COOKLANGHUB_INTERNAL_URL"),
        "the page must say what to change"
    );

    let delivered = app
        .deliver_webhook(
            "push",
            r#"{"action":"","repository":{"id":1,"name":"chili","owner":{"login":"alex"}}}"#,
        )
        .await;
    assert_eq!(delivered.status(), 202);

    let after = page(&app, Some(&alex), "/admin/index").await;
    assert!(
        after.contains("Last message"),
        "the page must report when Forgejo last posted"
    );
    assert!(
        !after.contains("no message has arrived yet"),
        "a message arrived, so the page must not still say none did: {after:.4000}"
    );
    assert!(
        after.contains("Forgejo holds it"),
        "the page must say whether Forgejo still holds the webhook"
    );
}

#[tokio::test]
async fn no_secret_reaches_the_diagnostics_page() {
    let forgejo = support::start_forgejo().await;
    forgejo.create_user("alex", true);
    forgejo.create_user("cooklanghub-bot", false);
    let admin = forgejo.access_token("alex");
    let automation = forgejo.access_token("cooklanghub-bot");

    let app = support::start_app(&forgejo.base_url).await;
    app.bootstrap(&admin).await;

    // The installation has an automation account, so its credential is
    // stored and the page reads it to ask Forgejo a question with it.
    cooklanghub::automation::record(&app.pool, &app.cipher, &app.forgejo, &automation)
        .await
        .expect("cannot record the automation account");

    let alex = support::sign_in(&app, &forgejo, "alex").await;
    let body = page(&app, Some(&alex), "/admin/index").await;

    // The page names the account, because an administrator has to know
    // which one it is.
    assert!(body.contains("cooklanghub-bot"));

    for (what, secret) in [
        ("the automation credential", automation.expose().to_string()),
        ("the administrator credential", admin.expose().to_string()),
        (
            "the webhook secret",
            support::TEST_WEBHOOK_SECRET.to_string(),
        ),
        ("the session of the reader", alex.clone()),
        (
            "the session key",
            "integration-test-session-secret".to_string(),
        ),
    ] {
        assert!(
            !secret.is_empty(),
            "the test must compare against a real value"
        );
        assert!(!body.contains(&secret), "{what} must never reach the page");
    }
}

// ------------------------------------------------------------- the outage

#[tokio::test]
async fn a_forgejo_outage_is_visible_refuses_every_edit_and_ends_with_a_reconciliation() {
    // The address does not change while the container is stopped, so the
    // application that was running before the outage is the one that has to
    // recover. Nothing is restarted in this test but Forgejo.
    let forgejo = support::start_forgejo_on_a_fixed_port().await;
    forgejo.create_user("alex", true);
    forgejo.create_user("sam", false);
    let admin = forgejo.access_token("alex");
    let sam_token = forgejo.access_token("sam");

    let app = support::start_app(&forgejo.base_url).await;
    app.bootstrap(&admin).await;

    let sam = support::sign_in(&app, &forgejo, "sam").await;
    let leaving = support::sign_in(&app, &forgejo, "sam").await;
    let alex = support::sign_in(&app, &forgejo, "alex").await;

    support::create_recipe(&app, &sam, "Chili sin Carne", GOOD, false).await;
    app.reconcile().await;

    // Before the outage the Recipe is on the list and readable.
    let before = page(&app, Some(&sam), "/").await;
    assert!(before.contains("Chili sin Carne"), "got: {before:.2000}");
    assert_eq!(
        read(&app, Some(&sam), "/recipes/sam/chili-sin-carne")
            .await
            .status(),
        200
    );

    let versions_before = versions(&forgejo, &sam_token).await;

    // ---------------------------------------------------------- the outage
    forgejo.stop().await;

    // 1. Every page says so, and none of them shows the cached title as
    //    though it were current.
    for path in [
        "/",
        "/explore",
        "/cookbooks",
        "/recipes/sam/chili-sin-carne",
        "/recipes/sam/chili-sin-carne/history",
        "/cooks/sam",
    ] {
        let body = page(&app, Some(&sam), path).await;
        assert!(
            body.contains("cannot reach Forgejo"),
            "`{path}` must say that Forgejo is away: {body:.1500}"
        );
        assert!(
            !body.contains("Chili sin Carne"),
            "`{path}` must not show the cached Recipe as current: {body:.1500}"
        );
    }

    // A page that cannot be built answers 503, so a proxy and a reader
    // agree that this is temporary and not a Recipe that is gone.
    assert_eq!(
        read(&app, Some(&sam), "/recipes/sam/chili-sin-carne")
            .await
            .status(),
        503
    );

    // 2. Every edit is refused.
    let created = support::create_recipe(&app, &sam, "Second Recipe", GOOD, false).await;
    assert_eq!(created.status(), 503, "a new Recipe must be refused");

    for (path, fields) in [
        (
            "/recipes/sam/chili-sin-carne/edit",
            vec![("source", GOOD), ("base_version", "")],
        ),
        ("/recipes/sam/chili-sin-carne/favorite", vec![]),
        (
            "/recipes/sam/chili-sin-carne/sharing/visibility",
            vec![("visibility", "private")],
        ),
        (
            "/recipes/sam/chili-sin-carne/discussions",
            vec![("title", "A question"), ("body", "Why?")],
        ),
        ("/cookbooks/new", vec![("title", "A Cookbook")]),
        ("/admin/index/rebuild", vec![]),
    ] {
        let refused = support::post_fields(&app, &sam, path, &fields).await;
        assert_eq!(
            refused.status(),
            503,
            "`{path}` must be refused while Forgejo is away"
        );
    }

    // The editor saves a draft with a script, so it gets the same words in
    // the shape it can read.
    let draft = support::client()
        .post(app.url("/recipes/sam/chili-sin-carne/draft"))
        .header("cookie", cookie(&sam))
        .form(&[("source", GOOD), ("base_version", "")])
        .send()
        .await
        .expect("cannot post the draft");
    assert_eq!(draft.status(), 503);
    let answer: Value = draft.json().await.expect("the editor needs JSON");
    assert!(
        answer["message"]
            .as_str()
            .unwrap_or_default()
            .contains("cannot reach Forgejo"),
        "got {answer}"
    );

    // 3. What this application holds on its own keeps working.
    let health = reqwest::get(app.url("/health"))
        .await
        .expect("cannot reach the health endpoint");
    assert_eq!(health.status(), 503);
    let body: Value = health.json().await.expect("the body is not JSON");
    assert_eq!(body["forgejo"]["status"], "error");
    assert_eq!(body["application"]["status"], "ok");
    assert_eq!(body["database"]["status"], "ok");

    let palette = support::post_fields(
        &app,
        &sam,
        "/preferences/theme",
        &[("theme", "dark"), ("return_to", "/")],
    )
    .await;
    assert!(
        palette.status().is_redirection(),
        "a person must still be able to change the appearance: {}",
        palette.status()
    );

    let signed_out = support::post_fields(&app, &leaving, "/auth/sign-out", &[]).await;
    assert_eq!(
        signed_out.status(),
        200,
        "a person must still be able to sign out"
    );

    // 4. The Diagnostics page says why it is empty. Forgejo names the
    //    administrators, so nobody can be named now, and the page shows the
    //    Forgejo card alone.
    let diagnostics = page(&app, Some(&alex), "/admin/index").await;
    assert!(
        diagnostics.contains("Forgejo") && diagnostics.contains("does not answer"),
        "got: {diagnostics:.3000}"
    );
    assert!(
        !diagnostics.contains("The automation"),
        "no internal detail while nobody can be named an administrator"
    );

    // ------------------------------------------------------- the recovery
    forgejo.start().await;

    // The sign-in survived the outage. A Forgejo that answered nothing
    // refused nothing, so nobody is signed out by a fault of the machine.
    let signed_in = page(&app, Some(&sam), "/").await;
    assert!(
        signed_in.contains("Sign out"),
        "the outage must not end a sign-in: {signed_in:.1500}"
    );

    let report = support::client()
        .post(app.url("/admin/index/rebuild"))
        .header("cookie", cookie(&alex))
        .header("accept", "text/html")
        .send()
        .await
        .expect("cannot ask for a reconciliation");
    assert_eq!(
        report.status(),
        200,
        "the reconciliation must run once Forgejo answers again"
    );
    assert!(
        report
            .text()
            .await
            .unwrap_or_default()
            .contains("complete again")
    );

    // Everything reads again, and nothing was written during the outage.
    let after = page(&app, Some(&sam), "/").await;
    assert!(after.contains("Chili sin Carne"), "got: {after:.2000}");
    assert_eq!(
        read(&app, Some(&sam), "/recipes/sam/chili-sin-carne")
            .await
            .status(),
        200
    );

    assert_eq!(
        versions(&forgejo, &sam_token).await,
        versions_before,
        "an edit that was refused must have written nothing"
    );

    let repositories = support::forgejo_api(&forgejo, &sam_token, "/user/repos").await;
    let names: Vec<String> = repositories
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|entry| entry["name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        !names.iter().any(|name| name.contains("second")),
        "the refused Recipe must not exist: {names:?}"
    );
}

/// How many Versions one Recipe has.
async fn versions(
    forgejo: &support::Forgejo,
    token: &cooklanghub::secret::Secret<String>,
) -> usize {
    support::forgejo_api(forgejo, token, "/repos/sam/chili-sin-carne/commits")
        .await
        .as_array()
        .map(Vec::len)
        .unwrap_or_default()
}

// ----------------------------------------------------------- no telemetry

#[tokio::test]
async fn the_application_sends_nothing_to_another_host() {
    let app = support::start_app(&support::unreachable_url().await).await;

    // Every page restricts the browser to this host, so no tracking pixel
    // and no analytics script can load, whoever puts one in later.
    for path in ["/", "/explore", "/admin/index", "/preferences"] {
        let response = read(&app, None, path).await;
        let policy = response
            .headers()
            .get("content-security-policy")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();

        assert!(
            policy.contains("default-src 'self'"),
            "`{path}` must restrict every asset to this host, got `{policy}`"
        );
        assert!(
            policy.contains("img-src 'self' data:"),
            "`{path}` must allow an image from this host only, got `{policy}`"
        );

        assert_eq!(
            response
                .headers()
                .get("referrer-policy")
                .and_then(|value| value.to_str().ok()),
            Some("no-referrer"),
            "`{path}` must send no address to another host"
        );
    }
}

#[tokio::test]
async fn no_page_and_no_asset_names_a_reporting_service() {
    // The check reads the sources rather than one rendered page, because a
    // tracker that reaches only one template still reaches a person.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));

    // Every name is lower case, and each one is long enough that an
    // ordinary word cannot match it. `rollbar` alone would match
    // `scrollbar`, which is a CSS property.
    let names = [
        "google-analytics.com",
        "googletagmanager.com",
        "gtag(",
        "sentry.io",
        "@sentry/",
        "bugsnag",
        "rollbar.com",
        "datadoghq",
        "posthog",
        "plausible.io",
        "matomo",
        "mixpanel",
        "segment.com",
        "sendbeacon",
        "crash-report",
        "newrelic",
    ];

    // A tracking pixel is an image from another host, and an analytics call
    // is a fetch to another host. Neither can be written without one of
    // these, and the policy header refuses both as well.
    let fetches = [
        "src=\"http",
        "src='http",
        "@import url(http",
        "url(\"http",
        "url('http",
        "fetch(\"http",
        "fetch('http",
        "new image(",
    ];

    for directory in ["templates", "static", "src"] {
        for file in files_under(&root.join(directory)) {
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            let lower = text.to_lowercase();

            for name in names.iter().chain(fetches.iter()) {
                assert!(
                    !lower.contains(name),
                    "`{}` names `{name}`, and this application reports to nobody",
                    file.display()
                );
            }
        }
    }

    // No dependency is a crash reporter or an exporter of measurements.
    let lock = std::fs::read_to_string(root.join("Cargo.lock")).expect("cannot read Cargo.lock");
    for crate_name in [
        "\"sentry",
        "\"opentelemetry",
        "\"tracing-opentelemetry",
        "\"datadog",
        "\"prometheus",
        "\"human-panic",
    ] {
        assert!(
            !lock.contains(&format!("name = {crate_name}")),
            "a dependency named {crate_name} would report to another service"
        );
    }
}

fn files_under(directory: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(directory) else {
        return found;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(files_under(&path));
        } else {
            found.push(path);
        }
    }

    found
}

// -------------------------------------------------------- backup and LTS

#[tokio::test]
async fn the_bundled_forgejo_is_the_release_that_the_tests_run() {
    // A deployment must never run a Forgejo that no test exercised, and the
    // tag must never float, because a floating tag upgrades the instance on
    // the next pull and an upgrade is a thing an administrator decides.
    let compose = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docker-compose.yml"),
    )
    .expect("cannot read docker-compose.yml");

    let line = compose
        .lines()
        .find(|line| {
            line.trim_start().starts_with("image:") && line.contains(support::FORGEJO_IMAGE)
        })
        .expect("the compose file names no Forgejo image");

    let named = line.trim().trim_start_matches("image:").trim();
    assert_eq!(
        named,
        format!("{}:{}", support::FORGEJO_IMAGE, support::FORGEJO_TAG),
        "the bundled Forgejo and the tested Forgejo must be the same release"
    );
    assert!(
        !named.ends_with(":latest"),
        "`latest` upgrades across a major release on its own"
    );
}

#[tokio::test]
async fn the_documentation_gives_the_backup_and_the_upgrade_procedure() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let operations =
        std::fs::read_to_string(root.join("docs/operations.md")).expect("docs/operations.md");

    // A backup of the Recipe repositories alone loses the collaboration
    // state, so the procedure has to name each part it keeps.
    for kept in [
        "Users",
        "permissions",
        "Recipes",
        "Cookbooks",
        "Forks",
        "Suggestions",
        "Discussions",
        "History",
    ] {
        assert!(
            operations.contains(kept),
            "the backup procedure must say that it keeps {kept}"
        );
    }

    // Forgejo makes its own dump. This application never opens the Forgejo
    // database and never touches its repository storage.
    assert!(operations.contains("forgejo dump"));
    assert!(operations.contains("cooklanghub.db"));
    assert!(operations.contains(".env"));

    // The upgrade procedure has a backup step before the change.
    let upgrade = operations
        .split("## Upgrade Forgejo")
        .nth(1)
        .expect("no upgrade procedure");
    assert!(
        upgrade.contains("Back up the whole instance"),
        "the upgrade procedure must start with a backup"
    );
    assert!(
        upgrade.contains("cargo test"),
        "the upgrade procedure must say how to check the new release"
    );

    // Rebuildable state is named, so that an administrator knows what a
    // lost CookLangHub database costs.
    assert!(operations.contains("rebuildable"));

    let readme = std::fs::read_to_string(root.join("README.md")).expect("README.md");
    assert!(
        readme.contains("docs/operations.md"),
        "the README must point at the procedures"
    );
}
