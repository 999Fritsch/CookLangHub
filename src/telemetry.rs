//! Structured local logging.
//!
//! The application writes JSON lines by default so that a self-hoster can
//! read the logs with ordinary tools. No log record leaves the machine.

use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt;
use tracing_subscriber::prelude::*;

use crate::config::LogFormat;

/// Install the global log subscriber. Call this once, before any other work.
pub fn init(format: LogFormat) {
    let filter = EnvFilter::try_from_env("COOKLANGHUB_LOG")
        .unwrap_or_else(|_| EnvFilter::new("info,cooklanghub=debug"));

    let registry = tracing_subscriber::registry().with(filter);

    match format {
        LogFormat::Json => registry.with(fmt::layer().json().flatten_event(true)).init(),
        LogFormat::Pretty => registry.with(fmt::layer().compact()).init(),
    }
}
