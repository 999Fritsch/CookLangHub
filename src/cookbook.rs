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
//! Later tickets add the Recipes of a Cookbook as Git submodules. They belong
//! in `.gitmodules` and in the gitlinks, and never in this database.
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
