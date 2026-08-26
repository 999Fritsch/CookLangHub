//! Shared test harness.
//!
//! The principal acceptance seam of this project is the application against a
//! real Forgejo and real Git. Every integration test therefore talks to a
//! disposable Forgejo container instead of a mock. Later tickets extend this
//! module with repositories and Git fixtures.

#![allow(dead_code)]

use std::ops::Deref;
use std::process::Command;
use std::time::Duration;

use cooklanghub::crypto::Cipher;
use cooklanghub::forgejo::ForgejoClient;
use cooklanghub::secret::Secret;
use cooklanghub::web::{self, AppState};
use testcontainers::core::ContainerPort;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};

/// The Forgejo LTS release that this application is tested against.
pub const FORGEJO_IMAGE: &str = "codeberg.org/forgejo/forgejo";
pub const FORGEJO_TAG: &str = "15";

const FORGEJO_PORT: u16 = 3000;
const READY_TIMEOUT: Duration = Duration::from_secs(120);

/// A password that meets the Forgejo policy, for test users.
pub const TEST_PASSWORD: &str = "Corr3ct-Horse-Battery";

/// A running, disposable Forgejo instance.
///
/// Dropping this value removes the container, so a test leaves no state
/// behind on the machine that ran it.
pub struct Forgejo {
    container: ContainerAsync<GenericImage>,
    pub base_url: String,
}

impl Forgejo {
    /// The container identifier, for a test that asserts the cleanup.
    pub fn container_id(&self) -> String {
        self.container.deref().id().to_string()
    }

    /// Run a Forgejo command inside the container.
    fn cli(&self, args: &[&str]) -> String {
        let mut command = Command::new("docker");
        command
            .arg("exec")
            .args(["-u", "git"])
            .arg(self.container_id())
            .arg("forgejo")
            .args(args);

        let output = command.output().expect("cannot run the Forgejo command");
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();

        assert!(
            output.status.success(),
            "forgejo {args:?} failed: {}{stdout}",
            String::from_utf8_lossy(&output.stderr)
        );

        stdout
    }

    /// Create a user. Later tickets use several to test Reader and Editor.
    pub fn create_user(&self, login: &str, admin: bool) {
        let email = format!("{login}@example.test");
        let mut args = vec![
            "admin",
            "user",
            "create",
            "--username",
            login,
            "--password",
            TEST_PASSWORD,
            "--email",
            &email,
            "--must-change-password=false",
        ];
        if admin {
            args.push("--admin");
        }
        self.cli(&args);
    }

    /// Make an access token that the bootstrap command can use.
    pub fn access_token(&self, login: &str) -> Secret<String> {
        let raw = self.cli(&[
            "admin",
            "user",
            "generate-access-token",
            "--username",
            login,
            "--scopes",
            "all",
            "--raw",
        ]);
        Secret::new(raw.trim().to_string())
    }
}

/// Start Forgejo and wait until its API answers.
pub async fn start_forgejo() -> Forgejo {
    // Readiness comes from the API poll below, not from a log message. Log
    // wording is not part of the Forgejo API and can change between
    // releases, so a test must not depend on it.
    let container = GenericImage::new(FORGEJO_IMAGE, FORGEJO_TAG)
        .with_exposed_port(ContainerPort::Tcp(FORGEJO_PORT))
        // The same settings as the bundled deployment, so a test exercises
        // the configuration that a self-hoster actually gets.
        .with_env_var("FORGEJO__security__INSTALL_LOCK", "true")
        .with_env_var("FORGEJO__database__DB_TYPE", "sqlite3")
        .with_env_var("FORGEJO__database__PATH", "/data/gitea/forgejo.db")
        .with_env_var("FORGEJO__server__HTTP_PORT", "3000")
        .with_env_var("FORGEJO__service__DEFAULT_KEEP_EMAIL_PRIVATE", "true")
        .start()
        .await
        .expect("cannot start the Forgejo container");

    let host = container
        .deref()
        .get_host()
        .await
        .expect("cannot read the container host");
    let port = container
        .deref()
        .get_host_port_ipv4(FORGEJO_PORT)
        .await
        .expect("cannot read the container port");

    let base_url = format!("http://{host}:{port}");
    wait_until_forgejo_answers(&base_url).await;

    Forgejo {
        container,
        base_url,
    }
}

