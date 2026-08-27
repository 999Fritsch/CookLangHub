//! Cookbooks: the convention, the README, and the index.
//!
//! A Cookbook is a Forgejo repository, exactly as a Recipe is. Forgejo holds
//! it and Git holds its content and its History. Nothing here is a second
//! authoritative store.
//!
//! # The marker
//!
//! Forgejo topics are the opt-in marker. A Recipe carries `cooklang` and
//! `recipe`. A Cookbook carries `cooklang` and `cookbook`. The two never mix
//! in one list, because a Recipe list keeps only what carries `recipe` and a
//! Cookbook list keeps only what carries `cookbook`. Removing a topic in
//! Forgejo takes the repository out of this application, which is what keeps
//! Forgejo authoritative.
//!
//! # The README
//!
//! `README.md` is the one file that a new Cookbook holds. Its first heading
//! is the title that a person sees, and the rest of it is the description.
//! There is no `cookbook.yaml`: a Cookbook stays readable, and editable, with
//! nothing but Git.
//!
//! # The Recipes
//!
//! A Cookbook holds each of its Recipes by reference, and never by copy. The
//! reference is a Git submodule: `.gitmodules` names the Recipe and its
//! address, and the tree of the Cookbook records the exact Version. Both live
//! in Git, and neither is ever in this database.
//!
//! Two things follow from that choice, and the tickets after this one rest on
//! both. A reference that names a branch moves to each new Version of the
//! Recipe, and one that names none keeps the Version it was made at. And a
//! reference is only a name and an address, so a Cookbook can name a Recipe
//! that the reader may not open, or one that is no longer there. Each of
//! those states is reported and none of them is repaired.
//!
//! One thing follows that a person must know. The reference is in the
//! Cookbook, so anybody who can read the Cookbook in Forgejo can read the
//! address of every Recipe in it, including a private one. These pages never
//! show it, and Forgejo is the authority for what Forgejo shows. This is why
//! the application says so before a private Recipe goes into a Cookbook that
//! other people can reach.
//!
//! # Access
//!
//! A Cookbook and a Recipe are two repositories. Forgejo keeps the
//! permissions of each one apart, so access to a Cookbook is never access to
//! a Recipe in it, and this application adds nothing that joins the two.
//! What it does instead is show the mismatch before it happens and offer a
//! Forgejo grant for it. Every answer comes from Forgejo, one question at a
//! time, through [`reach`].
//!
//! Two states are not the same thing and they are not shown as the same
//! thing. A Recipe that a person cannot read is [`UNAVAILABLE_MESSAGE`], and
//! a person who cannot read a Recipe is a line on the screen that shares.
//!
//! Private and deleted stay one message. Forgejo answers 404 for both, and
//! it answers 404 to the Owner of the Cookbook as well, because the Owner of
//! a Cookbook is nobody in particular to the Recipe. Telling the two apart
//! would need a fact that nobody here has, so the message names both causes
//! and the person is offered Forgejo.
//!
//! # The index
//!
//! The same three rules as the Recipe index apply here.
//!
//! 1. Forgejo names the Cookbooks that a person may see, on every request.
//!    The index only supplies the words on the card.
//! 2. Every row is rebuildable. [`reconcile`] reads Forgejo again and writes
//!    each row back, so deleting the index costs time only.
//! 3. Nothing here writes to Forgejo or to Git. The index reads.

use std::collections::{BTreeMap, HashMap};

use futures::StreamExt;
use pulldown_cmark::{CowStr, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use sqlx::sqlite::SqlitePool;

use crate::create_recipe::{self, MAIN_BRANCH};
use crate::crypto::Cipher;
use crate::forgejo::{
    ForgejoClient, ForgejoError, ForgejoUser, Ownership, Repository, RepositoryQuery,
};
use crate::git::{GitAdapter, GitError, InitialCommit};
use crate::secret::Secret;
use crate::session::now;

/// The one canonical file in a Cookbook repository.
pub const README_FILE: &str = "README.md";

/// The topics that mark a Forgejo repository as a Cookbook.
///
/// `cooklang` says that this application knows the repository. `cookbook`
/// says which kind it is. A Recipe carries `cooklang` and `recipe` instead,
/// so no repository can reach both kinds of list unless a person puts all
/// three topics on it by hand in Forgejo.
pub const COOKBOOK_TOPICS: [&str; 2] = ["cooklang", "cookbook"];

/// The friendly limit for a Cookbook README, in bytes.
pub const MAX_README_BYTES: usize = 1024 * 1024;

/// How many slugs to try before giving up on a collision.
const MAX_SLUG_ATTEMPTS: u32 = 50;

/// How many characters of the description a card shows.
const SUMMARY_CHARS: usize = 180;

/// Make a repository slug from a Cookbook title.
pub fn slug(title: &str) -> String {
    crate::recipe::slug_with(title, "cookbook")
}

/// Add a suffix to a slug so that a second Cookbook with the same title can
/// exist. `attempt` counts from 2, because the first try uses the plain slug.
pub fn slug_attempt(base: &str, attempt: u32) -> String {
    crate::recipe::slug_attempt(base, attempt)
}

// ------------------------------------------------------------- the README

/// What the Markdown parser is allowed to understand.
///
/// Tables, strikethrough, and task lists are ordinary writing. Heading
/// attributes stay off, because they let a written document choose a class
/// and an identifier on the page that shows it.
fn options() -> Options {
    Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS
}

/// Write the README of a Cookbook.
///
/// The title becomes the first heading, and the description follows it. This
/// is the whole file format: a person who opens the repository in Forgejo,
/// or clones it, sees a plain README and can edit it there.
pub fn readme(title: &str, description: &str) -> String {
    let title = one_line(title);
    let description = description.replace("\r\n", "\n");
    let description = description.trim();

    if description.is_empty() {
        format!("# {title}\n")
    } else {
        format!("# {title}\n\n{description}\n")
    }
}

/// Squeeze a title onto one line.
///
/// A heading is one line, so a title that carries a line break would end the
/// heading early and put the rest into the description.
fn one_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A README, read as a Cookbook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parts {
    /// The first heading, when the README has one.
    pub title: Option<String>,
    /// Everything else, as the raw Markdown that a person wrote.
    pub description: String,
}

/// Split a README into its title and its description.
///
/// The title is the first heading of level one. Everything else is the
/// description, including anything written above that heading, because a
/// README that this application did not write can put content there and
/// nothing may be lost.
pub fn split(readme: &str) -> Parts {
    let mut range = None;
    let mut title = String::new();
    let mut inside = false;

    for (event, span) in Parser::new_ext(readme, options()).into_offset_iter() {
        match event {
            Event::Start(Tag::Heading {
                level: HeadingLevel::H1,
                ..
            }) if range.is_none() => {
                inside = true;
                range = Some(span);
            }
            Event::End(TagEnd::Heading(HeadingLevel::H1)) if inside => break,
            Event::Text(text) | Event::Code(text) if inside => title.push_str(&text),
            _ => {}
        }
    }

    let Some(span) = range else {
        return Parts {
            title: None,
            description: readme.trim().to_string(),
        };
    };

    let before = readme[..span.start].trim();
    let after = readme[span.end..].trim();
    let description = match (before.is_empty(), after.is_empty()) {
        (true, _) => after.to_string(),
        (false, true) => before.to_string(),
        (false, false) => format!("{before}\n\n{after}"),
    };

    let title = one_line(&title);
    Parts {
        title: (!title.is_empty()).then_some(title),
        description,
    }
}

/// The first words of a description, as plain text, for a card.
pub fn summary(description: &str) -> String {
    let mut text = String::new();

    for event in Parser::new_ext(description, options()) {
        match event {
            Event::Text(piece) | Event::Code(piece) => text.push_str(&piece),
            Event::SoftBreak | Event::HardBreak => text.push(' '),
            Event::End(TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::Item) => text.push(' '),
            _ => {}
        }
        if text.chars().count() > SUMMARY_CHARS {
            break;
        }
    }

    let text = one_line(&text);
    if text.chars().count() <= SUMMARY_CHARS {
        return text;
    }

    let short: String = text.chars().take(SUMMARY_CHARS).collect();
    format!("{}…", short.trim_end())
}

// --------------------------------------------------------- safe Markdown

/// Where a link goes when its address is not one a page may follow.
const BLOCKED_URL: &str = "#";

/// Render a Cookbook description as HTML that is safe to put on a page.
///
/// The description is written by a person and read by everybody who can see
/// the Cookbook, so nothing in it may become an instruction to the browser.
/// Two rules give that, and they are applied to the events of the parser
/// rather than to the finished text, so no pattern has to be guessed at:
///
/// 1. Raw HTML never passes. It becomes the text that was written, so a
///    person sees exactly what they typed and the browser sees no tag.
/// 2. A link or an image keeps its address only when the address names
///    `http`, `https`, or `mailto`, or names no scheme at all.
///
/// Everything else that reaches the page comes from the renderer of the
/// parser, which escapes every character of text and every address it
/// writes. The set of tags is therefore closed, and no attribute in it
/// carries anything a person wrote unescaped.
pub fn render(markdown: &str) -> String {
    let events = Parser::new_ext(markdown, options()).map(make_safe);

    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, events);
    html
}

