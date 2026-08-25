//! Health of the application and of the systems that it depends on.
//!
//! A self-hoster must be able to tell an application fault from a Forgejo
//! fault. The report therefore names each component separately instead of
//! giving one opaque state.

use serde::Serialize;
use sqlx::sqlite::SqlitePool;

use crate::forgejo::ForgejoClient;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct Component {
    pub status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl Component {
    fn ok(detail: impl Into<String>) -> Self {
        Self {
            status: Status::Ok,
            detail: Some(detail.into()),
        }
    }

    fn error(detail: impl Into<String>) -> Self {
        Self {
            status: Status::Error,
            detail: Some(detail.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct HealthReport {
    /// `ok` only when every component is `ok`.
    pub status: Status,
    pub version: String,
    pub application: Component,
    pub database: Component,
    pub forgejo: Component,
}

impl HealthReport {
    pub fn is_healthy(&self) -> bool {
        self.status == Status::Ok
    }
}

/// Probe every component and build the report.
///
/// A fault in one component never hides the state of another, so each probe
/// happens even when an earlier one failed.
pub async fn report(pool: &SqlitePool, forgejo: &ForgejoClient) -> HealthReport {
    let application = Component::ok("the application answers requests");

    let database = match sqlx::query_scalar::<_, i64>("SELECT count(*) FROM installation")
        .fetch_one(pool)
        .await
    {
        Ok(_) => Component::ok("the operational database answers queries"),
        Err(error) => Component::error(error.to_string()),
    };

    let forgejo = match forgejo.version().await {
        Ok(version) => Component::ok(format!("Forgejo {version}")),
        Err(error) => Component::error(error.to_string()),
    };

    let status = if [application.status, database.status, forgejo.status]
        .iter()
        .all(|status| *status == Status::Ok)
    {
        Status::Ok
    } else {
        Status::Error
    };

    HealthReport {
        status,
        version: env!("CARGO_PKG_VERSION").to_string(),
        application,
        database,
        forgejo,
    }
}
