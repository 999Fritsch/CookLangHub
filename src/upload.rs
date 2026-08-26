//! Uploading a Recipe file and a photo.
//!
//! A Recipe starts from typed Cooklang or from a `.cook` file. The two are
//! exclusive, and the person selects which one. A form that carries both is
//! refused rather than resolved, so the person always knows which content
//! the application uses.
//!
//! A Recipe carries zero or one photo, stored beside `recipe.cook` as
//! `recipe.jpg`, `recipe.png`, or `recipe.webp`. The format comes from the
//! first bytes of the file and never from the name or the media type that
//! the browser sent, because both of those are only what somebody typed.
//! A photo that arrives with the wrong name is therefore stored under the
//! right one.
//!
//! The bytes are stored as they arrived. The application does not convert
//! a photo and does not compress it.
//!
//! A browser reads a photo from this application and never from Forgejo.
//! The Content Security Policy allows an image from this origin only, and
//! reading it here keeps Forgejo the one place that decides who may see a
//! private Recipe.

use std::collections::BTreeMap;
use std::sync::Arc;

use askama::Template;
use axum::Router;
use axum::extract::multipart::{Field, MultipartError};
use axum::extract::{DefaultBodyLimit, Multipart, Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::get;
use axum_extra::extract::CookieJar;

use crate::create_recipe::{self, MAIN_BRANCH};
use crate::forgejo::{ForgejoClient, Repository};
use crate::git::ChangeCommit;
use crate::recipe::MAX_SOURCE_BYTES;
use crate::secret::Secret;
use crate::web::{AppState, Layout, MaybeUser};

/// The friendly limit for a photo, in bytes.
pub const MAX_THUMBNAIL_BYTES: usize = 5 * 1024 * 1024;

/// How much of one request the application reads.
///
/// It is the two limits together, and one megabyte more for the field
/// names and the boundaries between the parts. A body above this never
/// reaches memory at all: the transport stops it, and the person gets the
/// reason from [`Refusal::TooMuch`].
pub const MAX_REQUEST_BYTES: usize = MAX_THUMBNAIL_BYTES + MAX_SOURCE_BYTES + 1024 * 1024;

// A request that carries both uploads at their limits has to fit, or a
// person who did nothing wrong would get the transport refusal instead of
// the reason.
const _: () = assert!(MAX_REQUEST_BYTES > MAX_THUMBNAIL_BYTES + MAX_SOURCE_BYTES);

/// The photo formats a Recipe can hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbnailFormat {
    Jpeg,
    Png,
    Webp,
}

impl ThumbnailFormat {
    /// Every format, in the order the application looks for them.
    pub const ALL: [ThumbnailFormat; 3] = [Self::Jpeg, Self::Png, Self::Webp];

    /// Where a photo of this format lives in a Recipe.
    pub fn path(self) -> &'static str {
        match self {
            Self::Jpeg => "recipe.jpg",
            Self::Png => "recipe.png",
            Self::Webp => "recipe.webp",
        }
    }

    /// What the browser is told the bytes are.
    pub fn content_type(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
            Self::Webp => "image/webp",
        }
    }

    /// Read the format out of the first bytes of a file.
    ///
    /// A name can say anything, so the bytes decide. A JPEG that arrives
    /// as `photo.png` is stored as `recipe.jpg`, which keeps the promise
    /// that the name of the file describes its content.
    pub fn sniff(bytes: &[u8]) -> Option<Self> {
        // JPEG: the start-of-image marker, then any marker.
        if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
            return Some(Self::Jpeg);
        }

        // PNG: the eight byte signature of the format.
        if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
            return Some(Self::Png);
        }

        // WebP: a RIFF container whose form type is WEBP. The four bytes
        // between the two names are the length, which says nothing here.
        if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
            return Some(Self::Webp);
        }

        None
    }
}

/// A photo that a Recipe can hold.
#[derive(Debug, Clone)]
pub struct Thumbnail {
    pub format: ThumbnailFormat,
    /// The bytes exactly as they arrived.
    pub bytes: Vec<u8>,
}

impl Thumbnail {
    /// Accept bytes as a photo, or say why they cannot be one.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, Refusal> {
        if bytes.len() > MAX_THUMBNAIL_BYTES {
            return Err(Refusal::PhotoTooLarge);
        }

        let format = ThumbnailFormat::sniff(&bytes).ok_or(Refusal::PhotoFormat)?;