fn make_safe(event: Event<'_>) -> Event<'_> {
    match event {
        Event::Html(raw) | Event::InlineHtml(raw) => Event::Text(raw),
        Event::Start(Tag::Link {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Link {
            link_type,
            dest_url: safe_url(dest_url),
            title,
            id,
        }),
        Event::Start(Tag::Image {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Image {
            link_type,
            dest_url: safe_url(dest_url),
            title,
            id,
        }),
        other => other,
    }
}

fn safe_url(url: CowStr<'_>) -> CowStr<'_> {
    if is_safe_url(&url) {
        url
    } else {
        CowStr::Borrowed(BLOCKED_URL)
    }
}

/// Whether a page may follow this address.
///
/// A scheme ends at the first colon, and only when no `/`, `?`, or `#` comes
/// before it. Whitespace and control characters are removed first, because a
/// browser removes them too and `java&#9;script:` would otherwise pass.
fn is_safe_url(url: &str) -> bool {
    let cleaned: String = url
        .chars()
        .filter(|character| !character.is_whitespace() && !character.is_control())
        .collect();

    match cleaned.find([':', '/', '?', '#']) {
        Some(at) if cleaned.as_bytes()[at] == b':' => {
            matches!(
                cleaned[..at].to_ascii_lowercase().as_str(),
                "http" | "https" | "mailto"
            )
        }
        // No scheme at all. The address stays inside this installation.
        _ => true,
    }
}

// ----------------------------------------------------------- the creation

/// What the person filled in.
#[derive(Debug, Clone)]
pub struct NewCookbook {
    pub title: String,
    /// The description, as the raw Markdown that the person wrote.
    pub description: String,
    pub private: bool,
    /// The domain Forgejo uses for a hidden address.
    pub noreply_domain: String,
}

/// A Cookbook that now exists.
#[derive(Debug, Clone)]
pub struct CreatedCookbook {
    pub owner: String,
    pub slug: String,
    pub title: String,
    /// Where the repository lives in Forgejo, for **Open in Forgejo**.
    pub forgejo_url: String,
    pub version: String,
}

#[derive(Debug, thiserror::Error)]
pub enum CreateError {
    #[error("the Cookbook needs a title")]
    MissingTitle,
    #[error("the Cookbook description is larger than 1 MB")]
    TooLarge,
    #[error("cannot find a free name for the Cookbook")]
    NoFreeName,
    #[error(transparent)]
    Forgejo(#[from] ForgejoError),
    #[error(transparent)]
    Git(#[from] GitError),
}

/// Create a Cookbook.
///
/// The steps follow the ones that create a Recipe, and for the same reason:
/// there is no transaction across Forgejo and Git, so a failure has to leave
/// a state that a person can understand. A repository with no Version is
/// visible and can be retried or removed, and a Version can never exist
/// without its repository. The topics come last, so a half-made Cookbook
/// never reaches a list.
pub async fn create(
    forgejo: &ForgejoClient,
    git: &dyn GitAdapter,
    token: &Secret<String>,
    user: &ForgejoUser,
    input: NewCookbook,
) -> Result<CreatedCookbook, CreateError> {
    let title = one_line(&input.title);
    if title.is_empty() {
        return Err(CreateError::MissingTitle);
    }

    let readme = readme(&title, &input.description);

    // The limit applies to what will be stored, not to what was typed.
    if readme.len() > MAX_README_BYTES {
        return Err(CreateError::TooLarge);
    }

    let repository = create_repository(forgejo, token, &title, input.private).await?;

    let identity = create_recipe::identity_of(forgejo, token, user, &input.noreply_domain).await;

    // README.md and nothing else. There is no `cookbook.yaml`, and
    // `.gitmodules` appears only when the first Recipe is added.
    let mut files = BTreeMap::new();
    files.insert(README_FILE.to_string(), readme.into_bytes());

    let version = git
        .create_initial_commit(InitialCommit {
            remote_url: &forgejo.git_url(&repository.full_name),
            token,
            identity: &identity,
            branch: MAIN_BRANCH,
            message: format!("Add {title}").as_str(),
            files,
        })
        .await?;

    create_recipe::wait_until_recorded(forgejo, token, &user.login, &repository.name).await;

    forgejo
        .set_topics(token, &user.login, &repository.name, &COOKBOOK_TOPICS)
        .await?;

    Ok(CreatedCookbook {
        owner: user.login.clone(),
        slug: repository.name,
        title,
        forgejo_url: forgejo.web_url(&repository.full_name),
        version,
    })
}

/// Make the repository, working around a name that is already used.
async fn create_repository(
    forgejo: &ForgejoClient,
    token: &Secret<String>,
    title: &str,
    private: bool,
) -> Result<Repository, CreateError> {
    let base = slug(title);

    for attempt in 1..=MAX_SLUG_ATTEMPTS {
        let candidate = slug_attempt(&base, attempt);

        match forgejo
            .create_repository(token, &candidate, private, MAIN_BRANCH)
            .await
        {
            Ok(repository) => return Ok(repository),
            // Forgejo answers 409 when the name belongs to something else.
            Err(ForgejoError::Status { status: 409, .. }) => continue,
            Err(ForgejoError::Status { status: 422, body }) if body.contains("already exist") => {
                continue;
            }
            Err(error) => return Err(error.into()),
        }
    }

    Err(CreateError::NoFreeName)
}

// ------------------------------------------------------------ the Recipes

/// The file that names each Recipe of a Cookbook and where it lives.
///
/// Git writes this file and Git reads it. A Cookbook that a person clones
/// therefore brings its Recipes with it, and nothing in this application is
/// needed to understand the file.
pub const MODULES_FILE: &str = ".gitmodules";

/// How a Cookbook holds one Recipe.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Holding {
    /// The Cookbook keeps the Version that the person selected. This is
    /// the default, and it is what makes a Cookbook reproducible.
    #[default]
    Pinned,
    /// The Cookbook moves to each new Version of the Recipe.
    Following,
}

impl Holding {
    /// The value that the form carries.
    pub fn as_str(&self) -> &'static str {
        match self {
            Holding::Pinned => "pinned",
            Holding::Following => "following",
        }
    }

    /// Read the choice that a person made.
    ///
    /// Anything that is not the exact word gives Pinned, so a form with no
    /// choice at all keeps the Version.
    pub fn parse(value: &str) -> Self {
        match value {
            "following" => Holding::Following,
            _ => Holding::Pinned,
        }
    }
}

/// One Recipe reference, exactly as the Cookbook records it.
///
/// This is what Git holds, and it is the authority. Nothing here comes from
/// the index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    /// Where the Recipe sits inside the Cookbook. The name comes from the
    /// Recipe and it never changes afterwards.
    pub path: String,
    /// The address of the Recipe repository. Empty when the Cookbook
    /// records none, which is a state a direct push can make.
    pub url: String,
    /// The branch that the Cookbook follows, when it names one. A Pinned
    /// Recipe names none.
    pub follow: Option<String>,
    /// The exact Version that the Cookbook holds, when it records one.
    pub version: Option<String>,
}

impl Reference {
    /// Whether the Cookbook keeps this Version or follows the Recipe.
    pub fn holding(&self) -> Holding {
        match self.follow {
            Some(_) => Holding::Following,
            None => Holding::Pinned,
        }
    }
}

/// Read `.gitmodules`.
///
/// The file is written by Git and can also be written by a person, so this
/// reads what is there and never assumes the shape that this application
/// writes. A section with no `path` falls back to its own name, which is
/// what Git does.
pub fn read_references(bytes: &[u8]) -> Vec<Reference> {
    let text = String::from_utf8_lossy(bytes);
    let mut found: Vec<Reference> = Vec::new();
    let mut open: Option<Section> = None;

    for line in text.lines() {
        let line = line.trim();

        if line.starts_with('#') || line.starts_with(';') {
            continue;
        }

        if line.starts_with('[') {
            found.extend(open.take().map(Section::close));
            open = section_name(line).map(Section::new);
            continue;
        }

        let Some(section) = open.as_mut() else {
            continue;
        };
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"').to_string();

        match key.trim().to_ascii_lowercase().as_str() {
            "path" => section.path = value,
            "url" => section.url = value,
            "branch" if !value.is_empty() => section.follow = Some(value),
            _ => {}
        }
    }

    found.extend(open.take().map(Section::close));

    // Git reads the last section that names a path, so this does too.
    let mut kept: Vec<Reference> = Vec::new();
    for reference in found {
        if let Some(older) = kept.iter_mut().find(|held| held.path == reference.path) {
            *older = reference;
        } else {
            kept.push(reference);
        }
    }
    kept
}

/// One `[submodule "name"]` section while it is being read.
struct Section {
    name: String,
    path: String,
    url: String,
    follow: Option<String>,
}

impl Section {
    fn new(name: String) -> Self {
        Self {
            name,
            path: String::new(),
            url: String::new(),
            follow: None,
        }
    }

    /// A section with no `path` falls back to its own name, the way Git
    /// reads it.
    fn close(self) -> Reference {
        let path = match self.path.trim() {
            "" => self.name,
            named => named.to_string(),
        };

        Reference {
            path,
            url: self.url.trim().to_string(),
            follow: self.follow,
            version: None,
        }
    }
}

/// The name of a `[submodule "name"]` section, when the line names one.
fn section_name(line: &str) -> Option<String> {
    let inside = line.strip_prefix('[')?.strip_suffix(']')?.trim();
    let rest = inside.strip_prefix("submodule")?.trim();
    Some(rest.trim_matches('"').to_string())
}

/// The address that a Cookbook records for a Recipe.
///
/// This is the address that a person clones, so it is the public one. The
/// application never fetches through it: it asks Forgejo about the Recipe
/// by its owner and its name.
pub fn recipe_address(forgejo: &ForgejoClient, owner: &str, slug: &str) -> String {
    format!("{}.git", forgejo.web_url(&format!("{owner}/{slug}")))
}

/// The Recipe that an address names, when it names one of this installation.
///
/// A Recipe that was renamed, or that lives on another Forgejo, gives
/// nothing. The application never repairs such an address.
pub fn recipe_named_by(forgejo: &ForgejoClient, url: &str) -> Option<(String, String)> {
    let cleaned = url.trim();
    let cleaned = cleaned.strip_suffix(".git").unwrap_or(cleaned);

    for base in [forgejo.public_url(), forgejo.api_url()] {
        let base = base.trim_end_matches('/');
        if base.is_empty() {
            continue;
        }

        let Some(rest) = cleaned.strip_prefix(base) else {
            continue;
        };
        // The rest must begin a path. Without this `http://forge.test`
        // would also match `http://forge.test.elsewhere/sam/chili`.
        let Some(rest) = rest.strip_prefix('/') else {
            continue;
        };
        let Some((owner, slug)) = rest.split_once('/') else {
            continue;
        };

        if !owner.is_empty() && !slug.is_empty() && !slug.contains('/') {
            return Some((owner.to_string(), slug.to_string()));
        }
    }

    None
}

/// Where a Recipe sits inside a Cookbook.
///
/// The name comes from the Recipe itself, and it stays for as long as the
/// Recipe is in the Cookbook. A second Recipe of the same name, from
/// another person, gets the next free name. The application chooses that
/// name and asks nobody.
pub fn reference_path(taken: &[String], slug: &str) -> Option<String> {
    let base = crate::recipe::slug_with(slug, "recipe");

    (1..=MAX_SLUG_ATTEMPTS)
        .map(|attempt| slug_attempt(&base, attempt))
        .find(|candidate| {
            !taken
                .iter()
                .any(|held| held.eq_ignore_ascii_case(candidate))
        })
}

/// What a Cookbook records about the Recipes it holds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Contents {
    /// One entry for each Recipe that the Cookbook names or holds.
    pub references: Vec<Reference>,
    /// Whether the Cookbook itself answered. When it did not, a Recipe
    /// with no Version means that nothing was read, and not that the
    /// Cookbook records none. A diagnosis must never come from a question
    /// that was never answered.
    pub complete: bool,
}

/// Read every Recipe reference that a Cookbook holds.
///
/// Two things are read, and both come from Git through Forgejo. The file
/// says which Recipe each name points at, and the Cookbook itself says
/// which Version it holds. A name that is in one and not the other is a
/// state that a direct push can make, and it is reported rather than
/// hidden.
pub async fn references(
    forgejo: &ForgejoClient,
    token: Option<&Secret<String>>,
    repository: &Repository,
) -> Contents {
    let owner = &repository.owner.login;
    let slug = &repository.name;
    let branch = repository.branch();

    let declared = match forgejo
        .raw_file(token, owner, slug, branch, MODULES_FILE)
        .await
    {
        Ok(bytes) => read_references(&bytes),
        // A Cookbook with no Recipes holds no file at all, which is the
        // ordinary state and not a fault.
        Err(_) => Vec::new(),
    };

    let (held, complete) = match forgejo.list_root_entries(token, owner, slug, branch).await {
        Ok(entries) => (entries, true),
        Err(error) => {
            tracing::info!(%error, %owner, %slug, "cannot read what this Cookbook holds");
            (Vec::new(), false)
        }
    };

    let mut references: Vec<Reference> = declared
        .into_iter()
        .map(|mut reference| {
            reference.version = held
                .iter()
                .find(|entry| entry.is_reference() && entry.name == reference.path)
                .map(|entry| entry.sha.clone());
            reference
        })
        .collect();

    // A Recipe that the Cookbook holds but does not name. Git allows it and
    // this interface cannot show it as a Recipe, so it is named here and
    // repaired nowhere.
    for entry in held.iter().filter(|entry| entry.is_reference()) {
        if !references
            .iter()
            .any(|reference| reference.path == entry.name)
        {
            references.push(Reference {
                path: entry.name.clone(),
                url: String::new(),
                follow: None,
                version: Some(entry.sha.clone()),
            });
        }
    }

    Contents {
        references,
        complete,
    }
}

// ------------------------------------------------- what a person is shown

/// Shown for a Recipe that this person cannot read.
///
/// The message names nothing about the Recipe. Its title, its owner, and
/// its name are all facts that the person may not have.
///
/// It also names two causes, because Forgejo gives one answer for both. A
/// Recipe that a person may not see and a Recipe that is gone look the same
/// from here, and that is on purpose: an answer that told them apart would
/// say that the Recipe exists.
pub const UNAVAILABLE_MESSAGE: &str = "This Cookbook holds a Recipe that you cannot open. The Recipe is private, or it is not there any more.";

/// Shown for a reference that names no Recipe of this installation.
pub const FOREIGN_MESSAGE: &str = "This Cookbook holds a Recipe from a different Forgejo. CookLangHub cannot show it. Open the Cookbook in Forgejo to see the address.";

/// Shown for a reference that records no address at all.
pub const NO_ADDRESS_MESSAGE: &str =
    "This Cookbook holds a Recipe with no address. Open the Cookbook in Forgejo to see this state.";

/// Shown for a reference that records no Version.
pub const NO_VERSION_MESSAGE: &str = "This Cookbook names a Recipe but holds no Version of it. Open the Cookbook in Forgejo to see this state.";

/// Shown for a reference to something that carries no Recipe marker.
pub const NOT_A_RECIPE_MESSAGE: &str =
    "This Cookbook holds something that is not a Recipe. Open the Cookbook in Forgejo to see it.";

/// One Recipe of a Cookbook, as a page needs it.
///
/// A Recipe that this person cannot read carries no title, no owner, and no
/// name. Those are exactly the facts that would say what the Recipe is, so
/// the page cannot show them even by mistake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Held {
    /// Whether this person can open the Recipe.
    pub available: bool,
    /// Where the Recipe sits in the Cookbook. Empty when it is not
    /// available, because that name comes from the Recipe title.
    pub path: String,
    pub owner: String,
    pub slug: String,
    pub title: String,
    /// Whether Forgejo says that only named people can read the Recipe.
    ///
    /// This is false for a Recipe that is not available, because a Recipe
    /// that this person cannot open says nothing about itself at all.
    pub private: bool,
    /// Whether the Cookbook moves to each new Version of this Recipe.
    pub following: bool,
    /// Why the Recipe is not available, when it is not.
    pub problem: String,
    /// What is wrong with a Recipe that is there and readable, when
    /// something is. A Cookbook that follows a Recipe which no longer holds
    /// the Versions it follows says so here. Nothing is repaired.
    pub warning: String,
}

impl Held {
    /// A Recipe that is there and that this person cannot open.
    fn hidden(message: &str) -> Self {
        Self {
            available: false,
            path: String::new(),
            owner: String::new(),
            slug: String::new(),
            title: String::new(),
            private: false,
            following: false,
            problem: message.to_string(),
            warning: String::new(),
        }
    }

    /// The Recipe, as one line for a person who can already read it.
    fn named(&self) -> Named {
        Named {
            owner: self.owner.clone(),
            slug: self.slug.clone(),
            title: self.title.clone(),
        }
    }
}

/// Turn what a Cookbook records into what a person may see.
///
/// Forgejo answers, for this person, whether each Recipe can be read. The
/// index only supplies the title, and only for a Recipe that Forgejo has
/// already shown to this person.
///
/// The order is alphabetical by Recipe title. A Recipe that this person
/// cannot read has no title, so it comes last.
pub async fn held_recipes(
    pool: &SqlitePool,
    forgejo: &ForgejoClient,
    token: Option<&Secret<String>>,
    contents: &Contents,
) -> Vec<Held> {
    let references = &contents.references;
    let complete = contents.complete;

    // Ask Forgejo about each Recipe. This is the permission decision, and
    // Forgejo makes it once for each reference.
    let mut reads = Vec::with_capacity(references.len());
    for reference in references {
        reads.push(async move {
            if reference.url.trim().is_empty() {
                return Err(NO_ADDRESS_MESSAGE);
            }
            // A Cookbook that did not answer records nothing that this can
            // judge, so no Version here is silence and not a fault.
            if complete && reference.version.is_none() {
                return Err(NO_VERSION_MESSAGE);
            }

            let Some((owner, slug)) = recipe_named_by(forgejo, &reference.url) else {
                return Err(FOREIGN_MESSAGE);
            };

            match forgejo.repository_as(token, &owner, &slug).await {
                Ok(repository) if crate::index::is_recipe(&repository) => Ok(repository),
                Ok(_) => Err(NOT_A_RECIPE_MESSAGE),
                Err(_) => Err(UNAVAILABLE_MESSAGE),
            }
        });
    }

    let answers: Vec<Result<Repository, &'static str>> = futures::stream::iter(reads)
        .buffered(READ_CONCURRENCY)
        .collect()
        .await;

    // The title comes from the Recipe index, and only for a Recipe that
    // Forgejo just showed to this person.
    let readable: Vec<Repository> = answers.iter().filter_map(|a| a.clone().ok()).collect();
    let titles = crate::index::entries(pool, forgejo, token, &readable).await;

    let mut available: Vec<Held> = Vec::new();
    let mut hidden: Vec<Held> = Vec::new();

    for (reference, answer) in references.iter().zip(answers) {
        match answer {
            Ok(repository) => {
                let title = titles
                    .iter()
                    .find(|entry| entry.repository_id == repository.id)
                    .map(|entry| entry.title.clone())
                    .unwrap_or_else(|| repository.name.clone());

                available.push(Held {
                    available: true,
                    path: reference.path.clone(),
                    owner: repository.owner.login.clone(),
                    slug: repository.name.clone(),
                    title,
                    private: repository.private,
                    following: reference.holding() == Holding::Following,
                    problem: String::new(),
                    warning: still_follows(forgejo, token, &repository, reference).await,
                });
            }
            Err(message) => hidden.push(Held::hidden(message)),
        }
    }

    available.sort_by(|one, two| {
        one.title
            .to_lowercase()
            .cmp(&two.title.to_lowercase())
            .then_with(|| one.owner.to_lowercase().cmp(&two.owner.to_lowercase()))
    });

    available.extend(hidden);
    available
}

/// What is wrong with one Recipe that a Cookbook follows, when something is.
///
/// A Pinned Recipe follows nothing, so nothing here can be wrong with it.
/// A Following one names what it follows, and Forgejo answers whether the
/// Recipe still holds it. Only the answer "there is no such thing" reports a
/// state: an outage must never become a diagnostic about a Recipe.
async fn still_follows(
    forgejo: &ForgejoClient,
    token: Option<&Secret<String>>,
    repository: &Repository,
    reference: &Reference,
) -> String {
    let Some(branch) = reference.follow.as_deref() else {
        return String::new();
    };

    match forgejo
        .branch_exists(token, &repository.owner.login, &repository.name, branch)
        .await
    {
        Ok(true) => String::new(),
        Ok(false) => crate::automation::NOTHING_TO_FOLLOW_MESSAGE.to_string(),
        Err(error) => {
            tracing::info!(
                %error,
                owner = %repository.owner.login,
                slug = %repository.name,
                "cannot ask Forgejo what this Recipe holds"
            );
            String::new()
        }
    }
}

// ------------------------------------------------- who can read what
//
// A Cookbook and a Recipe are two repositories, and Forgejo keeps the
// permissions of each one apart. Access to a Cookbook is therefore never
// access to the Recipes in it, and this application adds nothing that would
// join the two. What it does instead is show the mismatch before it happens
// and offer a Forgejo grant for it.
//
// Every answer below comes from Forgejo, one question at a time. Nothing is
// worked out from a list that this application holds.

/// The Forgejo access mode that a Reader gets.
///
/// Reader is Forgejo Read, the same word that the Sharing area of a Recipe
/// hands out. A grant made here is an ordinary Forgejo permission and it is
/// visible, and removable, in Forgejo.
pub const READER_ACCESS: &str = "read";

/// The Forgejo access mode of a person who can do nothing with a repository.
const NO_ACCESS: &str = "none";

/// What Forgejo says about one person and one Recipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// Forgejo says that this person can read the Recipe.
    Open,
    /// Forgejo says that this person cannot read the Recipe.
    Shut,
    /// Forgejo did not answer. Nothing is known, so nothing is decided.
    ///
    /// Forgejo answers only a person who can administer the Recipe. A
    /// Cookbook that holds the private Recipe of somebody else therefore
    /// reaches this state, and the person is told so and sent to Forgejo.
    Silent,
}

