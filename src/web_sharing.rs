//! The Sharing area of a Recipe.
//!
//! This screen is the sharpest case of the rule that Forgejo is
//! authoritative. The application keeps no record of who can read a Recipe.
//! Every answer on this page comes from Forgejo while the page is built, and
//! every change is a call to Forgejo.
//!
//! The words on the screen are cooking words. Reader is Forgejo Read, Editor
//! is Forgejo Write, and Owner is the person the Recipe belongs to. Forgejo
//! Administrator and Forgejo Manager stay in Forgejo: a person who holds one
//! is listed, because the Owner must see who has access, but this screen
//! does not hand that role out.
//!
//! Every action is a form that posts. No action needs a script.

use std::sync::Arc;

use askama::Template;
use axum::Form;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum_extra::extract::CookieJar;
use serde::Deserialize;

use crate::auth::OAUTH_APPLICATION_NAME;
use crate::forgejo::{ForgejoError, ForgejoUser, Repository};
use crate::recipe::{self, RECIPE_FILE};
use crate::secret::Secret;
use crate::session::CurrentUser;

use crate::web::{AppState, Layout, MaybeUser};
use crate::web_recipes::{RecipeArea, areas};

/// The Forgejo access mode that a Reader gets.
const FORGEJO_READ: &str = "read";
/// The Forgejo access mode that an Editor gets.
const FORGEJO_WRITE: &str = "write";

/// The name for an access mode that this screen does not hand out.
const UNMANAGED_ROLE: &str = "Set in Forgejo";

/// What the confirmation must say before a Recipe becomes Public.
///
/// The second half matters as much as the first. A Recipe that becomes
/// public publishes its Git history too, and a person must know that before
/// it happens and not after.
pub const PUBLIC_WARNING: &str = "All users can read this Recipe and its earlier Versions. An earlier Version holds every word that this Recipe held before, and it stays readable.";

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/recipes/{owner}/{slug}/sharing", get(show))
        .route(
            "/recipes/{owner}/{slug}/sharing/public",
            get(confirm_public),
        )
        .route(
            "/recipes/{owner}/{slug}/sharing/visibility",
            post(set_visibility),
        )
        .route("/recipes/{owner}/{slug}/sharing/people", post(add_person))
        .route(
            "/recipes/{owner}/{slug}/sharing/people/remove",
            post(remove_person),
        )
}

/// Whether a Forgejo login belongs to the application itself.
///
/// The platform can hold a read-only identity for reconciliation and an
/// automation identity for Following. They are ordinary Forgejo users and
/// they stay visible in Forgejo, but a person who shares a Recipe did not
/// put them there, so this screen leaves them out.
///
/// The application names its own identities after itself, which is the name
/// that the bootstrap command registers in Forgejo.
pub fn is_service_identity(login: &str) -> bool {
    let login = login.trim().to_ascii_lowercase();
    let name = OAUTH_APPLICATION_NAME.to_ascii_lowercase();

    login == name || login.starts_with(&format!("{name}-"))
}

/// The role that this screen shows for a Forgejo access mode.
fn role_label(permission: &str) -> &'static str {
    match permission {
        FORGEJO_READ => "Reader",
        FORGEJO_WRITE => "Editor",
        _ => UNMANAGED_ROLE,
    }
}

/// Whether this screen can change an access mode.
///
/// Forgejo Administrator and Forgejo Manager are real access that the Owner
/// must see. They are not roles this screen gives or takes, so a person who
/// holds one is listed with **Open in Forgejo** and nothing else.
fn is_manageable(permission: &str) -> bool {
    matches!(permission, FORGEJO_READ | FORGEJO_WRITE)
}

/// One person who can reach a Recipe.
pub struct Person {
    pub login: String,
    pub name: String,
    pub role: &'static str,
    /// Whether this screen gives the Owner a control for this person.
    pub managed: bool,
}

/// A signed-in person plus the credential to act as them in Forgejo.
struct Actor {
    user: ForgejoUser,
    token: Secret<String>,
}

