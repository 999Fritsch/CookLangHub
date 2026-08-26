//! Editing a Recipe and publishing the result as one new Version.
//!
//! The person edits the Cooklang source itself. The application never
//! reformats what they wrote: the bytes that reach Git are the bytes that
//! came out of the editor, and only the change that the person made is
//! different.
//!
//! Three systems meet here, and each keeps what belongs to it. Forgejo says
//! who may write. The parser says whether the source can be published: an
//! error stops it, a warning does not. Git holds the content and the
//! History, and Git alone decides whether a change can join a `main` that
//! moved while the person worked.
//!
//! When Git cannot join them, the published Recipe is left exactly as it
//! was and the person gets a diagnosis plus **Open in Forgejo**. The
//! application never repairs that state on its own.

use std::sync::Arc;

use askama::Template;
use axum::Form;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum_extra::extract::CookieJar;
use serde::Deserialize;

use crate::create_recipe::{self, MAIN_BRANCH};
use crate::forgejo::ForgejoUser;
use crate::git::{GitError, Identity, PublishVersion};
use crate::recipe::{self, MAX_SOURCE_BYTES, RECIPE_FILE};
use crate::render::{self, RenderedRecipe};
use crate::secret::Secret;
use crate::session::COOKIE_NAME;

use crate::web::{AppState, Layout, MaybeUser};

/// The longest change note that becomes a Version description.
const MAX_NOTE_CHARS: usize = 200;

/// Shown when the person may read the Recipe but may not write to it.
pub(crate) const NO_WRITE_MESSAGE: &str = "You can read this Recipe, but you cannot change it. Ask the owner to share it with you as an Editor.";

/// Shown when the stored file is not text that the application can read.
const NOT_TEXT_MESSAGE: &str = "This Recipe is not UTF-8 text, so the editor cannot open it. Open the Recipe in Forgejo to see the exact content.";

/// Shown when the Recipe holds no Version yet.
const NO_VERSION_MESSAGE: &str =
    "This Recipe has no Version yet, so there is nothing to edit. Open the Recipe in Forgejo.";

/// Shown when Git does not answer.
const UNREACHABLE_MESSAGE: &str =
    "CookLangHub cannot read this Recipe at the moment. Nothing changed. Try again.";

/// Shown when Git cannot join the change with the published Recipe.
const CONFLICT_MESSAGE: &str = "Somebody else published a change to the same lines while you wrote. CookLangHub did not change the published Recipe. Copy your text, open the Recipe again, and add your change to the current Version. Open in Forgejo to see what changed.";

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/recipes/preview", post(preview))
        .route("/recipes/{owner}/{slug}/edit", get(edit_form).post(publish))
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
#[template(path = "recipe_preview.html")]
struct PreviewTemplate {
    preview_title: String,
    cooked: RenderedRecipe,
    warnings: Vec<String>,
    parse_errors: Vec<String>,
}

impl PreviewTemplate {
    /// Read a Cooklang source and build the preview of it.
    fn of(source: &str, fallback_title: &str) -> Self {
        let parsed = recipe::parse(source);

        Self {
            preview_title: parsed
                .title
                .clone()
                .unwrap_or_else(|| fallback_title.to_string()),
            // A source the parser refused cannot be shown as a Recipe, so
            // the preview shows the messages and nothing else.
            cooked: recipe::parse_recipe(source)
                .as_ref()
                .map(render::render)
                .unwrap_or_default(),
            warnings: parsed.warnings.iter().map(|d| d.message.clone()).collect(),
            parse_errors: parsed.errors.iter().map(|d| d.message.clone()).collect(),
        }
    }
}

#[derive(Template)]
#[template(path = "recipe_edit.html")]
struct EditTemplate {
    layout: Layout,
    owner: String,
    slug: String,
    recipe_title: String,
    /// The Cooklang exactly as it is stored.
    source: String,
    /// The Version the person started from.
    base_version: String,
    /// The draft Version the page carries, or empty when there is none.
    /// The editor sends it back with each save, and that is what lets the
    /// application refuse a save from a tab that has fallen behind.
    draft_version: String,
    /// What the person reads about their draft. `None` when they have none.
    draft_notice: Option<&'static str>,
    note: String,
    forgejo_url: String,
    /// Why a publication did not happen. Empty on the way in.
    publish_errors: Vec<String>,
    // The preview fields. `recipe_preview.html` is included here and reads
    // them, so the editor and the preview route render the same fragment
    // from the same parser.
    preview_title: String,
    cooked: RenderedRecipe,
    warnings: Vec<String>,
    parse_errors: Vec<String>,
}