/// Ask Forgejo what one person may do with one Recipe.
///
/// This is the permission decision and Forgejo makes it. The application
/// keeps no list of who can read what, and it works nothing out from the
/// index.
pub async fn reach(
    forgejo: &ForgejoClient,
    token: &Secret<String>,
    owner: &str,
    slug: &str,
    login: &str,
) -> Reach {
    match forgejo
        .repository_permission(token, owner, slug, login)
        .await
    {
        Ok(permission) if permission.permission.trim() == NO_ACCESS => Reach::Shut,
        Ok(_) => Reach::Open,
        Err(error) => {
            tracing::info!(%error, %owner, %slug, %login, "Forgejo did not say what this person can do");
            Reach::Silent
        }
    }
}

/// One Recipe or one Cookbook, as a line that a person reads.
///
/// Only somebody who can already read the thing sees one of these. Forgejo
/// showed them the title and the owner first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Named {
    pub owner: String,
    pub slug: String,
    pub title: String,
}

/// One person who can reach a Cookbook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sharer {
    pub login: String,
    pub name: String,
}

/// The Recipes of a Cookbook that one person cannot read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecipeGap {
    /// Forgejo says that this person cannot read these Recipes.
    pub shut: Vec<Named>,
    /// Forgejo did not answer about these Recipes.
    pub silent: Vec<Named>,
}

/// The people of a Cookbook who cannot read one Recipe.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PersonGap {
    /// Forgejo says that these people cannot read the Recipe.
    pub shut: Vec<Sharer>,
    /// Forgejo did not answer about these people.
    pub silent: Vec<Sharer>,
}

impl RecipeGap {
    /// Whether there is nothing to tell the person.
    pub fn is_empty(&self) -> bool {
        self.shut.is_empty() && self.silent.is_empty()
    }

    /// Every Recipe of the report, so that one grant covers all of them.
    pub fn each(&self) -> Vec<Named> {
        let mut all = self.shut.clone();
        all.extend(self.silent.iter().cloned());
        all
    }
}

impl PersonGap {
    /// Whether there is nothing to tell the person.
    pub fn is_empty(&self) -> bool {
        self.shut.is_empty() && self.silent.is_empty()
    }

    /// Every person of the report, so that one grant covers all of them.
    pub fn each(&self) -> Vec<Sharer> {
        let mut all = self.shut.clone();
        all.extend(self.silent.iter().cloned());
        all
    }
}

/// Which Recipes of a Cookbook one person cannot read.
///
/// A public Recipe is out of reach of nobody, so Forgejo is asked about a
/// private Recipe only. A Recipe that this person owns is out of reach of
/// them least of all, and Forgejo names the owner of each one, so those are
/// not asked about either.
///
/// `recipes` holds what the person who asks can read, and a Recipe that they
/// cannot read themselves is not in it: this report says nothing that
/// Forgejo did not show them first.
pub async fn recipes_out_of_reach(
    forgejo: &ForgejoClient,
    token: &Secret<String>,
    recipes: &[Held],
    login: &str,
) -> RecipeGap {
    let asked: Vec<&Held> = recipes
        .iter()
        .filter(|recipe| {
            recipe.available && recipe.private && !recipe.owner.eq_ignore_ascii_case(login)
        })
        .collect();

    let mut questions = Vec::with_capacity(asked.len());
    for recipe in &asked {
        questions.push(reach(forgejo, token, &recipe.owner, &recipe.slug, login));
    }

    let answers: Vec<Reach> = futures::stream::iter(questions)
        .buffered(READ_CONCURRENCY)
        .collect()
        .await;

    let mut gap = RecipeGap::default();
    for (recipe, answer) in asked.into_iter().zip(answers) {
        match answer {
            Reach::Open => {}
            Reach::Shut => gap.shut.push(recipe.named()),
            Reach::Silent => gap.silent.push(recipe.named()),
        }
    }
    gap
}

/// Which people of a Cookbook cannot read one Recipe.
///
/// A public Recipe gives an empty report, because Forgejo lets every user
/// read one. The owner of the Recipe gives none either: Forgejo names them,
/// and a Recipe is never out of reach of the person it belongs to.
///
/// For everybody else Forgejo answers one person at a time, and the answer
/// is used exactly as it is given.
pub async fn people_out_of_reach(
    forgejo: &ForgejoClient,
    token: &Secret<String>,
    recipe: &Repository,
    people: &[Sharer],
) -> PersonGap {
    if !recipe.private {
        return PersonGap::default();
    }

    let owner = &recipe.owner.login;
    let slug = &recipe.name;

    let asked: Vec<&Sharer> = people
        .iter()
        .filter(|person| !person.login.eq_ignore_ascii_case(owner))
        .collect();

    let mut questions = Vec::with_capacity(asked.len());
    for person in &asked {
        questions.push(reach(forgejo, token, owner, slug, &person.login));
    }

    let answers: Vec<Reach> = futures::stream::iter(questions)
        .buffered(READ_CONCURRENCY)
        .collect()
        .await;

    let mut gap = PersonGap::default();
    for (person, answer) in asked.into_iter().zip(answers) {
        match answer {
            Reach::Open => {}
            Reach::Shut => gap.shut.push(person.clone()),
            Reach::Silent => gap.silent.push(person.clone()),
        }
    }
    gap
}

/// Give one person Reader access to some Recipes.
///
/// Each grant is one ordinary Forgejo collaborator permission. Forgejo makes
/// it, Forgejo holds it, and Forgejo can take it away again. This
/// application records none of it.
///
/// A Recipe that Forgejo refuses is named, and it holds no other Recipe
/// back. The answer is one message for each refusal.
pub async fn grant_reader(
    forgejo: &ForgejoClient,
    token: &Secret<String>,
    login: &str,
    recipes: &[Named],
) -> Vec<String> {
    let mut refusals = Vec::new();

    for recipe in recipes {
        match forgejo
            .add_collaborator(token, &recipe.owner, &recipe.slug, login, READER_ACCESS)
            .await
        {
            Ok(()) => {
                tracing::info!(
                    owner = %recipe.owner,
                    slug = %recipe.slug,
                    %login,
                    "a person can read a Recipe of a Cookbook"
                );
            }
            Err(error) => {
                tracing::warn!(%error, owner = %recipe.owner, slug = %recipe.slug, %login, "Forgejo gave no access to a Recipe");
                refusals.push(format!(
                    "Forgejo did not give {login} access to {}. Open that Recipe in Forgejo to give access there.",
                    recipe.title
                ));
            }
        }
    }

    refusals
}

