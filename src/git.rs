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

/// How many times a publication tries again when the branch moves under it.
///
/// Each try applies the same one change, so a retry can never leave two
/// Versions behind.
const MAX_PUBLISH_ATTEMPTS: u32 = 3;

/// The file that names every Recipe of a Cookbook and where it lives.
const MODULES_FILE: &str = ".gitmodules";
/// The file mode that Git gives a reference to another repository.
const REFERENCE_MODE: &str = "160000";

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
    /// The path is not a plain name inside the Recipe. The path itself
    /// never reaches the message, for the same reason an argument does not.
    #[error("a file name in this Version is not allowed")]
    Name,
    /// The draft moved on while the person wrote, so nothing was written.
    /// What the draft held is left exactly as it was.
    #[error("the draft changed somewhere else")]
    Stale,
    /// The Version asked for is not one that this Recipe holds.
    #[error("this Recipe does not hold that Version")]
    Missing,
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

/// What a draft holds now.
///
/// A draft is one Version that nobody published. `base_version` is the
/// published Version it was built on, which is the side a publication needs
/// when the published Recipe moved while the person wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftState {
    /// The draft Version.
    pub version: String,
    /// The published Version the draft was built on.
    pub base_version: String,
}

/// A request to write a draft, replacing whatever the draft held before.
///
/// A draft is always exactly one Version on top of `base_version`, so a
/// person who writes for an hour leaves one draft Version behind and not
/// hundreds.
#[derive(Debug)]
pub struct SaveDraft<'a> {
    /// Where to push, without any credential in it.
    pub remote_url: &'a str,
    pub token: &'a Secret<String>,
    pub identity: &'a Identity,
    /// The branch that carries the published Recipe. Only this branch
    /// travels, because `base_version` is on it.
    pub published_branch: &'a str,
    /// The branch that carries the draft.
    pub branch: &'a str,
    pub message: &'a str,
    /// The published Version the draft is built on.
    pub base_version: &'a str,
    /// The draft Version this writer started from. `None` says the person
    /// has no draft yet, and then a draft that already exists refuses the
    /// write.
    pub expected: Option<&'a str>,
    /// File name to content.
    pub files: BTreeMap<String, Vec<u8>>,
}

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
    /// Add one Version to a Recipe that exists.
    ///
    /// Returns the identifier of the Version. A change that makes no
    /// difference to the content adds no Version, and the identifier of
    /// the published Version comes back instead.
    async fn commit_change(&self, request: ChangeCommit<'_>) -> Result<String, GitError>;

    /// Read what a draft holds, when there is one.
    ///
    /// Gives `None` when the person has no draft. Git holds the content, so
    /// this asks Git and not the Forgejo API.
    async fn draft_state(
        &self,
        remote_url: &str,
        token: &Secret<String>,
        branch: &str,
    ) -> Result<Option<DraftState>, GitError>;

    /// Write a draft, replacing what it held.
    ///
    /// The write happens only while the draft still holds
    /// [`SaveDraft::expected`]. If a second tab or another device wrote
    /// first, this gives [`GitError::Stale`] and the stored draft keeps
    /// exactly what it had.
    ///
    /// Returns the identifier of the draft Version that the write made.
    async fn save_draft(&self, request: SaveDraft<'_>) -> Result<String, GitError>;

    /// Remove a branch that this application made for one person.
    ///
    /// A branch that is already gone is the state that was asked for, so
    /// that is not a fault.
    async fn remove_branch(
        &self,
        remote_url: &str,
        token: &Secret<String>,
        branch: &str,
    ) -> Result<(), GitError>;

    /// Move a branch back to an earlier Version that it already holds.
    ///
    /// A Variation that starts at an earlier Version is made this way. The
    /// copy carries the whole Recipe first, and the published branch is then
    /// moved before anybody can read it.
    ///
    /// Only a Version that the branch already holds is accepted, so nothing
    /// from outside the published Recipe can ever be published by this. A
    /// Version that the branch does not hold gives [`GitError::Missing`] and
    /// the branch keeps exactly what it had.
    async fn move_branch(
        &self,
        remote_url: &str,
        token: &Secret<String>,
        branch: &str,
        version: &str,
    ) -> Result<(), GitError>;

    /// Write one Recipe reference into a Cookbook.
    ///
    /// The reference records the address of the Recipe and the exact
    /// Version that the Cookbook holds. The Recipe repository is not read
    /// and not written: only the Cookbook gets a new Version.
    ///
    /// A reference that is already at this path is replaced, which is how a
    /// Cookbook moves a Recipe to another Version.
    ///
    /// Returns the identifier of the Cookbook Version that this made.
    async fn write_reference(&self, request: WriteReference<'_>) -> Result<String, GitError>;

    /// Take one Recipe reference out of a Cookbook.
    ///
    /// Only the Cookbook changes. The Recipe repository is not read and not
    /// written, so a Recipe that leaves a Cookbook keeps every Version it
    /// had.
    ///
    /// Returns the identifier of the Cookbook Version that this made.
    async fn remove_reference(&self, request: RemoveReference<'_>) -> Result<String, GitError>;
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

        self.run_in(home, &work, request.token, &["add", "--all"])
            .await?;

        // A change that makes no difference must not become an empty
        // Version, because History is for a person to read.
        let pending = self
            .run_in(home, &work, request.token, &["status", "--porcelain"])
            .await?;
        if pending.trim().is_empty() {
            return self.head(&work, home, request.token).await;
        }

        self.run_in(
            home,
            &work,
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
            home,
            &work,
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

    async fn draft_state(
        &self,
        remote_url: &str,
        token: &Secret<String>,
        branch: &str,
    ) -> Result<Option<DraftState>, GitError> {
        let Some(version) = self.branch_head(remote_url, token, branch).await? else {
            return Ok(None);
        };

        let workspace = tempfile::tempdir().map_err(GitError::Workspace)?;
        let root = workspace.path();

        // Two Versions are enough: the draft, and the published Version it
        // was built on. The rest of the History never travels, so reading a
        // draft costs the same on a Recipe that is years old.
        self.run(root, token, &["init", "--quiet"]).await?;
        self.run(
            root,
            token,
            &["fetch", "--quiet", "--depth=2", remote_url, branch],
        )
        .await?;

        let base_version = self
            .run(root, token, &["rev-parse", "FETCH_HEAD^"])
            .await?
            .trim()
            .to_string();

        drop(workspace);

        Ok(Some(DraftState {
            version,
            base_version,
        }))
    }

    async fn save_draft(&self, request: SaveDraft<'_>) -> Result<String, GitError> {
        // Ask what the draft holds before anything is built. A second tab
        // is then refused for the price of one question, and the person
        // gets the reason rather than a failed push.
        let held = self
            .branch_head(request.remote_url, request.token, request.branch)
            .await?;
        if held.as_deref() != request.expected {
            return Err(GitError::Stale);
        }

        let workspace = tempfile::tempdir().map_err(GitError::Workspace)?;
        let root = workspace.path();

        // Git reads its per-user configuration from the home directory, and
        // that directory must sit outside the work tree.
        let home = root.join("home");
        tokio::fs::create_dir_all(&home)
            .await
            .map_err(GitError::Workspace)?;
        let repo = root.join("repo");

        // Only the published branch travels. The draft itself is replaced
        // whole, so nothing that it holds is needed here.
        self.run_in(
            &home,
            root,
            request.token,
            &[
                "clone",
                "--quiet",
                "--single-branch",
                "--branch",
                request.published_branch,
                request.remote_url,
                "repo",
            ],
        )
        .await?;

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

        write_files(&repo, &request.files).await?;

        self.run_in(&home, &repo, request.token, &["add", "--all"])
            .await?;

        // A draft always answers with a Version, even when the person typed
        // something and then took it out again. Without this, a save that
        // lands back on the published text would give the writer nothing to
        // send with the next one.
        self.run_in(
            &home,
            &repo,
            request.token,
            &[
                "-c",
                &format!("user.name={}", request.identity.name),
                "-c",
                &format!("user.email={}", request.identity.email),
                "commit",
                "--quiet",
                "--allow-empty",
                "--message",
                request.message,
            ],
        )
        .await?;

        let version = self.head(&repo, &home, request.token).await?;

        // The lease is the guard that matters. Between the question above
        // and this push a second tab can still write, and then Git refuses
        // this push instead of letting it win.
        //
        // An empty expected value means the draft must not exist yet, which
        // is what keeps two first saves from overwriting each other.
        let lease = format!(
            "--force-with-lease=refs/heads/{}:{}",
            request.branch,
            request.expected.unwrap_or_default()
        );
        let refspec = format!("{version}:refs/heads/{}", request.branch);

        let pushed = self
            .attempt(
                &home,
                &repo,
                request.token,
                &["push", "--quiet", &lease, request.remote_url, &refspec],
            )
            .await?;

        if pushed.success {
            drop(workspace);
            return Ok(version);
        }

        // Git refuses a lease it cannot honour, and that is the answer this
        // caller wants: somebody else wrote first.
        if pushed.stderr.contains("stale info") || pushed.stderr.contains("[rejected]") {
            return Err(GitError::Stale);
        }

        Err(GitError::Command {
            command: "push".to_string(),
            message: redact(&pushed.stderr, request.token),
        })
    }

    async fn remove_branch(
        &self,
        remote_url: &str,
        token: &Secret<String>,
        branch: &str,
    ) -> Result<(), GitError> {
        let workspace = tempfile::tempdir().map_err(GitError::Workspace)?;
        let root = workspace.path();

        // A push needs a repository to run from, and nothing of the Recipe
        // is read, so an empty one is enough.
        self.run(root, token, &["init", "--quiet"]).await?;

        let attempt = self
            .attempt(
                root,
                root,
                token,
                &[
                    "push",
                    "--quiet",
                    remote_url,
                    "--delete",
                    &format!("refs/heads/{branch}"),
                ],
            )
            .await?;

        if attempt.success {
            return Ok(());
        }

        // A branch that is already gone is the state that was asked for.
        if attempt.stderr.contains("remote ref does not exist") {
            return Ok(());
        }

        Err(GitError::Command {
            command: "push".to_string(),
            message: redact(&attempt.stderr, token),
        })
    }

    async fn move_branch(
        &self,
        remote_url: &str,
        token: &Secret<String>,
        branch: &str,
        version: &str,
    ) -> Result<(), GitError> {
        let workspace = tempfile::tempdir().map_err(GitError::Workspace)?;
        let root = workspace.path();

        // Only the published branch travels, and with it every Version that
        // it holds. Nothing else of the Recipe is needed here.
        self.run(root, token, &["init", "--quiet"]).await?;
        self.run(root, token, &["fetch", "--quiet", remote_url, branch])
            .await?;

        // The Version has to be one that the published branch holds. This
        // is the check, and it is a read of Git rather than a promise from
        // the caller.
        let held = self
            .attempt(
                root,
                root,
                token,
                &[
                    "merge-base",
                    "--is-ancestor",
                    &format!("{version}^{{commit}}"),
                    "FETCH_HEAD",
                ],
            )
            .await?;

        if !held.success {
            return Err(GitError::Missing);
        }

        self.run(
            root,
            token,
            &[
                "push",
                "--quiet",
                "--force",
                remote_url,
                &format!("{version}:refs/heads/{branch}"),
            ],
        )
        .await?;

        drop(workspace);

        Ok(())
    }

    async fn write_reference(&self, request: WriteReference<'_>) -> Result<String, GitError> {
        self.change_references(
            request.remote_url,
            request.token,
            request.identity,
            request.branch,
            request.message,
            &Reference::Write {
                path: request.path,
                url: request.url,
                version: request.version,
                follow: request.follow,
            },
        )
        .await
    }

    async fn remove_reference(&self, request: RemoveReference<'_>) -> Result<String, GitError> {
        self.change_references(
            request.remote_url,
            request.token,
            request.identity,
            request.branch,
            request.message,
            &Reference::Remove { path: request.path },
        )
        .await
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
    /// The identifier of the Version that the workspace is on.
    async fn head(
        &self,
        work: &Path,
        home: &Path,
        token: &Secret<String>,
    ) -> Result<String, GitError> {
        Ok(self
            .run_in(home, work, token, &["rev-parse", "HEAD"])
            .await?
            .trim()
            .to_string())
    }

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

    /// Change one Recipe reference of a Cookbook and publish the result.
    ///
    /// The change is made on what the Cookbook holds now, so two people who
    /// add a Recipe at the same moment both keep their Recipe. If the
    /// Cookbook moved between the read and the push, the whole change is
    /// made again on the new state, and the abandoned attempt reached
    /// nobody.
    ///
    /// `git config` writes `.gitmodules`, so the file keeps exactly the
    /// shape that Git itself writes and any Git client reads it.
    async fn change_references(
        &self,
        remote_url: &str,
        token: &Secret<String>,
        identity: &Identity,
        branch: &str,
        message: &str,
        change: &Reference<'_>,
    ) -> Result<String, GitError> {
        let mut last = String::new();

        for _ in 0..MAX_PUBLISH_ATTEMPTS {
            let workspace = tempfile::tempdir().map_err(GitError::Workspace)?;
            // Git reads its per-user configuration from the home directory,
            // and that directory must sit outside the work tree.
            let home = workspace.path();
            let work = home.join("work");

            self.run_in(
                home,
                home,
                token,
                &[
                    "clone",
                    "--quiet",
                    "--depth",
                    "1",
                    "--single-branch",
                    "--branch",
                    branch,
                    remote_url,
                    "work",
                ],
            )
            .await?;

            self.apply_reference(home, &work, token, change).await?;

            // A change that makes no difference must not become a Version,
            // because History is for a person to read.
            let pending = self
                .run_in(home, &work, token, &["status", "--porcelain"])
                .await?;
            if pending.trim().is_empty() {
                return self.head(&work, home, token).await;
            }

            self.run_in(
                home,
                &work,
                token,
                &[
                    "-c",
                    &format!("user.name={}", identity.name),
                    "-c",
                    &format!("user.email={}", identity.email),
                    "commit",
                    "--quiet",
                    "--message",
                    message,
                ],
            )
            .await?;

            let version = self.head(&work, home, token).await?;

            let pushed = self
                .attempt(
                    home,
                    &work,
                    token,
                    &[
                        "push",
                        "--quiet",
                        remote_url,
                        &format!("{version}:refs/heads/{branch}"),
                    ],
                )
                .await?;

            if pushed.success {
                return Ok(version);
            }

            last = redact(&pushed.stderr, token);
        }

        Err(GitError::Command {
            command: "push".to_string(),
            message: last,
        })
    }

    /// Make the one change inside a Cookbook that was just read.
    async fn apply_reference(
        &self,
        home: &Path,
        work: &Path,
        token: &Secret<String>,
        change: &Reference<'_>,
    ) -> Result<(), GitError> {
        // The path is built from a Recipe name, so it is checked here as
        // well. Nothing may reach outside the Cookbook.
        let inside = safe_path(work, change.path())?;
        let modules = work.join(MODULES_FILE);
        let section = format!("submodule.{}", change.path());

        match change {
            Reference::Write {
                path,
                url,
                version,
                follow,
            } => {
                // Git records a directory for the Recipe. It stays empty
                // here: the Recipe lives in its own repository and this
                // Version records only which one, and which Version of it.
                tokio::fs::create_dir_all(&inside)
                    .await
                    .map_err(GitError::Workspace)?;

                self.write_setting(home, work, token, &format!("{section}.path"), path)
                    .await?;
                self.write_setting(home, work, token, &format!("{section}.url"), url)
                    .await?;

                match follow {
                    // A Cookbook that follows a Recipe names the branch it
                    // follows. A Pinned Recipe names none at all, which is
                    // what keeps it on the Version that was selected.
                    Some(branch) => {
                        self.write_setting(home, work, token, &format!("{section}.branch"), branch)
                            .await?;
                    }
                    None => {
                        let _ = self
                            .attempt(
                                home,
                                work,
                                token,
                                &[
                                    "config",
                                    "-f",
                                    MODULES_FILE,
                                    "--unset",
                                    &format!("{section}.branch"),
                                ],
                            )
                            .await?;
                    }
                }

                self.run_in(home, work, token, &["add", MODULES_FILE])
                    .await?;
                self.run_in(
                    home,
                    work,
                    token,
                    &[
                        "update-index",
                        "--add",
                        "--cacheinfo",
                        &format!("{REFERENCE_MODE},{version},{path}"),
                    ],
                )
                .await?;
            }
            Reference::Remove { path } => {
                // A section that is not there is the state that was asked
                // for, so a refusal here is an answer and not a fault.
                let _ = self
                    .attempt(
                        home,
                        work,
                        token,
                        &["config", "-f", MODULES_FILE, "--remove-section", &section],
                    )
                    .await?;

                let _ = self
                    .attempt(home, work, token, &["update-index", "--force-remove", path])
                    .await?;

                let left = tokio::fs::read_to_string(&modules)
                    .await
                    .unwrap_or_default();

                if left.trim().is_empty() {
                    // The last Recipe left, so the file goes with it. This
                    // is what Git itself does, and it is what keeps a
                    // Cookbook with no Recipes exactly as a new one.
                    let _ = self
                        .attempt(
                            home,
                            work,
                            token,
                            &[
                                "rm",
                                "--quiet",
                                "--cached",
                                "--ignore-unmatch",
                                MODULES_FILE,
                            ],
                        )
                        .await?;
                    let _ = tokio::fs::remove_file(&modules).await;
                } else {
                    self.run_in(home, work, token, &["add", MODULES_FILE])
                        .await?;
                }

                let _ = tokio::fs::remove_dir_all(&inside).await;
            }
        }

        Ok(())
    }

    /// Write one setting into `.gitmodules`.
    ///
    /// The value can be a Recipe address, so it never reaches a message.
    async fn write_setting(
        &self,
        home: &Path,
        work: &Path,
        token: &Secret<String>,
        key: &str,
        value: &str,
    ) -> Result<(), GitError> {
        self.run_in(
            home,
            work,
            token,
            &["config", "-f", MODULES_FILE, key, value],
        )
        .await?;
        Ok(())
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
    fn a_refused_draft_write_names_no_git_word() {
        // A person reads this one too, so it says what happened without
        // naming a branch or a push.
        let text = GitError::Stale.to_string();
        for word in ["branch", "commit", "push", "merge", "rebase", "ref"] {
            assert!(!text.contains(word), "`{word}` must not reach the person");
        }
    }

    #[test]
    fn a_version_that_is_not_there_names_no_git_word() {
        // A person reads this one when a Variation is asked for from a
        // Version that the Recipe does not hold.
        let text = GitError::Missing.to_string();
        for word in ["branch", "commit", "sha", "ref", "fork", "head"] {
            assert!(!text.contains(word), "`{word}` must not reach the person");
        }
    }

    #[tokio::test]
    async fn a_branch_moves_back_only_to_a_version_that_it_holds() {
        // This is what makes a Variation start at an earlier Version. A
        // Version from anywhere else must never reach the published branch.
        let source = tempfile::tempdir().unwrap();
        let git = SystemGit;
        let token = Secret::new(String::new());
        let identity = Identity {
            name: "Sam Cook".to_string(),
            email: "sam@noreply.localhost".to_string(),
        };

        git.run(source.path(), &token, &["init", "--quiet", "--bare"])
            .await
            .expect("cannot make the probe repository");
        let remote = source.path().to_string_lossy().replace('\\', "/");

        let mut first_files = BTreeMap::new();
        first_files.insert("recipe.cook".to_string(), b"Chop it.\n".to_vec());
        let first = git
            .create_initial_commit(InitialCommit {
                remote_url: &remote,
                token: &token,
                identity: &identity,
                branch: "main",
                message: "Add Chili",
                files: first_files,
            })
            .await
            .expect("cannot write the first Version");

        let mut second_files = BTreeMap::new();
        second_files.insert("recipe.cook".to_string(), b"Chop it. Fry it.\n".to_vec());
        let second = git
            .publish_version(PublishVersion {
                remote_url: &remote,
                token: &token,
                identity: &identity,
                branch: "main",
                message: "Fry it too",
                base_version: &first,
                files: second_files,
            })
            .await
            .expect("cannot write the second Version");

        assert_eq!(
            git.branch_head(&remote, &token, "main").await.unwrap(),
            Some(second.clone())
        );

        git.move_branch(&remote, &token, "main", &first)
            .await
            .expect("the branch must move back");
        assert_eq!(
            git.branch_head(&remote, &token, "main").await.unwrap(),
            Some(first.clone())
        );

        // A Version that this Recipe never held is refused, and the branch
        // keeps what it has.
        let outside = git
            .move_branch(
                &remote,
                &token,
                "main",
                "0123456789abcdef0123456789abcdef01234567",
            )
            .await;
        assert!(matches!(outside, Err(GitError::Missing)));
        assert_eq!(
            git.branch_head(&remote, &token, "main").await.unwrap(),
            Some(first)
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
}

/// A request to write one Recipe reference into a Cookbook.
///
/// The Cookbook is the repository that changes. The Recipe is named by its
/// address and by the exact Version that the Cookbook holds, and nothing at
/// all is written to it.
#[derive(Debug)]
pub struct WriteReference<'a> {
    /// Where to push, without any credential in it. This is the Cookbook.
    pub remote_url: &'a str,
    pub token: &'a Secret<String>,
    pub identity: &'a Identity,
    /// The branch that carries the published Cookbook.
    pub branch: &'a str,
    pub message: &'a str,
    /// Where the Recipe sits inside the Cookbook.
    pub path: &'a str,
    /// The address of the Recipe repository.
    pub url: &'a str,
    /// The exact Version of the Recipe that the Cookbook holds.
    pub version: &'a str,
    /// The branch of the Recipe to follow. `None` keeps the Version that
    /// `version` names and follows nothing.
    pub follow: Option<&'a str>,
}

/// A request to take one Recipe reference out of a Cookbook.
#[derive(Debug)]
pub struct RemoveReference<'a> {
    /// Where to push, without any credential in it. This is the Cookbook.
    pub remote_url: &'a str,
    pub token: &'a Secret<String>,
    pub identity: &'a Identity,
    /// The branch that carries the published Cookbook.
    pub branch: &'a str,
    pub message: &'a str,
    /// Where the Recipe sits inside the Cookbook.
    pub path: &'a str,
}

/// The one change that a Cookbook Version carries.
enum Reference<'a> {
    Write {
        path: &'a str,
        url: &'a str,
        version: &'a str,
        follow: Option<&'a str>,
    },
    Remove {
        path: &'a str,
    },
}

impl Reference<'_> {
    fn path(&self) -> &str {
        match self {
            Reference::Write { path, .. } | Reference::Remove { path } => path,
        }
    }
}