        Ok(Self { format, bytes })
    }
}

/// Why an upload cannot be used. Each message is for the person.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    #[error("the photo is larger than 5 MB")]
    PhotoTooLarge,
    #[error("the photo must be a JPEG, a PNG, or a WebP image")]
    PhotoFormat,
    #[error("select a photo")]
    PhotoMissing,
    #[error("the Recipe file is larger than 1 MB")]
    FileTooLarge,
    #[error("the Recipe file is not UTF-8 text")]
    FileNotText,
    #[error("select a Recipe file, or write the Recipe as text")]
    FileMissing,
    #[error("the Recipe has typed text and a file: remove one of them")]
    TwoSources,
    #[error(
        "the upload is larger than the application accepts: a photo must be 5 MB or less, and a Recipe file must be 1 MB or less"
    )]
    TooMuch,
}

/// Where the Cooklang comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SourceMode {
    /// The person writes the Recipe in the form.
    #[default]
    Text,
    /// The person uploads a `.cook` file.
    File,
}

impl SourceMode {
    fn read(value: &str) -> Self {
        if value.trim() == "file" {
            Self::File
        } else {
            Self::Text
        }
    }

    pub fn is_file(self) -> bool {
        self == Self::File
    }
}

/// The create form, as it arrived.
#[derive(Debug, Default)]
pub struct CreateUpload {
    pub title: String,
    /// Which of the two sources the person selected.
    pub mode: SourceMode,
    /// What the person typed. The form keeps it when something stops the
    /// creation, so nobody has to write it again.
    pub typed: String,
    pub private: bool,
    /// The bytes of the `.cook` file, when there is one.
    file: Option<Vec<u8>>,
    /// The bytes of the photo, when there is one.
    photo: Option<Vec<u8>>,
    /// Set when the body itself could not be read.
    read_error: Option<Refusal>,
}

/// What the application will store.
#[derive(Debug)]
pub struct Content {
    pub source: String,
    pub thumbnail: Option<Thumbnail>,
}

impl CreateUpload {
    /// Decide what the application will use, or give every reason it will
    /// not.
    ///
    /// Every reason comes back together, so that a person who made two
    /// mistakes does not have to find them one at a time.
    pub fn content(&self) -> Result<Content, Vec<Refusal>> {
        let mut refusals = Vec::new();

        if let Some(refusal) = self.read_error {
            return Err(vec![refusal]);
        }

        let has_file = self.file.as_ref().is_some_and(|bytes| !bytes.is_empty());
        let has_text = !self.typed.trim().is_empty();

        // The two modes are exclusive. A form that carries both is refused
        // rather than resolved, because a person must never have to guess
        // which content the application kept.
        if has_file && has_text {
            refusals.push(Refusal::TwoSources);
        }

        let source = match self.mode {
            SourceMode::Text => Ok(self.typed.clone()),
            SourceMode::File => match &self.file {
                Some(bytes) if bytes.len() > MAX_SOURCE_BYTES => Err(Refusal::FileTooLarge),
                Some(bytes) if !bytes.is_empty() => String::from_utf8(bytes.clone())
                    .map_err(|_| Refusal::FileNotText)
                    .map(|text| text.replace("\r\n", "\n")),
                _ => Err(Refusal::FileMissing),
            },
        };

        let source = match source {
            Ok(source) => Some(source),
            Err(refusal) => {
                refusals.push(refusal);
                None
            }
        };

        let thumbnail = match &self.photo {
            Some(bytes) if !bytes.is_empty() => match Thumbnail::from_bytes(bytes.clone()) {
                Ok(thumbnail) => Some(thumbnail),
                Err(refusal) => {
                    refusals.push(refusal);
                    None
                }
            },
            _ => None,
        };

        if !refusals.is_empty() {
            return Err(refusals);
        }

        Ok(Content {
            source: source.unwrap_or_default(),
            thumbnail,
        })
    }
}

