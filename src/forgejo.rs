//! The Forgejo boundary.
//!
//! The application reaches Forgejo only through its supported HTTP API. It
//! never opens the database of Forgejo and never touches its repository
//! storage. Later tickets add authenticated calls behind this same type.

use std::time::Duration;

use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
pub enum ForgejoError {
    #[error("cannot build the HTTP client: {0}")]
    Client(#[source] reqwest::Error),
    #[error("cannot reach Forgejo: {0}")]
    Unreachable(String),
    #[error("Forgejo answered with status {0}")]
    Status(u16),
    #[error("Forgejo sent an answer that the application cannot read: {0}")]
    Body(String),
}

#[derive(Debug, Deserialize)]
struct VersionResponse {
    version: String,
}

/// A client for one Forgejo instance.
#[derive(Debug, Clone)]
pub struct ForgejoClient {
    base_url: String,
    http: reqwest::Client,
}

impl ForgejoClient {
    pub fn new(base_url: impl Into<String>) -> Result<Self, ForgejoError> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .user_agent(concat!("cooklanghub/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(ForgejoError::Client)?;

        Ok(Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            http,
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Read the version of the Forgejo instance.
    ///
    /// This endpoint needs no credential, so the health probe carries no
    /// token and cannot leak one.
    pub async fn version(&self) -> Result<String, ForgejoError> {
        let url = format!("{}/api/v1/version", self.base_url);

        let response = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|error| ForgejoError::Unreachable(error.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            return Err(ForgejoError::Status(status.as_u16()));
        }

        let parsed: VersionResponse = response
            .json()
            .await
            .map_err(|error| ForgejoError::Body(error.to_string()))?;

        Ok(parsed.version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_loses_a_trailing_slash() {
        let client = ForgejoClient::new("http://forgejo:3000/").unwrap();
        assert_eq!(client.base_url(), "http://forgejo:3000");
    }
}