impl EditTemplate {
    #[allow(clippy::too_many_arguments)]
    fn new(
        layout: Layout,
        owner: &str,
        slug: &str,
        source: String,
        base_version: String,
        draft_version: String,
        note: String,
        forgejo_url: String,
        publish_errors: Vec<String>,
    ) -> Self {
        let preview = PreviewTemplate::of(&source, slug);
        // A page carries a draft exactly when it carries a draft Version,
        // so the two can never disagree.
        let draft_notice = (!draft_version.is_empty()).then_some(crate::draft::NOTICE_MESSAGE);

        Self {
            layout,
            owner: owner.to_string(),
            slug: slug.to_string(),
            recipe_title: preview.preview_title.clone(),
            source,
            base_version,
            draft_version,
            draft_notice,
            note,
            forgejo_url,
            publish_errors,
            preview_title: preview.preview_title,
            cooked: preview.cooked,
            warnings: preview.warnings,
            parse_errors: preview.parse_errors,
        }
    }
}

/// What the editor sends when it asks for a preview.
#[derive(Debug, Deserialize)]
struct PreviewForm {
    #[serde(default)]
    source: String,
}

/// Render a Cooklang source and give back the fragment for the preview.
///
/// This route exists so that the preview and the Recipe page come out of
/// one parser. A second parser in the browser would drift from the Rust one
/// and would show a person something that publishing then refuses.
async fn preview(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
    Form(form): Form<PreviewForm>,
) -> Response {
    let _ = &state;

    // The route renders text that somebody typed. A session keeps it out of
    // reach of anybody who is not signed in.
    if jar.get(COOKIE_NAME).is_none() {
        return (StatusCode::UNAUTHORIZED, "Sign in to use the editor.").into_response();
    }

    if form.source.len() > MAX_SOURCE_BYTES {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            "The Recipe source is larger than 1 MB.",
        )
            .into_response();
    }

    // Every value in the fragment is escaped by the template, so a Recipe
    // that holds markup shows those characters and cannot run.
    respond(PreviewTemplate::of(&normalize(&form.source), "Recipe"))
}

/// Everything the editor needs about the Recipe it is about to open.
struct Target {
    branch: String,
    forgejo_url: String,
}

/// Why the editor cannot open a Recipe.
pub(crate) enum Refused {
    /// There is no such Recipe, or this person may not see it.
    Missing,
    /// The Recipe is there, and the interface cannot handle its state.
    Blocked {
        status: StatusCode,
        message: &'static str,
        forgejo_url: String,
    },
}

#[derive(Template)]
#[template(path = "recipe_blocked.html")]
struct BlockedTemplate {
    layout: Layout,
    owner: String,
    slug: String,
    message: String,
    forgejo_url: String,
}

/// Turn a refusal into the page that the person reads.
///
/// A state the interface cannot handle is diagnosed and offers **Open in
/// Forgejo**. It is never repaired here.
pub(crate) fn refusal(layout: Layout, owner: &str, slug: &str, refused: Refused) -> Response {
    match refused {
        Refused::Missing => {
            (StatusCode::NOT_FOUND, "This Recipe is not available.").into_response()
        }
        Refused::Blocked {
            status,
            message,
            forgejo_url,
        } => {
            let body = respond(BlockedTemplate {
                layout,
                owner: owner.to_string(),
                slug: slug.to_string(),
                message: message.to_string(),
                forgejo_url,
            });
            (status, body).into_response()
        }
    }
}