/// Read the session and fetch the Forgejo identity behind it.
async fn actor(state: &AppState, jar: &CookieJar) -> Option<Actor> {
    let token = crate::web::viewer_token(state, jar).await?;
    let user = state.forgejo.current_user(&token).await.ok()?;
    Some(Actor { user, token })
}

#[derive(Template)]
#[template(path = "recipe_sharing.html")]
struct SharingTemplate {
    layout: Layout,
    owner: String,
    slug: String,
    title: String,
    forgejo_url: String,
    areas: Vec<RecipeArea>,
    /// Whether Forgejo says the person who asks owns this Recipe.
    is_owner: bool,
    /// Whether Forgejo says that all users can read this Recipe.
    public: bool,
    /// The one address that a person shares. There is no second one.
    recipe_url: String,
    /// Where each form posts to, and where a cancel returns to.
    sharing_path: String,
    people: Vec<Person>,
    /// The public Cookbooks that hold this Recipe.
    ///
    /// A Recipe that stops being public makes each of them partly
    /// unavailable, so the Owner reads the list before they decide.
    affected: Vec<crate::cookbook::Named>,
    /// Show the Private to Public confirmation instead of the controls.
    confirming: bool,
    /// What the confirmation says, kept in one place.
    public_warning: &'static str,
    errors: Vec<String>,
}

/// What Forgejo says about one Recipe and the person who asks about it.
struct Context {
    actor: Actor,
    repository: Repository,
    is_owner: bool,
}

/// Why a page or an action cannot go on.
enum Stop {
    /// Nobody is signed in.
    SignIn,
    /// Forgejo does not show this Recipe to this person.
    Unknown,
    /// Forgejo says this person does not own the Recipe.
    NotOwner(Box<Context>),
}

/// One request for the Sharing area.
///
/// Each handler makes one of these, asks it for the Forgejo answers, and
/// then asks it to draw a page. Nothing is remembered between requests.
struct Screen<'a> {
    state: &'a AppState,
    headers: &'a HeaderMap,
    current: Option<&'a CurrentUser>,
    owner: &'a str,
    slug: &'a str,
}

