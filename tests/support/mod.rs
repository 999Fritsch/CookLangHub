//! Shared test harness.
//!
//! The principal acceptance seam of this project is the application against a
//! real Forgejo and real Git. Every integration test therefore talks to a
//! disposable Forgejo container instead of a mock. Later tickets extend this
//! module with Forgejo users, repositories, and Git fixtures.

#![allow(dead_code)]

use std::ops::Deref;
use std::time::Duration;

use cooklanghub::forgejo::ForgejoClient;
use cooklanghub::web::{self, AppState};
use testcontainers::core::ContainerPort;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};

/// The Forgejo LTS release that this application is tested against.
pub const FORGEJO_IMAGE: &str = "codeberg.org/forgejo/forgejo";
pub const FORGEJO_TAG: &str = "15";

const FORGEJO_PORT: u16 = 3000;
const READY_TIMEOUT: Duration = Duration::from_secs(120);

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
    /// Held so that the temporary database outlives the test.
    _database: tempfile::TempDir,
}

impl TestApp {
    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }
}

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
    let forgejo = ForgejoClient::new(forgejo_url).expect("cannot build the Forgejo client");

    let static_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/static");
    let router = web::router(
        AppState {
            pool,
            forgejo,
            forgejo_public_url: forgejo_public_url.to_string(),
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
        _database: database,
    }
}

/// A URL where nothing listens, for the outage tests.
pub async fn unreachable_url() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("cannot bind a probe listener");
    let address = listener.local_addr().expect("cannot read the probe address");
    drop(listener);
    format!("http://{address}")
}
