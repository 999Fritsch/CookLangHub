//! SQLite operational state.
//!
//! The database holds operational information only. It never holds
//! authoritative Recipe or Cookbook state, which stays in Forgejo and Git.
//! Deleting this database must never destroy domain state.

use std::path::Path;

use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("cannot create the database directory: {0}")]
    Directory(#[source] std::io::Error),
    #[error("cannot connect to the database: {0}")]
    Connect(#[source] sqlx::Error),
    #[error("cannot migrate the database: {0}")]
    Migrate(#[source] sqlx::migrate::MigrateError),
    #[error("cannot query the database: {0}")]
    Query(#[source] sqlx::Error),
}

/// Open the pool, create the parent directory if it is absent, and migrate.
pub async fn connect(database_url: &str) -> Result<SqlitePool, DbError> {
    if let Some(path) = file_path(database_url)
        && let Some(parent) = Path::new(path).parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(DbError::Directory)?;
    }

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect(database_url)
        .await
        .map_err(DbError::Connect)?;

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .map_err(DbError::Migrate)?;

    Ok(pool)
}

/// Return the identifier of this installation, and create it on first start.
pub async fn installation_id(pool: &SqlitePool) -> Result<String, DbError> {
    let candidate = uuid::Uuid::new_v4().to_string();

    sqlx::query("INSERT OR IGNORE INTO installation (id, installation_id) VALUES (1, ?)")
        .bind(&candidate)
        .execute(pool)
        .await
        .map_err(DbError::Query)?;

    let stored: (String,) = sqlx::query_as("SELECT installation_id FROM installation WHERE id = 1")
        .fetch_one(pool)
        .await
        .map_err(DbError::Query)?;

    Ok(stored.0)
}

/// Extract the file path from a `sqlite://` URL, if it names one.
fn file_path(database_url: &str) -> Option<&str> {
    let rest = database_url.strip_prefix("sqlite://")?;
    if rest.starts_with(':') {
        return None;
    }
    Some(rest.split('?').next().unwrap_or(rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_path_ignores_an_in_memory_database() {
        assert_eq!(file_path("sqlite://:memory:"), None);
    }

    #[test]
    fn file_path_drops_the_query_string() {
        assert_eq!(
            file_path("sqlite://data/cooklanghub.db?mode=rwc"),
            Some("data/cooklanghub.db")
        );
    }

    #[tokio::test]
    async fn installation_id_is_stable_across_calls() {
        let pool = connect("sqlite://:memory:").await.unwrap();
        let first = installation_id(&pool).await.unwrap();
        let second = installation_id(&pool).await.unwrap();
        assert_eq!(first, second);
    }
}