impl Screen<'_> {
    fn sharing_path(&self) -> String {
        format!("/recipes/{}/{}/sharing", self.owner, self.slug)
    }

    /// Read the Recipe and ask Forgejo who this person is to it.
    ///
    /// This runs before a page is drawn and again before any form acts,
    /// because a check that happens only in the interface is not a check.
    async fn context(&self, jar: &CookieJar) -> Result<Context, Stop> {
        let Some(actor) = actor(self.state, jar).await else {
            return Err(Stop::SignIn);
        };

        // Forgejo applies its own permissions here, so a Recipe that this
        // person may not see never reaches the next line.
        let repository = match self
            .state
            .forgejo
            .repository(&actor.token, self.owner, self.slug)
            .await
        {
            Ok(repository) => repository,
            Err(error) => {
                tracing::info!(%error, owner = %self.owner, slug = %self.slug, "cannot read the Recipe repository");
                return Err(Stop::Unknown);
            }
        };

        // Two answers, and Forgejo gives both: who the Recipe belongs to,
        // and what this person may do with it.
        let permission = self
            .state
            .forgejo
            .repository_permission(&actor.token, self.owner, self.slug, &actor.user.login)
            .await;

        let holds_the_keys = match &permission {
            Ok(permission) => matches!(permission.permission.as_str(), "owner" | "admin"),
            Err(error) => {
                tracing::info!(%error, owner = %self.owner, slug = %self.slug, "Forgejo gave no permission for this person");
                false
            }
        };

        // A Forgejo Administrator holds the keys to a Recipe that is not
        // theirs. They are not the Owner, and this screen belongs to the
        // Owner, so they go to Forgejo instead.
        let is_owner = repository.owner.login == actor.user.login && holds_the_keys;

        let context = Context {
            actor,
            repository,
            is_owner,
        };

        if context.is_owner {
            Ok(context)
        } else {
            Err(Stop::NotOwner(Box::new(context)))
        }
    }

    /// Turn a stop into the answer that a person gets.
    async fn refuse(&self, stop: Stop) -> Response {
        match stop {
            Stop::SignIn => Redirect::to("/auth/sign-in").into_response(),
            Stop::Unknown => {
                (StatusCode::NOT_FOUND, "This Recipe is not available.").into_response()
            }
            Stop::NotOwner(context) => {
                // This person can read the Recipe, so the page tells them who
                // owns it and offers Forgejo. It carries no control.
                let page = self.draw(&context, Vec::new(), false).await;
                (StatusCode::FORBIDDEN, page).into_response()
            }
        }
    }

    /// Draw the page from what Forgejo says right now.
    async fn draw(&self, context: &Context, errors: Vec<String>, confirming: bool) -> Response {
        let public = !context.repository.private;

        let people = if context.is_owner {
            self.people(context, public).await
        } else {
            Vec::new()
        };

        // Public to Private is allowed, and it is not free. A public
        // Cookbook that holds this Recipe becomes partly unavailable, so
        // the Owner reads which ones before they press the button. Git
        // holds the answer and every Cookbook is read again for it.
        let affected = if context.is_owner && public {
            crate::cookbook::public_cookbooks_with(
                &self.state.pool,
                &self.state.forgejo,
                &context.actor.token,
                self.owner,
                self.slug,
            )
            .await
        } else {
            Vec::new()
        };

        respond(SharingTemplate {
            layout: Layout::new(self.current).on(self.headers, &self.sharing_path()),
            owner: self.owner.to_string(),
            slug: self.slug.to_string(),
            title: self.title(context).await,
            forgejo_url: self.state.forgejo.web_url(&context.repository.full_name),
            areas: areas(self.owner, self.slug, &context.repository),
            is_owner: context.is_owner,
            public,
            recipe_url: self.recipe_url().await,
            sharing_path: self.sharing_path(),
            people,
            affected,
            confirming,
            public_warning: PUBLIC_WARNING,
            errors,
        })
    }

    /// Draw the page again with one message on it.
    async fn draw_error(&self, context: &Context, message: String) -> Response {
        self.draw(context, vec![message], false).await
    }

    /// Send the person back to the page they acted on.
    fn done(&self) -> Response {
        Redirect::to(&self.sharing_path()).into_response()
    }

    /// The title that a cook gave the Recipe.
    ///
    /// It lives in the Cooklang source and never in the address, so this
    /// heading matches the heading on the Recipe page. A Recipe that cannot
    /// be read falls back to the last part of its address.
    async fn title(&self, context: &Context) -> String {
        let branch = if context.repository.default_branch.is_empty() {
            crate::create_recipe::MAIN_BRANCH.to_string()
        } else {
            context.repository.default_branch.clone()
        };

        self.state
            .forgejo
            .raw_file(
                Some(&context.actor.token),
                self.owner,
                self.slug,
                &branch,
                RECIPE_FILE,
            )
            .await
            .ok()
            .and_then(|bytes| recipe::parse(&String::from_utf8_lossy(&bytes)).title)
            .unwrap_or_else(|| context.repository.name.clone())
    }

    /// The one address that Share copies.
    ///
    /// The bootstrap command stored the address that Forgejo returns a
    /// person to after a sign-in. That address is the public address of this
    /// installation, so the Recipe address is built from it. A path shows
    /// when the application holds no such address, which is still the
    /// address a person needs, without its host.
    async fn recipe_url(&self) -> String {
        let path = format!("/recipes/{}/{}", self.owner, self.slug);

        let base = crate::auth::load_client(&self.state.pool, &self.state.cipher)
            .await
            .ok()
            .flatten()
            .and_then(|client| {
                client
                    .redirect_uri
                    .strip_suffix("/auth/callback")
                    .map(str::to_string)
            });

        match base {
            Some(base) => format!("{base}{path}"),
            None => path,
        }
    }

    /// Who can reach this Recipe, as Forgejo records it now.
    ///
    /// A private Recipe lists its Readers, because their access is explicit.
    /// A public Recipe lists only the people with more than read access:
    /// everybody is already a reader there, so a list of readers says
    /// nothing.
    async fn people(&self, context: &Context, public: bool) -> Vec<Person> {
        let collaborators = match self
            .state
            .forgejo
            .list_collaborators(&context.actor.token, self.owner, self.slug)
            .await
        {
            Ok(collaborators) => collaborators,
            Err(error) => {
                tracing::warn!(%error, owner = %self.owner, slug = %self.slug, "cannot read who shares this Recipe");
                return Vec::new();
            }
        };

        let wanted: Vec<ForgejoUser> = collaborators
            .into_iter()
            .filter(|user| !is_service_identity(&user.login))
            .collect();

        // Forgejo answers about one person at a time, so ask about all of
        // them together instead of one after the other.
        let permissions = futures::future::join_all(wanted.iter().map(|user| {
            let forgejo = self.state.forgejo.clone();
            let token = context.actor.token.clone();
            let login = user.login.clone();
            let owner = self.owner.to_string();
            let slug = self.slug.to_string();

            async move {
                forgejo
                    .repository_permission(&token, &owner, &slug, &login)
                    .await
            }
        }))
        .await;

        let mut people: Vec<Person> = wanted
            .into_iter()
            .zip(permissions)
            .filter_map(|(user, permission)| {
                let permission = permission.ok()?;

                // On a public Recipe a Reader is nobody special.
                if public && permission.is_read_only() {
                    return None;
                }

                Some(Person {
                    name: user.display_name().to_string(),
                    login: user.login,
                    role: role_label(&permission.permission),
                    managed: is_manageable(&permission.permission),
                })
            })
            .collect();

        people.sort_by_key(|person| person.login.to_lowercase());
        people
    }
}

