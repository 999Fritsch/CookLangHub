use std::process::ExitCode;

use cooklanghub::web::AppState;
use cooklanghub::{Config, db, forgejo, telemetry, web};

#[tokio::main]
async fn main() -> ExitCode {
    match start().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // The subscriber can be absent when the configuration itself
            // failed, so write the reason to stderr as well.
            eprintln!("cooklanghub cannot start: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn start() -> anyhow::Result<()> {
    let config = Config::from_env()?;
    telemetry::init(config.log_format);

    tracing::info!(
        bind = %config.bind,
        forgejo_url = %config.forgejo_url,
        version = env!("CARGO_PKG_VERSION"),
        "cooklanghub starts"
    );

    let pool = db::connect(&config.database_url).await?;
    let installation_id = db::installation_id(&pool).await?;
    tracing::info!(%installation_id, "operational database is ready");

    let forgejo = forgejo::ForgejoClient::new(&config.forgejo_url)?;

    // Forgejo can be absent at start. The application still serves pages and
    // reports the fault at /health, instead of a refusal to start.
    match forgejo.version().await {
        Ok(version) => tracing::info!(forgejo_version = %version, "Forgejo answers"),
        Err(error) => tracing::warn!(%error, "Forgejo does not answer at start"),
    }

    let state = AppState {
        pool,
        forgejo,
        forgejo_public_url: config.forgejo_public_url.clone(),
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