/// Poll the version endpoint until Forgejo answers or the timeout expires.
async fn wait_until_forgejo_answers(base_url: &str) {
    let client = ForgejoClient::new(base_url).expect("cannot build the Forgejo client");
    let deadline = std::time::Instant::now() + READY_TIMEOUT;
    let mut last_error = String::new();

    while std::time::Instant::now() < deadline {
        match client.version().await {
            Ok(_) => return,
            Err(error) => last_error = error.to_string(),
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    panic!("Forgejo did not answer within {READY_TIMEOUT:?}: {last_error}");
}

/// The application under test, served on an address that the operating
/// system chooses.
pub struct TestApp {
    pub base_url: String,
    pub pool: sqlx::SqlitePool,
    pub cipher: Cipher,
    pub forgejo: ForgejoClient,
    pub database_url: String,
    /// Held so that the temporary database outlives the test. A restarted
    /// application borrows the directory of the first one.
    _database: Option<tempfile::TempDir>,
}

impl TestApp {
    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    pub fn redirect_uri(&self) -> String {
        self.url("/auth/callback")
    }

    /// Register the OAuth client and the system webhook, the way the
    /// administrator command does.
    pub async fn bootstrap(&self, admin_token: &Secret<String>) -> cooklanghub::bootstrap::Outcome {
        cooklanghub::bootstrap::run(
            &self.pool,
            &self.cipher,
            &self.forgejo,
            admin_token,
            &self.redirect_uri(),
            &self.webhook_url(),
            &self.webhook_secret(),
        )
        .await
        .expect("the bootstrap command failed")
    }

    /// The address that Forgejo would post a webhook message to.
    ///
    /// A container cannot reach a listener that is bound to the loopback
    /// address of the host, so no test waits for a real delivery. What the
    /// tests assert instead is what this application controls: that Forgejo
    /// holds exactly one webhook that points here, with the right events,
    /// and that the handler refuses a body it did not sign.
    pub fn webhook_url(&self) -> String {
        self.url(cooklanghub::webhook::PATH)
    }

    /// The secret that Forgejo signs each webhook body with.
    pub fn webhook_secret(&self) -> Secret<String> {
        Secret::new(TEST_WEBHOOK_SECRET.to_string())
    }

    /// Post a webhook message the way Forgejo does, with a signature.
    pub async fn deliver_webhook(&self, event: &str, body: &str) -> reqwest::Response {
        self.deliver_signed_webhook(
            event,
            body,
            &cooklanghub::webhook::sign(TEST_WEBHOOK_SECRET, body.as_bytes()),
        )
        .await
    }

    /// Post a webhook message with a signature of the caller's choosing.
    pub async fn deliver_signed_webhook(
        &self,
        event: &str,
        body: &str,
        signature: &str,
    ) -> reqwest::Response {
        client()
            .post(self.webhook_url())
            .header("content-type", "application/json")
            .header("x-forgejo-event", event)
            .header("x-forgejo-signature", signature)
            .body(body.to_string())
            .send()
            .await
            .expect("cannot post the webhook message")
    }

    /// Read Forgejo again and make the Recipe index match.
    ///
    /// This is what the application does when it starts, and it is safe at
    /// any moment.
    pub async fn reconcile(&self) -> cooklanghub::index::Report {
        cooklanghub::index::reconcile(&self.pool, &self.cipher, &self.forgejo).await
    }
}

/// The webhook secret that the tests use.
pub const TEST_WEBHOOK_SECRET: &str = "integration-test-webhook-secret";

/// Start the application against the given Forgejo base URL.
///
/// The URL can point at a port where nothing listens. That is how a test
/// reproduces a Forgejo outage.
pub async fn start_app(forgejo_url: &str) -> TestApp {
    start_app_with_public_forgejo_url(forgejo_url, forgejo_url).await
}

/// Start the application with a separate browser-facing Forgejo URL.
///
/// The bundled stack always has two URLs: the app reaches Forgejo by its
/// service name, and a browser reaches it at the published address.
pub async fn start_app_with_public_forgejo_url(
    forgejo_url: &str,
    forgejo_public_url: &str,
) -> TestApp {
    let database = tempfile::tempdir().expect("cannot create the temporary directory");
    let database_url = format!(
        "sqlite://{}/cooklanghub.db?mode=rwc",
        database
            .path()
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/")
    );

    let pool = cooklanghub::db::connect(&database_url)
        .await
        .expect("cannot open the operational database");
    let installation_id = cooklanghub::db::installation_id(&pool)
        .await
        .expect("cannot read the installation identifier");

    let cipher = Cipher::from_session_secret("integration-test-session-secret")
        .expect("cannot derive the installation key");
    let forgejo = ForgejoClient::with_urls(forgejo_url, forgejo_public_url)
        .expect("cannot build the Forgejo client");

    let static_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/static");
    let router = web::router(
        AppState {
            pool: pool.clone(),
            forgejo: forgejo.clone(),
            git: std::sync::Arc::new(cooklanghub::git::SystemGit),
            cipher: cipher.clone(),
            // The test drives plain HTTP on a loopback address, and the
            // harness reads the Set-Cookie header directly, so the attribute
            // is asserted rather than relied on.
            cookie_secure: true,
            forgejo_noreply_domain: cooklanghub::create_recipe::DEFAULT_NOREPLY_DOMAIN.to_string(),
            installation_id,
        },
        static_dir,
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("cannot bind the test listener");
    let address = listener.local_addr().expect("cannot read the test address");

    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("server stopped");
    });

    TestApp {
        base_url: format!("http://{address}"),
        pool,
        cipher,
        forgejo,
        database_url,
        _database: Some(database),
    }
}

/// Start a second server over the same operational database.
///
/// This is what a restart looks like from the outside: the process is new
/// and its memory is empty, but the database file is the one from before.
pub async fn restart(app: &TestApp) -> TestApp {
    let pool = cooklanghub::db::connect(&app.database_url)
        .await
        .expect("cannot reopen the operational database");
    let installation_id = cooklanghub::db::installation_id(&pool)
        .await
        .expect("cannot read the installation identifier");

    let static_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/static");
    let router = web::router(
        AppState {
            pool: pool.clone(),
            forgejo: app.forgejo.clone(),
            git: std::sync::Arc::new(cooklanghub::git::SystemGit),
            cipher: app.cipher.clone(),
            cookie_secure: true,
            forgejo_noreply_domain: cooklanghub::create_recipe::DEFAULT_NOREPLY_DOMAIN.to_string(),
            installation_id,
        },
        static_dir,
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("cannot bind the test listener");
    let address = listener.local_addr().expect("cannot read the test address");

    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("server stopped");
    });

    TestApp {
        base_url: format!("http://{address}"),
        pool,
        cipher: app.cipher.clone(),
        forgejo: app.forgejo.clone(),
        database_url: app.database_url.clone(),
        _database: None,
    }
}

