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
    pub owner: RepositoryOwner,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RepositoryOwner {
    pub login: String,
}

/// Why a repository could not be created.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateRepositoryOutcome {
    /// A repository with that name already belongs to this person.
    NameTaken,
    /// Something else went wrong.
    Other,
}

/// The answer of the token endpoint.
#[derive(Debug, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<i64>,
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

        read_json(response).await
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

    /// Find the Recipe repositories of one person.
    ///
    /// The topic is the opt-in marker, so a repository without it never
    /// appears here. Forgejo applies the permissions of the token, so a
    /// private Recipe reaches only somebody who may see it.
    pub async fn search_repositories_by_topic(
        &self,
        token: &Secret<String>,
        topic: &str,
        owner_id: i64,
        limit: u32,
    ) -> Result<Vec<Repository>, ForgejoError> {
        #[derive(Deserialize)]
        struct SearchResults {
            data: Vec<Repository>,
        }

        let response = self
            .send(
                self.http
                    .get(format!("{}/api/v1/repos/search", self.api_url))
                    .bearer_auth(token.expose())
                    .query(&[
                        ("q", topic),
                        ("topic", "true"),
                        ("uid", &owner_id.to_string()),
                        ("exclusive", "true"),
                        ("sort", "updated"),
                        ("order", "desc"),
                        ("limit", &limit.to_string()),
                    ]),
            )
            .await?;

        let results: SearchResults = read_json(response).await?;
        Ok(results.data)
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
/// dot-separated base64url parts and begins with `eyJ`.
fn strip_credentials(text: &str) -> String {
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