/// Give some people Reader access to one Recipe.
///
/// This is the same grant as [`grant_reader`], from the other side: one
/// Recipe and many people. Forgejo makes each one.
pub async fn grant_readers(
    forgejo: &ForgejoClient,
    token: &Secret<String>,
    recipe: &Named,
    people: &[Sharer],
) -> Vec<String> {
    let mut refusals = Vec::new();

    for person in people {
        match forgejo
            .add_collaborator(
                token,
                &recipe.owner,
                &recipe.slug,
                &person.login,
                READER_ACCESS,
            )
            .await
        {
            Ok(()) => {
                tracing::info!(
                    owner = %recipe.owner,
                    slug = %recipe.slug,
                    login = %person.login,
                    "a person can read a Recipe of a Cookbook"
                );
            }
            Err(error) => {
                tracing::warn!(%error, owner = %recipe.owner, slug = %recipe.slug, login = %person.login, "Forgejo gave no access to a Recipe");
                refusals.push(format!(
                    "Forgejo did not give {} access to {}. Open that Recipe in Forgejo to give access there.",
                    person.login, recipe.title
                ));
            }
        }
    }

    refusals
}

/// The public Cookbooks that hold one Recipe.
///
/// A Recipe that stops being public makes each of these partly unavailable.
/// The Cookbook stays readable and the entry stays visible, and the Recipe
/// behind it becomes one that most people cannot open.
///
/// Git holds the answer, so each Cookbook is read again here. The index
/// supplies the title only.
pub async fn public_cookbooks_with(
    pool: &SqlitePool,
    forgejo: &ForgejoClient,
    token: &Secret<String>,
    owner: &str,
    slug: &str,
) -> Vec<Named> {
    let found = match visible(forgejo, Some(token), Ownership::Anybody).await {
        Ok((found, _)) => found,
        Err(error) => {
            tracing::warn!(%error, "cannot ask Forgejo for the public Cookbooks");
            return Vec::new();
        }
    };

    let public: Vec<Repository> = found
        .into_iter()
        .filter(|repository| !repository.private)
        .collect();

    let mut reads = Vec::with_capacity(public.len());
    for repository in &public {
        reads.push(async move {
            let bytes = forgejo
                .raw_file(
                    Some(token),
                    &repository.owner.login,
                    &repository.name,
                    repository.branch(),
                    MODULES_FILE,
                )
                .await
                .ok()?;

            read_references(&bytes)
                .iter()
                .any(|reference| {
                    recipe_named_by(forgejo, &reference.url).is_some_and(|(named, name)| {
                        named.eq_ignore_ascii_case(owner) && name.eq_ignore_ascii_case(slug)
                    })
                })
                .then_some(repository)
        });
    }

    let answers: Vec<Option<&Repository>> = futures::stream::iter(reads)
        .buffered(READ_CONCURRENCY)
        .collect()
        .await;

    let holders: Vec<Repository> = answers.into_iter().flatten().cloned().collect();
    let entries = entries(pool, forgejo, Some(token), &holders).await;

    let mut named: Vec<Named> = entries
        .into_iter()
        .map(|entry| Named {
            owner: entry.owner,
            slug: entry.slug,
            title: entry.title,
        })
        .collect();

    named.sort_by(|one, two| {
        one.title
            .to_lowercase()
            .cmp(&two.title.to_lowercase())
            .then_with(|| one.owner.to_lowercase().cmp(&two.owner.to_lowercase()))
    });
    named
}

// ------------------------------------------- adding and removing a Recipe

#[derive(Debug, thiserror::Error)]
pub enum HoldError {
    #[error("select a Recipe to add")]
    NoRecipe,
    #[error("this Cookbook holds that Recipe already")]
    AlreadyHeld,
    #[error("that Recipe has no Version yet, so a Cookbook cannot hold it")]
    NoVersion,
    #[error("cannot find a free name for the Recipe in this Cookbook")]
    NoFreePath,
    #[error("this Cookbook does not hold that Recipe")]
    NotHeld,
    #[error("this Cookbook holds no Version of that Recipe")]
    NoHeldVersion,
    #[error("that Recipe has no Versions to follow")]
    NothingToFollow,
    #[error("that Recipe is not available")]
    Unavailable,
    #[error(transparent)]
    Forgejo(#[from] ForgejoError),
    #[error(transparent)]
    Git(#[from] GitError),
}

/// What a person asked for when they added a Recipe.
#[derive(Debug, Clone)]
pub struct AddRecipe<'a> {
    /// The Cookbook that gains the Recipe.
    pub cookbook: &'a Repository,
    /// The Recipe, as Forgejo reports it to the person who asked.
    pub recipe: &'a Repository,
    pub holding: Holding,
    /// The title that History records for the Recipe.
    pub title: &'a str,
    /// The domain Forgejo uses for a hidden address.
    pub noreply_domain: &'a str,
}

/// A Recipe that a Cookbook now holds.
#[derive(Debug, Clone)]
pub struct Added {
    /// Where the Recipe sits inside the Cookbook.
    pub path: String,
    /// The exact Version of the Recipe that the Cookbook holds.
    pub version: String,
    /// The Version of the Cookbook that this made.
    pub cookbook_version: String,
    /// What the Cookbook holds now, for every Recipe in it. The caller
    /// gives the automation the access this Cookbook needs from it, and
    /// asks Forgejo nothing a second time.
    pub references: Vec<Reference>,
}

/// Add a Recipe to a Cookbook.
///
/// Nothing is copied. The Cookbook gains one reference to the Recipe
/// repository and one Version of its own, and the Recipe repository is not
/// written to at all. It keeps its owner, its permissions, and every
/// Version it had.
pub async fn add_recipe(
    forgejo: &ForgejoClient,
    git: &dyn GitAdapter,
    token: &Secret<String>,
    user: &ForgejoUser,
    input: AddRecipe<'_>,
) -> Result<Added, HoldError> {
    let held = references(forgejo, Some(token), input.cookbook)
        .await
        .references;

    // One Recipe sits in one Cookbook at most once. A second reference to
    // it would make two entries that mean the same thing.
    let already = held.iter().any(|reference| {
        recipe_named_by(forgejo, &reference.url).is_some_and(|(owner, slug)| {
            owner.eq_ignore_ascii_case(&input.recipe.owner.login)
                && slug.eq_ignore_ascii_case(&input.recipe.name)
        })
    });
    if already {
        return Err(HoldError::AlreadyHeld);
    }

    let taken: Vec<String> = held
        .iter()
        .map(|reference| reference.path.clone())
        .collect();
    let path = reference_path(&taken, &input.recipe.name).ok_or(HoldError::NoFreePath)?;

    // The exact Version comes from Git, which is the authority for it.
    let recipe_url = forgejo.git_url(&input.recipe.full_name);
    let version = git
        .branch_head(&recipe_url, token, input.recipe.branch())
        .await?
        .ok_or(HoldError::NoVersion)?;

    let identity = create_recipe::identity_of(forgejo, token, user, input.noreply_domain).await;
    let address = recipe_address(forgejo, &input.recipe.owner.login, &input.recipe.name);

    let follow = match input.holding {
        Holding::Following => Some(input.recipe.branch()),
        Holding::Pinned => None,
    };

    let cookbook_version = git
        .write_reference(crate::git::WriteReference {
            remote_url: &forgejo.git_url(&input.cookbook.full_name),
            token,
            identity: &identity,
            branch: input.cookbook.branch(),
            message: &format!("Add {}", input.title),
            path: &path,
            url: &address,
            version: &version,
            follow,
        })
        .await?;

    let mut references = held;
    references.push(Reference {
        path: path.clone(),
        url: address,
        follow: follow.map(str::to_string),
        version: Some(version.clone()),
    });

    Ok(Added {
        path,
        version,
        cookbook_version,
        references,
    })
}

// -------------------------------------------------- Pinned and Following

/// What a person asked for when they changed how a Cookbook holds a Recipe.
#[derive(Debug, Clone)]
pub struct SetHolding<'a> {
    /// The Cookbook that changes. The Recipe does not change at all.
    pub cookbook: &'a Repository,
    /// Where the Recipe sits inside the Cookbook.
    pub path: &'a str,
    /// How the Cookbook is to hold it from now on.
    pub holding: Holding,
    /// The title that History records for the Recipe.
    pub title: &'a str,
    /// The domain Forgejo uses for a hidden address.
    pub noreply_domain: &'a str,
}

/// What the change came to.
#[derive(Debug, Clone)]
pub struct Switched {
    /// The Version of the Recipe that the Cookbook holds now.
    pub version: String,
    /// The Version of the Cookbook that this made. There is none when the
    /// Cookbook already held the Recipe this way.
    pub cookbook_version: Option<String>,
    /// What the Cookbook holds now, for every Recipe in it.
    pub references: Vec<Reference>,
}

/// Change one Recipe of a Cookbook between Pinned and Following.
///
/// Only the Cookbook changes. The Recipe repository is not written to, and
/// every other Cookbook that holds the same Recipe is left exactly as it is.
///
/// Following means current and future, so a Recipe that starts to follow
/// moves to the Version that the Recipe has now. Pinned means stop where
/// this Cookbook is, so a Recipe that stops following keeps the Version the
/// Cookbook holds and never reads the Recipe at all.
pub async fn set_holding(
    forgejo: &ForgejoClient,
    git: &dyn GitAdapter,
    token: &Secret<String>,
    user: &ForgejoUser,
    input: SetHolding<'_>,
) -> Result<Switched, HoldError> {
    let held = references(forgejo, Some(token), input.cookbook)
        .await
        .references;

    let Some(current) = held
        .iter()
        .find(|reference| reference.path == input.path)
        .cloned()
    else {
        return Err(HoldError::NotHeld);
    };

    if current.url.trim().is_empty() {
        return Err(HoldError::Unavailable);
    }

    let Some(recorded) = current.version.clone() else {
        return Err(HoldError::NoHeldVersion);
    };

    // The Cookbook holds it this way already, so nothing is written. A
    // Version that changes nothing must never reach History.
    if current.holding() == input.holding {
        return Ok(Switched {
            version: recorded,
            cookbook_version: None,
            references: held,
        });
    }

    let (follow, version) = match input.holding {
        Holding::Following => {
            let Some((owner, slug)) = recipe_named_by(forgejo, &current.url) else {
                return Err(HoldError::Unavailable);
            };

            // Forgejo says whether this person may read the Recipe, and it
            // says which Versions the Recipe publishes.
            let repository = forgejo
                .repository_as(Some(token), &owner, &slug)
                .await
                .map_err(|_| HoldError::Unavailable)?;
            let branch = repository.branch().to_string();

            let head = git
                .branch_head(&forgejo.git_url(&repository.full_name), token, &branch)
                .await?
                .ok_or(HoldError::NothingToFollow)?;

            (Some(branch), head)
        }
        // The Version that the Cookbook holds now is the Version it keeps.
        // The Recipe is not read for this.
        Holding::Pinned => (None, recorded),
    };

    let identity = create_recipe::identity_of(forgejo, token, user, input.noreply_domain).await;

    let message = match input.holding {
        Holding::Following => format!("Follow {}", input.title),
        Holding::Pinned => format!("Keep this Version of {}", input.title),
    };

    let cookbook_version = git
        .write_reference(crate::git::WriteReference {
            remote_url: &forgejo.git_url(&input.cookbook.full_name),
            token,
            identity: &identity,
            branch: input.cookbook.branch(),
            message: &message,
            path: input.path,
            url: &current.url,
            version: &version,
            follow: follow.as_deref(),
        })
        .await?;

    let references = held
        .into_iter()
        .map(|mut reference| {
            if reference.path == input.path {
                reference.follow = follow.clone();
                reference.version = Some(version.clone());
            }
            reference
        })
        .collect();

    Ok(Switched {
        version,
        cookbook_version: Some(cookbook_version),
        references,
    })
}

/// What a person asked for when they took a Recipe out.
#[derive(Debug, Clone)]
pub struct RemoveRecipe<'a> {
    /// The Cookbook that loses the Recipe.
    pub cookbook: &'a Repository,
    /// Where the Recipe sits inside the Cookbook.
    pub path: &'a str,
    /// The title that History records for the Recipe.
    pub title: &'a str,
    /// The domain Forgejo uses for a hidden address.
    pub noreply_domain: &'a str,
}

/// A Recipe that a Cookbook no longer holds.
#[derive(Debug, Clone)]
pub struct Removed {
    /// The Version of the Cookbook that this made.
    pub cookbook_version: String,
    /// What the Cookbook holds now, for every Recipe left in it.
    pub references: Vec<Reference>,
}