/// A URL where nothing listens, for the outage tests.
pub async fn unreachable_url() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("cannot bind a probe listener");
    let address = listener
        .local_addr()
        .expect("cannot read the probe address");
    drop(listener);
    format!("http://{address}")
}

/// An HTTP client that does not follow a redirect, so a test can read the
/// `Location` header itself.
pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("cannot build the test client")
}

/// Read one cookie value out of the `Set-Cookie` headers of a response.
pub fn set_cookie(response: &reqwest::Response, name: &str) -> Option<String> {
    response
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find(|value| value.starts_with(&format!("{name}=")))
        .map(|value| value.to_string())
}

/// The value part of a `Set-Cookie` header.
pub fn cookie_value(header: &str) -> String {
    header
        .split(';')
        .next()
        .and_then(|pair| pair.split_once('='))
        .map(|(_, value)| value.to_string())
        .unwrap_or_default()
}

/// Drive Forgejo far enough to get the callback address, without calling it.
///
/// A test that needs to replay a callback, or to inspect it, starts here.
pub async fn authorized_callback_url(app: &TestApp, forgejo: &Forgejo, login: &str) -> String {
    // A cookie store is needed because Forgejo carries its own session and
    // CSRF cookies through this flow.
    let browser = reqwest::Client::builder()
        .cookie_store(true)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("cannot build the browser client");

    forgejo_sign_in(&browser, &forgejo.base_url, login).await;

    // Ask the application to start a sign-in.
    let start = browser
        .get(app.url("/auth/sign-in"))
        .send()
        .await
        .expect("cannot start the sign-in");
    assert_eq!(start.status(), 303, "sign-in must redirect to Forgejo");

    let authorize_url = location(&start);

    // Follow it to Forgejo and approve the application.
    forgejo_authorize(&browser, &authorize_url).await
}

/// Finish a sign-in and give back the whole `Set-Cookie` header.
pub async fn sign_in_raw_cookie(app: &TestApp, forgejo: &Forgejo, login: &str) -> String {
    let callback = authorized_callback_url(app, forgejo, login).await;

    let finished = client()
        .get(&callback)
        .send()
        .await
        .expect("cannot reach the callback");

    assert_eq!(
        finished.status(),
        303,
        "the callback must redirect after it starts the session"
    );

    set_cookie(&finished, cooklanghub::session::COOKIE_NAME)
        .expect("the callback set no session cookie")
}

/// Sign in through the real Forgejo web interface and finish the OAuth flow.
///
/// This is what a browser does: it signs in to Forgejo, approves the
/// application, follows the redirect back, and keeps the session cookie.
/// The value returned is the CookLangHub session cookie.
pub async fn sign_in(app: &TestApp, forgejo: &Forgejo, login: &str) -> String {
    cookie_value(&sign_in_raw_cookie(app, forgejo, login).await)
}

