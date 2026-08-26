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
use std::path::{Component, Path, PathBuf};

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
    /// The path is not a plain name inside the Recipe. The path itself
    /// never reaches the message, for the same reason an argument does not.
    #[error("a file name in this Version is not allowed")]
    Name,
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
    /// File name to content. The content is bytes, so a photo is as
    /// ordinary here as a Cooklang source.
    pub files: BTreeMap<String, Vec<u8>>,
}

/// A request to add one Version to a Recipe that exists.
///
/// One request makes one Version. A photo that replaces a photo of another
/// format therefore writes the new file and removes the old file together,
/// and History never holds a Version with two photos in it.
#[derive(Debug)]
pub struct ChangeCommit<'a> {
    /// Where to push, without any credential in it.
    pub remote_url: &'a str,
    pub token: &'a Secret<String>,
    pub identity: &'a Identity,
    pub branch: &'a str,
    pub message: &'a str,
    /// File name to content, for each file to write.
    pub write: BTreeMap<String, Vec<u8>>,
    /// File names to remove. A name that is not there is not a fault.
    pub delete: Vec<String>,
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

    /// Add one Version to a Recipe that exists.
    ///
    /// Returns the identifier of the Version. A change that makes no
    /// difference to the content adds no Version, and the identifier of
    /// the published Version comes back instead.
    async fn commit_change(&self, request: ChangeCommit<'_>) -> Result<String, GitError>;
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
            &[
                "symbolic-ref",
                "HEAD",
                &format!("refs/heads/{}", request.branch),
            ],
        )
        .await?;

        write_files(path, &request.files).await?;

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

    async fn commit_change(&self, request: ChangeCommit<'_>) -> Result<String, GitError> {
        let workspace = tempfile::tempdir().map_err(GitError::Workspace)?;
        // The clone goes into a folder of its own, so that the workspace
        // above it can stay the home folder that keeps Git away from the
        // configuration of the machine.
        let home = workspace.path();
        let work = home.join("work");

        // One Version needs one branch, and it needs only the state that
        // the new Version is built on. A Recipe with a long History
        // therefore costs the same as a new one.
        self.run_in(
            home,
            home,
            request.token,
            &[
                "clone",
                "--quiet",
                "--depth",
                "1",
                "--single-branch",
                "--branch",
                request.branch,
                request.remote_url,
                "work",
            ],
        )
        .await?;

        write_files(&work, &request.write).await?;

        for name in &request.delete {
            let file = safe_path(&work, name)?;
            match tokio::fs::remove_file(&file).await {
                Ok(()) => {}
                // A file that is already gone is the state that was asked
                // for, so this is not a fault.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(GitError::Workspace(error)),
            }
        }

        self.run_in(&work, home, request.token, &["add", "--all"])
            .await?;

        // A change that makes no difference must not become an empty
        // Version, because History is for a person to read.
        let pending = self
            .run_in(&work, home, request.token, &["status", "--porcelain"])
            .await?;
        if pending.trim().is_empty() {
            return self.head(&work, home, request.token).await;
        }

        self.run_in(
            &work,
            home,
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

        self.run_in(
            &work,
            home,
            request.token,
            &[
                "push",
                "--quiet",
                request.remote_url,
                &format!("{}:{}", request.branch, request.branch),
            ],
        )
        .await?;

        let sha = self.head(&work, home, request.token).await?;

        drop(workspace);

        Ok(sha)
    }
}

impl SystemGit {
    /// The identifier of the Version that the workspace is on.
    async fn head(
        &self,
        work: &Path,
        home: &Path,
        token: &Secret<String>,
    ) -> Result<String, GitError> {
        Ok(self
            .run_in(work, home, token, &["rev-parse", "HEAD"])
            .await?
            .trim()
            .to_string())
    }

    async fn run(
        &self,
        workspace: &Path,
        token: &Secret<String>,
        args: &[&str],
    ) -> Result<String, GitError> {
        self.run_in(workspace, workspace, token, args).await
    }

    /// Run Git in one folder while the home folder is another.
    ///
    /// A clone puts the work tree below the workspace, and the workspace
    /// stays the home folder so that Git still reads no configuration of
    /// the machine or of the person who runs the server.
    async fn run_in(
        &self,
        cwd: &Path,
        home: &Path,
        token: &Secret<String>,
        args: &[&str],
    ) -> Result<String, GitError> {
        let mut command = Command::new("git");
        command
            .current_dir(cwd)
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
            .env("HOME", home)
            .env("USERPROFILE", home)
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

/// Write each file into the work tree, making the folders it needs.
async fn write_files(root: &Path, files: &BTreeMap<String, Vec<u8>>) -> Result<(), GitError> {
    for (name, content) in files {
        let file = safe_path(root, name)?;
        if let Some(parent) = file.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(GitError::Workspace)?;
        }
        tokio::fs::write(&file, content)
            .await
            .map_err(GitError::Workspace)?;
    }
    Ok(())
}

/// Turn a name from a Recipe into a path inside the work tree.
///
/// The names this adapter gets are constants today. The check is here
/// anyway, because a caller that one day builds a name from something a
/// person typed must not be able to write outside the Recipe.
fn safe_path(root: &Path, name: &str) -> Result<PathBuf, GitError> {
    let candidate = Path::new(name);

    if name.trim().is_empty() || candidate.is_absolute() {
        return Err(GitError::Name);
    }

    for part in candidate.components() {
        match part {
            Component::Normal(part) if part != ".git" => {}
            _ => return Err(GitError::Name),
        }
    }

    Ok(root.join(candidate))
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

    #[test]
    fn a_plain_file_name_stays_inside_the_recipe() {
        let root = Path::new("/work");
        assert_eq!(
            safe_path(root, "recipe.cook").unwrap(),
            root.join("recipe.cook")
        );
        assert_eq!(
            safe_path(root, "recipe.jpg").unwrap(),
            root.join("recipe.jpg")
        );
    }

    #[test]
    fn a_name_that_leaves_the_recipe_is_refused() {
        let root = Path::new("/work");
        for name in [
            "../escape",
            "a/../../escape",
            "/etc/passwd",
            ".git/config",
            "a/.git/hooks/pre-commit",
            "",
            "   ",
        ] {
            assert!(
                safe_path(root, name).is_err(),
                "`{name}` must not be written"
            );
        }
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
