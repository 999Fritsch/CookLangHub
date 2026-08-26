//! The Forgejo boundary.
//!
//! The application reaches Forgejo only through its supported HTTP API and
//! its OAuth endpoints. It never opens the database of Forgejo and never
//! touches its repository storage.
//!
//! Two base URLs matter. `api_url` is where this process reaches Forgejo,
//! which inside the bundled stack is a name on the internal network. The
//! browser cannot resolve that name, so an address that a person follows
//! comes from `public_url` instead.

use std::time::Duration;

use serde::Deserialize;

use crate::secret::Secret;

#[derive(Debug, thiserror::Error)]
pub enum ForgejoError {
    #[error("cannot build the HTTP client: {0}")]
    Client(#[source] reqwest::Error),
    #[error("cannot reach Forgejo: {0}")]
    Unreachable(String),
    #[error("Forgejo answered with status {status}: {body}")]
    Status { status: u16, body: String },
    #[error("Forgejo sent an answer that the application cannot read: {0}")]
    Body(String),
}

#[derive(Debug, Deserialize)]
struct VersionResponse {
    version: String,
}

/// A Forgejo user, as the API gives it.
#[derive(Debug, Clone, Deserialize)]
pub struct ForgejoUser {
    pub id: i64,
    pub login: String,
    #[serde(default)]
    pub full_name: String,
    #[serde(default)]
    pub avatar_url: String,
    /// The address Forgejo gives for this person. It is a no-reply address
    /// when they keep their address private, and the application uses it
    /// exactly as given so that the privacy setting is obeyed.
    #[serde(default)]
    pub email: String,
}

impl ForgejoUser {
    /// The name to show. Forgejo lets the full name be empty.
    pub fn display_name(&self) -> &str {
        if self.full_name.trim().is_empty() {
            &self.login
        } else {
            &self.full_name
        }
    }
}

/// An OAuth2 application as Forgejo records it.
///
/// Forgejo returns `client_secret` only when it creates the application or
/// when it regenerates the secret. A plain list gives an empty string.
#[derive(Debug, Clone, Deserialize)]
pub struct OAuthApplication {
    pub id: i64,
    pub name: String,
    pub client_id: String,
    #[serde(default)]
    pub client_secret: String,
    #[serde(default)]
    pub redirect_uris: Vec<String>,
}

/// The account settings of the signed-in person.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct UserSettings {
    /// Whether this person keeps their address out of public view.
    #[serde(default)]
    pub hide_email: bool,
}

/// A Forgejo repository.
#[derive(Debug, Clone, Deserialize)]
pub struct Repository {
    /// The identifier that survives a rename.
    #[serde(default)]
    pub id: i64,
    pub name: String,
    pub full_name: String,
    pub html_url: String,
    pub clone_url: String,
    #[serde(default)]
    pub default_branch: String,
    #[serde(default)]
    pub private: bool,
    #[serde(default)]
    pub empty: bool,
    /// Whether Forgejo offers Issues for this repository.
    ///
    /// A Discussion is a Forgejo issue, so this field decides whether the
    /// Recipe has a Discussions area at all. The application only reads it.
    /// It never turns Issues on again. An answer without the field counts as
    /// off, which keeps the application on the safe side of that rule.
    #[serde(default)]
    pub has_issues: bool,
    /// The opt-in markers. A repository without them is not a Recipe.
    #[serde(default)]
    pub topics: Vec<String>,
    /// When Forgejo last recorded a change, as Forgejo writes it.
    #[serde(default)]
    pub updated_at: String,
    pub owner: RepositoryOwner,
}

impl Repository {
    /// The branch that holds the published Recipe.
    ///
    /// Forgejo can report an empty name for a repository that holds nothing
    /// yet, and a read needs a name either way.
    pub fn branch(&self) -> &str {
        if self.default_branch.is_empty() {
            crate::create_recipe::MAIN_BRANCH
        } else {
            &self.default_branch
        }
    }

    /// Whether this repository carries every topic in `topics`.
    ///
    /// The comparison ignores case, because Forgejo stores a topic in lower
    /// case and a person can type it either way.
    pub fn has_topics(&self, topics: &[&str]) -> bool {
        topics.iter().all(|wanted| {
            self.topics
                .iter()
                .any(|topic| topic.eq_ignore_ascii_case(wanted))
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct RepositoryOwner {
    #[serde(default)]
    pub id: i64,
    pub login: String,
}

/// Which repositories a search must return.
#[derive(Debug, Clone, Copy)]
pub enum Ownership {
    /// Everything the credential may see. No credential means public only.
    Anybody,
    /// What this person owns or may work on.
    ReachableBy(i64),
    /// What this person owns.
    OwnedBy(i64),
}

/// One page of a repository search.
#[derive(Debug, Clone)]
pub struct RepositoryQuery<'a> {
    /// The topic that marks the kind of repository, such as `cooklang`.
    pub topic: &'a str,
    pub ownership: Ownership,
    /// Counts from 1, the way Forgejo counts.
    pub page: u32,
    pub limit: u32,
}

/// Why a repository could not be created.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateRepositoryOutcome {
    /// A repository with that name already belongs to this person.
    NameTaken,
    /// Something else went wrong.
    Other,
}

/// A system webhook, as Forgejo reports it.
#[derive(Debug, Clone, Deserialize)]
pub struct SystemHook {
    pub id: i64,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub config: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub events: Vec<String>,
    #[serde(default)]
    pub active: bool,
}

impl SystemHook {
    /// Where Forgejo posts.
    ///
    /// Forgejo reports the address in two places, and which of the two
    /// carries it depends on the release, so read the field first and the
    /// older configuration map second.
    pub fn target_url(&self) -> &str {
        if !self.url.is_empty() {
            return &self.url;
        }
        self.config
            .get("url")
            .map(String::as_str)
            .unwrap_or_default()
    }
}

/// The answer of the token endpoint.
///
/// Two of these fields are credentials. `Debug` is written by hand rather
/// than derived, because a derived one prints both of them, and one
/// `tracing` field that formats this value would put a working credential
/// into a log for as long as that log is kept.
#[derive(Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<i64>,
}

impl std::fmt::Debug for TokenResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenResponse")
            .field("access_token", &crate::secret::REDACTED)
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| crate::secret::REDACTED),
            )
            .field("expires_in", &self.expires_in)
            .finish()
    }
}

impl TokenResponse {
    /// The moment the access token stops working, in seconds since the
    /// epoch, when Forgejo says how long it lives.
    pub fn expires_at(&self, now: i64) -> Option<i64> {
        // A lifetime that is zero or negative is not a lifetime. Treat it
        // as unknown rather than as a token that is already dead, so a
        // strange answer cannot put a session into a renewal loop.
        self.expires_in
            .filter(|value| *value > 0)
            .map(|value| now + value)
    }
}