/// Read the create form out of a multipart body.
///
/// Nothing here fails: a body that cannot be read becomes a reason that the
/// form shows, so the person keeps the page and their title.
pub async fn read_create_form(mut multipart: Multipart) -> CreateUpload {
    let mut upload = CreateUpload::default();

    loop {
        let field = match multipart.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(error) => {
                tracing::info!(%error, "cannot read an upload");
                upload.read_error = Some(Refusal::TooMuch);
                break;
            }
        };

        let name = field.name().unwrap_or_default().to_string();

        // Each part is read with a cap of its own, so a large part never
        // grows memory past what the limit for that part allows.
        let limit = match name.as_str() {
            "thumbnail" => MAX_THUMBNAIL_BYTES,
            _ => MAX_SOURCE_BYTES,
        };

        let bytes = match read_capped(field, limit).await {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::info!(%error, "cannot read a part of an upload");
                upload.read_error = Some(Refusal::TooMuch);
                break;
            }
        };

        match name.as_str() {
            "title" => upload.title = text(&bytes),
            "mode" => upload.mode = SourceMode::read(&text(&bytes)),
            "source" => upload.typed = text(&bytes),
            "visibility" => upload.private = text(&bytes).trim() == "private",
            "recipe_file" => upload.file = Some(bytes),
            "thumbnail" => upload.photo = Some(bytes),
            _ => {}
        }
    }

    upload
}

/// Read a photo out of a multipart body that carries only one.
async fn read_photo(mut multipart: Multipart) -> Result<Thumbnail, Vec<Refusal>> {
    let mut bytes: Option<Vec<u8>> = None;

    loop {
        let field = match multipart.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(error) => {
                tracing::info!(%error, "cannot read a photo");
                return Err(vec![Refusal::TooMuch]);
            }
        };

        let name = field.name().unwrap_or_default().to_string();

        match read_capped(field, MAX_THUMBNAIL_BYTES).await {
            Ok(read) if name == "thumbnail" => bytes = Some(read),
            Ok(_) => {}
            Err(error) => {
                tracing::info!(%error, "cannot read a part of a photo");
                return Err(vec![Refusal::TooMuch]);
            }
        }
    }

    match bytes {
        Some(bytes) if !bytes.is_empty() => Thumbnail::from_bytes(bytes).map_err(|e| vec![e]),
        _ => Err(vec![Refusal::PhotoMissing]),
    }
}

/// Read one part, and stop keeping bytes once it passes its limit.
///
/// One byte more than the limit is kept, which is what lets the caller see
/// that the limit was passed. The rest of the part is read and dropped, so
/// that the answer still reaches the browser.
async fn read_capped(mut field: Field<'_>, limit: usize) -> Result<Vec<u8>, MultipartError> {
    let mut out = Vec::new();

    while let Some(chunk) = field.chunk().await? {
        let room = (limit + 1).saturating_sub(out.len());
        if room > 0 {
            out.extend_from_slice(&chunk[..room.min(chunk.len())]);
        }
    }

    Ok(out)
}

/// Read a part that carries text. A part that is not UTF-8 loses nothing
/// that a person typed, because a browser sends a form as UTF-8.
fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).to_string()
}

/// What photos a Recipe holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Photos {
    None,
    One(ThumbnailFormat),
    /// More than one. The interface cannot say which one is meant.
    Several,
}

impl Photos {
    pub fn format(self) -> Option<ThumbnailFormat> {
        match self {
            Self::One(format) => Some(format),
            _ => None,
        }
    }

    pub fn is_some(self) -> bool {
        matches!(self, Self::One(_))
    }
}

/// Decide the photo state from the names at the top of a Recipe.
///
/// Public so that the Recipe page can read the photo state out of the file
/// listing it already asked for, rather than asking Forgejo a second time.
pub fn photos_in(names: &[String]) -> Photos {
    let found: Vec<ThumbnailFormat> = ThumbnailFormat::ALL
        .into_iter()
        .filter(|format| names.iter().any(|name| name == format.path()))
        .collect();

    match found.len() {
        0 => Photos::None,
        1 => Photos::One(found[0]),
        _ => Photos::Several,
    }
}

/// Ask Forgejo what photos a Recipe holds.
///
/// A Recipe the application cannot read gives `None`, because a photo that
/// cannot be found is the same to a reader as a photo that is not there.
pub async fn photos(
    forgejo: &ForgejoClient,
    token: Option<&Secret<String>>,
    owner: &str,
    slug: &str,
    branch: &str,
) -> Photos {
    match forgejo.list_root_files(token, owner, slug, branch).await {
        Ok(names) => photos_in(&names),
        Err(error) => {
            tracing::info!(%error, %owner, %slug, "cannot list the files of a Recipe");
            Photos::None
        }
    }
}