/// Take a Recipe out of a Cookbook.
///
/// Only the Cookbook changes. The Recipe repository is not written to, so
/// it keeps its owner, its permissions, and every Version it had. A Recipe
/// that leaves one Cookbook stays in every other Cookbook that holds it.
pub async fn remove_recipe(
    forgejo: &ForgejoClient,
    git: &dyn GitAdapter,
    token: &Secret<String>,
    user: &ForgejoUser,
    input: RemoveRecipe<'_>,
) -> Result<Removed, HoldError> {
    let held = references(forgejo, Some(token), input.cookbook)
        .await
        .references;

    if !held.iter().any(|reference| reference.path == input.path) {
        return Err(HoldError::NotHeld);
    }

    let identity = create_recipe::identity_of(forgejo, token, user, input.noreply_domain).await;

    let cookbook_version = git
        .remove_reference(crate::git::RemoveReference {
            remote_url: &forgejo.git_url(&input.cookbook.full_name),
            token,
            identity: &identity,
            branch: input.cookbook.branch(),
            message: &format!("Remove {}", input.title),
            path: input.path,
        })
        .await?;

    let references = held
        .into_iter()
        .filter(|reference| reference.path != input.path)
        .collect();

    Ok(Removed {
        cookbook_version,
        references,
    })
}

// -------------------------------------------------------------- the index

/// How many repositories the application asks Forgejo for at a time.
const SEARCH_PAGE: u32 = 50;

/// The most repositories that one question about Forgejo covers.
pub const MAX_REPOSITORIES: usize = 200;

/// How many Cookbooks the application reads at the same time.
const READ_CONCURRENCY: usize = 8;

/// The topic that a search asks Forgejo about.
///
/// Forgejo matches one topic per search, so the search asks for the wider
/// marker and the application then keeps only what carries every topic in
/// [`COOKBOOK_TOPICS`]. This is also what keeps a Recipe out of a Cookbook
/// list: both kinds answer this search, and only one kind survives the
/// filter.
const SEARCH_TOPIC: &str = COOKBOOK_TOPICS[0];

/// One Cookbook, as a list needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Indexed {
    pub repository_id: i64,
    pub owner: String,
    pub slug: String,
    /// The first heading of the README, or the slug when it has none.
    pub title: String,
    pub private: bool,
    /// What Forgejo last reported as the moment of the change.
    pub updated_at: String,
    /// The first words of the description, as plain text.
    pub summary: String,
}

/// What a refresh found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refreshed {
    /// The Cookbook is in the index and current.
    Indexed,
    /// Forgejo no longer has it, or no longer shows it to this application.
    Gone,
    /// The repository lost its topics, so it is not a Cookbook any more.
    NotACookbook,
}

/// What a reconciliation did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub scanned: usize,
    pub written: usize,
    pub removed: u64,
    pub failures: usize,
}

/// Whether a repository is a Cookbook that this application shows.
///
/// Both topics must be there. This is the opt-in marker, and removing
/// either one in Forgejo takes the repository out of the application.
pub fn is_cookbook(repository: &Repository) -> bool {
    repository.has_topics(&COOKBOOK_TOPICS)
}

/// Write one Cookbook into the index.
pub async fn put(pool: &SqlitePool, entry: &Indexed) -> Result<(), sqlx::Error> {
    // A rename gives an old name to a new repository, so a row that still
    // holds the name has to go before this one takes it.
    sqlx::query("DELETE FROM cookbook_index WHERE owner = ? AND slug = ? AND repository_id <> ?")
        .bind(&entry.owner)
        .bind(&entry.slug)
        .bind(entry.repository_id)
        .execute(pool)
        .await?;

    sqlx::query(
        "INSERT INTO cookbook_index (
             repository_id, owner, slug, title, private, updated_at,
             summary, indexed_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(repository_id) DO UPDATE SET
             owner      = excluded.owner,
             slug       = excluded.slug,
             title      = excluded.title,
             private    = excluded.private,
             updated_at = excluded.updated_at,
             summary    = excluded.summary,
             indexed_at = excluded.indexed_at",
    )
    .bind(entry.repository_id)
    .bind(&entry.owner)
    .bind(&entry.slug)
    .bind(&entry.title)
    .bind(i64::from(entry.private))
    .bind(&entry.updated_at)
    .bind(&entry.summary)
    .bind(now())
    .execute(pool)
    .await?;

    Ok(())
}

/// Take one Cookbook out of the index, by the name it had.
pub async fn forget(pool: &SqlitePool, owner: &str, slug: &str) -> Result<u64, sqlx::Error> {
    let removed = sqlx::query("DELETE FROM cookbook_index WHERE owner = ? AND slug = ?")
        .bind(owner)
        .bind(slug)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(removed)
}

/// Take one Cookbook out of the index, by the identifier Forgejo gave it.
pub async fn forget_repository(pool: &SqlitePool, repository_id: i64) -> Result<u64, sqlx::Error> {
    let removed = sqlx::query("DELETE FROM cookbook_index WHERE repository_id = ?")
        .bind(repository_id)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(removed)
}

/// How many Cookbooks the index holds.
pub async fn count(pool: &SqlitePool) -> Result<i64, sqlx::Error> {
    let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM cookbook_index")
        .fetch_one(pool)
        .await?;
    Ok(row.0)
}

/// Read one Cookbook out of the index.
pub async fn get(
    pool: &SqlitePool,
    owner: &str,
    slug: &str,
) -> Result<Option<Indexed>, sqlx::Error> {
    let row: Option<Row> = sqlx::query_as(
        "SELECT repository_id, owner, slug, title, private, updated_at, summary
         FROM cookbook_index WHERE owner = ? AND slug = ?",
    )
    .bind(owner)
    .bind(slug)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(Indexed::from))
}

/// Read every Cookbook in the index. Diagnostics and tests use this.
pub async fn all(pool: &SqlitePool) -> Result<Vec<Indexed>, sqlx::Error> {
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT repository_id, owner, slug, title, private, updated_at, summary
         FROM cookbook_index ORDER BY owner, slug",
    )
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(Indexed::from).collect())
}

/// The stored shape of one row.
type Row = (i64, String, String, String, i64, String, String);

impl From<Row> for Indexed {
    fn from(row: Row) -> Self {
        let (repository_id, owner, slug, title, private, updated_at, summary) = row;
        Self {
            repository_id,
            owner,
            slug,
            title,
            private: private != 0,
            updated_at,
            summary,
        }
    }
}

/// Read the rows for a set of repositories, keyed by their identifier.
async fn known(
    pool: &SqlitePool,
    repositories: &[Repository],
) -> Result<HashMap<i64, Indexed>, sqlx::Error> {
    if repositories.is_empty() {
        return Ok(HashMap::new());
    }

    // The only part of this text that varies is how many `?` it holds, and
    // every value still arrives as a bound parameter.
    let places = vec!["?"; repositories.len()].join(",");
    let sql = format!(
        "SELECT repository_id, owner, slug, title, private, updated_at, summary
         FROM cookbook_index WHERE repository_id IN ({places})"
    );

    let mut query = sqlx::query_as::<_, Row>(sqlx::AssertSqlSafe(sql));
    for repository in repositories {
        query = query.bind(repository.id);
    }

    let rows = query.fetch_all(pool).await?;
    Ok(rows
        .into_iter()
        .map(Indexed::from)
        .map(|entry| (entry.repository_id, entry))
        .collect())
}

/// What the README of a Cookbook holds, as a page needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Readme {
    pub title: Option<String>,
    /// The description, as the raw Markdown that a person wrote.
    pub description: String,
    /// Messages about a state that this interface cannot show properly.
    pub problems: Vec<String>,
}

/// Shown when the stored README is not text that the application can read.
pub const NOT_TEXT_MESSAGE: &str = "This Cookbook description is not UTF-8 text. Each character that could not be read appears below as a replacement mark. Open the Cookbook in Forgejo to see the exact content.";

/// Shown when the README is larger than the application shows.
pub const TOO_LARGE_MESSAGE: &str = "This Cookbook has a README.md file that is larger than 1 MB, so CookLangHub does not show the description. Open the Cookbook in Forgejo to read it.";

/// Shown when the README names no title.
pub const NO_TITLE_MESSAGE: &str =
    "This Cookbook has no heading in README.md, so its name is used as the title.";

/// Shown when the Cookbook holds no README at all.
pub const NO_README_MESSAGE: &str = "This Cookbook has no README.md file, so it has no title and no description. Open the Cookbook in Forgejo to add one.";

/// Read the README bytes as a Cookbook.
///
/// Git accepts any bytes, so a direct push can put something that is not
/// text in the file, and a file can be of any size. Each of these states is
/// named rather than repaired.
pub fn read_readme(bytes: &[u8]) -> Readme {
    let mut problems = Vec::new();

    if bytes.len() > MAX_README_BYTES {
        return Readme {
            title: None,
            description: String::new(),
            problems: vec![TOO_LARGE_MESSAGE.to_string()],
        };
    }

    if std::str::from_utf8(bytes).is_err() {
        problems.push(NOT_TEXT_MESSAGE.to_string());
    }

    let source = String::from_utf8_lossy(bytes);
    let parts = split(&source);

    if parts.title.is_none() {
        problems.push(NO_TITLE_MESSAGE.to_string());
    }

    Readme {
        title: parts.title,
        description: parts.description,
        problems,
    }
}

/// Read a Cookbook and build its index entry.
///
/// The title and the summary come from `README.md`, which is the only place
/// that holds them. Nothing here writes.
async fn read_entry(
    forgejo: &ForgejoClient,
    token: Option<&Secret<String>>,
    repository: &Repository,
) -> Result<Indexed, ForgejoError> {
    let bytes = forgejo
        .raw_file(
            token,
            &repository.owner.login,
            &repository.name,
            repository.branch(),
            README_FILE,
        )
        .await?;

    let readme = read_readme(&bytes);

    Ok(Indexed {
        repository_id: repository.id,
        owner: repository.owner.login.clone(),
        slug: repository.name.clone(),
        title: readme.title.unwrap_or_else(|| repository.name.clone()),
        private: repository.private,
        updated_at: repository.updated_at.clone(),
        summary: summary(&readme.description),
    })
}

/// What a card shows when the Cookbook itself could not be read.
///
/// The slug stands in for the title. The row is not written, so the next
/// attempt reads the Cookbook again instead of keeping a poor title forever.
fn placeholder(repository: &Repository) -> Indexed {
    Indexed {
        repository_id: repository.id,
        owner: repository.owner.login.clone(),
        slug: repository.name.clone(),
        title: repository.name.clone(),
        private: repository.private,
        updated_at: repository.updated_at.clone(),
        summary: String::new(),
    }
}

/// Whether the index entry still matches what Forgejo reports.
fn is_current(entry: &Indexed, repository: &Repository) -> bool {
    entry.updated_at == repository.updated_at
        && entry.owner == repository.owner.login
        && entry.slug == repository.name
        && entry.private == repository.private
}

/// Give back one index entry for each repository, in the same order.
///
/// A Cookbook whose row is missing or out of date is read from Forgejo and
/// written into the index here. This is also what makes the table
/// disposable: an empty index costs one read for each Cookbook on the next
/// page, and nothing else.
pub async fn entries(
    pool: &SqlitePool,
    forgejo: &ForgejoClient,
    token: Option<&Secret<String>>,
    repositories: &[Repository],
) -> Vec<Indexed> {
    let known = match known(pool, repositories).await {
        Ok(known) => known,
        Err(error) => {
            tracing::warn!(%error, "cannot read the Cookbook index");
            HashMap::new()
        }
    };

    let stale: Vec<&Repository> = repositories
        .iter()
        .filter(|repository| {
            known
                .get(&repository.id)
                .is_none_or(|entry| !is_current(entry, repository))
        })
        .collect();

    // A Cookbook that cannot be read leaves the index alone, so a good title
    // is never replaced by a slug because Forgejo was busy for a moment.
    let mut reads = Vec::with_capacity(stale.len());
    for repository in stale {
        reads.push(async move {
            match read_entry(forgejo, token, repository).await {
                Ok(entry) => Some(entry),
                Err(error) => {
                    tracing::info!(
                        %error,
                        owner = %repository.owner.login,
                        slug = %repository.name,
                        "cannot read this Cookbook for the index"
                    );
                    None
                }
            }
        });
    }

    let read: Vec<Option<Indexed>> = futures::stream::iter(reads)
        .buffer_unordered(READ_CONCURRENCY)
        .collect()
        .await;

    let mut fresh: HashMap<i64, Indexed> = HashMap::new();
    for entry in read.into_iter().flatten() {
        if let Err(error) = put(pool, &entry).await {
            tracing::warn!(%error, "cannot write the Cookbook index");
        }
        fresh.insert(entry.repository_id, entry);
    }

    repositories
        .iter()
        .map(|repository| {
            fresh
                .get(&repository.id)
                .or_else(|| known.get(&repository.id))
                .cloned()
                .unwrap_or_else(|| placeholder(repository))
        })
        .collect()
}

