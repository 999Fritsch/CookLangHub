//! The Git adapter.
//!
//! Git owns Recipe content and History, so every content operation goes
//! through this boundary and never through an ad hoc command elsewhere.
//!
//! The MVP implementation runs the system Git executable in a temporary
//! workspace that it removes afterwards. That is accepted technical debt:
//! the trait exists so that a library, a worker, or deeper Forgejo
//! integration can replace it without a change to the Recipe model.
//!
//! Credentials never reach the command line or a file. Git argv is readable
//! by any user on the machine, so the token travels in an environment
//! variable that a credential helper reads.

use std::collections::BTreeMap;
use std::path::Path;

use async_trait::async_trait;
use tokio::process::Command;

use crate::secret::Secret;

/// The environment variable that the credential helper reads.
const TOKEN_VAR: &str = "COOKLANGHUB_GIT_TOKEN";

/// A credential helper written inline. It prints the credential that Git
/// asks for and reads the value from the environment, so no token appears
/// in argv, in a config file, or in a remote address.
const CREDENTIAL_HELPER: &str = concat!(
    "!f() { echo username=oauth2; echo \"password=$",
    "COOKLANGHUB_GIT_TOKEN",
    "\"; }; f"
);

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("cannot prepare a Git workspace: {0}")]
    Workspace(#[source] std::io::Error),
    #[error("cannot start Git: {0}")]
    Start(#[source] std::io::Error),
    #[error("git {command} failed: {message}")]
    Command { command: String, message: String },
}

/// Who a Version belongs to.
///
/// Both the author and the committer carry this identity, so History stays
/// meaningful when somebody reads it outside this application.
#[derive(Debug, Clone)]
pub struct Identity {
    pub name: String,
    /// The address that Forgejo gives for this person. It is the no-reply
    /// address when they keep their address private.
    pub email: String,
}

/// A request to create the first Version of a Recipe.
#[derive(Debug)]
pub struct InitialCommit<'a> {
    /// Where to push, without any credential in it.
    pub remote_url: &'a str,
    pub token: &'a Secret<String>,
    pub identity: &'a Identity,
    pub branch: &'a str,
    pub message: &'a str,
    /// File name to content.
    pub files: BTreeMap<String, Vec<u8>>,
}

/// The Git operations that the Recipe model needs.
///
/// Later tickets add draft branches, publication, comparison, and
/// submodules here.
#[async_trait]
pub trait GitAdapter: Send + Sync + std::fmt::Debug {
    /// Create a repository with one commit and push it.
    ///
    /// Returns the identifier of the Version that it created.
    async fn create_initial_commit(&self, request: InitialCommit<'_>) -> Result<String, GitError>;
}

/// Runs the system Git executable in a temporary workspace.
#[derive(Debug, Clone, Default)]
pub struct SystemGit;

#[async_trait]
impl GitAdapter for SystemGit {
    async fn create_initial_commit(&self, request: InitialCommit<'_>) -> Result<String, GitError> {
        let workspace = tempfile::tempdir().map_err(GitError::Workspace)?;
        let path = workspace.path();

        self.run(path, request.token, &["init", "--quiet"]).await?;
        self.run(
            path,
            request.token,
            &["symbolic-ref", "HEAD", &format!("refs/heads/{}", request.branch)],
        )
        .await?;

        for (name, content) in &request.files {
            let file = path.join(name);
            if let Some(parent) = file.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(GitError::Workspace)?;
            }
            tokio::fs::write(&file, content)
                .await
                .map_err(GitError::Workspace)?;
        }

        self.run(path, request.token, &["add", "--all"]).await?;

        // The author and the committer are the same person. Passing the
        // identity with -c keeps it out of any file that survives the run.
        self.run(
            path,
            request.token,
            &[
                "-c",
                &format!("user.name={}", request.identity.name),
                "-c",
                &format!("user.email={}", request.identity.email),
                "commit",
                "--quiet",
                "--message",
                request.message,
            ],
        )
        .await?;

        self.run(
            path,
            request.token,
            &[
                "push",
                "--quiet",
                request.remote_url,
                &format!("{}:{}", request.branch, request.branch),
            ],
        )
        .await?;

        let sha = self
            .run(path, request.token, &["rev-parse", "HEAD"])
            .await?
            .trim()
            .to_string();

        // Dropping the workspace removes the clone. Nothing authoritative
        // lives here: Forgejo holds the repository.
        drop(workspace);

        Ok(sha)
    }
}

impl SystemGit {
    async fn run(
        &self,
        workspace: &Path,
        token: &Secret<String>,
        args: &[&str],
    ) -> Result<String, GitError> {
        let mut command = Command::new("git");
        command
            .current_dir(workspace)
            // The helper reads the token from the environment, so it never
            // reaches argv, a config file, or a remote address.
            .arg("-c")
            .arg(format!("credential.helper={CREDENTIAL_HELPER}"))
            .args(args)
            .env(TOKEN_VAR, token.expose())
            // Keep the run from reading the machine or user configuration,
            // so a self-hoster setting cannot change what this does.
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("HOME", workspace)
            .env("USERPROFILE", workspace)
            .kill_on_drop(true);

        let output = command.output().await.map_err(GitError::Start)?;

        if !output.status.success() {
            let message = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(GitError::Command {
                // Only the verb is recorded. An argument can hold a Recipe
                // title or a path, which does not belong in a log.
                command: args.first().unwrap_or(&"git").to_string(),
                message: redact(&message, token),
            });
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}

/// Remove the token from a message, whatever shape Git echoed it in.
fn redact(message: &str, token: &Secret<String>) -> String {
    let value = token.expose();
    if value.is_empty() {
        return message.to_string();
    }
    message.replace(value.as_str(), "[redacted]")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_credential_helper_reads_the_environment_and_holds_no_token() {
        // The helper text goes into argv, so it must carry a variable name
        // and never a value.
        assert!(CREDENTIAL_HELPER.contains(TOKEN_VAR));
        assert!(CREDENTIAL_HELPER.contains("username=oauth2"));
        assert!(!CREDENTIAL_HELPER.contains("gto_"));
    }

    #[test]
    fn a_token_never_survives_in_a_git_message() {
        let token = Secret::new("gto_secret_value".to_string());
        let message = "fatal: cannot read https://oauth2:gto_secret_value@forge/x.git";

        let clean = redact(message, &token);

        assert!(!clean.contains("gto_secret_value"));
        assert!(clean.contains("[redacted]"));
    }

    #[tokio::test]
    async fn a_failing_command_names_the_verb_and_not_the_arguments() {
        let workspace = tempfile::tempdir().unwrap();
        let git = SystemGit;

        let error = git
            .run(
                workspace.path(),
                &Secret::new("gto_x".to_string()),
                &["rev-parse", "--verify", "a-secret-branch-name"],
            )
            .await
            .expect_err("the command must fail in an empty directory");

        let text = error.to_string();
        assert!(text.contains("rev-parse"));
        assert!(
            !text.contains("a-secret-branch-name"),
            "an argument must not reach the message: {text}"
        );
    }
}