async fn show(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug)): Path<(String, String)>,
) -> Response {
    let screen = Screen {
        state: &state,
        headers: &headers,
        current: current.as_ref(),
        owner: &owner,
        slug: &slug,
    };

    match screen.context(&jar).await {
        Ok(context) => screen.draw(&context, Vec::new(), false).await,
        Err(stop) => screen.refuse(stop).await,
    }
}

/// The step between Private and Public.
///
/// This is a page and not a dialog, because the wording is part of the
/// decision and it must reach a person who runs no scripts.
async fn confirm_public(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug)): Path<(String, String)>,
) -> Response {
    let screen = Screen {
        state: &state,
        headers: &headers,
        current: current.as_ref(),
        owner: &owner,
        slug: &slug,
    };

    match screen.context(&jar).await {
        Ok(context) => screen.draw(&context, Vec::new(), true).await,
        Err(stop) => screen.refuse(stop).await,
    }
}

/// What the visibility form sends.
#[derive(Debug, Deserialize)]
struct VisibilityForm {
    visibility: String,
    /// The confirmation. Public needs it, and the server needs it, not only
    /// the page that asked for it.
    #[serde(default)]
    confirm: String,
}

async fn set_visibility(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug)): Path<(String, String)>,
    Form(form): Form<VisibilityForm>,
) -> Response {
    let screen = Screen {
        state: &state,
        headers: &headers,
        current: current.as_ref(),
        owner: &owner,
        slug: &slug,
    };

    let context = match screen.context(&jar).await {
        Ok(context) => context,
        Err(stop) => return screen.refuse(stop).await,
    };

    let private = match form.visibility.as_str() {
        "private" => true,
        "public" => {
            // A Recipe becomes public only after the person confirms. The
            // check lives here, so a post that skips the page changes
            // nothing.
            if form.confirm != "yes" {
                return screen.draw(&context, Vec::new(), true).await;
            }
            false
        }
        other => {
            tracing::info!(%other, "a visibility that the application does not know");
            return screen
                .draw_error(&context, "Select Public or Private.".to_string())
                .await;
        }
    };

    match state
        .forgejo
        .set_repository_private(&context.actor.token, &owner, &slug, private)
        .await
    {
        // Forgejo can answer 200 and change nothing: it refuses to make a
        // copy private while the Recipe it came from is public, and it says
        // so only by giving the repository back unchanged. Reporting that as
        // done would tell a person their Recipe is hidden when it is not.
        Ok(answer) if answer.private == private => {
            tracing::info!(%owner, %slug, private, "the visibility of a Recipe changed");
            screen.done()
        }
        Ok(_) => {
            tracing::warn!(%owner, %slug, private, "Forgejo kept the visibility as it was");
            screen
                .draw_error(
                    &context,
                    "Forgejo kept this Recipe as it was. Forgejo holds a copy at the \
                     same visibility as the Recipe it came from, and it \
                     changes neither one on its own. Open the Recipe in \
                     Forgejo to see its state."
                        .to_string(),
                )
                .await
        }
        Err(error) => {
            tracing::warn!(%error, %owner, %slug, "cannot change the visibility");
            screen
                .draw_error(
                    &context,
                    format!(
                        "Forgejo did not change who can read this Recipe: {}. Open the Recipe in Forgejo to see its state.",
                        short(&error)
                    ),
                )
                .await
        }
    }
}