/// Read the Recipe and check that this person may write to it.
///
/// Forgejo decides. The application reads the permissions that Forgejo
/// computed for this token and never works them out for itself.
async fn target(
    state: &AppState,
    actor: &Actor,
    owner: &str,
    slug: &str,
) -> Result<Target, Refused> {
    let repository = match state.forgejo.repository(&actor.token, owner, slug).await {
        Ok(repository) => repository,
        Err(error) => {
            tracing::info!(%error, %owner, %slug, "cannot read the Recipe repository");
            return Err(Refused::Missing);
        }
    };

    let can_write = match state.forgejo.can_write(&actor.token, owner, slug).await {
        Ok(can_write) => can_write,
        Err(error) => {
            tracing::warn!(%error, %owner, %slug, "cannot read the permissions of this person");
            false
        }
    };

    let forgejo_url = state.forgejo.web_url(&repository.full_name);

    if !can_write {
        tracing::info!(%owner, %slug, login = %actor.user.login, "a person without write access asked to edit");
        return Err(Refused::Blocked {
            status: StatusCode::FORBIDDEN,
            message: NO_WRITE_MESSAGE,
            forgejo_url,
        });
    }

    let branch = if repository.default_branch.is_empty() {
        MAIN_BRANCH.to_string()
    } else {
        repository.default_branch.clone()
    };

    Ok(Target {
        branch,
        forgejo_url,
    })
}

/// Show the editor.
async fn edit_form(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug)): Path<(String, String)>,
) -> Response {
    let here = format!("/recipes/{owner}/{slug}/edit");
    let layout = || Layout::new(current.as_ref()).on(&headers, &here);

    let Some(actor) = actor(&state, &jar).await else {
        return Redirect::to("/auth/sign-in").into_response();
    };

    let target = match target(&state, &actor, &owner, &slug).await {
        Ok(target) => target,
        // A person who can read this Recipe but not write to it does not
        // lose their work here. The editor opens as a Suggestion instead,
        // which Forgejo holds and an Editor can accept.
        Err(Refused::Blocked { status, .. }) if status == StatusCode::FORBIDDEN => {
            return Redirect::to(&crate::web_suggestions::editor_href(&owner, &slug))
                .into_response();
        }
        Err(refused) => return refusal(layout(), &owner, &slug, refused),
    };

    // Git holds History, so Git says which Version is published now. The
    // source is then read at that exact Version, which keeps the text on
    // the page and the Version behind it from ever disagreeing.
    let remote = state.forgejo.git_url(&format!("{owner}/{slug}"));
    let base_version = match state
        .git
        .branch_head(&remote, &actor.token, &target.branch)
        .await
    {
        Ok(Some(version)) => version,
        Ok(None) => {
            return refusal(
                layout(),
                &owner,
                &slug,
                Refused::Blocked {
                    status: StatusCode::CONFLICT,
                    message: NO_VERSION_MESSAGE,
                    forgejo_url: target.forgejo_url,
                },
            );
        }
        Err(error) => {
            tracing::warn!(%error, %owner, %slug, "cannot read the published Version");
            return refusal(
                layout(),
                &owner,
                &slug,
                Refused::Blocked {
                    status: StatusCode::BAD_GATEWAY,
                    message: UNREACHABLE_MESSAGE,
                    forgejo_url: target.forgejo_url,
                },
            );
        }
    };

    // A draft comes first. The person left unfinished work here, possibly
    // on another device, and the editor has to open on that and not on the
    // published Recipe. The draft says which Version it was built on, so
    // publishing later joins the change with the Version the person really
    // started from and not with whatever `main` holds now.
    let draft =
        match crate::draft::read(&state, &actor.token, &owner, &slug, &actor.user.login).await {
            Ok(draft) => draft,
            Err(error) => {
                tracing::warn!(%error, %owner, %slug, "cannot read the draft");
                return refusal(
                    layout(),
                    &owner,
                    &slug,
                    Refused::Blocked {
                        status: StatusCode::BAD_GATEWAY,
                        message: UNREACHABLE_MESSAGE,
                        forgejo_url: target.forgejo_url,
                    },
                );
            }
        };

    let (read_at, base_version, draft_version) = match draft {
        Some(draft) => (
            draft.version.clone(),
            draft.base_version,
            draft.version.clone(),
        ),
        None => (base_version.clone(), base_version, String::new()),
    };

    let bytes = match state
        .forgejo
        .raw_file(Some(&actor.token), &owner, &slug, &read_at, RECIPE_FILE)
        .await
    {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::info!(%error, %owner, %slug, "cannot read the Recipe file");
            Vec::new()
        }
    };

    // A Recipe written through this application is always UTF-8 text. Git
    // accepts any bytes, so a direct push can put something else there.
    // Editing it here would replace bytes that this application cannot even
    // show, so say so and send the person to Forgejo instead.
    let Ok(source) = std::str::from_utf8(&bytes) else {
        tracing::info!(%owner, %slug, "the Recipe file is not UTF-8 text");
        return refusal(
            layout(),
            &owner,
            &slug,
            Refused::Blocked {
                status: StatusCode::CONFLICT,
                message: NOT_TEXT_MESSAGE,
                forgejo_url: target.forgejo_url,
            },
        );
    };

    respond(EditTemplate::new(
        layout(),
        &owner,
        &slug,
        source.to_string(),
        base_version,
        draft_version,
        String::new(),
        target.forgejo_url,
        Vec::new(),
    ))
}