/// Bring one Cookbook up to date, or take it out of the index.
///
/// The webhook calls this after Forgejo reports a change. It asks Forgejo
/// what the repository looks like now rather than trusting the message,
/// because the message describes a moment that has already passed.
pub async fn refresh(
    pool: &SqlitePool,
    forgejo: &ForgejoClient,
    token: Option<&Secret<String>>,
    owner: &str,
    slug: &str,
) -> Refreshed {
    let repository = match forgejo.repository_as(token, owner, slug).await {
        Ok(repository) => repository,
        Err(error) => {
            tracing::info!(%error, %owner, %slug, "Forgejo does not show this Cookbook");
            forget_quietly(pool, owner, slug).await;
            return Refreshed::Gone;
        }
    };

    if !is_cookbook(&repository) {
        forget_quietly(pool, owner, slug).await;
        return Refreshed::NotACookbook;
    }

    match read_entry(forgejo, token, &repository).await {
        Ok(entry) => {
            if let Err(error) = put(pool, &entry).await {
                tracing::warn!(%error, %owner, %slug, "cannot write the Cookbook index");
            }
            Refreshed::Indexed
        }
        Err(error) => {
            // The Cookbook exists but could not be read. Keep whatever the
            // index already holds rather than replace a good title with a
            // slug, and let the next reconciliation try again.
            tracing::info!(%error, %owner, %slug, "cannot read this Cookbook for the index");
            Refreshed::Indexed
        }
    }
}

async fn forget_quietly(pool: &SqlitePool, owner: &str, slug: &str) {
    if let Err(error) = forget(pool, owner, slug).await {
        tracing::warn!(%error, %owner, %slug, "cannot remove this Cookbook from the index");
    }
}

// -------------------------------------------------------------- searching

/// Ask Forgejo which Cookbooks a credential may see.
///
/// This is the permission decision, and Forgejo makes it. The answer is
/// capped at [`MAX_REPOSITORIES`], and the second value says whether the cap
/// cut the answer short.
pub async fn visible(
    forgejo: &ForgejoClient,
    token: Option<&Secret<String>>,
    ownership: Ownership,
) -> Result<(Vec<Repository>, bool), ForgejoError> {
    let mut found = Vec::new();
    let mut page = 1;

    loop {
        let batch = forgejo
            .search_repositories(
                token,
                &RepositoryQuery {
                    topic: SEARCH_TOPIC,
                    ownership,
                    page,
                    limit: SEARCH_PAGE,
                },
            )
            .await?;

        let complete = batch.len() < SEARCH_PAGE as usize;

        // Forgejo matches one topic. Keeping only what carries every topic
        // is what makes a Recipe invisible here.
        found.extend(batch.into_iter().filter(is_cookbook));

        if complete {
            return Ok((found, false));
        }
        if found.len() >= MAX_REPOSITORIES {
            found.truncate(MAX_REPOSITORIES);
            return Ok((found, true));
        }
        page += 1;
    }
}

/// Ask Forgejo which Cookbooks a person made a Favorite.
///
/// A Favorite is a Forgejo star, and Forgejo holds it. The application keeps
/// no list of its own, so a star that a person adds in Forgejo counts here
/// at once.
pub async fn favorites(
    forgejo: &ForgejoClient,
    token: &Secret<String>,
) -> Result<(Vec<Repository>, bool), ForgejoError> {
    let mut found = Vec::new();
    let mut page = 1;

    loop {
        let batch = forgejo
            .starred_repositories(token, page, SEARCH_PAGE)
            .await?;

        let complete = batch.len() < SEARCH_PAGE as usize;
        found.extend(batch.into_iter().filter(is_cookbook));

        if complete {
            return Ok((found, false));
        }
        if found.len() >= MAX_REPOSITORIES {
            found.truncate(MAX_REPOSITORIES);
            return Ok((found, true));
        }
        page += 1;
    }
}

// ---------------------------------------------------------- reconciliation

/// Read Forgejo again, and make the Cookbook index match.
///
/// This runs when the application starts. It is safe at any moment, and it
/// changes nothing in Forgejo and nothing in Git: every call it makes is a
/// read. This is the proof that each row is rebuildable.
pub async fn reconcile(pool: &SqlitePool, cipher: &Cipher, forgejo: &ForgejoClient) -> Report {
    let mut report = Report::default();

    sweep(
        pool,
        forgejo,
        None,
        Ownership::Anybody,
        Prune::Public,
        &mut report,
    )
    .await;

    let people = match crate::session::signed_in_people(pool, cipher).await {
        Ok(people) => people,
        Err(error) => {
            tracing::warn!(%error, "cannot read the sessions for the reconciliation");
            Vec::new()
        }
    };

    for person in people {
        sweep(
            pool,
            forgejo,
            Some(&person.token),
            Ownership::ReachableBy(person.forgejo_user_id),
            Prune::Owner(person.login.clone()),
            &mut report,
        )
        .await;
    }

    tracing::info!(
        scanned = report.scanned,
        written = report.written,
        removed = report.removed,
        failures = report.failures,
        "the Cookbook index matches Forgejo again"
    );

    // The Diagnostics page reports when this last ran and what it found.
    crate::diagnostics::record_sweep(
        pool,
        crate::diagnostics::COOKBOOK_INDEX,
        report.scanned as i64,
        report.written as i64,
        report.removed as i64,
        report.failures as i64,
    )
    .await;

    report
}

/// What a finished sweep is allowed to remove.
enum Prune {
    /// Everything public that Forgejo did not name.
    Public,
    /// Everything of this person that Forgejo did not name.
    Owner(String),
}

async fn sweep(
    pool: &SqlitePool,
    forgejo: &ForgejoClient,
    token: Option<&Secret<String>>,
    ownership: Ownership,
    prune: Prune,
    report: &mut Report,
) {
    let (repositories, truncated) = match visible(forgejo, token, ownership).await {
        Ok(found) => found,
        Err(error) => {
            tracing::warn!(%error, "cannot ask Forgejo for the Cookbooks");
            report.failures += 1;
            return;
        }
    };

    report.scanned += repositories.len();

    for repository in &repositories {
        match read_entry(forgejo, token, repository).await {
            Ok(entry) => match put(pool, &entry).await {
                Ok(()) => report.written += 1,
                Err(error) => {
                    tracing::warn!(%error, "cannot write the Cookbook index");
                    report.failures += 1;
                }
            },
            Err(error) => {
                tracing::info!(
                    %error,
                    owner = %repository.owner.login,
                    slug = %repository.name,
                    "cannot read this Cookbook for the index"
                );
                report.failures += 1;
            }
        }
    }

    // A short answer is not proof that nothing else exists, so a sweep that
    // hit the cap removes nothing.
    if truncated {
        tracing::warn!("Forgejo has more Cookbooks than one sweep covers; nothing was removed");
        return;
    }

    let seen: Vec<i64> = repositories.iter().map(|r| r.id).collect();
    match prune_missing(pool, &prune, &seen).await {
        Ok(removed) => report.removed += removed,
        Err(error) => {
            tracing::warn!(%error, "cannot remove what Forgejo no longer has");
            report.failures += 1;
        }
    }
}