/// What the add form sends.
#[derive(Debug, Deserialize)]
struct PersonForm {
    login: String,
    role: String,
}

async fn add_person(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug)): Path<(String, String)>,
    Form(form): Form<PersonForm>,
) -> Response {
    let screen = Screen {
        state: &state,
        headers: &headers,
        current: current.as_ref(),
        owner: &owner,
        slug: &slug,
    };

    let context = match screen.context(&jar).await {
        Ok(context) => context,
        Err(stop) => return screen.refuse(stop).await,
    };

    let typed = form.login.trim().to_string();

    let permission = match form.role.as_str() {
        "reader" => FORGEJO_READ,
        "editor" => FORGEJO_WRITE,
        _ => "",
    };

    let refused: Option<String> = if typed.is_empty() {
        Some("Type the name of the person in Forgejo.".to_string())
    } else if permission.is_empty() {
        Some("Select Reader or Editor.".to_string())
    } else if is_service_identity(&typed) {
        // Diagnose it and send them to Forgejo. The application does not
        // change what its own identities can do from a Recipe screen.
        Some(format!(
            "The name `{typed}` belongs to {OAUTH_APPLICATION_NAME}. Open the Recipe in Forgejo to change what it can do."
        ))
    } else if typed.eq_ignore_ascii_case(&context.repository.owner.login) {
        Some(format!("{typed} owns this Recipe already."))
    } else {
        None
    };

    if let Some(message) = refused {
        return screen.draw_error(&context, message).await;
    }

    // Ask Forgejo about the person first. Forgejo hides a profile that its
    // visibility setting keeps from this person, and it answers 404 for one.
    // The application uses that answer as it is given, so it cannot reach
    // past the setting.
    let found = match state.forgejo.user(&context.actor.token, &typed).await {
        Ok(user) => user,
        Err(ForgejoError::Status { status: 404, .. }) => {
            return screen
                .draw_error(
                    &context,
                    format!("Forgejo shows no user with the name `{typed}`."),
                )
                .await;
        }
        Err(error) => {
            tracing::warn!(%error, "cannot read the user that a person named");
            return screen
                .draw_error(
                    &context,
                    format!(
                        "Forgejo did not answer about `{typed}`: {}. Open the Recipe in Forgejo to add the person there.",
                        short(&error)
                    ),
                )
                .await;
        }
    };

    match state
        .forgejo
        .add_collaborator(
            &context.actor.token,
            &owner,
            &slug,
            &found.login,
            permission,
        )
        .await
    {
        Ok(()) => {
            tracing::info!(%owner, %slug, login = %found.login, permission, "a person can reach a Recipe");
            screen.done()
        }
        Err(error) => {
            tracing::warn!(%error, %owner, %slug, "cannot share the Recipe with a person");
            screen
                .draw_error(
                    &context,
                    format!(
                        "Forgejo did not give `{}` access: {}. Open the Recipe in Forgejo to add the person there.",
                        found.login,
                        short(&error)
                    ),
                )
                .await
        }
    }
}

