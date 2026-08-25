//! Configuration read from the environment.
//!
//! Every value has a development default except the session secret, which
//! must be set explicitly so that a deployment cannot start with a known key.

use std::env::{self, VarError};
use std::net::SocketAddr;

use crate::secret::Secret;

/// Name of the environment variable prefix used by every setting.
const PREFIX: &str = "COOKLANGHUB_";

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{0} is not set")]
    Missing(String),
    #[error("{name} is not valid: {reason}")]
    Invalid { name: String, reason: String },
}

#[derive(Debug, Clone)]
pub struct Config {
    /// Address that the HTTP server binds to.
    pub bind: SocketAddr,
    /// Base URL that a browser uses to reach this application. Forgejo
    /// sends a person back to it after a sign-in.
    pub public_url: String,
    /// SQLite connection string for the operational state.
    pub database_url: String,
    /// Base URL that the application uses to reach the Forgejo API. Inside
    /// the bundled stack this is a name on the internal Docker network.
    pub forgejo_url: String,
    /// Base URL that a browser uses to reach Forgejo. It differs from
    /// `forgejo_url` whenever the application and the browser sit on
    /// different networks, which is the normal case for the bundled stack.
    pub forgejo_public_url: String,
    /// The domain Forgejo uses when a person hides their address. It must
    /// match `service.NO_REPLY_ADDRESS` in the Forgejo configuration.
    pub forgejo_noreply_domain: String,
    /// Key used to sign session cookies and to encrypt stored credentials.
    pub session_secret: Secret<String>,
    /// Whether the session cookie carries the `Secure` attribute. It stays
    /// on unless a deployment serves plain HTTP on a name that is not
    /// `localhost`, where a browser would then drop the cookie.
    pub cookie_secure: bool,
    /// Log output format.
    pub log_format: LogFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    Json,
    Pretty,
}

impl Config {
    /// Read the configuration from the process environment.
    pub fn from_env() -> Result<Self, ConfigError> {
        let bind = var_or("BIND", "0.0.0.0:8080")?
            .parse::<SocketAddr>()
            .map_err(|error| ConfigError::Invalid {
                name: format!("{PREFIX}BIND"),
                reason: error.to_string(),
            })?;

        let public_url = var_or("PUBLIC_URL", "http://localhost:8080")?;
        let database_url = var_or("DATABASE_URL", "sqlite://data/cooklanghub.db?mode=rwc")?;
        let forgejo_url = var_or("FORGEJO_URL", "http://localhost:3000")?;
        // A single-host installation needs no second URL, so the public URL
        // falls back to the API URL.
        let forgejo_public_url = var_or("FORGEJO_PUBLIC_URL", &forgejo_url)?;
        let forgejo_noreply_domain = var_or(
            "FORGEJO_NOREPLY_DOMAIN",
            crate::create_recipe::DEFAULT_NOREPLY_DOMAIN,
        )?;
        let session_secret = Secret::new(required("SESSION_SECRET")?);
        let cookie_secure = flag("COOKIE_SECURE", true)?;

        let log_format = match var_or("LOG_FORMAT", "json")?.as_str() {
            "json" => LogFormat::Json,
            "pretty" => LogFormat::Pretty,
            other => {
                return Err(ConfigError::Invalid {
                    name: format!("{PREFIX}LOG_FORMAT"),
                    reason: format!("expected `json` or `pretty`, got `{other}`"),
                });
            }
        };

        Ok(Self {
            bind,
            public_url: trim(public_url),
            database_url,
            forgejo_url: trim(forgejo_url),
            forgejo_public_url: trim(forgejo_public_url),
            forgejo_noreply_domain,
            session_secret,
            cookie_secure,
            log_format,
        })
    }

    /// The address that Forgejo sends a person back to after a sign-in.
    pub fn redirect_uri(&self) -> String {
        format!("{}/auth/callback", self.public_url)
    }
}

fn trim(url: String) -> String {
    url.trim_end_matches('/').to_string()
}

fn required(name: &str) -> Result<String, ConfigError> {
    let full = format!("{PREFIX}{name}");
    match env::var(&full) {
        Ok(value) if !value.is_empty() => Ok(value),
        Ok(_) | Err(VarError::NotPresent) => Err(ConfigError::Missing(full)),
        Err(VarError::NotUnicode(_)) => Err(ConfigError::Invalid {
            name: full,
            reason: "value is not valid unicode".to_string(),
        }),
    }
}

fn var_or(name: &str, default: &str) -> Result<String, ConfigError> {
    let full = format!("{PREFIX}{name}");
    match env::var(&full) {
        Ok(value) if !value.is_empty() => Ok(value),
        Ok(_) | Err(VarError::NotPresent) => Ok(default.to_string()),
        Err(VarError::NotUnicode(_)) => Err(ConfigError::Invalid {
            name: full,
            reason: "value is not valid unicode".to_string(),
        }),
    }
}

fn flag(name: &str, default: bool) -> Result<bool, ConfigError> {
    let full = format!("{PREFIX}{name}");
    match env::var(&full) {
        Ok(value) if !value.is_empty() => match value.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Ok(true),
            "0" | "false" | "no" | "off" => Ok(false),
            other => Err(ConfigError::Invalid {
                name: full,
                reason: format!("expected a true or false value, got `{other}`"),
            }),
        },
        _ => Ok(default),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::REDACTED;

    fn sample() -> Config {
        Config {
            bind: "127.0.0.1:8080".parse().unwrap(),
            public_url: "http://localhost:8080".to_string(),
            database_url: "sqlite://:memory:".to_string(),
            forgejo_url: "http://forgejo:3000".to_string(),
            forgejo_public_url: "http://localhost:3000".to_string(),
            forgejo_noreply_domain: "noreply.localhost".to_string(),
            session_secret: Secret::new("super-secret-key".to_string()),
            cookie_secure: true,
            log_format: LogFormat::Json,
        }
    }

    #[test]
    fn debug_output_hides_the_session_secret() {
        let rendered = format!("{:?}", sample());

        assert!(!rendered.contains("super-secret-key"));
        assert!(rendered.contains(REDACTED));
        assert!(rendered.contains("http://forgejo:3000"));
    }

    #[test]
    fn the_redirect_address_sits_under_the_public_url() {
        assert_eq!(
            sample().redirect_uri(),
            "http://localhost:8080/auth/callback"
        );
    }

    #[test]
    fn a_trailing_slash_never_doubles_in_the_redirect_address() {
        let mut config = sample();
        config.public_url = trim("http://localhost:8080/".to_string());

        assert_eq!(
            config.redirect_uri(),
            "http://localhost:8080/auth/callback"
        );
    }
}