/// A Forgejo issue, which is one Discussion.
///
/// Forgejo holds every word of it. The application keeps no copy.
#[derive(Debug, Clone, Deserialize)]
pub struct Issue {
    /// The number a person sees in Forgejo, and the one in the address here.
    pub number: i64,
    #[serde(default)]
    pub title: String,
    /// The first message. Forgejo stores Markdown.
    #[serde(default)]
    pub body: String,
    /// `open` or `closed`, in the words of Forgejo.
    #[serde(default)]
    pub state: String,
    /// Forgejo answers with a Ghost user for a deleted account, but an
    /// answer without the field must not stop the page.
    #[serde(default)]
    pub user: Option<ForgejoUser>,
    #[serde(default)]
    pub created_at: String,
    /// How many comments follow the first message.
    #[serde(default)]
    pub comments: i64,
    /// Forgejo puts pull requests in the same list and marks them with this
    /// field. A Suggestion is a pull request, so an entry that carries it is
    /// not a Discussion.
    #[serde(default)]
    pub pull_request: Option<serde_json::Value>,
}

impl Issue {
    /// Whether this entry is a Discussion and not a Suggestion.
    pub fn is_discussion(&self) -> bool {
        self.pull_request.is_none()
    }

    pub fn is_open(&self) -> bool {
        self.state != "closed"
    }
}

/// One comment inside a Discussion.
#[derive(Debug, Clone, Deserialize)]
pub struct IssueComment {
    pub id: i64,
    /// The text of the comment. Forgejo stores Markdown.
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub user: Option<ForgejoUser>,
    #[serde(default)]
    pub created_at: String,
}

/// What one person may do with one repository, as Forgejo answers it.
///
/// `permission` is the plain access mode that Forgejo keeps: `none`, `read`,
/// `write`, `admin`, or `owner`. The application never keeps a copy of this
/// answer. It asks Forgejo again each time it needs one.
#[derive(Debug, Clone, Deserialize)]
pub struct RepositoryPermission {
    #[serde(default)]
    pub permission: String,
    /// The name Forgejo shows for the same access mode.
    #[serde(default)]
    pub role_name: String,
}

impl RepositoryPermission {
    /// Whether Forgejo gives this person read access and no more.
    pub fn is_read_only(&self) -> bool {
        matches!(self.permission.as_str(), "read" | "none" | "")
    }
}

/// One published Version, as the Forgejo commit endpoints report it.
///
/// Git holds History and Forgejo reads it out. The application keeps no copy
/// of any part of it.
#[derive(Debug, Clone, Deserialize)]
pub struct Commit {
    /// The identifier of this Version.
    #[serde(default)]
    pub sha: String,
    /// The Forgejo account behind the author, when Forgejo knows one. A
    /// Version written outside this application can name somebody that
    /// Forgejo has no account for.
    #[serde(default)]
    pub author: Option<ForgejoUser>,
    #[serde(default)]
    pub commit: CommitDetail,
}

/// What Git itself records about one Version.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct CommitDetail {
    /// What the person wrote about the change.
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub author: Option<CommitIdentity>,
}

/// The name and the moment that Git holds for one Version.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct CommitIdentity {
    #[serde(default)]
    pub name: String,
    /// RFC 3339, as Git writes it.
    #[serde(default)]
    pub date: String,
}

/// A client for one Forgejo instance.
#[derive(Debug, Clone)]
pub struct ForgejoClient {
    api_url: String,
    public_url: String,
    http: reqwest::Client,
}

impl ForgejoClient {
    pub fn new(api_url: impl Into<String>) -> Result<Self, ForgejoError> {
        let api_url = api_url.into();
        let public_url = api_url.clone();
        Self::with_urls(api_url, public_url)
    }

    pub fn with_urls(
        api_url: impl Into<String>,
        public_url: impl Into<String>,
    ) -> Result<Self, ForgejoError> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .user_agent(concat!("cooklanghub/", env!("CARGO_PKG_VERSION")))
            // A redirect on the token endpoint would send the credential
            // somewhere the application did not choose.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(ForgejoError::Client)?;