/// What the remove form sends.
#[derive(Debug, Deserialize)]
struct RemoveForm {
    login: String,
}

async fn remove_person(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug)): Path<(String, String)>,
    Form(form): Form<RemoveForm>,
) -> Response {
    let screen = Screen {
        state: &state,
        headers: &headers,
        current: current.as_ref(),
        owner: &owner,
        slug: &slug,
    };

    let context = match screen.context(&jar).await {
        Ok(context) => context,
        Err(stop) => return screen.refuse(stop).await,
    };

    let login = form.login.trim().to_string();
    if login.is_empty() {
        return screen
            .draw_error(&context, "Select the person to remove.".to_string())
            .await;
    }

    match state
        .forgejo
        .remove_collaborator(&context.actor.token, &owner, &slug, &login)
        .await
    {
        Ok(()) => {
            tracing::info!(%owner, %slug, %login, "a person cannot reach a Recipe any more");
            screen.done()
        }
        Err(error) => {
            tracing::warn!(%error, %owner, %slug, "cannot remove a person from a Recipe");
            screen
                .draw_error(
                    &context,
                    format!(
                        "Forgejo did not remove `{login}`: {}. Open the Recipe in Forgejo to remove the person there.",
                        short(&error)
                    ),
                )
                .await
        }
    }
}

/// A short sentence about a Forgejo failure, for a person to read.
///
/// The whole body of a Forgejo answer belongs in the log and not on a page.
fn short(error: &ForgejoError) -> String {
    match error {
        ForgejoError::Status { status, .. } => format!("it answered {status}"),
        other => other.to_string(),
    }
}

fn respond<T: Template>(template: T) -> Response {
    match template.render() {
        Ok(body) => Html(body).into_response(),
        Err(error) => {
            tracing::error!(%error, "cannot render a template");
            (StatusCode::INTERNAL_SERVER_ERROR, "template error").into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_identities_of_the_application_are_recognised() {
        assert!(is_service_identity("cooklanghub"));
        assert!(is_service_identity("CookLangHub"));
        assert!(is_service_identity("cooklanghub-automation"));
        assert!(is_service_identity("CookLangHub-Index"));
        assert!(is_service_identity("  cooklanghub-automation  "));
    }

    #[test]
    fn a_person_is_never_taken_for_an_identity_of_the_application() {
        for login in ["sam", "cook", "cooklanghubfan", "hub", "sam-cooklanghub"] {
            assert!(
                !is_service_identity(login),
                "`{login}` is a person and must show"
            );
        }
    }

    #[test]
    fn reader_is_forgejo_read_and_editor_is_forgejo_write() {
        assert_eq!(role_label(FORGEJO_READ), "Reader");
        assert_eq!(role_label(FORGEJO_WRITE), "Editor");
        assert!(is_manageable(FORGEJO_READ));
        assert!(is_manageable(FORGEJO_WRITE));
    }

    #[test]
    fn a_forgejo_role_that_this_screen_does_not_hand_out_says_so() {
        // Administrator and Manager stay in Forgejo. A person who holds one
        // is still listed, because the Owner must see who has access.
        assert_eq!(role_label("admin"), UNMANAGED_ROLE);
        assert_eq!(role_label("owner"), UNMANAGED_ROLE);
        assert!(!is_manageable("admin"));
        assert!(!is_manageable("owner"));
    }

    #[test]
    fn the_confirmation_names_the_recipe_and_its_earlier_versions() {
        assert!(PUBLIC_WARNING.contains("All users can read"));
        assert!(PUBLIC_WARNING.contains("earlier Versions"));
    }

    #[test]
    fn a_forgejo_failure_becomes_one_short_sentence() {
        let error = ForgejoError::Status {
            status: 403,
            body: "a long body that a person does not need".to_string(),
        };

        let message = short(&error);
        assert_eq!(message, "it answered 403");
        assert!(!message.contains("long body"));
    }
}