/// Sign in to the Forgejo web interface.
async fn forgejo_sign_in(browser: &reqwest::Client, forgejo_url: &str, login: &str) {
    let page = browser
        .get(format!("{forgejo_url}/user/login"))
        .send()
        .await
        .expect("cannot open the Forgejo sign-in page");

    // Forgejo 15 does not put a CSRF token in the sign-in form. Send one
    // only when the page carries one, so this keeps working either way.
    let mut fields = vec![
        ("user_name".to_string(), login.to_string()),
        ("password".to_string(), TEST_PASSWORD.to_string()),
    ];
    if let Some(csrf) = csrf_token(&page.text().await.unwrap_or_default()) {
        fields.push(("_csrf".to_string(), csrf));
    }

    let response = browser
        .post(format!("{forgejo_url}/user/login"))
        .form(&fields)
        .send()
        .await
        .expect("cannot sign in to Forgejo");

    assert!(
        response.status().is_redirection(),
        "Forgejo refused the sign-in of {login}: status {}",
        response.status()
    );
}

/// Approve the application and return the callback address that Forgejo
/// redirects the browser to.
async fn forgejo_authorize(browser: &reqwest::Client, authorize_url: &str) -> String {
    let page = browser
        .get(authorize_url)
        .send()
        .await
        .expect("cannot open the Forgejo approval page");

    // Forgejo redirects at once when the user approved this application
    // before. Otherwise it shows a form that has to be posted.
    if page.status().is_redirection() {
        return location(&page);
    }

    let body = page.text().await.unwrap_or_default();

    let mut fields: Vec<(String, String)> =
        ["client_id", "state", "scope", "nonce", "redirect_uri"]
            .iter()
            .filter_map(|name| hidden_field(&body, name).map(|value| (name.to_string(), value)))
            .collect();
    fields.push(("granted".to_string(), "true".to_string()));
    if let Some(csrf) = csrf_token(&body) {
        fields.push(("_csrf".to_string(), csrf));
    }

    assert!(
        fields.iter().any(|(name, _)| name == "client_id"),
        "the approval page did not carry the expected fields: {body:.400}"
    );

    let granted = browser
        .post(
            authorize_url
                .split_once("/login/oauth/authorize")
                .map(|(base, _)| format!("{base}/login/oauth/grant"))
                .expect("the authorize address has an unexpected shape"),
        )
        .form(&fields)
        .send()
        .await
        .expect("cannot approve the application");

    assert!(
        granted.status().is_redirection(),
        "Forgejo did not redirect after approval: status {}",
        granted.status()
    );

    location(&granted)
}

fn location(response: &reqwest::Response) -> String {
    response
        .headers()
        .get("location")
        .and_then(|value| value.to_str().ok())
        .expect("the response has no location header")
        .to_string()
}

/// Pull the CSRF token out of a Forgejo page.
fn csrf_token(html: &str) -> Option<String> {
    hidden_field(html, "_csrf")
}

/// Read the value of a named input from an HTML form.
fn hidden_field(html: &str, name: &str) -> Option<String> {
    let needle = format!("name=\"{name}\"");
    let start = html.find(&needle)?;

    // The value attribute can sit before or after the name attribute, so
    // look in the whole tag.
    let tag_start = html[..start].rfind('<')?;
    let tag_end = start + html[start..].find('>')?;
    let tag = &html[tag_start..tag_end];

    let value_at = tag.find("value=\"")? + "value=\"".len();
    let value_end = tag[value_at..].find('"')? + value_at;

    Some(tag[value_at..value_end].to_string())
}

/// Post the create-Recipe form as the holder of a session cookie.
pub async fn create_recipe(
    app: &TestApp,
    session: &str,
    title: &str,
    source: &str,
    private: bool,
) -> reqwest::Response {
    let visibility = if private { "private" } else { "public" };

    client()
        .post(app.url("/recipes/new"))
        .header(
            "cookie",
            format!("{}={session}", cooklanghub::session::COOKIE_NAME),
        )
        .form(&[
            ("title", title),
            ("source", source),
            ("visibility", visibility),
        ])
        .send()
        .await
        .expect("cannot post the create form")
}

/// Ask Forgejo directly, for a test that checks what actually landed there.
///
/// Forgejo answers 409 for a short moment after the first push, while it
/// finishes recording the new state of the repository. That is a property
/// of Forgejo and not of this application, so the helper waits it out
/// rather than making every test carry the retry.
pub async fn forgejo_api(
    forgejo: &Forgejo,
    token: &Secret<String>,
    path: &str,
) -> serde_json::Value {
    let client = reqwest::Client::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    let mut last = String::new();

    while std::time::Instant::now() < deadline {
        let response = client
            .get(format!("{}/api/v1{path}", forgejo.base_url))
            .header("Authorization", format!("token {}", token.expose()))
            .send()
            .await
            .expect("cannot reach the Forgejo API");

        let status = response.status();
        if status.is_success() {
            return response.json().await.expect("the answer is not JSON");
        }

        last = format!("{status}");
        if status.as_u16() != 409 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    panic!("GET {path} answered {last}");
}
