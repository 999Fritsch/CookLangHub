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

/// How many times a publication tries again when the branch moves under it.
///
/// Each try applies the same one change, so a retry can never leave two
/// Versions behind.
const MAX_PUBLISH_ATTEMPTS: u32 = 3;

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("cannot prepare a Git workspace: {0}")]
    Workspace(#[source] std::io::Error),
    #[error("cannot start Git: {0}")]
    Start(#[source] std::io::Error),
    #[error("git {command} failed: {message}")]
    Command { command: String, message: String },
    /// Git cannot combine the change with what the branch holds now. The
    /// published state is left exactly as it was.
    #[error("the change and the published Recipe cannot be combined")]
    Conflict,
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

/// A request to publish one new Version of a Recipe.
///
/// The change is built on `base_version`, which is the Version the person
/// started from. Git therefore has the three sides it needs to combine the
/// change with a branch that moved while the person worked.
#[derive(Debug)]
pub struct PublishVersion<'a> {
    /// Where to push, without any credential in it.
    pub remote_url: &'a str,
    pub token: &'a Secret<String>,
    pub identity: &'a Identity,
    pub branch: &'a str,
    pub message: &'a str,
    /// The Version the person started their change from.
    pub base_version: &'a str,
    /// File name to content.
    pub files: BTreeMap<String, Vec<u8>>,
}

/// The Git operations that the Recipe model needs.
///
/// Later tickets add draft branches, comparison, and submodules here.
#[async_trait]
pub trait GitAdapter: Send + Sync + std::fmt::Debug {
    /// Create a repository with one commit and push it.
    ///
    /// Returns the identifier of the Version that it created.
    async fn create_initial_commit(&self, request: InitialCommit<'_>) -> Result<String, GitError>;

    /// Read the Version that a branch points at.
    ///
    /// Gives `None` when the branch does not exist. Git holds History, so
    /// this asks Git and not the Forgejo API.
    async fn branch_head(
        &self,
        remote_url: &str,
        token: &Secret<String>,
        branch: &str,
    ) -> Result<Option<String>, GitError>;

    /// Publish one new Version on a branch.
    ///
    /// Exactly one Version reaches the branch. If the branch moved while
    /// the person worked, Git combines the change with the new state. If
    /// Git cannot combine them, this gives [`GitError::Conflict`] and the
    /// branch keeps exactly what it had.
    ///
    /// Returns the identifier of the Version that the branch now points at.
    async fn publish_version(&self, request: PublishVersion<'_>) -> Result<String, GitError>;
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

    async fn branch_head(
        &self,
        remote_url: &str,
        token: &Secret<String>,
        branch: &str,
    ) -> Result<Option<String>, GitError> {
        let workspace = tempfile::tempdir().map_err(GitError::Workspace)?;

        // `ls-remote` answers `<sha>\trefs/heads/<branch>`, or nothing at
        // all when the branch does not exist.
        let output = self
            .run(
                workspace.path(),
                token,
                &["ls-remote", remote_url, &format!("refs/heads/{branch}")],
            )
            .await?;

        Ok(output.split_whitespace().next().map(str::to_string))
    }

    async fn publish_version(&self, request: PublishVersion<'_>) -> Result<String, GitError> {
        let workspace = tempfile::tempdir().map_err(GitError::Workspace)?;
        let root = workspace.path();

        // Git reads its per-user configuration from the home directory. That
        // directory must sit outside the work tree, or `add --all` would put
        // it into the Recipe.
        let home = root.join("home");
        tokio::fs::create_dir_all(&home)
            .await
            .map_err(GitError::Workspace)?;
        let repo = root.join("repo");

        self.run_in(
            &home,
            root,
            request.token,
            &["clone", "--quiet", request.remote_url, "repo"],
        )
        .await?;

        // Build the change on the Version the person started from. This one
        // commit is the whole change, and it is the only thing that is ever
        // applied, however many times the push has to be tried.
        self.run_in(
            &home,
            &repo,
            request.token,
            &[
                "-c",
                "advice.detachedHead=false",
                "checkout",
                "--quiet",
                "--detach",
                request.base_version,
            ],
        )
        .await?;

        for (name, content) in &request.files {
            let file = repo.join(name);
            if let Some(parent) = file.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(GitError::Workspace)?;
            }
            tokio::fs::write(&file, content)
                .await
                .map_err(GitError::Workspace)?;
        }

        self.run_in(&home, &repo, request.token, &["add", "--all"])
            .await?;

        let name = format!("user.name={}", request.identity.name);
        let email = format!("user.email={}", request.identity.email);

        self.run_in(
            &home,
            &repo,
            request.token,
            &[
                "-c",
                &name,
                "-c",
                &email,
                "commit",
                "--quiet",
                "--message",
                request.message,
            ],
        )
        .await?;

        let change = self
            .run_in(&home, &repo, request.token, &["rev-parse", "HEAD"])
            .await?
            .trim()
            .to_string();

        let mut last = String::new();

        for _ in 0..MAX_PUBLISH_ATTEMPTS {
            self.run_in(
                &home,
                &repo,
                request.token,
                &["fetch", "--quiet", "origin", request.branch],
            )
            .await?;

            let tip = self
                .run_in(&home, &repo, request.token, &["rev-parse", "FETCH_HEAD"])
                .await?
                .trim()
                .to_string();

            let candidate = if tip == request.base_version {
                // Nothing moved. The change already sits on the branch tip.
                change.clone()
            } else {
                match self
                    .apply_onto(&home, &repo, &request, &tip, &change, &name, &email)
                    .await?
                {
                    Applied::Version(version) => version,
                    // Somebody published the same content first, so the
                    // branch already carries what this person wrote.
                    Applied::AlreadyThere => return Ok(tip),
                }
            };

            let pushed = self
                .attempt(
                    &home,
                    &repo,
                    request.token,
                    &[
                        "push",
                        "--quiet",
                        "origin",
                        &format!("{candidate}:refs/heads/{}", request.branch),
                    ],
                )
                .await?;

            if pushed.success {
                drop(workspace);
                return Ok(candidate);
            }

            // The branch moved between the read and the push. Read it again
            // and apply the same one change to the new state. The Version
            // built a moment ago is abandoned and never reached anybody.
            last = redact(&pushed.stderr, request.token);
        }

        Err(GitError::Command {
            command: "push".to_string(),
            message: last,
        })
    }
}

/// What happened when the change was applied to a branch that moved.
enum Applied {
    /// The change became this Version on top of the branch tip.
    Version(String),
    /// The branch already holds this content.
    AlreadyThere,
}

impl SystemGit {
    /// Put the one change commit on top of what the branch holds now.
    ///
    /// `cherry-pick` is a three-way combination: the Version the person
    /// started from is the common side, so a change that touches other
    /// lines or other files joins without a question.
    #[allow(clippy::too_many_arguments)]
    async fn apply_onto(
        &self,
        home: &Path,
        repo: &Path,
        request: &PublishVersion<'_>,
        tip: &str,
        change: &str,
        name: &str,
        email: &str,
    ) -> Result<Applied, GitError> {
        self.run_in(
            home,
            repo,
            request.token,
            &[
                "-c",
                "advice.detachedHead=false",
                "checkout",
                "--quiet",
                "--detach",
                tip,
            ],
        )
        .await?;

        let picked = self
            .attempt(
                home,
                repo,
                request.token,
                &["-c", name, "-c", email, "cherry-pick", change],
            )
            .await?;

        if picked.success {
            let version = self
                .run_in(home, repo, request.token, &["rev-parse", "HEAD"])
                .await?
                .trim()
                .to_string();
            return Ok(Applied::Version(version));
        }

        // A file that Git could not combine is left unmerged in the index.
        // That is the state, not the wording of a message, so read it.
        let unmerged = self
            .attempt(home, repo, request.token, &["ls-files", "--unmerged"])
            .await?;
        let conflicted = !unmerged.stdout.trim().is_empty();

        // Leave nothing half-applied behind, whatever comes next.
        let _ = self
            .attempt(home, repo, request.token, &["cherry-pick", "--abort"])
            .await;

        if conflicted {
            return Err(GitError::Conflict);
        }

        if picked.stderr.contains("empty") || picked.stdout.contains("empty") {
            return Ok(Applied::AlreadyThere);
        }

        Err(GitError::Command {
            command: "cherry-pick".to_string(),
            message: redact(&picked.stderr, request.token),
        })
    }

    async fn run(
        &self,
        workspace: &Path,
        token: &Secret<String>,
        args: &[&str],
    ) -> Result<String, GitError> {
        self.run_in(workspace, workspace, token, args).await
    }

    /// Run Git with the home directory kept apart from the work tree.
    async fn run_in(
        &self,
        home: &Path,
        workspace: &Path,
        token: &Secret<String>,
        args: &[&str],
    ) -> Result<String, GitError> {
        let attempt = self.attempt(home, workspace, token, args).await?;

        if !attempt.success {
            return Err(GitError::Command {
                // Only the verb is recorded. An argument can hold a Recipe
                // title or a path, which does not belong in a log.
                command: verb(args),
                message: redact(&attempt.stderr, token),
            });
        }

        Ok(attempt.stdout)
    }

    /// Run Git and report the outcome instead of failing on it.
    ///
    /// Some Git commands are expected to fail: a push loses a race, and a
    /// cherry-pick finds a conflict. Both are answers, not faults.
    async fn attempt(
        &self,
        home: &Path,
        workspace: &Path,
        token: &Secret<String>,
        args: &[&str],
    ) -> Result<Attempt, GitError> {
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
            .env("HOME", home)
            .env("USERPROFILE", home)
            .kill_on_drop(true);

        let output = command.output().await.map_err(GitError::Start)?;

        Ok(Attempt {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }
}

/// What one Git run produced.
struct Attempt {
    success: bool,
    stdout: String,
    stderr: String,
}

/// The Git verb that a set of arguments asks for.
///
/// A run can carry `-c key=value` settings first, and those hold a Recipe
/// title or an address. The verb is the first argument that is not one of
/// them, and it is the only part that reaches a message or a log.
fn verb(args: &[&str]) -> String {
    let mut index = 0;
    while index < args.len() {
        if args[index] == "-c" {
            index += 2;
            continue;
        }
        return args[index].to_string();
    }
    "git".to_string()
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

    #[test]
    fn a_setting_in_front_of_the_verb_never_becomes_the_verb() {
        // A commit carries `-c user.name=<the person>`. Reporting that as
        // the command would put a name into every log line.
        assert_eq!(verb(&["-c", "user.name=Sam Cook", "commit"]), "commit");
        assert_eq!(
            verb(&["-c", "advice.detachedHead=false", "checkout", "--detach"]),
            "checkout"
        );
        assert_eq!(verb(&["push"]), "push");
        assert_eq!(verb(&[]), "git");
    }

    #[test]
    fn a_conflict_is_its_own_answer_and_names_no_git_word() {
        // The person reads this. It has to say what happened without the
        // words branch, commit, or merge.
        let text = GitError::Conflict.to_string();
        for word in ["branch", "commit", "merge", "rebase", "cherry"] {
            assert!(!text.contains(word), "`{word}` must not reach the person");
        }
    }

    #[tokio::test]
    async fn a_missing_branch_is_reported_as_nothing_rather_than_a_fault() {
        // `ls-remote` on a repository with no such branch answers with an
        // empty list, which is an answer and not a failure.
        let source = tempfile::tempdir().unwrap();
        let git = SystemGit;
        let token = Secret::new(String::new());

        git.run(source.path(), &token, &["init", "--quiet", "--bare"])
            .await
            .expect("cannot make the probe repository");

        let head = git
            .branch_head(&source.path().to_string_lossy(), &token, "main")
            .await
            .expect("reading a branch must not fail");

        assert_eq!(head, None);
    }
}