/// Said on the Recipe page when a Recipe holds more than one photo.
///
/// The application never removes one of them on its own. A person made
/// that state, and only a person decides which photo is the right one.
pub const SEVERAL_PHOTOS_MESSAGE: &str = "This Recipe has more than one photo. The application cannot show which photo is the correct one. To correct this, open the Recipe in Forgejo and delete the photos that you do not want.";

/// The branch a Recipe publishes from.
pub fn branch_of(repository: &Repository) -> String {
    if repository.default_branch.is_empty() {
        MAIN_BRANCH.to_string()
    } else {
        repository.default_branch.clone()
    }
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route(
        "/recipes/{owner}/{slug}/thumbnail",
        get(show_photo)
            .post(change_photo)
            .layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES)),
    )
}

/// The credential of the person who is looking, when there is one.
///
/// Serving a photo needs the credential and not the name behind it, so
/// this asks Forgejo nothing. Every page carries several photos, and one
/// call for each of them would be one call too many.
async fn viewer_token(state: &AppState, jar: &CookieJar) -> Option<Secret<String>> {
    let cookie = jar.get(crate::session::COOKIE_NAME)?;
    crate::session::access_token(&state.pool, &state.cipher, cookie.value())
        .await
        .ok()
        .flatten()
}

/// Serve the photo of a Recipe.
///
/// The bytes travel through this application rather than from Forgejo, for
/// two reasons. The Content Security Policy allows an image from this
/// origin only. And Forgejo answers this request with the credential of the
/// person who is looking, so a private Recipe keeps its photo private.
async fn show_photo(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
    Path((owner, slug)): Path<(String, String)>,
) -> Response {
    let token = viewer_token(&state, &jar).await;
    let anonymous = Secret::new(String::new());

    let Ok(repository) = state
        .forgejo
        .repository(token.as_ref().unwrap_or(&anonymous), &owner, &slug)
        .await
    else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let branch = branch_of(&repository);

    let Some(format) = photos(&state.forgejo, token.as_ref(), &owner, &slug, &branch)
        .await
        .format()
    else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let Ok(bytes) = state
        .forgejo
        .raw_file(token.as_ref(), &owner, &slug, &branch, format.path())
        .await
    else {
        return StatusCode::NOT_FOUND.into_response();
    };

    (
        [
            (header::CONTENT_TYPE, format.content_type()),
            // A photo follows the permission of the Recipe, so no shared
            // cache may keep it, and a new photo must show at once.
            (header::CACHE_CONTROL, "private, no-cache"),
        ],
        bytes,
    )
        .into_response()
}

/// Put a photo on a Recipe that exists.
///
/// One Version carries the change. When the new photo has another format,
/// the same Version removes the old file, so a Recipe never holds two.
async fn change_photo(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    MaybeUser(current): MaybeUser,
    Path((owner, slug)): Path<(String, String)>,
    multipart: Multipart,
) -> Response {
    let here = format!("/recipes/{owner}/{slug}");

    let Some(actor) = crate::web_recipes::actor(&state, &jar).await else {
        return Redirect::to("/auth/sign-in").into_response();
    };

    let problem = |reasons: Vec<String>, forgejo_url: Option<String>| {
        respond(PhotoProblemTemplate {
            layout: Layout::new(current.as_ref()).on(&headers, &here),
            back: here.clone(),
            reasons,
            forgejo_url,
        })
    };

    let thumbnail = match read_photo(multipart).await {
        Ok(thumbnail) => thumbnail,
        Err(refusals) => {
            return problem(refusals.iter().map(ToString::to_string).collect(), None);
        }
    };

    let repository = match state.forgejo.repository(&actor.token, &owner, &slug).await {
        Ok(repository) => repository,
        Err(error) => {
            tracing::info!(%error, %owner, %slug, "cannot read the Recipe for a photo");
            return problem(vec!["This Recipe is not available.".to_string()], None);
        }
    };

    let forgejo_url = state.forgejo.web_url(&repository.full_name);
    let branch = branch_of(&repository);

    // Every other photo goes in the same Version, so the Recipe holds one
    // photo when the Version is published and never two.
    let held = photos(&state.forgejo, Some(&actor.token), &owner, &slug, &branch).await;
    let delete: Vec<String> = ThumbnailFormat::ALL
        .into_iter()
        .filter(|format| *format != thumbnail.format)
        .map(|format| format.path().to_string())
        .collect();

    let message = if held.is_some() {
        "Change the photo"
    } else {
        "Add a photo"
    };

    let identity = create_recipe::identity_of(
        &state.forgejo,
        &actor.token,
        &actor.user,
        &state.forgejo_noreply_domain,
    )
    .await;

    let mut write = BTreeMap::new();
    write.insert(thumbnail.format.path().to_string(), thumbnail.bytes);

    let version = state
        .git
        .commit_change(ChangeCommit {
            remote_url: &state.forgejo.git_url(&repository.full_name),
            token: &actor.token,
            identity: &identity,
            branch: &branch,
            message,
            write,
            delete,
        })
        .await;

    match version {
        Ok(version) => {
            tracing::info!(%owner, %slug, %version, "changed the photo of a Recipe");
            Redirect::to(&here).into_response()
        }
        Err(error) => {
            tracing::warn!(%error, %owner, %slug, "cannot store the photo of a Recipe");
            problem(
                vec!["The photo cannot be stored. Nothing changed.".to_string()],
                Some(forgejo_url),
            )
        }
    }
}

