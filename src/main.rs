use std::process::ExitCode;

use clap::{Parser, Subcommand};
use cooklanghub::secret::Secret;
use cooklanghub::web::AppState;
use cooklanghub::{Config, bootstrap, crypto, db, forgejo, session, telemetry, web};

#[derive(Debug, Parser)]
#[command(
    name = "cooklanghub",
    version,
    about = "A collaborative Cooklang Recipe platform backed by Forgejo"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Start the web server. This is what happens with no subcommand.
    Serve,
    /// Register this installation as an OAuth application in Forgejo.
    ///
    /// Run this once before the first sign-in. Running it again reuses the
    /// same application and issues a new client secret.
    Bootstrap {
        /// A Forgejo access token of an administrator, with the scope
        /// `write:user`. Make one with:
        /// `forgejo admin user generate-access-token --username NAME
        /// --scopes write:user --raw`
        #[arg(long, env = "COOKLANGHUB_FORGEJO_ADMIN_TOKEN")]
        forgejo_admin_token: String,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    let result = match cli.command.unwrap_or(Command::Serve) {
        Command::Serve => serve().await,
        Command::Bootstrap {
            forgejo_admin_token,
        } => run_bootstrap(Secret::new(forgejo_admin_token)).await,
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // The subscriber can be absent when the configuration itself
            // failed, so write the reason to stderr as well.
            eprintln!("cooklanghub cannot continue: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// Open everything that both subcommands need.
async fn prepare() -> anyhow::Result<(
    Config,
    sqlx::SqlitePool,
    crypto::Cipher,
    forgejo::ForgejoClient,
)> {
    let config = Config::from_env()?;
    telemetry::init(config.log_format);

    let pool = db::connect(&config.database_url).await?;
    let cipher = crypto::Cipher::from_session_secret(config.session_secret.expose())?;
    let forgejo =
        forgejo::ForgejoClient::with_urls(&config.forgejo_url, &config.forgejo_public_url)?;

    Ok((config, pool, cipher, forgejo))
}

async fn run_bootstrap(admin_token: Secret<String>) -> anyhow::Result<()> {
    let (config, pool, cipher, forgejo) = prepare().await?;
    let redirect_uri = config.redirect_uri();
    let webhook_url = config.webhook_url();

    let outcome = bootstrap::run(
        &pool,
        &cipher,
        &forgejo,
        &admin_token,
        &redirect_uri,
        &webhook_url,
        &config.webhook_secret,
    )
    .await?;

    match &outcome {
        bootstrap::Outcome::Created { .. } => {
            tracing::info!(client_id = outcome.client_id(), %redirect_uri,
                "registered the OAuth application in Forgejo");
        }
        bootstrap::Outcome::Reused { .. } => {
            tracing::info!(client_id = outcome.client_id(), %redirect_uri,
                "the OAuth application existed; issued a new client secret");
        }
    }

    println!("CookLangHub is registered with Forgejo.");
    println!("  client id:    {}", outcome.client_id());
    println!("  redirect uri: {redirect_uri}");
    println!("  webhook url:  {webhook_url}");
    println!("Users can sign in now.");

    Ok(())
}

async fn serve() -> anyhow::Result<()> {
    let (config, pool, cipher, forgejo) = prepare().await?;

    tracing::info!(
        bind = %config.bind,
        public_url = %config.public_url,
        forgejo_url = %config.forgejo_url,
        version = env!("CARGO_PKG_VERSION"),
        "cooklanghub starts"
    );

    let installation_id = db::installation_id(&pool).await?;
    tracing::info!(%installation_id, "operational database is ready");

    match session::prune(&pool).await {
        Ok(removed) if removed > 0 => tracing::info!(removed, "removed expired sessions"),
        Ok(_) => {}
        Err(error) => tracing::warn!(%error, "cannot prune expired sessions"),
    }

    // Forgejo can be absent at start. The application still serves pages and
    // reports the fault at /health, instead of a refusal to start.
    //
    // A Forgejo of another major release is not a refusal either. The
    // adapter was never exercised against it, so an administrator has to
    // know, and the Diagnostics page says the same thing on the page.
    match forgejo.version().await {
        Ok(version) => {
            tracing::info!(forgejo_version = %version, "Forgejo answers");

            let tested = cooklanghub::diagnostics::TESTED_FORGEJO_MAJOR;
            if cooklanghub::diagnostics::major(&version).as_deref() != Some(tested) {
                tracing::warn!(
                    forgejo_version = %version,
                    tested_major = %tested,
                    "this Forgejo is not the release that CookLangHub was tested against; \
                     read docs/operations.md"
                );
            }
        }
        Err(error) => tracing::warn!(%error, "Forgejo does not answer at start"),
    }

    match cooklanghub::auth::load_client(&pool, &cipher).await {
        Ok(Some(_)) => tracing::info!("the OAuth client is registered"),
        Ok(None) => tracing::warn!(
            "no OAuth client is registered; run `cooklanghub bootstrap` before the first sign-in"
        ),
        Err(error) => tracing::warn!(%error, "cannot read the OAuth client"),
    }

    if !config.cookie_secure {
        tracing::warn!(
            "COOKLANGHUB_COOKIE_SECURE is off; the session cookie can travel on a plain connection"
        );
    }

    // The automation account is the author of every Version that a Cookbook
    // gets from following a Recipe. Forgejo is asked who the credential
    // belongs to, so a wrong name cannot be recorded. An installation with
    // no Cookbook that follows a Recipe needs none of this.
    if let Some(token) = &config.automation_token {
        match cooklanghub::automation::record(&pool, &cipher, &forgejo, token).await {
            Ok(automation) => {
                tracing::info!(login = %automation.login, "the automation account answers")
            }
            Err(error) => tracing::warn!(
                %error,
                "cannot register the automation account; a Cookbook that follows a Recipe stays where it is"
            ),
        }
    }

    // The Recipe index and the Cookbook index are caches, and a restart is
    // when they can be behind: anything that changed while this process was
    // stopped arrived nowhere. The sweeps read Forgejo and Git, write to
    // neither, and run beside the server so that a slow Forgejo never delays
    // the first page.
    //
    // The same is true of a Cookbook that follows a Recipe. A Recipe that
    // gained a Version while this application was stopped reported it to
    // nobody, so the sweep asks Git for every Recipe that a reachable
    // Cookbook follows and moves the Cookbooks that are behind.
    {
        let pool = pool.clone();
        let cipher = cipher.clone();
        let forgejo = forgejo.clone();
        let noreply_domain = config.forgejo_noreply_domain.clone();
        tokio::spawn(async move {
            cooklanghub::index::reconcile(&pool, &cipher, &forgejo).await;
            cooklanghub::cookbook::reconcile(&pool, &cipher, &forgejo).await;
            cooklanghub::automation::advance(
                &pool,
                &cipher,
                &forgejo,
                &cooklanghub::git::SystemGit,
                &noreply_domain,
                None,
            )
            .await;
        });
    }

    let state = AppState {
        pool,
        forgejo,
        git: std::sync::Arc::new(cooklanghub::git::SystemGit),
        cipher,
        cookie_secure: config.cookie_secure,
        forgejo_noreply_domain: config.forgejo_noreply_domain.clone(),
        installation_id,
    };

    let static_dir =
        std::env::var("COOKLANGHUB_STATIC_DIR").unwrap_or_else(|_| "static".to_string());
    let app = web::router(state, &static_dir);

    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    tracing::info!(address = %listener.local_addr()?, "http server listens");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    tracing::info!("cooklanghub stops");
    Ok(())
}

/// Wait for Ctrl-C or for the termination signal that Docker sends.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("cannot install the Ctrl-C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("cannot install the terminate handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}