/// What the editor sends when the person publishes.
#[derive(Debug, Deserialize)]
struct PublishForm {
    #[serde(default)]
    source: String,
    /// The Version the person started from.
    #[serde(default)]
    base_version: String,
    /// The draft Version the page carried, when it carried one.
    #[serde(default)]
    draft_version: String,
    #[serde(default)]
    note: String,
}

async fn publish(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug)): Path<(String, String)>,
    Form(form): Form<PublishForm>,
) -> Response {
    let here = format!("/recipes/{owner}/{slug}/edit");
    let layout = || Layout::new(current.as_ref()).on(&headers, &here);

    let Some(actor) = actor(&state, &jar).await else {
        return Redirect::to("/auth/sign-in").into_response();
    };

    // The check happens again here. A check that only the page makes is not
    // a check: this request can arrive without the page.
    let target = match target(&state, &actor, &owner, &slug).await {
        Ok(target) => target,
        // The Recipe is not published to by somebody who cannot write to
        // it. Their work becomes a Suggestion, so they go there.
        Err(Refused::Blocked { status, .. }) if status == StatusCode::FORBIDDEN => {
            return Redirect::to(&crate::web_suggestions::editor_href(&owner, &slug))
                .into_response();
        }
        Err(refused) => return refusal(layout(), &owner, &slug, refused),
    };

    let source = normalize(&form.source);
    let note = clean_note(&form.note);

    let page = |errors: Vec<String>| {
        respond(EditTemplate::new(
            layout(),
            &owner,
            &slug,
            source.clone(),
            form.base_version.clone(),
            form.draft_version.clone(),
            note.clone(),
            target.forgejo_url.clone(),
            errors,
        ))
    };

    if form.base_version.trim().is_empty() {
        return page(vec![
            "CookLangHub does not know which Version you started from. Open the Recipe again."
                .to_string(),
        ]);
    }

    if source.len() > MAX_SOURCE_BYTES {
        return page(vec!["The Recipe source is larger than 1 MB.".to_string()]);
    }

    // An error stops the publication. A warning does not: the person
    // decides whether it matters, and it travels with the Version.
    let parsed = recipe::parse(&source);
    if !parsed.is_valid() {
        return page(parsed.errors.iter().map(|d| d.message.clone()).collect());
    }

    let title = parsed.title.clone().unwrap_or_else(|| slug.clone());
    let message = if note.is_empty() {
        format!("Update {title}")
    } else {
        note.clone()
    };

    // Ask Forgejo whether this person hides their address. A failure here
    // is treated as "hide", because publishing an address by accident
    // cannot be undone.
    let hide_email = match state.forgejo.user_settings(&actor.token).await {
        Ok(settings) => settings.hide_email,
        Err(error) => {
            tracing::warn!(%error, "cannot read the privacy setting; using the no-reply address");
            true
        }
    };

    let identity = Identity {
        name: actor.user.display_name().to_string(),
        email: create_recipe::commit_email(
            &actor.user.login,
            &actor.user.email,
            hide_email,
            &state.forgejo_noreply_domain,
        ),
    };

    let mut files = std::collections::BTreeMap::new();
    files.insert(RECIPE_FILE.to_string(), source.clone().into_bytes());

    let published = state
        .git
        .publish_version(PublishVersion {
            remote_url: &state.forgejo.git_url(&format!("{owner}/{slug}")),
            token: &actor.token,
            identity: &identity,
            branch: &target.branch,
            message: &message,
            base_version: form.base_version.trim(),
            files,
        })
        .await;

    match published {
        Ok(version) => {
            tracing::info!(
                %owner, %slug, %version,
                warnings = parsed.warnings.len(),
                "published a Version"
            );

            // The publication consumed the draft, so the draft goes. This
            // is one of the two moments a draft is ever removed, and the
            // person asked for it by publishing.
            //
            // A removal that fails leaves the draft where it is. The
            // Version is published either way, so the person is not held
            // up, and the editor shows the draft again with the same text
            // in it.
            if let Err(error) =
                crate::draft::remove(&state, &actor.token, &owner, &slug, &actor.user.login).await
            {
                tracing::warn!(%error, %owner, %slug, "cannot remove the draft after a publication");
            }

            Redirect::to(&format!("/recipes/{owner}/{slug}")).into_response()
        }
        // Git could not join the change with what is published now. The
        // published Recipe is untouched, and the person keeps their text.
        Err(GitError::Conflict) => {
            tracing::info!(%owner, %slug, "a change could not be joined with the published Recipe");
            page(vec![CONFLICT_MESSAGE.to_string()])
        }
        Err(error) => {
            tracing::warn!(%error, %owner, %slug, "cannot publish a Version");
            page(vec![
                "CookLangHub cannot write to this Recipe at the moment. Nothing changed. Try again."
                    .to_string(),
            ])
        }
    }
}