#[derive(Template)]
#[template(path = "photo_problem.html")]
struct PhotoProblemTemplate {
    layout: Layout,
    /// Where the Recipe is, so the person gets back to it.
    back: String,
    reasons: Vec<String>,
    /// Offered when the application cannot handle the state itself.
    forgejo_url: Option<String>,
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

    fn jpeg(size: usize) -> Vec<u8> {
        let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xE0];
        bytes.resize(size.max(4), 0x42);
        bytes
    }

    fn png(size: usize) -> Vec<u8> {
        let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        bytes.resize(size.max(8), 0x42);
        bytes
    }

    fn webp(size: usize) -> Vec<u8> {
        let mut bytes = Vec::from(*b"RIFF\x1a\x00\x00\x00WEBPVP8 ");
        bytes.resize(size.max(16), 0x42);
        bytes
    }

    #[test]
    fn each_format_is_read_from_its_own_first_bytes() {
        assert_eq!(
            ThumbnailFormat::sniff(&jpeg(64)),
            Some(ThumbnailFormat::Jpeg)
        );
        assert_eq!(ThumbnailFormat::sniff(&png(64)), Some(ThumbnailFormat::Png));
        assert_eq!(
            ThumbnailFormat::sniff(&webp(64)),
            Some(ThumbnailFormat::Webp)
        );
    }

    #[test]
    fn a_file_that_is_not_an_image_has_no_format() {
        assert_eq!(ThumbnailFormat::sniff(b"---\ntitle: Toast\n---\n"), None);
        assert_eq!(ThumbnailFormat::sniff(b""), None);
        assert_eq!(ThumbnailFormat::sniff(&[0xFF, 0xD8]), None);
        // RIFF alone is not enough: a WAV file starts the same way.
        assert_eq!(
            ThumbnailFormat::sniff(b"RIFF\x00\x00\x00\x00WAVEfmt "),
            None
        );
    }

    #[test]
    fn the_bytes_decide_the_name_and_not_the_person_who_uploaded_it() {
        // Somebody can name a JPEG `photo.png`. The stored name has to
        // describe the content, so the first bytes decide it.
        let thumbnail = Thumbnail::from_bytes(jpeg(64)).expect("a JPEG is a photo");
        assert_eq!(thumbnail.format.path(), "recipe.jpg");
        assert_eq!(thumbnail.format.content_type(), "image/jpeg");
    }

    #[test]
    fn every_format_has_its_own_name_and_media_type() {
        let paths: Vec<&str> = ThumbnailFormat::ALL.iter().map(|f| f.path()).collect();
        assert_eq!(paths, vec!["recipe.jpg", "recipe.png", "recipe.webp"]);

        let types: Vec<&str> = ThumbnailFormat::ALL
            .iter()
            .map(|f| f.content_type())
            .collect();
        assert_eq!(types, vec!["image/jpeg", "image/png", "image/webp"]);
    }

    #[test]
    fn a_photo_keeps_every_byte_that_arrived() {
        let bytes = png(2048);
        let thumbnail = Thumbnail::from_bytes(bytes.clone()).expect("a PNG is a photo");
        assert_eq!(thumbnail.bytes, bytes, "the application converts nothing");
    }

    #[test]
    fn a_photo_over_five_megabytes_is_refused_with_the_reason() {
        let error = Thumbnail::from_bytes(jpeg(MAX_THUMBNAIL_BYTES + 1))
            .expect_err("a photo above the limit cannot be used");
        assert_eq!(error, Refusal::PhotoTooLarge);
        assert!(error.to_string().contains("larger than 5 MB"));

        // Exactly at the limit is still accepted.
        assert!(Thumbnail::from_bytes(jpeg(MAX_THUMBNAIL_BYTES)).is_ok());
    }

    #[test]
    fn the_size_is_read_before_the_format() {
        // A very large file that is not an image gets the reason a person
        // can act on, which is the size.
        let error = Thumbnail::from_bytes(vec![0x00; MAX_THUMBNAIL_BYTES + 1])
            .expect_err("this cannot be a photo");
        assert_eq!(error, Refusal::PhotoTooLarge);
    }

    #[test]
    fn a_file_that_is_not_an_image_is_refused_with_the_reason() {
        let error =
            Thumbnail::from_bytes(b"not a picture".to_vec()).expect_err("this cannot be a photo");
        assert_eq!(error, Refusal::PhotoFormat);
        assert!(error.to_string().contains("JPEG"));
        assert!(error.to_string().contains("WebP"));
    }

    fn typed(text: &str) -> CreateUpload {
        CreateUpload {
            title: "Chili".to_string(),
            mode: SourceMode::Text,
            typed: text.to_string(),
            ..CreateUpload::default()
        }
    }

    fn uploaded(bytes: Vec<u8>) -> CreateUpload {
        CreateUpload {
            title: "Chili".to_string(),
            mode: SourceMode::File,
            file: Some(bytes),
            ..CreateUpload::default()
        }
    }

    #[test]
    fn typed_text_is_the_source_in_the_text_mode() {
        let content = typed("Chop the @onion{1}.").content().expect("valid");
        assert_eq!(content.source, "Chop the @onion{1}.");
        assert!(content.thumbnail.is_none());
    }

    #[test]
    fn the_file_is_the_source_in_the_file_mode() {
        let content = uploaded(b"Chop the @onion{1}.".to_vec())
            .content()
            .expect("valid");
        assert_eq!(content.source, "Chop the @onion{1}.");
    }

    #[test]
    fn the_two_modes_are_exclusive() {
        let mut upload = uploaded(b"From the file.".to_vec());
        upload.typed = "From the form.".to_string();

        let refusals = upload.content().expect_err("two sources cannot be used");
        assert!(refusals.contains(&Refusal::TwoSources), "got {refusals:?}");
        assert!(
            refusals[0].to_string().contains("remove one of them"),
            "the person must be told what to do"
        );
    }

    #[test]
    fn text_that_is_only_spaces_does_not_count_as_a_second_source() {
        let mut upload = uploaded(b"From the file.".to_vec());
        upload.typed = "   \n ".to_string();
        assert!(upload.content().is_ok());
    }

    #[test]
    fn the_file_mode_needs_a_file() {
        let refusals = CreateUpload {
            mode: SourceMode::File,
            ..CreateUpload::default()
        }
        .content()
        .expect_err("there is nothing to store");
        assert!(refusals.contains(&Refusal::FileMissing), "got {refusals:?}");
    }

    #[test]
    fn a_recipe_file_over_one_megabyte_is_refused_with_the_reason() {
        let refusals = uploaded(vec![b'a'; MAX_SOURCE_BYTES + 1])
            .content()
            .expect_err("a file above the limit cannot be used");
        assert!(
            refusals.contains(&Refusal::FileTooLarge),
            "got {refusals:?}"
        );
        assert!(refusals[0].to_string().contains("larger than 1 MB"));
    }

    #[test]
    fn a_recipe_file_that_is_not_text_is_refused_with_the_reason() {
        // A person can pick a photo where a Recipe was meant.
        let refusals = uploaded(jpeg(64))
            .content()
            .expect_err("a Recipe has to be text");
        assert!(refusals.contains(&Refusal::FileNotText), "got {refusals:?}");
    }

    #[test]
    fn a_recipe_file_written_on_windows_keeps_its_lines() {
        let content = uploaded(b"---\r\ntitle: Toast\r\n---\r\n\r\nToast it.".to_vec())
            .content()
            .expect("valid");
        assert!(!content.source.contains('\r'), "got {:?}", content.source);
        assert!(content.source.starts_with("---\ntitle: Toast"));
    }

    #[test]
    fn a_recipe_file_keeps_its_umlauts() {
        let content = uploaded("Die @Äpfel{2} schälen.".as_bytes().to_vec())
            .content()
            .expect("valid");
        assert_eq!(content.source, "Die @Äpfel{2} schälen.");
    }

    #[test]
    fn every_reason_arrives_together() {
        let mut upload = uploaded(vec![b'a'; MAX_SOURCE_BYTES + 1]);
        upload.photo = Some(b"not a picture".to_vec());

        let refusals = upload.content().expect_err("two things are wrong");
        assert!(
            refusals.contains(&Refusal::FileTooLarge),
            "got {refusals:?}"
        );
        assert!(refusals.contains(&Refusal::PhotoFormat), "got {refusals:?}");
    }

    #[test]
    fn a_body_that_cannot_be_read_gives_one_reason_with_both_limits() {
        let upload = CreateUpload {
            read_error: Some(Refusal::TooMuch),
            ..CreateUpload::default()
        };
        let refusals = upload.content().expect_err("the body was refused");
        assert_eq!(refusals, vec![Refusal::TooMuch]);
        assert!(refusals[0].to_string().contains("5 MB"));
        assert!(refusals[0].to_string().contains("1 MB"));
    }

    #[test]
    fn an_empty_photo_part_means_no_photo() {
        // A form always sends the file part, and it is empty when nobody
        // selected a file. That is not a refusal.
        let mut upload = typed("Toast it.");
        upload.photo = Some(Vec::new());
        assert!(upload.content().expect("valid").thumbnail.is_none());
    }

    #[test]
    fn a_photo_arrives_beside_the_source() {
        let mut upload = typed("Toast it.");
        upload.photo = Some(webp(512));

        let content = upload.content().expect("valid");
        let thumbnail = content.thumbnail.expect("the photo must survive");
        assert_eq!(thumbnail.format, ThumbnailFormat::Webp);
        assert_eq!(thumbnail.format.path(), "recipe.webp");
    }

    #[test]
    fn the_mode_comes_from_the_form_and_falls_back_to_text() {
        assert_eq!(SourceMode::read("file"), SourceMode::File);
        assert_eq!(SourceMode::read("text"), SourceMode::Text);
        assert_eq!(SourceMode::read(""), SourceMode::Text);
        assert_eq!(SourceMode::read("something else"), SourceMode::Text);
        assert_eq!(SourceMode::default(), SourceMode::Text);
    }

    #[test]
    fn a_recipe_holds_zero_or_one_photo() {
        assert_eq!(photos_in(&[]), Photos::None);
        assert_eq!(
            photos_in(&["recipe.cook".to_string()]),
            Photos::None,
            "a Recipe without a photo is an ordinary Recipe"
        );
        assert_eq!(
            photos_in(&["recipe.cook".to_string(), "recipe.png".to_string()]),
            Photos::One(ThumbnailFormat::Png)
        );
    }

    #[test]
    fn more_than_one_photo_is_a_state_the_interface_names() {
        // Somebody can push two photos with Git. The application says so
        // and offers Forgejo; it never removes one of them by itself.
        let state = photos_in(&[
            "recipe.cook".to_string(),
            "recipe.jpg".to_string(),
            "recipe.webp".to_string(),
        ]);
        assert_eq!(state, Photos::Several);
        assert_eq!(state.format(), None);
        assert!(!state.is_some());
        assert!(SEVERAL_PHOTOS_MESSAGE.contains("Forgejo"));
    }

    #[test]
    fn a_file_that_only_looks_like_a_photo_is_not_one() {
        // The name decides nothing. `recipes.jpg.txt` is not `recipe.jpg`.
        assert_eq!(photos_in(&["recipe.jpg.txt".to_string()]), Photos::None);
        assert_eq!(photos_in(&["photo.jpg".to_string()]), Photos::None);
    }

    #[test]
    fn the_branch_of_a_recipe_falls_back_to_the_published_one() {
        let repository = |default_branch: &str| Repository {
            name: "chili".to_string(),
            full_name: "sam/chili".to_string(),
            html_url: String::new(),
            clone_url: String::new(),
            default_branch: default_branch.to_string(),
            private: false,
            empty: false,
            has_issues: true,
            id: 1,
            topics: vec!["cooklang".to_string(), "recipe".to_string()],
            updated_at: String::new(),
            owner: crate::forgejo::RepositoryOwner {
                id: 1,
                login: "sam".to_string(),
            },
        };

        assert_eq!(branch_of(&repository("live")), "live");
        assert_eq!(branch_of(&repository("")), MAIN_BRANCH);
    }
}