/// Remove the rows in this scope that Forgejo did not name.
async fn prune_missing(pool: &SqlitePool, prune: &Prune, seen: &[i64]) -> Result<u64, sqlx::Error> {
    // `NOT IN ()` cannot be written, and `NOT IN (NULL)` matches nothing at
    // all, so a sweep that found nothing drops the clause instead.
    let keep = if seen.is_empty() {
        String::new()
    } else {
        format!(
            " AND repository_id NOT IN ({})",
            vec!["?"; seen.len()].join(",")
        )
    };

    // A row that says private cannot be judged by a public sweep, because a
    // sweep with no credential never sees one.
    //
    // The only part of this text that varies is how many `?` it holds, and
    // every value still arrives as a bound parameter.
    let sql = match prune {
        Prune::Public => format!("DELETE FROM cookbook_index WHERE private = 0{keep}"),
        Prune::Owner(_) => format!("DELETE FROM cookbook_index WHERE owner = ?{keep}"),
    };

    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
    if let Prune::Owner(login) = prune {
        query = query.bind(login);
    }
    for id in seen {
        query = query.bind(id);
    }

    Ok(query.execute(pool).await?.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forgejo::RepositoryOwner;

    fn repository(id: i64, owner: &str, name: &str, topics: &[&str]) -> Repository {
        Repository {
            id,
            name: name.to_string(),
            full_name: format!("{owner}/{name}"),
            html_url: String::new(),
            clone_url: String::new(),
            default_branch: "main".to_string(),
            private: false,
            empty: false,
            has_issues: true,
            topics: topics.iter().map(|t| t.to_string()).collect(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            owner: RepositoryOwner {
                id: 1,
                login: owner.to_string(),
            },
        }
    }

    fn entry(id: i64, owner: &str, slug: &str, title: &str) -> Indexed {
        Indexed {
            repository_id: id,
            owner: owner.to_string(),
            slug: slug.to_string(),
            title: title.to_string(),
            private: false,
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            summary: "Everything for a long evening.".to_string(),
        }
    }

    // ------------------------------------------------------- the marker

    #[test]
    fn a_cookbook_needs_both_of_its_topics() {
        assert!(is_cookbook(&repository(
            1,
            "sam",
            "sunday",
            &["cooklang", "cookbook"]
        )));
        assert!(!is_cookbook(&repository(1, "sam", "sunday", &["cooklang"])));
        assert!(!is_cookbook(&repository(1, "sam", "sunday", &["cookbook"])));
        assert!(!is_cookbook(&repository(1, "sam", "sunday", &[])));
    }

    #[test]
    fn a_recipe_is_never_a_cookbook_and_a_cookbook_is_never_a_recipe() {
        // The two markers are what keep the two kinds out of each other's
        // lists, so this is the rule that both areas rest on.
        let recipe = repository(1, "sam", "chili", &["cooklang", "recipe"]);
        let cookbook = repository(2, "sam", "sunday", &["cooklang", "cookbook"]);

        assert!(!is_cookbook(&recipe));
        assert!(crate::index::is_recipe(&recipe));

        assert!(is_cookbook(&cookbook));
        assert!(!crate::index::is_recipe(&cookbook));
    }

    #[test]
    fn a_topic_matches_whatever_case_it_carries() {
        assert!(is_cookbook(&repository(
            1,
            "sam",
            "sunday",
            &["CookLang", "Cookbook", "dinner"]
        )));
    }

    // ------------------------------------------------------- the README

    #[test]
    fn the_readme_carries_the_title_as_its_first_heading() {
        let out = readme("Sunday Dinners", "Everything for a long evening.");
        assert!(out.starts_with("# Sunday Dinners\n"));
        assert!(out.contains("Everything for a long evening."));
    }

    #[test]
    fn a_cookbook_with_no_description_still_has_a_title() {
        let out = readme("Sunday Dinners", "   ");
        assert_eq!(out, "# Sunday Dinners\n");
    }

    #[test]
    fn the_title_survives_a_round_trip_through_the_readme() {
        for title in ["Sunday Dinners", "Chili & Co.", "Pfannekuchen für Gäste"] {
            let parts = split(&readme(title, "Some words."));
            assert_eq!(parts.title.as_deref(), Some(title));
            assert_eq!(parts.description, "Some words.");
        }
    }

    #[test]
    fn a_title_on_two_lines_becomes_one_line() {
        // A heading is one line. Without this the second line would leave
        // the heading and become the description.
        let out = readme("Sunday\nDinners", "Some words.");
        assert_eq!(split(&out).title.as_deref(), Some("Sunday Dinners"));
        assert_eq!(split(&out).description, "Some words.");
    }

    #[test]
    fn the_first_heading_is_the_title_and_a_later_one_is_not() {
        let parts = split("# Sunday Dinners\n\nSome words.\n\n# Not the title\n");
        assert_eq!(parts.title.as_deref(), Some("Sunday Dinners"));
        assert!(parts.description.contains("# Not the title"));
    }

    #[test]
    fn an_underlined_heading_is_also_a_title() {
        let parts = split("Sunday Dinners\n==============\n\nSome words.\n");
        assert_eq!(parts.title.as_deref(), Some("Sunday Dinners"));
        assert_eq!(parts.description, "Some words.");
    }

    #[test]
    fn a_readme_with_no_heading_gives_no_title_and_keeps_every_word() {
        let parts = split("Just some words.\n");
        assert_eq!(parts.title, None);
        assert_eq!(parts.description, "Just some words.");
    }

    #[test]
    fn words_written_above_the_heading_stay_in_the_description() {
        // A README that this application did not write can put content
        // there, and nothing may be lost.
        let parts = split("A note.\n\n# Sunday Dinners\n\nSome words.\n");
        assert_eq!(parts.title.as_deref(), Some("Sunday Dinners"));
        assert!(parts.description.contains("A note."));
        assert!(parts.description.contains("Some words."));
    }

    #[test]
    fn a_second_level_heading_is_not_the_title() {
        let parts = split("## Sunday Dinners\n\nSome words.\n");
        assert_eq!(parts.title, None);
        assert!(parts.description.contains("## Sunday Dinners"));
    }

    #[test]
    fn a_readme_that_is_too_large_is_named_and_not_shown() {
        let large = vec![b'a'; MAX_README_BYTES + 1];
        let readme = read_readme(&large);

        assert!(readme.description.is_empty());
        assert_eq!(readme.problems, vec![TOO_LARGE_MESSAGE.to_string()]);
    }

    #[test]
    fn a_readme_that_is_not_text_is_named() {
        let readme = read_readme(&[0xff, 0xfe, b'#', b' ', b'A']);
        assert!(readme.problems.iter().any(|p| p == NOT_TEXT_MESSAGE));
    }

    // ------------------------------------------------------ the Markdown

    #[test]
    fn ordinary_markdown_becomes_ordinary_html() {
        let html = render("A **bold** word and a [link](https://example.test).");
        assert!(html.contains("<strong>bold</strong>"));
        assert!(html.contains("href=\"https://example.test\""));
    }

    #[test]
    fn a_list_and_a_table_are_understood() {
        assert!(render("- one\n- two\n").contains("<li>one</li>"));
        assert!(render("| a | b |\n|---|---|\n| 1 | 2 |\n").contains("<table>"));
    }

    /// The tags that a written document must never put on the page.
    ///
    /// A written `<` reaches the page as `&lt;`, so a test that looks for
    /// the word `onerror` finds the escaped text and proves nothing. What
    /// matters is that no tag begins.
    fn opens_a_tag(html: &str, name: &str) -> bool {
        html.contains(&format!("<{name}"))
    }

    #[test]
    fn a_written_tag_never_becomes_a_tag_on_the_page() {
        for source in [
            "<script>alert(1)</script>",
            "<img src=x onerror=alert(1)>",
            "<iframe src=\"https://example.test\"></iframe>",
            "Some words <b>and a tag</b>.",
            "<div onclick=\"alert(1)\">text</div>",
            "<style>body{display:none}</style>",
            "<a href=\"javascript:alert(1)\">click</a>",
        ] {
            let html = render(source);
            for tag in ["script", "img", "iframe", "b", "div", "style", "a "] {
                assert!(
                    !opens_a_tag(&html, tag),
                    "`{source}` reached the page as `{html}`"
                );
            }
            assert!(
                html.contains("&lt;"),
                "`{source}` must reach the page as the text that was written, not as `{html}`"
            );
        }
    }

    #[test]
    fn a_link_that_runs_a_script_cannot_run_it() {
        for source in [
            "[click](javascript:alert(1))",
            "[click](JavaScript:alert(1))",
            "[click](java\tscript:alert(1))",
            "[click](data:text/html,<script>alert(1)</script>)",
            "[click](vbscript:msgbox(1))",
            "[click](<javascript:alert(1)>)",
        ] {
            let html = render(source).to_lowercase();
            for scheme in ["javascript:", "vbscript:", "data:"] {
                assert!(
                    !html.contains(&format!("href=\"{scheme}")),
                    "`{source}` reached the page as `{html}`"
                );
            }
        }
    }

    #[test]
    fn a_blocked_address_becomes_the_one_that_goes_nowhere() {
        let html = render("[click](javascript:alert(1))");
        assert!(html.contains(&format!("href=\"{BLOCKED_URL}\"")), "{html}");
    }

    #[test]
    fn an_image_that_runs_a_script_loses_its_address() {
        let html = render("![a](javascript:alert(1))");
        assert!(html.contains(&format!("src=\"{BLOCKED_URL}\"")), "{html}");
    }

    #[test]
    fn an_ordinary_address_is_kept() {
        for address in [
            "https://example.test/a",
            "http://example.test/a",
            "HTTPS://example.test/a",
            "mailto:sam@example.test",
            "/cookbooks/sam/sunday",
            "sunday.md",
            "#a-heading",
        ] {
            assert!(is_safe_url(address), "`{address}` must be kept");
        }
    }

    #[test]
    fn an_address_that_runs_something_is_refused() {
        for address in [
            "javascript:alert(1)",
            "JavaScript:alert(1)",
            // A browser removes the whitespace and the control character
            // before it reads the scheme, so this test does too.
            "java\tscript:alert(1)",
            "java script:alert(1)",
            "java\u{0}script:alert(1)",
            "  javascript:alert(1)",
            "vbscript:msgbox(1)",
            "data:text/html,<script>alert(1)</script>",
            "file:///etc/passwd",
        ] {
            assert!(!is_safe_url(address), "`{address}` must be refused");
        }
    }

    #[test]
    fn a_quotation_mark_cannot_leave_an_attribute() {
        let html = render("[click](https://example.test/\"onmouseover=\"alert(1))");
        assert!(!html.contains("onmouseover=\"alert"));
    }

    // ------------------------------------------------------- the summary

    #[test]
    fn the_summary_is_plain_text() {
        let out = summary("A **bold** word and a [link](https://example.test).");
        assert_eq!(out, "A bold word and a link.");
    }

    #[test]
    fn a_long_summary_stops() {
        let out = summary(&"word ".repeat(200));
        assert!(out.chars().count() <= SUMMARY_CHARS + 1);
        assert!(out.ends_with('…'));
    }

    // ---------------------------------------------------------- the slug

    #[test]
    fn a_slug_comes_from_the_title() {
        assert_eq!(slug("Sunday Dinners"), "sunday-dinners");
        assert_eq!(slug("Pfannekuchen für Gäste"), "pfannekuchen-fuer-gaeste");
    }

    #[test]
    fn a_title_with_no_letters_falls_back_to_the_word_cookbook() {
        // A Recipe falls back to `recipe` here. A Cookbook must not.
        assert_eq!(slug("!!!"), "cookbook");
        assert_eq!(slug(""), "cookbook");
    }

    // ------------------------------------------------------- the Recipes

    /// A Forgejo that a browser and this server reach at two addresses,
    /// which is what the bundled stack really looks like.
    fn client() -> ForgejoClient {
        ForgejoClient::with_urls("http://forgejo:3000", "http://localhost:3000")
            .expect("cannot build the Forgejo client")
    }

    #[test]
    fn a_recipe_reference_is_read_back_exactly_as_git_writes_it() {
        // This is the shape that `git submodule add` writes, and the shape
        // that the adapter writes through `git config`.
        let file =
            "[submodule \"chili\"]\n\tpath = chili\n\turl = http://localhost:3000/sam/chili.git\n";
        let held = read_references(file.as_bytes());

        assert_eq!(held.len(), 1);
        assert_eq!(held[0].path, "chili");
        assert_eq!(held[0].url, "http://localhost:3000/sam/chili.git");
        assert_eq!(held[0].follow, None);
        assert_eq!(held[0].holding(), Holding::Pinned);
    }

    #[test]
    fn a_reference_that_names_a_branch_follows_the_recipe() {
        let file = "[submodule \"chili\"]\n\tpath = chili\n\turl = http://localhost:3000/sam/chili.git\n\tbranch = main\n";
        let held = read_references(file.as_bytes());

        assert_eq!(held[0].follow.as_deref(), Some("main"));
        assert_eq!(held[0].holding(), Holding::Following);
    }

    #[test]
    fn a_pinned_reference_names_no_branch_at_all() {
        // This is the difference between the two, and it is the whole of
        // it. A Pinned Recipe stays on the Version it was added at because
        // nothing says which branch to read.
        let file = "[submodule \"chili\"]\n\tpath = chili\n\turl = http://x/sam/chili.git\n";
        assert!(read_references(file.as_bytes())[0].follow.is_none());
    }

    #[test]
    fn several_recipes_are_all_read() {
        let file = concat!(
            "[submodule \"chili\"]\n\tpath = chili\n\turl = http://x/sam/chili.git\n",
            "[submodule \"toast\"]\n\tpath = toast\n\turl = http://x/alex/toast.git\n\tbranch = main\n",
        );
        let held = read_references(file.as_bytes());

        assert_eq!(held.len(), 2);
        assert_eq!(held[0].path, "chili");
        assert_eq!(held[1].path, "toast");
        assert_eq!(held[1].follow.as_deref(), Some("main"));
    }

    #[test]
    fn a_file_written_by_hand_is_still_read() {
        // A person can write this file, and Git accepts what they write.
        // Line endings, spaces, comments, quotation marks, and a section
        // with no path of its own all have to survive.
        let file = "# my recipes\r\n[submodule \"chili\"]\r\n  url = \"http://x/sam/chili.git\"\r\n; a note\r\n";
        let held = read_references(file.as_bytes());

        assert_eq!(held.len(), 1);
        assert_eq!(held[0].path, "chili", "the section name stands in");
        assert_eq!(held[0].url, "http://x/sam/chili.git");
    }

    #[test]
    fn a_file_with_no_recipes_gives_no_recipes() {
        assert!(read_references(b"").is_empty());
        assert!(read_references(b"[core]\n\tbare = false\n").is_empty());
    }

    #[test]
    fn the_last_section_for_a_name_wins_as_it_does_in_git() {
        let file = concat!(
            "[submodule \"chili\"]\n\tpath = chili\n\turl = http://x/sam/chili.git\n",
            "[submodule \"again\"]\n\tpath = chili\n\turl = http://x/alex/chili.git\n",
        );
        let held = read_references(file.as_bytes());

        assert_eq!(held.len(), 1);
        assert_eq!(held[0].url, "http://x/alex/chili.git");
    }

    #[test]
    fn the_address_of_a_recipe_is_the_one_a_person_clones() {
        // The application reaches Forgejo at another address, and that
        // address means nothing outside this installation. What a Cookbook
        // records is the address a person uses.
        assert_eq!(
            recipe_address(&client(), "sam", "chili"),
            "http://localhost:3000/sam/chili.git"
        );
    }

    #[test]
    fn an_address_names_the_recipe_it_points_at() {
        let forgejo = client();

        for address in [
            "http://localhost:3000/sam/chili.git",
            "http://localhost:3000/sam/chili",
            // The address this server uses counts too, because an older
            // installation can have recorded it.
            "http://forgejo:3000/sam/chili.git",
        ] {
            assert_eq!(
                recipe_named_by(&forgejo, address),
                Some(("sam".to_string(), "chili".to_string())),
                "`{address}` must name the Recipe"
            );
        }
    }

    #[test]
    fn an_address_somewhere_else_names_no_recipe() {
        let forgejo = client();

        for address in [
            "http://elsewhere.test/sam/chili.git",
            // A host that only begins with the address of this Forgejo.
            "http://localhost:3000.elsewhere.test/sam/chili.git",
            "git@localhost:sam/chili.git",
            "http://localhost:3000/sam",
            "http://localhost:3000/",
            "",
        ] {
            assert_eq!(
                recipe_named_by(&forgejo, address),
                None,
                "`{address}` must name no Recipe of this installation"
            );
        }
    }

    #[test]
    fn an_address_survives_a_round_trip() {
        let forgejo = client();
        let address = recipe_address(&forgejo, "sam", "chili");

        assert_eq!(
            recipe_named_by(&forgejo, &address),
            Some(("sam".to_string(), "chili".to_string()))
        );
    }

    #[test]
    fn the_name_inside_a_cookbook_comes_from_the_recipe() {
        assert_eq!(reference_path(&[], "chili").as_deref(), Some("chili"));
    }

    #[test]
    fn a_second_recipe_of_the_same_name_gets_the_next_free_one() {
        // Two people can each own a Recipe called Chili, and both can be in
        // one Cookbook. Nobody is asked about this.
        let taken = vec!["chili".to_string()];
        assert_eq!(reference_path(&taken, "chili").as_deref(), Some("chili-2"));

        let taken = vec!["chili".to_string(), "chili-2".to_string()];
        assert_eq!(reference_path(&taken, "chili").as_deref(), Some("chili-3"));
    }

    #[test]
    fn a_name_that_is_free_is_not_moved_by_another_recipe() {
        // The name has to stay the same for as long as the Recipe is in the
        // Cookbook, so a name that nothing holds is always the first try.
        let taken = vec!["toast".to_string(), "chili-2".to_string()];
        assert_eq!(reference_path(&taken, "chili").as_deref(), Some("chili"));
    }

    #[test]
    fn a_recipe_that_a_person_cannot_read_says_nothing_about_itself() {
        // The title, the owner, and the name all say what the Recipe is.
        // A person who may not know that must get none of them, so this is
        // checked here and not only in the page.
        for message in [
            UNAVAILABLE_MESSAGE,
            FOREIGN_MESSAGE,
            NO_ADDRESS_MESSAGE,
            NO_VERSION_MESSAGE,
            NOT_A_RECIPE_MESSAGE,
        ] {
            let hidden = Held::hidden(message);

            assert!(!hidden.available);
            assert!(hidden.title.is_empty());
            assert!(hidden.owner.is_empty());
            assert!(hidden.slug.is_empty());
            assert!(hidden.path.is_empty());
            // Whether only named people can read it is a fact about the
            // Recipe as well, so it stays out too.
            assert!(!hidden.private);
            assert_eq!(hidden.problem, message);
        }
    }

    #[test]
    fn keeping_the_version_is_what_a_form_gives_when_it_says_nothing() {
        assert_eq!(Holding::parse(""), Holding::Pinned);
        assert_eq!(Holding::parse("pinned"), Holding::Pinned);
        assert_eq!(Holding::parse("sideways"), Holding::Pinned);
        assert_eq!(Holding::parse("following"), Holding::Following);
        assert_eq!(Holding::default(), Holding::Pinned);
    }

    #[test]
    fn every_message_a_person_reads_uses_cooking_words() {
        // A person reads these. They say what is wrong in cooking words,
        // and they offer Forgejo instead of repairing anything.
        let refusals = [
            HoldError::NoRecipe.to_string(),
            HoldError::AlreadyHeld.to_string(),
            HoldError::NoVersion.to_string(),
            HoldError::NoFreePath.to_string(),
            HoldError::NotHeld.to_string(),
        ];

        let messages = [
            UNAVAILABLE_MESSAGE,
            FOREIGN_MESSAGE,
            NO_ADDRESS_MESSAGE,
            NO_VERSION_MESSAGE,
            NOT_A_RECIPE_MESSAGE,
        ]
        .into_iter()
        .map(str::to_string)
        .chain(refusals);

        for message in messages {
            for word in [
                "submodule",
                "repository",
                "branch",
                "commit",
                "gitlink",
                "fork",
                "pull request",
            ] {
                assert!(
                    !message.to_lowercase().contains(word),
                    "`{word}` must not reach the person: {message}"
                );
            }
        }
    }

    #[tokio::test]
    async fn a_cookbook_that_did_not_answer_is_not_called_broken() {
        // A diagnosis must come from an answer. When the Cookbook itself
        // could not be read, no Version is silence, and saying that the
        // Cookbook holds none would name a fault that nobody has.
        let pool = crate::db::connect("sqlite://:memory:").await.unwrap();
        let forgejo = client();

        let reference = Reference {
            path: "chili".to_string(),
            url: recipe_address(&forgejo, "sam", "chili"),
            follow: None,
            version: None,
        };

        let silent = Contents {
            references: vec![reference.clone()],
            complete: false,
        };
        let answered = Contents {
            references: vec![reference],
            complete: true,
        };

        // Nothing here reaches Forgejo, because the client points at a host
        // that does not exist. That is the point: the only difference
        // between the two answers is what was read.
        let quiet = held_recipes(&pool, &forgejo, None, &silent).await;
        let told = held_recipes(&pool, &forgejo, None, &answered).await;

        assert_eq!(quiet[0].problem, UNAVAILABLE_MESSAGE);
        assert_eq!(told[0].problem, NO_VERSION_MESSAGE);
    }

    // --------------------------------------------------- who can read what

    /// One Recipe of a Cookbook that this person can read.
    fn readable(owner: &str, slug: &str, title: &str, private: bool) -> Held {
        Held {
            available: true,
            path: slug.to_string(),
            owner: owner.to_string(),
            slug: slug.to_string(),
            title: title.to_string(),
            private,
            following: false,
            problem: String::new(),
            warning: String::new(),
        }
    }

    /// One Recipe, as Forgejo reports it.
    fn a_recipe(owner: &str, name: &str, private: bool) -> Repository {
        let mut found = repository(1, owner, name, &["cooklang", "recipe"]);
        found.private = private;
        found
    }

    fn sharer(login: &str) -> Sharer {
        Sharer {
            login: login.to_string(),
            name: login.to_string(),
        }
    }

    #[tokio::test]
    async fn a_forgejo_that_does_not_answer_decides_nothing() {
        // The client points at a host that does not exist, so Forgejo says
        // nothing at all. Nothing may be read into that silence.
        let answer = reach(
            &client(),
            &Secret::new("t".to_string()),
            "sam",
            "chili",
            "robin",
        )
        .await;

        assert_eq!(answer, Reach::Silent);
    }

    #[tokio::test]
    async fn a_public_recipe_is_out_of_reach_of_nobody() {
        // Forgejo lets every user read a public Recipe, so it is never a
        // mismatch and Forgejo is not asked about it. The client here cannot
        // answer, so an empty report proves that no question was sent.
        let recipes = vec![readable("sam", "chili", "Chili", false)];

        let gap =
            recipes_out_of_reach(&client(), &Secret::new("t".to_string()), &recipes, "robin").await;

        assert!(gap.is_empty(), "got: {gap:?}");

        let people = people_out_of_reach(
            &client(),
            &Secret::new("t".to_string()),
            &a_recipe("sam", "chili", false),
            &[sharer("robin")],
        )
        .await;

        assert!(people.is_empty(), "got: {people:?}");
    }

    #[tokio::test]
    async fn a_private_recipe_that_forgejo_says_nothing_about_is_reported_as_that() {
        // Silence is its own state. It is never read as `can read` and it is
        // never read as `cannot read`.
        let recipes = vec![readable("sam", "secret", "Secret Sauce", true)];

        let gap =
            recipes_out_of_reach(&client(), &Secret::new("t".to_string()), &recipes, "robin").await;

        assert!(gap.shut.is_empty());
        assert_eq!(gap.silent.len(), 1);
        assert_eq!(gap.silent[0].title, "Secret Sauce");
        assert!(!gap.is_empty());
        assert_eq!(gap.each().len(), 1);

        let people = people_out_of_reach(
            &client(),
            &Secret::new("t".to_string()),
            &a_recipe("sam", "secret", true),
            &[sharer("robin")],
        )
        .await;

        assert!(people.shut.is_empty());
        assert_eq!(people.silent, vec![sharer("robin")]);
        assert_eq!(people.each().len(), 1);
    }

    #[tokio::test]
    async fn a_recipe_is_never_out_of_reach_of_the_person_who_owns_it() {
        // Forgejo names the owner of each Recipe, so this needs no question
        // at all. The client here cannot answer, so an empty report proves
        // that no question was sent.
        let recipes = vec![readable("robin", "secret", "Secret Sauce", true)];

        let gap =
            recipes_out_of_reach(&client(), &Secret::new("t".to_string()), &recipes, "robin").await;

        assert!(gap.is_empty(), "got: {gap:?}");

        let people = people_out_of_reach(
            &client(),
            &Secret::new("t".to_string()),
            &a_recipe("robin", "secret", true),
            &[sharer("robin")],
        )
        .await;

        assert!(people.is_empty(), "got: {people:?}");
    }

    #[tokio::test]
    async fn a_recipe_that_this_person_cannot_read_is_never_named_to_them() {
        // A Recipe that is not available carries no owner and no name, so it
        // cannot become a line that says what it is. It stays out of the
        // report altogether.
        let recipes = vec![Held::hidden(UNAVAILABLE_MESSAGE)];

        let gap =
            recipes_out_of_reach(&client(), &Secret::new("t".to_string()), &recipes, "robin").await;

        assert!(gap.is_empty(), "got: {gap:?}");
    }

    #[tokio::test]
    async fn a_grant_that_forgejo_refuses_is_named_and_offers_forgejo() {
        let refusals = grant_reader(
            &client(),
            &Secret::new("t".to_string()),
            "robin",
            &[Named {
                owner: "sam".to_string(),
                slug: "secret".to_string(),
                title: "Secret Sauce".to_string(),
            }],
        )
        .await;

        assert_eq!(refusals.len(), 1);
        assert!(refusals[0].contains("robin"));
        assert!(refusals[0].contains("Secret Sauce"));
        assert!(refusals[0].contains("Open that Recipe in Forgejo"));

        for word in ["submodule", "repository", "collaborator", "permission"] {
            assert!(
                !refusals[0].to_lowercase().contains(word),
                "`{word}` must not reach the person: {}",
                refusals[0]
            );
        }
    }

    #[test]
    fn reader_is_forgejo_read() {
        // A grant made here is an ordinary Forgejo permission, and it is the
        // same one that the Sharing area of a Recipe hands out.
        assert_eq!(READER_ACCESS, "read");
    }

    // --------------------------------------------------------- the index

    #[tokio::test]
    async fn a_cookbook_survives_a_round_trip_through_the_index() {
        let pool = crate::db::connect("sqlite://:memory:").await.unwrap();
        let written = entry(7, "sam", "sunday", "Sunday Dinners");

        put(&pool, &written).await.unwrap();
        let read = get(&pool, "sam", "sunday").await.unwrap().unwrap();

        assert_eq!(read, written);
        assert_eq!(count(&pool).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn writing_the_same_cookbook_twice_keeps_one_row() {
        let pool = crate::db::connect("sqlite://:memory:").await.unwrap();
        put(&pool, &entry(7, "sam", "sunday", "Sunday"))
            .await
            .unwrap();
        put(&pool, &entry(7, "sam", "sunday", "Sunday Dinners"))
            .await
            .unwrap();

        assert_eq!(count(&pool).await.unwrap(), 1);
        assert_eq!(
            get(&pool, "sam", "sunday").await.unwrap().unwrap().title,
            "Sunday Dinners"
        );
    }

    #[tokio::test]
    async fn a_renamed_cookbook_keeps_one_row_and_frees_its_old_name() {
        let pool = crate::db::connect("sqlite://:memory:").await.unwrap();
        put(&pool, &entry(7, "sam", "sunday", "Sunday"))
            .await
            .unwrap();
        put(&pool, &entry(7, "sam", "sunday-dinners", "Sunday"))
            .await
            .unwrap();

        assert_eq!(count(&pool).await.unwrap(), 1);
        assert!(get(&pool, "sam", "sunday").await.unwrap().is_none());
        assert!(get(&pool, "sam", "sunday-dinners").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn forgetting_removes_the_row() {
        let pool = crate::db::connect("sqlite://:memory:").await.unwrap();
        put(&pool, &entry(7, "sam", "sunday", "Sunday"))
            .await
            .unwrap();

        assert_eq!(forget(&pool, "sam", "sunday").await.unwrap(), 1);
        assert_eq!(count(&pool).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn a_public_sweep_never_removes_a_private_cookbook() {
        let pool = crate::db::connect("sqlite://:memory:").await.unwrap();

        let mut hidden = entry(7, "sam", "secret", "Secret");
        hidden.private = true;
        put(&pool, &hidden).await.unwrap();
        put(&pool, &entry(8, "sam", "sunday", "Sunday"))
            .await
            .unwrap();

        let removed = prune_missing(&pool, &Prune::Public, &[]).await.unwrap();

        assert_eq!(removed, 1, "only the public row goes");
        assert!(get(&pool, "sam", "secret").await.unwrap().is_some());
        assert!(get(&pool, "sam", "sunday").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_sweep_of_one_person_leaves_everybody_else_alone() {
        let pool = crate::db::connect("sqlite://:memory:").await.unwrap();
        put(&pool, &entry(7, "sam", "sunday", "Sunday"))
            .await
            .unwrap();
        put(&pool, &entry(8, "alex", "weeknights", "Weeknights"))
            .await
            .unwrap();

        let removed = prune_missing(&pool, &Prune::Owner("sam".to_string()), &[])
            .await
            .unwrap();

        assert_eq!(removed, 1);
        assert!(get(&pool, "alex", "weeknights").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn the_recipe_index_and_the_cookbook_index_are_separate_tables() {
        // One table per kind is what keeps a Cookbook out of a Recipe list
        // even when the two carry the same owner and the same name.
        let pool = crate::db::connect("sqlite://:memory:").await.unwrap();

        put(&pool, &entry(7, "sam", "sunday", "Sunday Dinners"))
            .await
            .unwrap();

        assert_eq!(count(&pool).await.unwrap(), 1);
        assert_eq!(crate::index::count(&pool).await.unwrap(), 0);
    }
}
