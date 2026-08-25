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
async fn prepare() -> anyhow::Result<(Config, sqlx::SqlitePool, crypto::Cipher, forgejo::ForgejoClient)>
{
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

    let outcome = bootstrap::run(&pool, &cipher, &forgejo, &admin_token, &redirect_uri).await?;

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
    match forgejo.version().await {
        Ok(version) => tracing::info!(forgejo_version = %version, "Forgejo answers"),
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