        Ok(Self {
            api_url: trim(api_url.into()),
            public_url: trim(public_url.into()),
            http,
        })
    }

    /// Where this process reaches Forgejo.
    pub fn api_url(&self) -> &str {
        &self.api_url
    }

    /// Where a browser reaches Forgejo. Every link uses this value.
    pub fn public_url(&self) -> &str {
        &self.public_url
    }

    /// The address that the Git adapter pushes to.
    ///
    /// This is built from `api_url` and never from the `clone_url` that
    /// Forgejo reports. Forgejo builds that field from its own ROOT_URL,
    /// which names the address a browser uses. Inside the bundled stack
    /// that address does not reach Forgejo from this process at all, so
    /// trusting it would make every push fail.
    pub fn git_url(&self, full_name: &str) -> String {
        format!("{}/{}.git", self.api_url, full_name.trim_matches('/'))
    }

    /// The address a person follows for **Open in Forgejo**.
    ///
    /// Built from `public_url` for the same reason as [`Self::git_url`]:
    /// the `html_url` that Forgejo reports comes from its own ROOT_URL,
    /// which is a configured value and need not match the address that
    /// this browser actually used.
    pub fn web_url(&self, full_name: &str) -> String {
        format!("{}/{}", self.public_url, full_name.trim_matches('/'))
    }

    /// Read the version of the Forgejo instance.
    ///
    /// This endpoint needs no credential, so the health probe carries no
    /// token and cannot leak one.
    pub async fn version(&self) -> Result<String, ForgejoError> {
        let response = self
            .send(self.http.get(format!("{}/api/v1/version", self.api_url)))
            .await?;
        let parsed: VersionResponse = read_json(response).await?;
        Ok(parsed.version)
    }

    /// The address a browser opens to approve the application.
    pub fn authorize_url(
        &self,
        client_id: &str,
        redirect_uri: &str,
        state: &str,
        pkce_challenge: &str,
    ) -> String {
        let query = [
            ("client_id", client_id),
            ("redirect_uri", redirect_uri),
            ("response_type", "code"),
            ("state", state),
            ("code_challenge_method", "S256"),
            ("code_challenge", pkce_challenge),
        ]
        .iter()
        .map(|(key, value)| format!("{key}={}", urlencode(value)))
        .collect::<Vec<_>>()
        .join("&");

        format!("{}/login/oauth/authorize?{query}", self.public_url)
    }

    /// Trade an authorization code for an access token.
    pub async fn exchange_code(
        &self,
        client_id: &str,
        client_secret: &Secret<String>,
        code: &str,
        redirect_uri: &str,
        pkce_verifier: &str,
    ) -> Result<TokenResponse, ForgejoError> {
        let form = [
            ("grant_type", "authorization_code"),
            ("client_id", client_id),
            ("client_secret", client_secret.expose().as_str()),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("code_verifier", pkce_verifier),
        ];

        let response = self
            .send(
                self.http
                    .post(format!("{}/login/oauth/access_token", self.api_url))
                    .form(&form),
            )
            .await?;

        read_json(response).await
    }

    /// Trade a refresh token for a new access token.
    ///
    /// Forgejo gives a new refresh token with every answer and refuses the
    /// old one from then on, so the caller must store what comes back. Two
    /// callers that refresh the same session at once would spend the same
    /// one-use token twice, which is why only one place in this application
    /// may call this.
    pub async fn refresh_access_token(
        &self,
        client_id: &str,
        client_secret: &Secret<String>,
        refresh_token: &Secret<String>,
    ) -> Result<TokenResponse, ForgejoError> {
        let form = [
            ("grant_type", "refresh_token"),
            ("client_id", client_id),
            ("client_secret", client_secret.expose().as_str()),
            ("refresh_token", refresh_token.expose().as_str()),
        ];

        let response = self
            .send(
                self.http
                    .post(format!("{}/login/oauth/access_token", self.api_url))
                    .form(&form),
            )
            .await?;

        read_json(response).await
    }

    /// Read the user that an access token belongs to.
    pub async fn current_user(
        &self,
        access_token: &Secret<String>,
    ) -> Result<ForgejoUser, ForgejoError> {
        let response = self
            .send(
                self.http
                    .get(format!("{}/api/v1/user", self.api_url))
                    .bearer_auth(access_token.expose()),
            )
            .await?;

        read_json(response).await
    }

    /// Create a repository that belongs to the token holder.
    ///
    /// The repository starts empty. The Git adapter puts the first Version
    /// in it, so that the commit carries the identity of the person.
    pub async fn create_repository(
        &self,
        token: &Secret<String>,
        name: &str,
        private: bool,
        default_branch: &str,
    ) -> Result<Repository, ForgejoError> {
        let response = self
            .send(
                self.http
                    .post(format!("{}/api/v1/user/repos", self.api_url))
                    .bearer_auth(token.expose())
                    .json(&serde_json::json!({
                        "name": name,
                        "private": private,
                        "auto_init": false,
                        "default_branch": default_branch,
                    })),
            )
            .await?;

        read_json(response).await
    }

    /// Replace the topics of a repository.
    ///
    /// Topics are the opt-in marker: a repository without them does not
    /// appear in this application.
    pub async fn set_topics(
        &self,
        token: &Secret<String>,
        owner: &str,
        repository: &str,
        topics: &[&str],
    ) -> Result<(), ForgejoError> {
        self.send(
            self.http
                .put(format!(
                    "{}/api/v1/repos/{owner}/{repository}/topics",
                    self.api_url
                ))
                .bearer_auth(token.expose())
                .json(&serde_json::json!({ "topics": topics })),
        )
        .await?;

        Ok(())
    }

    /// Read one repository.
    pub async fn repository(
        &self,
        token: &Secret<String>,
        owner: &str,
        repository: &str,
    ) -> Result<Repository, ForgejoError> {
        self.repository_as(Some(token), owner, repository).await
    }

    /// Read one repository, with or without a credential.
    ///
    /// No credential means the question is asked as an anonymous visitor,
    /// so Forgejo answers only for a public repository. This is how the
    /// application proves that Public really is public.
    pub async fn repository_as(
        &self,
        token: Option<&Secret<String>>,
        owner: &str,
        repository: &str,
    ) -> Result<Repository, ForgejoError> {
        let mut request = self.http.get(format!(
            "{}/api/v1/repos/{owner}/{repository}",
            self.api_url
        ));
        if let Some(token) = token {
            request = request.bearer_auth(token.expose());
        }

        read_json(self.send(request).await?).await
    }

    /// Whether a repository name is already used by this person.
    pub async fn repository_exists(
        &self,
        token: &Secret<String>,
        owner: &str,
        repository: &str,
    ) -> Result<bool, ForgejoError> {
        match self.repository(token, owner, repository).await {
            Ok(_) => Ok(true),
            Err(ForgejoError::Status { status: 404, .. }) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Delete a repository. Used by tests and by a failed creation that
    /// needs to leave nothing behind.
    pub async fn delete_repository(
        &self,
        token: &Secret<String>,
        owner: &str,
        repository: &str,
    ) -> Result<(), ForgejoError> {
        self.send(
            self.http
                .delete(format!(
                    "{}/api/v1/repos/{owner}/{repository}",
                    self.api_url
                ))
                .bearer_auth(token.expose()),
        )
        .await?;
        Ok(())
    }

    /// Find repositories that carry a topic.
    ///
    /// The topic is the opt-in marker, so a repository without it never
    /// appears here. Forgejo applies the permissions of the credential, and
    /// no credential means public repositories only. This is what makes
    /// Forgejo the authority on who sees what: the application asks this
    /// question again on every request instead of trusting its own index.
    ///
    /// The newest change comes first, which is the order that the recent
    /// sort shows.
    pub async fn search_repositories(
        &self,
        token: Option<&Secret<String>>,
        query: &RepositoryQuery<'_>,
    ) -> Result<Vec<Repository>, ForgejoError> {
        #[derive(Deserialize)]
        struct SearchResults {
            data: Vec<Repository>,
        }

        let mut parameters: Vec<(&str, String)> = vec![
            ("q", query.topic.to_string()),
            ("topic", "true".to_string()),
            ("sort", "updated".to_string()),
            ("order", "desc".to_string()),
            ("page", query.page.to_string()),
            ("limit", query.limit.to_string()),
        ];

        match query.ownership {
            Ownership::Anybody => {}
            Ownership::ReachableBy(id) => parameters.push(("uid", id.to_string())),
            Ownership::OwnedBy(id) => {
                parameters.push(("uid", id.to_string()));
                parameters.push(("exclusive", "true".to_string()));
            }
        }

        let mut request = self
            .http
            .get(format!("{}/api/v1/repos/search", self.api_url))
            .query(&parameters);
        if let Some(token) = token {
            request = request.bearer_auth(token.expose());
        }

        let results: SearchResults = read_json(self.send(request).await?).await?;
        Ok(results.data)
    }

    /// Whether the token belongs to a Forgejo administrator.
    ///
    /// Forgejo decides this, the same as every other permission question.
    pub async fn is_administrator(&self, token: &Secret<String>) -> Result<bool, ForgejoError> {
        #[derive(Deserialize)]
        struct AdminFlag {
            #[serde(default)]
            is_admin: bool,
        }

        let response = self
            .send(
                self.http
                    .get(format!("{}/api/v1/user", self.api_url))
                    .bearer_auth(token.expose()),
            )
            .await?;

        let flag: AdminFlag = read_json(response).await?;
        Ok(flag.is_admin)
    }

    /// Read one file from a repository at its published state.
    pub async fn raw_file(
        &self,
        token: Option<&Secret<String>>,
        owner: &str,
        repository: &str,
        reference: &str,
        path: &str,
    ) -> Result<Vec<u8>, ForgejoError> {
        let mut request = self.http.get(format!(
            "{}/api/v1/repos/{owner}/{repository}/raw/{path}",
            self.api_url
        ));
        request = request.query(&[("ref", reference)]);
        if let Some(token) = token {
            request = request.bearer_auth(token.expose());
        }

        let response = self.send(request).await?;

        response
            .bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|error| ForgejoError::Body(error.to_string()))
    }

    /// List the file names at the top of a repository.
    ///
    /// This is how the application learns which photo a Recipe carries,
    /// instead of guessing at a name. Forgejo applies the permissions of
    /// the token, so a private Recipe answers only somebody who may see it.
    pub async fn list_root_files(
        &self,
        token: Option<&Secret<String>>,
        owner: &str,
        repository: &str,
        reference: &str,
    ) -> Result<Vec<String>, ForgejoError> {
        #[derive(Deserialize)]
        struct Entry {
            #[serde(default)]
            name: String,
            #[serde(rename = "type", default)]
            kind: String,
        }

        let mut request = self
            .http
            .get(format!(
                "{}/api/v1/repos/{owner}/{repository}/contents",
                self.api_url
            ))
            .query(&[("ref", reference)]);
        if let Some(token) = token {
            request = request.bearer_auth(token.expose());
        }

        let response = self.send(request).await?;
        let entries: Vec<Entry> = read_json(response).await?;

        Ok(entries
            .into_iter()
            .filter(|entry| entry.kind == "file")
            .map(|entry| entry.name)
            .collect())
    }

    /// Read the account settings of the token holder.
    ///
    /// `/api/v1/user` gives the real address to the person it belongs to,
    /// whatever their privacy setting says. This endpoint is what tells the
    /// application whether that address may be written into History.
    pub async fn user_settings(
        &self,
        token: &Secret<String>,
    ) -> Result<UserSettings, ForgejoError> {
        let response = self
            .send(
                self.http
                    .get(format!("{}/api/v1/user/settings", self.api_url))
                    .bearer_auth(token.expose()),
            )
            .await?;

        read_json(response).await
    }

    /// List the OAuth2 applications of the token holder.
    pub async fn list_oauth_applications(
        &self,
        admin_token: &Secret<String>,
    ) -> Result<Vec<OAuthApplication>, ForgejoError> {
        let response = self
            .send(
                self.http
                    .get(format!("{}/api/v1/user/applications/oauth2", self.api_url))
                    .header("Authorization", format!("token {}", admin_token.expose())),
            )
            .await?;

        read_json(response).await
    }

    /// Create an OAuth2 application. The answer carries the client secret.
    pub async fn create_oauth_application(
        &self,
        admin_token: &Secret<String>,
        name: &str,
        redirect_uri: &str,
    ) -> Result<OAuthApplication, ForgejoError> {
        let response = self
            .send(
                self.http
                    .post(format!("{}/api/v1/user/applications/oauth2", self.api_url))
                    .header("Authorization", format!("token {}", admin_token.expose()))
                    .json(&serde_json::json!({
                        "name": name,
                        "redirect_uris": [redirect_uri],
                        "confidential_client": true,
                    })),
            )
            .await?;

        read_json(response).await
    }

    /// Update an OAuth2 application. Forgejo regenerates the client secret
    /// and returns it, which is how the bootstrap command recovers a secret
    /// that it does not hold.
    pub async fn update_oauth_application(
        &self,
        admin_token: &Secret<String>,
        app_id: i64,
        name: &str,
        redirect_uri: &str,
    ) -> Result<OAuthApplication, ForgejoError> {
        let response = self
            .send(
                self.http
                    .patch(format!(
                        "{}/api/v1/user/applications/oauth2/{app_id}",
                        self.api_url
                    ))
                    .header("Authorization", format!("token {}", admin_token.expose()))
                    .json(&serde_json::json!({
                        "name": name,
                        "redirect_uris": [redirect_uri],
                        "confidential_client": true,
                    })),
            )
            .await?;

        read_json(response).await
    }

    /// List the Discussions of a Recipe.
    ///
    /// Forgejo puts pull requests in the same list, so the request asks for
    /// issues only. A Suggestion is a pull request and belongs to the
    /// Suggestions area.
    ///
    /// The token can be absent, because a public Recipe is readable without
    /// a session. Forgejo applies the permissions of whoever asks, so the
    /// application computes no permission of its own.
    pub async fn list_issues(
        &self,
        token: Option<&Secret<String>>,
        owner: &str,
        repository: &str,
        limit: u32,
    ) -> Result<Vec<Issue>, ForgejoError> {
        let limit = limit.to_string();
        let mut request = self
            .http
            .get(format!(
                "{}/api/v1/repos/{owner}/{repository}/issues",
                self.api_url
            ))
            .query(&[
                ("state", "all"),
                ("type", "issues"),
                ("limit", limit.as_str()),
            ]);
        if let Some(token) = token {
            request = request.bearer_auth(token.expose());
        }

        let response = self.send(request).await?;
        read_json(response).await
    }

    /// Read one Discussion.
    pub async fn issue(
        &self,
        token: Option<&Secret<String>>,
        owner: &str,
        repository: &str,
        number: i64,
    ) -> Result<Issue, ForgejoError> {
        let mut request = self.http.get(format!(
            "{}/api/v1/repos/{owner}/{repository}/issues/{number}",
            self.api_url
        ));
        if let Some(token) = token {
            request = request.bearer_auth(token.expose());
        }

        let response = self.send(request).await?;
        read_json(response).await
    }

    /// Start a Discussion. Forgejo records it as an issue of the repository.
    pub async fn create_issue(
        &self,
        token: &Secret<String>,
        owner: &str,
        repository: &str,
        title: &str,
        body: &str,
    ) -> Result<Issue, ForgejoError> {
        let response = self
            .send(
                self.http
                    .post(format!(
                        "{}/api/v1/repos/{owner}/{repository}/issues",
                        self.api_url
                    ))
                    .bearer_auth(token.expose())
                    .json(&serde_json::json!({ "title": title, "body": body })),
            )
            .await?;

        read_json(response).await
    }

    /// Read one system webhook, or nothing when Forgejo does not have it.
    ///
    /// This is the reliable way to find the webhook again. Forgejo 15
    /// answers `GET /api/v1/admin/hooks` with an empty list even directly
    /// after it created one, while this endpoint answers correctly.
    pub async fn system_hook(
        &self,
        admin_token: &Secret<String>,
        hook_id: i64,
    ) -> Result<Option<SystemHook>, ForgejoError> {
        let response = self
            .send(
                self.http
                    .get(format!("{}/api/v1/admin/hooks/{hook_id}", self.api_url))
                    .header("Authorization", format!("token {}", admin_token.expose())),
            )
            .await;

        match response {
            Ok(response) => read_json(response).await.map(Some),
            Err(ForgejoError::Status { status: 404, .. }) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// List the system webhooks of this Forgejo instance.
    pub async fn list_system_hooks(
        &self,
        admin_token: &Secret<String>,
    ) -> Result<Vec<SystemHook>, ForgejoError> {
        let response = self
            .send(
                self.http
                    .get(format!("{}/api/v1/admin/hooks", self.api_url))
                    .query(&[("limit", "50")])
                    .header("Authorization", format!("token {}", admin_token.expose())),
            )
            .await?;

        read_json(response).await
    }

    /// List the comments of one Discussion.
    pub async fn list_issue_comments(
        &self,
        token: Option<&Secret<String>>,
        owner: &str,
        repository: &str,
        number: i64,
    ) -> Result<Vec<IssueComment>, ForgejoError> {
        let mut request = self.http.get(format!(
            "{}/api/v1/repos/{owner}/{repository}/issues/{number}/comments",
            self.api_url
        ));
        if let Some(token) = token {
            request = request.bearer_auth(token.expose());
        }

        let response = self.send(request).await?;
        read_json(response).await
    }

    /// Write a comment in a Discussion.
    pub async fn create_issue_comment(
        &self,
        token: &Secret<String>,
        owner: &str,
        repository: &str,
        number: i64,
        body: &str,
    ) -> Result<IssueComment, ForgejoError> {
        let response = self
            .send(
                self.http
                    .post(format!(
                        "{}/api/v1/repos/{owner}/{repository}/issues/{number}/comments",
                        self.api_url
                    ))
                    .bearer_auth(token.expose())
                    .json(&serde_json::json!({ "body": body })),
            )
            .await?;

        read_json(response).await
    }

    /// Close a Discussion, or open it again.
    ///
    /// Forgejo names the two states `open` and `closed`. The caller passes
    /// one of those two words and never a value that a person typed.
    pub async fn set_issue_state(
        &self,
        token: &Secret<String>,
        owner: &str,
        repository: &str,
        number: i64,
        state: &str,
    ) -> Result<Issue, ForgejoError> {
        let response = self
            .send(
                self.http
                    .patch(format!(
                        "{}/api/v1/repos/{owner}/{repository}/issues/{number}",
                        self.api_url
                    ))
                    .bearer_auth(token.expose())
                    .json(&serde_json::json!({ "state": state })),
            )
            .await?;

        read_json(response).await
    }

    /// Create one system webhook.
    ///
    /// The secret makes Forgejo sign every body, which is what lets this
    /// application refuse a message that Forgejo did not send.
    pub async fn create_system_hook(
        &self,
        admin_token: &Secret<String>,
        target_url: &str,
        secret: &Secret<String>,
        events: &[&str],
    ) -> Result<SystemHook, ForgejoError> {
        let response = self
            .send(
                self.http
                    .post(format!("{}/api/v1/admin/hooks", self.api_url))
                    .header("Authorization", format!("token {}", admin_token.expose()))
                    .json(&serde_json::json!({
                        "type": "forgejo",
                        "active": true,
                        "events": events,
                        "config": {
                            "url": target_url,
                            "content_type": "json",
                            "secret": secret.expose(),
                        },
                    })),
            )
            .await?;

        read_json(response).await
    }

    /// Whether the token holder may write to a repository.
    ///
    /// Forgejo decides this and this application only reads the answer.
    /// Forgejo reports the permissions it computed for the token holder on
    /// the repository itself, which covers the owner, an organization team,
    /// a collaborator, and an administrator in one value.
    pub async fn can_write(
        &self,
        token: &Secret<String>,
        owner: &str,
        repository: &str,
    ) -> Result<bool, ForgejoError> {
        #[derive(Deserialize)]
        struct WithPermissions {
            #[serde(default)]
            permissions: Permissions,
        }

        #[derive(Deserialize, Default)]
        struct Permissions {
            #[serde(default)]
            admin: bool,
            #[serde(default)]
            push: bool,
        }

        let response = self
            .send(
                self.http
                    .get(format!(
                        "{}/api/v1/repos/{owner}/{repository}",
                        self.api_url
                    ))
                    .bearer_auth(token.expose()),
            )
            .await?;

        let answer: WithPermissions = read_json(response).await?;
        Ok(answer.permissions.push || answer.permissions.admin)
    }

    /// Point an existing system webhook at this application again.
    pub async fn update_system_hook(
        &self,
        admin_token: &Secret<String>,
        hook_id: i64,
        target_url: &str,
        secret: &Secret<String>,
        events: &[&str],
    ) -> Result<SystemHook, ForgejoError> {
        let response = self
            .send(
                self.http
                    .patch(format!("{}/api/v1/admin/hooks/{hook_id}", self.api_url))
                    .header("Authorization", format!("token {}", admin_token.expose()))
                    .json(&serde_json::json!({
                        "active": true,
                        "events": events,
                        "config": {
                            "url": target_url,
                            "content_type": "json",
                            "secret": secret.expose(),
                        },
                    })),
            )
            .await?;

        read_json(response).await
    }

    /// Remove one system webhook.
    pub async fn delete_system_hook(
        &self,
        admin_token: &Secret<String>,
        hook_id: i64,
    ) -> Result<(), ForgejoError> {
        self.send(
            self.http
                .delete(format!("{}/api/v1/admin/hooks/{hook_id}", self.api_url))
                .header("Authorization", format!("token {}", admin_token.expose())),
        )
        .await?;
        Ok(())
    }

    /// Send a request and turn a non-success status into an error.
    ///
    /// The body of a failed answer goes into the error so that an
    /// administrator can act on it, with anything token-shaped removed.
    async fn send(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, ForgejoError> {
        let response = request
            .send()
            .await
            .map_err(|error| ForgejoError::Unreachable(strip_credentials(&error.to_string())))?;

        let status = response.status();
        if !status.is_success() {
            let body: String = response.text().await.unwrap_or_default();
            let short: String = body.chars().take(400).collect();
            return Err(ForgejoError::Status {
                status: status.as_u16(),
                body: strip_credentials(&short),
            });
        }

        Ok(response)
    }

    /// Change whether a repository is private.
    ///
    /// Forgejo owns visibility. This call moves a Recipe between Public and
    /// Private, and Forgejo then applies the new rule to the repository, to
    /// its Git history, and to every later request.
    pub async fn set_repository_private(
        &self,
        token: &Secret<String>,
        owner: &str,
        repository: &str,
        private: bool,
    ) -> Result<Repository, ForgejoError> {
        let response = self
            .send(
                self.http
                    .patch(format!(
                        "{}/api/v1/repos/{owner}/{repository}",
                        self.api_url
                    ))
                    .bearer_auth(token.expose())
                    .json(&serde_json::json!({ "private": private })),
            )
            .await?;

        read_json(response).await
    }

    /// Read one user by name, as this token holder can see them.
    ///
    /// Forgejo answers 404 for a person whose profile it hides from the
    /// asker. The application uses that answer as it is given, so the
    /// profile visibility setting of Forgejo stays in force.
    pub async fn user(
        &self,
        token: &Secret<String>,
        login: &str,
    ) -> Result<ForgejoUser, ForgejoError> {
        let response = self
            .send(
                self.http
                    .get(format!(
                        "{}/api/v1/users/{}",
                        self.api_url,
                        urlencode(login)
                    ))
                    .bearer_auth(token.expose()),
            )
            .await?;

        read_json(response).await
    }

    /// The people that Forgejo records on a repository.
    ///
    /// The answer carries no permission. Forgejo gives that one person at a
    /// time through [`Self::repository_permission`].
    pub async fn list_collaborators(
        &self,
        token: &Secret<String>,
        owner: &str,
        repository: &str,
    ) -> Result<Vec<ForgejoUser>, ForgejoError> {
        let response = self
            .send(
                self.http
                    .get(format!(
                        "{}/api/v1/repos/{owner}/{repository}/collaborators",
                        self.api_url
                    ))
                    .bearer_auth(token.expose()),
            )
            .await?;

        read_json(response).await
    }

    /// Ask Forgejo what one person may do with one repository.
    pub async fn repository_permission(
        &self,
        token: &Secret<String>,
        owner: &str,
        repository: &str,
        login: &str,
    ) -> Result<RepositoryPermission, ForgejoError> {
        let response = self
            .send(
                self.http
                    .get(format!(
                        "{}/api/v1/repos/{owner}/{repository}/collaborators/{}/permission",
                        self.api_url,
                        urlencode(login)
                    ))
                    .bearer_auth(token.expose()),
            )
            .await?;

        read_json(response).await
    }

    /// Give one person a permission on a repository.
    ///
    /// `permission` is a Forgejo access mode: `read`, `write`, or `admin`.
    /// Forgejo refuses the call when the token holder cannot administer the
    /// repository, so this is the check and not a second copy of it.
    pub async fn add_collaborator(
        &self,
        token: &Secret<String>,
        owner: &str,
        repository: &str,
        login: &str,
        permission: &str,
    ) -> Result<(), ForgejoError> {
        self.send(
            self.http
                .put(format!(
                    "{}/api/v1/repos/{owner}/{repository}/collaborators/{}",
                    self.api_url,
                    urlencode(login)
                ))
                .bearer_auth(token.expose())
                .json(&serde_json::json!({ "permission": permission })),
        )
        .await?;

        Ok(())
    }

    /// Take the permission of one person away.
    pub async fn remove_collaborator(
        &self,
        token: &Secret<String>,
        owner: &str,
        repository: &str,
        login: &str,
    ) -> Result<(), ForgejoError> {
        self.send(
            self.http
                .delete(format!(
                    "{}/api/v1/repos/{owner}/{repository}/collaborators/{}",
                    self.api_url,
                    urlencode(login)
                ))
                .bearer_auth(token.expose()),
        )
        .await?;

        Ok(())
    }

    /// List the Versions that a reference holds, newest first.
    ///
    /// `reference` says where the list starts. The published branch of a
    /// Recipe therefore gives the published Versions and nothing else: work
    /// that sits on another branch never appears in this answer.
    ///
    /// The token can be absent, because a public Recipe is readable without
    /// a session. Forgejo applies the permissions of whoever asks, so the
    /// application computes no permission of its own.
    pub async fn list_commits(
        &self,
        token: Option<&Secret<String>>,
        owner: &str,
        repository: &str,
        reference: &str,
        limit: u32,
    ) -> Result<Vec<Commit>, ForgejoError> {
        let limit = limit.to_string();
        let mut request = self
            .http
            .get(format!(
                "{}/api/v1/repos/{owner}/{repository}/commits",
                self.api_url
            ))
            .query(&[
                ("sha", reference),
                ("limit", limit.as_str()),
                // The page needs the author, the moment, and the
                // description. Asking for nothing else keeps the answer
                // small on a Recipe with a long History.
                ("stat", "false"),
                ("verification", "false"),
                ("files", "false"),
            ]);
        if let Some(token) = token {
            request = request.bearer_auth(token.expose());
        }

        match self.send(request).await {
            Ok(response) => read_json(response).await,
            // Forgejo answers 409 for a repository that holds nothing yet.
            // A Recipe without a Version is an answer, not a fault.
            Err(ForgejoError::Status { status: 409, .. }) => Ok(Vec::new()),
            Err(error) => Err(error),
        }
    }

    /// Read one Version by its identifier.
    pub async fn commit(
        &self,
        token: Option<&Secret<String>>,
        owner: &str,
        repository: &str,
        sha: &str,
    ) -> Result<Commit, ForgejoError> {
        let mut request = self
            .http
            .get(format!(
                "{}/api/v1/repos/{owner}/{repository}/git/commits/{}",
                self.api_url,
                urlencode(sha)
            ))
            .query(&[
                ("stat", "false"),
                ("verification", "false"),
                ("files", "false"),
            ]);
        if let Some(token) = token {
            request = request.bearer_auth(token.expose());
        }

        read_json(self.send(request).await?).await
    }

    /// Read one user by name, as the person who is looking sees them.
    ///
    /// `None` is an anonymous visitor, and Forgejo then answers as it
    /// answers anybody with no account. Forgejo answers 404 for a person
    /// whose profile it hides from the asker, so a limited profile answers
    /// 404 to a visitor and a private profile answers 404 to almost
    /// everybody. The application uses that answer as it is given, which is
    /// what keeps the profile visibility setting of Forgejo in force.
    ///
    /// [`Self::user`] is the same question asked with a credential that is
    /// always present.
    pub async fn user_as(
        &self,
        token: Option<&Secret<String>>,
        login: &str,
    ) -> Result<ForgejoUser, ForgejoError> {
        let mut request = self.http.get(format!(
            "{}/api/v1/users/{}",
            self.api_url,
            urlencode(login)
        ));
        if let Some(token) = token {
            request = request.bearer_auth(token.expose());
        }

        read_json(self.send(request).await?).await
    }

    /// List the repositories that the token holder made a Favorite.
    ///
    /// A Favorite is a Forgejo star, and Forgejo holds the list. The
    /// application keeps no copy, so a star that a person adds in Forgejo
    /// counts here at once.
    ///
    /// `page` counts from 1, the way Forgejo counts.
    pub async fn starred_repositories(
        &self,
        token: &Secret<String>,
        page: u32,
        limit: u32,
    ) -> Result<Vec<Repository>, ForgejoError> {
        let response = self
            .send(
                self.http
                    .get(format!("{}/api/v1/user/starred", self.api_url))
                    .query(&[("page", page.to_string()), ("limit", limit.to_string())])
                    .bearer_auth(token.expose()),
            )
            .await?;

        read_json(response).await
    }

    /// Make a Variation: a copy of a Recipe that belongs to the token holder.
    ///
    /// Forgejo does the whole of it. It copies the content and the History,
    /// it records that the new repository came from this one, and it gives
    /// the new repository the visibility that this one has. The application
    /// writes no marker of its own for any of that.
    ///
    /// `name` is the repository name to ask for. Forgejo answers 409 when
    /// the token holder cannot have it, so the caller offers another one.
    pub async fn fork_repository(
        &self,
        token: &Secret<String>,
        owner: &str,
        repository: &str,
        name: &str,
    ) -> Result<Repository, ForgejoError> {
        let response = self
            .send(
                self.http
                    .post(format!(
                        "{}/api/v1/repos/{owner}/{repository}/forks",
                        self.api_url
                    ))
                    .bearer_auth(token.expose())
                    .json(&serde_json::json!({ "name": name })),
            )
            .await?;

        read_json(response).await
    }

    /// Read what Forgejo records about where a repository came from.
    ///
    /// Forgejo is the authority on this, and it is the only store of it.
    /// When Forgejo stops recording a source, the repository has none.
    ///
    /// The token can be absent, because a public Recipe is readable without
    /// a session.
    pub async fn repository_lineage(
        &self,
        token: Option<&Secret<String>>,
        owner: &str,
        repository: &str,
    ) -> Result<Lineage, ForgejoError> {
        let mut request = self.http.get(format!(
            "{}/api/v1/repos/{owner}/{repository}",
            self.api_url
        ));
        if let Some(token) = token {
            request = request.bearer_auth(token.expose());
        }

        read_json(self.send(request).await?).await
    }

    /// List the repositories that were made from this one.
    ///
    /// Forgejo holds the list. The application keeps no copy, so a Variation
    /// that somebody makes in Forgejo appears here at once.
    pub async fn list_forks(
        &self,
        token: Option<&Secret<String>>,
        owner: &str,
        repository: &str,
        limit: u32,
    ) -> Result<Vec<Repository>, ForgejoError> {
        let mut request = self
            .http
            .get(format!(
                "{}/api/v1/repos/{owner}/{repository}/forks",
                self.api_url
            ))
            .query(&[("page", "1".to_string()), ("limit", limit.to_string())]);
        if let Some(token) = token {
            request = request.bearer_auth(token.expose());
        }

        read_json(self.send(request).await?).await
    }
}

/// Where a repository came from, as Forgejo records it.
///
/// A Variation is a Forgejo fork, and Forgejo holds that relationship on its
/// own. This application stores no lineage and writes none into Git, so this
/// answer is everything there is to know about the source of a Recipe.
///
/// Forgejo removes the relationship when the source repository is deleted.
/// A Variation of a deleted Recipe therefore reports no source at all, and
/// it stays a complete, usable Recipe.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Lineage {
    /// Whether Forgejo reports this repository as made from another one.
    #[serde(default)]
    pub fork: bool,
    /// The repository it was made from, when Forgejo still names one.
    #[serde(default)]
    pub parent: Option<Parent>,
}

/// The repository that another one was made from.
#[derive(Debug, Clone, Deserialize)]
pub struct Parent {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub name: String,
    /// `owner/name`, as Forgejo writes it.
    #[serde(default)]
    pub full_name: String,
    #[serde(default)]
    pub private: bool,
    #[serde(default)]
    pub owner: Option<RepositoryOwner>,
}

impl Parent {
    /// The login of the person the source belongs to.
    ///
    /// Forgejo reports it twice, and an answer can carry either one, so the
    /// owner comes first and the full name is read second.
    pub fn owner_login(&self) -> &str {
        if let Some(owner) = &self.owner
            && !owner.login.is_empty()
        {
            return &owner.login;
        }

        self.full_name
            .split_once('/')
            .map(|(owner, _)| owner)
            .unwrap_or_default()
    }

    /// The repository name of the source.
    pub fn repository_name(&self) -> &str {
        if !self.name.is_empty() {
            return &self.name;
        }

        self.full_name
            .split_once('/')
            .map(|(_, name)| name)
            .unwrap_or_default()
    }
}

async fn read_json<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T, ForgejoError> {
    let body = response
        .text()
        .await
        .map_err(|error| ForgejoError::Body(error.to_string()))?;

    serde_json::from_str(&body).map_err(|error| ForgejoError::Body(error.to_string()))
}

fn trim(url: String) -> String {
    url.trim_end_matches('/').to_string()
}

/// Remove anything that looks like a credential from a message that the
/// application will log or show.
///
/// Two shapes matter. A Forgejo personal access token carries a `gto_` or
/// `gt_` prefix. An OAuth2 access token is a JWT, which is three
/// dot-separated base64url parts and begins with `eyJ`. A refresh token has
/// the same shape as an access token.
///
/// Public so that a test can hold it against a credential that a real
/// Forgejo issued, rather than against a shape this file assumed.
pub fn strip_credentials(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for word in text.split_inclusive(char::is_whitespace) {
        if looks_like_credential(word.trim()) {
            out.push_str("[redacted] ");
        } else {
            out.push_str(word);
        }
    }
    out
}

fn looks_like_credential(word: &str) -> bool {
    if word.contains("gto_") || word.contains("gt_") {
        return true;
    }

    // A JWT: header.payload.signature, where the header begins `eyJ`.
    let parts: Vec<&str> = word.split('.').collect();
    parts.len() == 3 && parts[0].starts_with("eyJ") && parts.iter().all(|p| p.len() > 4)
}

/// Percent-encode a query value. Only the unreserved set of RFC 3986 stays.
fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_loses_a_trailing_slash() {
        let client = ForgejoClient::new("http://forgejo:3000/").unwrap();
        assert_eq!(client.api_url(), "http://forgejo:3000");
    }

    #[test]
    fn one_url_becomes_both_when_only_one_is_given() {
        let client = ForgejoClient::new("http://forgejo:3000").unwrap();
        assert_eq!(client.api_url(), client.public_url());
    }

    #[test]
    fn the_authorize_url_uses_the_public_address() {
        let client =
            ForgejoClient::with_urls("http://forgejo:3000", "https://forge.example").unwrap();
        let url = client.authorize_url("abc", "https://app.example/auth/callback", "st", "ch");

        assert!(url.starts_with("https://forge.example/login/oauth/authorize?"));
        assert!(
            !url.contains("forgejo:3000"),
            "a browser cannot resolve the internal name"
        );
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("response_type=code"));
        // The redirect address must survive as one query value.
        assert!(url.contains("redirect_uri=https%3A%2F%2Fapp.example%2Fauth%2Fcallback"));
    }

    #[test]
    fn the_git_address_comes_from_the_api_url() {
        // Forgejo reports a clone_url built from its ROOT_URL, which is the
        // address a browser uses. This process may not be able to reach
        // that address at all.
        let client =
            ForgejoClient::with_urls("http://forgejo:3000", "http://localhost:3000").unwrap();

        assert_eq!(
            client.git_url("sam/chili"),
            "http://forgejo:3000/sam/chili.git"
        );
        assert!(!client.git_url("sam/chili").contains("localhost"));
    }

    #[test]
    fn the_open_in_forgejo_address_comes_from_the_public_url() {
        let client =
            ForgejoClient::with_urls("http://forgejo:3000", "http://localhost:3000").unwrap();

        assert_eq!(
            client.web_url("sam/chili"),
            "http://localhost:3000/sam/chili"
        );
        assert!(!client.web_url("sam/chili").contains("forgejo:3000"));
    }

    #[test]
    fn display_name_falls_back_to_the_login() {
        let user = ForgejoUser {
            id: 1,
            login: "sam".to_string(),
            full_name: "  ".to_string(),
            avatar_url: String::new(),
            email: "sam@example.test".to_string(),
        };
        assert_eq!(user.display_name(), "sam");
    }

    #[test]
    fn a_personal_access_token_never_survives_into_an_error_message() {
        let dirty = "failed to call gto_abcdefghijklmnop while reading";
        let clean = strip_credentials(dirty);
        assert!(!clean.contains("gto_abcdefghijklmnop"));
        assert!(clean.contains("[redacted]"));
    }

    #[test]
    fn an_oauth_access_token_never_survives_into_an_error_message() {
        // Forgejo issues a JWT from the token endpoint, so a prefix match on
        // gto_ alone would let it through.
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
        let clean = strip_credentials(&format!("token {jwt} was refused"));

        assert!(!clean.contains(jwt), "the JWT survived: {clean}");
        assert!(clean.contains("[redacted]"));
        assert!(
            clean.contains("was refused"),
            "the message must stay useful"
        );
    }

    #[test]
    fn ordinary_words_are_left_alone() {
        let message = "cannot reach http://forgejo:3000/api/v1/user after 3 tries";
        assert_eq!(strip_credentials(message), message);
    }

    #[test]
    fn a_repository_says_whether_forgejo_offers_issues() {
        let with = r#"{"name":"chili","full_name":"sam/chili","html_url":"h","clone_url":"c","has_issues":true,"owner":{"login":"sam"}}"#;
        let without = r#"{"name":"chili","full_name":"sam/chili","html_url":"h","clone_url":"c","has_issues":false,"owner":{"login":"sam"}}"#;

        let on: Repository = serde_json::from_str(with).unwrap();
        let off: Repository = serde_json::from_str(without).unwrap();
        assert!(on.has_issues);
        assert!(!off.has_issues);

        // An answer that carries no such field counts as off, because the
        // application must never turn Issues on.
        let silent = r#"{"name":"chili","full_name":"sam/chili","html_url":"h","clone_url":"c","owner":{"login":"sam"}}"#;
        let quiet: Repository = serde_json::from_str(silent).unwrap();
        assert!(!quiet.has_issues);
    }

    #[test]
    fn a_pull_request_in_the_issue_list_is_not_a_discussion() {
        // Forgejo answers the issue endpoint with pull requests too. A
        // Suggestion is a pull request, so it belongs to another area.
        let discussion: Issue =
            serde_json::from_str(r#"{"number":1,"title":"How much salt?","state":"open"}"#)
                .unwrap();
        let suggestion: Issue = serde_json::from_str(
            r#"{"number":2,"title":"Less salt","state":"open","pull_request":{"merged":false}}"#,
        )
        .unwrap();

        assert!(discussion.is_discussion());
        assert!(discussion.is_open());
        assert!(!suggestion.is_discussion());
    }

    #[test]
    fn a_closed_discussion_reports_itself_as_closed() {
        let closed: Issue =
            serde_json::from_str(r#"{"number":1,"title":"Done","state":"closed"}"#).unwrap();
        assert!(!closed.is_open());
    }

    #[test]
    fn a_name_that_a_person_typed_stays_one_path_part() {
        // A login arrives from a form, so it can hold characters that would
        // otherwise make the request reach a different endpoint.
        assert_eq!(urlencode("sam"), "sam");
        assert_eq!(urlencode("sam.the-cook_1"), "sam.the-cook_1");
        assert_eq!(urlencode("../../admin/users"), "..%2F..%2Fadmin%2Fusers");
    }

    #[test]
    fn the_source_of_a_variation_is_read_whichever_way_forgejo_names_it() {
        // Forgejo names the owner twice. An answer that carries only the
        // full name must still say who the source belongs to, because the
        // page builds an address out of it.
        let full: Lineage = serde_json::from_str(
            r#"{"fork":true,"parent":{"id":2,"name":"chili","full_name":"sam/chili","private":false,"owner":{"id":1,"login":"sam"}}}"#,
        )
        .unwrap();
        let parent = full.parent.expect("the answer names a source");
        assert!(full.fork);
        assert_eq!(parent.owner_login(), "sam");
        assert_eq!(parent.repository_name(), "chili");

        let short: Lineage =
            serde_json::from_str(r#"{"fork":true,"parent":{"full_name":"sam/chili"}}"#).unwrap();
        let parent = short.parent.expect("the answer names a source");
        assert_eq!(parent.owner_login(), "sam");
        assert_eq!(parent.repository_name(), "chili");
    }

    #[test]
    fn a_recipe_that_forgejo_records_no_source_for_has_none() {
        // Forgejo forgets the relationship when the source is deleted. The
        // application must read that as "no source" and never invent one.
        let alone: Lineage =
            serde_json::from_str(r#"{"name":"chili","fork":false,"parent":null}"#).unwrap();
        assert!(!alone.fork);
        assert!(alone.parent.is_none());
    }

    #[test]
    fn read_access_is_told_apart_from_more_than_read() {
        let read = RepositoryPermission {
            permission: "read".to_string(),
            role_name: "read".to_string(),
        };
        let write = RepositoryPermission {
            permission: "write".to_string(),
            role_name: "write".to_string(),
        };

        assert!(read.is_read_only());
        assert!(!write.is_read_only());
    }
}
