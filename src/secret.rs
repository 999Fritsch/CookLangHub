//! A wrapper that keeps a secret out of logs.
//!
//! `Secret<T>` deliberately has no `Display` and its `Debug` prints a fixed
//! placeholder, so a secret cannot reach a log line through the usual
//! `tracing` field formatting. Reading the value requires an explicit
//! [`Secret::expose`] call, which is easy to review.

use std::fmt;

/// Text shown wherever a secret would otherwise be formatted.
pub const REDACTED: &str = "[redacted]";

#[derive(Clone, PartialEq, Eq)]
pub struct Secret<T>(T);

impl<T> Secret<T> {
    pub fn new(value: T) -> Self {
        Self(value)
    }

    /// Return the wrapped value. Every call site is a place where a secret
    /// can leak, so keep them few and obvious.
    pub fn expose(&self) -> &T {
        &self.0
    }
}

impl<T> fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

impl<T> From<T> for Secret<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_hides_the_value() {
        let secret = Secret::new("hunter2".to_string());
        assert_eq!(format!("{secret:?}"), REDACTED);
        assert!(!format!("{secret:?}").contains("hunter2"));
    }

    #[test]
    fn debug_output_of_a_containing_struct_hides_the_value() {
        #[derive(Debug)]
        struct Holder {
            name: &'static str,
            token: Secret<String>,
        }

        let holder = Holder {
            name: "session",
            token: Secret::new("hunter2".to_string()),
        };
        let rendered = format!("{holder:?}");
        assert_eq!(holder.name, "session");
        assert_eq!(holder.token.expose(), "hunter2");
        assert!(rendered.contains("session"));
        assert!(!rendered.contains("hunter2"));
    }

    #[test]
    fn expose_returns_the_value() {
        let secret = Secret::new("hunter2".to_string());
        assert_eq!(secret.expose(), "hunter2");
    }
}