/// Make the text the browser sent into the text the person typed.
///
/// A form sends every line break as CR LF, whatever the person wrote and
/// whatever the file held. Writing that back would rewrite every line of
/// the Recipe, so the carriage returns that the browser added come out
/// again. Nothing else about the source is touched.
fn normalize(source: &str) -> String {
    source.replace("\r\n", "\n").replace('\r', "\n")
}

/// Make a change note fit on one line of History.
fn clean_note(note: &str) -> String {
    let single_line: String = note
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();

    single_line
        .trim()
        .chars()
        .take(MAX_NOTE_CHARS)
        .collect::<String>()
        .trim()
        .to_string()
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
    fn the_carriage_returns_a_browser_adds_come_out_again() {
        // A form sends CR LF for every line break. Keeping them would
        // rewrite every line of a file that holds LF.
        assert_eq!(normalize("a\r\nb\r\n"), "a\nb\n");
        assert_eq!(normalize("a\rb"), "a\nb");
    }

    #[test]
    fn a_source_that_holds_no_carriage_return_is_untouched() {
        let source = "---\ntitle: Chili\n---\n\nChop the @onion{1}.\n\n\n  trailing  \n";
        assert_eq!(normalize(source), source);
    }

    #[test]
    fn an_empty_note_stays_empty_so_a_default_message_is_written() {
        assert_eq!(clean_note("   "), "");
        assert_eq!(clean_note("\n\n"), "");
        assert_eq!(clean_note(""), "");
    }

    #[test]
    fn a_note_becomes_one_line() {
        assert_eq!(
            clean_note("  less salt\nmore garlic  "),
            "less salt more garlic"
        );
    }

    #[test]
    fn a_very_long_note_is_cut_to_a_readable_length() {
        let note = "x".repeat(500);
        assert_eq!(clean_note(&note).chars().count(), MAX_NOTE_CHARS);
    }

    #[test]
    fn a_note_of_multibyte_letters_is_cut_by_letter_and_stays_text() {
        // Cutting by byte would split a letter in half and give a message
        // that is not text at all.
        let note = "ü".repeat(500);
        let cut = clean_note(&note);
        assert_eq!(cut.chars().count(), MAX_NOTE_CHARS);
        assert!(cut.chars().all(|c| c == 'ü'));
    }

    #[test]
    fn the_diagnosis_page_says_what_happened_and_offers_forgejo() {
        let page = BlockedTemplate {
            layout: Layout::new(None),
            owner: "sam".to_string(),
            slug: "chili".to_string(),
            message: NO_WRITE_MESSAGE.to_string(),
            forgejo_url: "https://forge.test/sam/chili".to_string(),
        }
        .render()
        .expect("the page must render");

        assert!(page.contains("cannot be edited here"));
        assert!(page.contains("Open in Forgejo"));
        assert!(page.contains("https://forge.test/sam/chili"));
        assert!(page.contains("/recipes/sam/chili"));
    }

    #[test]
    fn every_message_a_person_reads_uses_cooking_words() {
        // Recipe, Version, Editor. Never branch, commit, or merge.
        for message in [
            NO_WRITE_MESSAGE,
            NOT_TEXT_MESSAGE,
            NO_VERSION_MESSAGE,
            UNREACHABLE_MESSAGE,
            CONFLICT_MESSAGE,
        ] {
            let lower = message.to_lowercase();
            for word in [
                "branch",
                "commit",
                "fork",
                "pull request",
                "merge",
                "rebase",
            ] {
                assert!(
                    !lower.contains(word),
                    "`{word}` must not reach the person: {message}"
                );
            }
        }
    }

    /// The editor page, with or without a draft on it.
    fn editor_page(draft_version: &str) -> String {
        EditTemplate::new(
            Layout::new(None),
            "sam",
            "chili",
            "Chop the @onion{1}.".to_string(),
            "a".repeat(40),
            draft_version.to_string(),
            String::new(),
            "https://forge.test/sam/chili".to_string(),
            Vec::new(),
        )
        .render()
        .expect("the page must render")
    }

    #[test]
    fn a_page_with_a_draft_carries_it_and_offers_to_discard_it() {
        let version = "b".repeat(40);
        let page = editor_page(&version);

        // The draft Version travels with the form, so the next save can be
        // measured against what the draft holds.
        assert!(page.contains(&format!("name=\"draft_version\" value=\"{version}\"")));
        assert!(page.contains(crate::draft::NOTICE_MESSAGE));
        assert!(page.contains("Discard draft"));
        assert!(page.contains("action=\"/recipes/sam/chili/draft/discard\""));

        // Saving is a served file and a data attribute, never an attribute
        // that runs. The policy is `default-src 'self'`.
        assert!(page.contains("data-draft-url=\"/recipes/sam/chili/draft\""));
        assert!(!page.contains("onclick="));
        assert!(!page.contains("<script>"));
    }

    #[test]
    fn a_page_without_a_draft_offers_nothing_to_discard() {
        let page = editor_page("");

        assert!(page.contains("name=\"draft_version\" value=\"\""));
        assert!(!page.contains("Discard draft"));
        assert!(!page.contains("draft/discard"));
        assert!(!page.contains(crate::draft::NOTICE_MESSAGE));

        // Publishing still works with no script at all, draft or no draft.
        assert!(page.contains("Publish Version"));
    }

    #[test]
    fn a_source_without_a_title_still_names_the_preview() {
        let preview = PreviewTemplate::of("Chop the @onion{1}.", "chili");
        assert_eq!(preview.preview_title, "chili");
    }

    #[test]
    fn a_preview_reports_an_error_and_shows_no_recipe() {
        let preview = PreviewTemplate::of("---\ntitle: T\n---\n\nWait ~{5%bananas}.", "t");
        assert!(!preview.parse_errors.is_empty());
        assert!(preview.cooked.is_empty());
    }

    #[test]
    fn a_preview_reports_a_warning_and_still_shows_the_recipe() {
        // A warning must never hide the Recipe, because it never stops a
        // publication either.
        let preview = PreviewTemplate::of(
            "---\ntitle: T\nservings: many\n---\n\nChop the @onion{1}.",
            "t",
        );
        assert!(preview.parse_errors.is_empty());
        assert!(!preview.warnings.is_empty());
        assert!(!preview.cooked.is_empty());
    }
}
