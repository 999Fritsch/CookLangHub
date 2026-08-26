//! The Recipe index: a cache, and never an authority.
//!
//! A person searches for `Chili sin Carne`, but Forgejo knows that
//! repository as `chili-sin-carne`. The title a person sees lives in the
//! Cooklang metadata inside `recipe.cook`, so no question that Forgejo can
//! answer finds a Recipe by its title. This index holds that title, and the
//! few culinary facts that a card shows, so that a list costs one read of
//! Forgejo instead of one read for every Recipe.
//!
//! Three rules keep this from becoming a second source of truth.
//!
//! 1. Forgejo names the Recipes that a person may see, on every request.
//!    The index only supplies the words on the card. A row that Forgejo did
//!    not name is never shown, whatever the row says.
//! 2. Every row is rebuildable. [`reconcile`] reads Forgejo and Git again
//!    and writes each row back, so deleting the index costs time only.
//! 3. Nothing here writes to Forgejo or to Git. The index reads.

use std::collections::HashMap;

use futures::StreamExt;
use sqlx::sqlite::SqlitePool;

use crate::crypto::Cipher;
use crate::forgejo::{ForgejoClient, ForgejoError, Ownership, Repository, RepositoryQuery};
use crate::recipe::{RECIPE_FILE, RECIPE_TOPICS};
use crate::secret::Secret;
use crate::session::now;

/// How many repositories the application asks Forgejo for at a time.
const SEARCH_PAGE: u32 = 50;

/// The most repositories that one question about Forgejo covers.
///
/// A list this long already fills more screens than a person reads. Paging
/// through a larger collection arrives with a later ticket, and until then
/// this bound keeps one page view to a small, fixed number of requests.
pub const MAX_REPOSITORIES: usize = 200;

/// How many Recipes the application reads at the same time.
const READ_CONCURRENCY: usize = 8;

/// The topic that a search asks Forgejo about.
///
/// Forgejo matches one topic per search, so the search asks for the wider
/// marker and the application then keeps only what carries every topic in
/// [`RECIPE_TOPICS`].
const SEARCH_TOPIC: &str = RECIPE_TOPICS[0];

/// One Recipe, as a list needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Indexed {
    pub repository_id: i64,
    pub owner: String,
    pub slug: String,
    /// The title from the Cooklang metadata, or the slug when the Recipe
    /// names none.
    pub title: String,
    pub private: bool,
    /// What Forgejo last reported as the moment of the change.
    pub updated_at: String,
    pub servings: Option<String>,
    pub tags: Vec<String>,
    pub ingredients: i64,
    /// Whether the Recipe has a photo. The card asks this application for
    /// the image, so Forgejo still decides who may see it.
    pub thumbnail: bool,
}

/// What a refresh found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refreshed {
    /// The Recipe is in the index and current.
    Indexed,
    /// Forgejo no longer has it, or no longer shows it to this application.
    Gone,
    /// The repository lost its topics, so it is not a Recipe any more.
    NotARecipe,
}

/// What a reconciliation did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// How many Recipes Forgejo named.
    pub scanned: usize,
    /// How many rows the application wrote.
    pub written: usize,
    /// How many rows the application removed.
    pub removed: u64,
    /// How many questions Forgejo did not answer.
    pub failures: usize,
}

/// Whether a repository is a Recipe that this application shows.
///
/// Both topics must be there. This is the opt-in marker, and removing
/// either one in Forgejo takes the repository out of the application.
pub fn is_recipe(repository: &Repository) -> bool {
    repository.has_topics(&RECIPE_TOPICS)
}

// ---------------------------------------------------------------- storage

/// Write one Recipe into the index.
pub async fn put(pool: &SqlitePool, entry: &Indexed) -> Result<(), sqlx::Error> {
    // A rename gives an old name to a new repository, so a row that still
    // holds the name has to go before this one takes it.
    sqlx::query("DELETE FROM recipe_index WHERE owner = ? AND slug = ? AND repository_id <> ?")
        .bind(&entry.owner)
        .bind(&entry.slug)
        .bind(entry.repository_id)
        .execute(pool)
        .await?;

    sqlx::query(
        "INSERT INTO recipe_index (
             repository_id, owner, slug, title, private, updated_at,
             servings, tags, ingredients, thumbnail, indexed_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(repository_id) DO UPDATE SET
             owner       = excluded.owner,
             slug        = excluded.slug,
             title       = excluded.title,
             private     = excluded.private,
             updated_at  = excluded.updated_at,
             servings    = excluded.servings,
             tags        = excluded.tags,
             ingredients = excluded.ingredients,
             thumbnail   = excluded.thumbnail,
             indexed_at  = excluded.indexed_at",
    )
    .bind(entry.repository_id)
    .bind(&entry.owner)
    .bind(&entry.slug)
    .bind(&entry.title)
    .bind(i64::from(entry.private))
    .bind(&entry.updated_at)
    .bind(&entry.servings)
    .bind(entry.tags.join("\n"))
    .bind(entry.ingredients)
    .bind(i64::from(entry.thumbnail))
    .bind(now())
    .execute(pool)
    .await?;

    Ok(())
}

/// Take one Recipe out of the index, by the name it had.
pub async fn forget(pool: &SqlitePool, owner: &str, slug: &str) -> Result<u64, sqlx::Error> {
    let removed = sqlx::query("DELETE FROM recipe_index WHERE owner = ? AND slug = ?")
        .bind(owner)
        .bind(slug)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(removed)
}

/// Take one Recipe out of the index, by the identifier Forgejo gave it.
pub async fn forget_repository(pool: &SqlitePool, repository_id: i64) -> Result<u64, sqlx::Error> {
    let removed = sqlx::query("DELETE FROM recipe_index WHERE repository_id = ?")
        .bind(repository_id)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(removed)
}

/// How many Recipes the index holds.
pub async fn count(pool: &SqlitePool) -> Result<i64, sqlx::Error> {
    let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM recipe_index")
        .fetch_one(pool)
        .await?;
    Ok(row.0)
}

/// Read one Recipe out of the index.
pub async fn get(
    pool: &SqlitePool,
    owner: &str,
    slug: &str,
) -> Result<Option<Indexed>, sqlx::Error> {
    let row: Option<Row> = sqlx::query_as(
        "SELECT repository_id, owner, slug, title, private, updated_at,
                servings, tags, ingredients, thumbnail
         FROM recipe_index WHERE owner = ? AND slug = ?",
    )
    .bind(owner)
    .bind(slug)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(Indexed::from))
}

/// Read every Recipe in the index. Diagnostics and tests use this.
pub async fn all(pool: &SqlitePool) -> Result<Vec<Indexed>, sqlx::Error> {
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT repository_id, owner, slug, title, private, updated_at,
                servings, tags, ingredients, thumbnail
         FROM recipe_index ORDER BY owner, slug",
    )
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(Indexed::from).collect())
}

/// The stored shape of one row.
type Row = (
    i64,
    String,
    String,
    String,
    i64,
    String,
    Option<String>,
    String,
    i64,
    i64,
);

impl From<Row> for Indexed {
    fn from(row: Row) -> Self {
        let (
            repository_id,
            owner,
            slug,
            title,
            private,
            updated_at,
            servings,
            tags,
            ingredients,
            thumbnail,
        ) = row;
        Self {
            repository_id,
            owner,
            slug,
            title,
            private: private != 0,
            updated_at,
            servings,
            tags: tags
                .split('\n')
                .filter(|tag| !tag.is_empty())
                .map(str::to_string)
                .collect(),
            ingredients,
            thumbnail: thumbnail != 0,
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
        "SELECT repository_id, owner, slug, title, private, updated_at,
                servings, tags, ingredients, thumbnail
         FROM recipe_index WHERE repository_id IN ({places})"
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

// ------------------------------------------------------------- refreshing

/// Read a Recipe and build its index entry.
///
/// The title and the culinary facts come from the Cooklang source, which is
/// the only place that holds them. Nothing here writes.
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
            RECIPE_FILE,
        )
        .await?;

    // Git accepts any bytes, so a direct push can put something that is not
    // text in the file. The Recipe page says so plainly; a card only needs
    // whatever words can be read.
    let source = String::from_utf8_lossy(&bytes);

    let title = crate::recipe::parse(&source)
        .title
        .unwrap_or_else(|| repository.name.clone());

    let cooked = crate::recipe::parse_recipe(&source)
        .as_ref()
        .map(crate::render::render)
        .unwrap_or_default();

    // Whether a photo is there is a fact about the files of the Recipe, not
    // about its text, so it needs its own question. It is asked here, while
    // the Recipe is being read anyway, and never once for every card.
    let photos = crate::upload::photos(
        forgejo,
        token,
        &repository.owner.login,
        &repository.name,
        repository.branch(),
    )
    .await;

    Ok(Indexed {
        repository_id: repository.id,
        owner: repository.owner.login.clone(),
        slug: repository.name.clone(),
        title,
        private: repository.private,
        updated_at: repository.updated_at.clone(),
        servings: cooked.servings,
        tags: cooked.tags,
        ingredients: cooked.ingredients.len() as i64,
        thumbnail: photos.is_some(),
    })
}

/// What a card shows when the Recipe itself could not be read.
///
/// The slug stands in for the title. The row is not written, so the next
/// attempt reads the Recipe again instead of keeping a poor title forever.
fn placeholder(repository: &Repository) -> Indexed {
    Indexed {
        repository_id: repository.id,
        owner: repository.owner.login.clone(),
        slug: repository.name.clone(),
        title: repository.name.clone(),
        private: repository.private,
        updated_at: repository.updated_at.clone(),
        servings: None,
        tags: Vec::new(),
        ingredients: 0,
        thumbnail: false,
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
/// A Recipe whose row is missing or out of date is read from Forgejo and
/// written into the index here. In the ordinary case the webhook already
/// did that, and this reads nothing.
pub async fn entries(
    pool: &SqlitePool,
    forgejo: &ForgejoClient,
    token: Option<&Secret<String>>,
    repositories: &[Repository],
) -> Vec<Indexed> {
    let known = match known(pool, repositories).await {
        Ok(known) => known,
        Err(error) => {
            tracing::warn!(%error, "cannot read the Recipe index");
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

    // A Recipe that cannot be read leaves the index alone, so a good title
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
                        "cannot read this Recipe for the index"
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
            tracing::warn!(%error, "cannot write the Recipe index");
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

/// Bring one Recipe up to date, or take it out of the index.
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
            tracing::info!(%error, %owner, %slug, "Forgejo does not show this Recipe");
            forget_quietly(pool, owner, slug).await;
            return Refreshed::Gone;
        }
    };

    if !is_recipe(&repository) {
        tracing::info!(%owner, %slug, "the topics are gone, so this is not a Recipe any more");
        forget_quietly(pool, owner, slug).await;
        return Refreshed::NotARecipe;
    }

    match read_entry(forgejo, token, &repository).await {
        Ok(entry) => {
            if let Err(error) = put(pool, &entry).await {
                tracing::warn!(%error, %owner, %slug, "cannot write the Recipe index");
            }
            Refreshed::Indexed
        }
        Err(error) => {
            // The Recipe exists but could not be read. Keep whatever the
            // index already holds rather than replace a good title with a
            // slug, and let the next reconciliation try again.
            tracing::info!(%error, %owner, %slug, "cannot read this Recipe for the index");
            Refreshed::Indexed
        }
    }
}

async fn forget_quietly(pool: &SqlitePool, owner: &str, slug: &str) {
    if let Err(error) = forget(pool, owner, slug).await {
        tracing::warn!(%error, %owner, %slug, "cannot remove this Recipe from the index");
    }
}

// -------------------------------------------------------------- searching

/// Ask Forgejo which Recipes a credential may see.
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
        // is what makes a repository without them invisible here.
        found.extend(batch.into_iter().filter(is_recipe));

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

/// Read Forgejo and Git again, and make the index match.
///
/// This runs when the application starts and whenever an administrator asks
/// for it. It is safe at any moment, and it changes nothing in Forgejo and
/// nothing in Git: every call it makes is a read.
///
/// Two questions cover the whole instance. What is public, which needs no
/// credential. And what each signed-in person can reach, which uses the
/// credential of that person and therefore indexes only what they could
/// already see for themselves.
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
        "the Recipe index matches Forgejo again"
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
            tracing::warn!(%error, "cannot ask Forgejo for the Recipes");
            report.failures += 1;
            return;
        }
    };

    report.scanned += repositories.len();

    // Read every Recipe again, because a reconciliation exists for the case
    // where the application missed the message that said it changed.
    for repository in &repositories {
        match read_entry(forgejo, token, repository).await {
            Ok(entry) => match put(pool, &entry).await {
                Ok(()) => report.written += 1,
                Err(error) => {
                    tracing::warn!(%error, "cannot write the Recipe index");
                    report.failures += 1;
                }
            },
            Err(error) => {
                tracing::info!(
                    %error,
                    owner = %repository.owner.login,
                    slug = %repository.name,
                    "cannot read this Recipe for the index"
                );
                report.failures += 1;
            }
        }
    }

    // A short answer is not proof that nothing else exists, so a sweep that
    // hit the cap removes nothing.
    if truncated {
        tracing::warn!("Forgejo has more Recipes than one sweep covers; nothing was removed");
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
        Prune::Public => format!("DELETE FROM recipe_index WHERE private = 0{keep}"),
        Prune::Owner(_) => format!("DELETE FROM recipe_index WHERE owner = ?{keep}"),
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
            servings: Some("4".to_string()),
            tags: vec!["vegan".to_string(), "quick".to_string()],
            ingredients: 6,
            thumbnail: false,
        }
    }

    #[test]
    fn both_topics_are_needed() {
        assert!(is_recipe(&repository(
            1,
            "sam",
            "chili",
            &["cooklang", "recipe"]
        )));
        assert!(!is_recipe(&repository(1, "sam", "chili", &["cooklang"])));
        assert!(!is_recipe(&repository(1, "sam", "chili", &["recipe"])));
        assert!(!is_recipe(&repository(1, "sam", "chili", &[])));
    }

    #[test]
    fn a_topic_matches_whatever_case_it_carries() {
        assert!(is_recipe(&repository(
            1,
            "sam",
            "chili",
            &["CookLang", "Recipe", "dinner"]
        )));
    }

    #[test]
    fn a_row_is_current_only_while_forgejo_reports_the_same_state() {
        let mut forgejo = repository(1, "sam", "chili", &["cooklang", "recipe"]);
        let row = entry(1, "sam", "chili", "Chili sin Carne");
        assert!(is_current(&row, &forgejo));

        forgejo.updated_at = "2026-02-02T00:00:00Z".to_string();
        assert!(!is_current(&row, &forgejo), "a change must be read again");

        let mut renamed = repository(1, "sam", "chili-2", &["cooklang", "recipe"]);
        renamed.updated_at = row.updated_at.clone();
        assert!(!is_current(&row, &renamed), "a rename must be read again");

        let mut hidden = repository(1, "sam", "chili", &["cooklang", "recipe"]);
        hidden.private = true;
        assert!(!is_current(&row, &hidden), "a change of visibility counts");
    }

    #[tokio::test]
    async fn a_recipe_survives_a_round_trip_through_the_index() {
        let pool = crate::db::connect("sqlite://:memory:").await.unwrap();
        let written = entry(7, "sam", "chili", "Chili sin Carne");

        put(&pool, &written).await.unwrap();
        let read = get(&pool, "sam", "chili").await.unwrap().unwrap();

        assert_eq!(read, written);
        assert_eq!(count(&pool).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn writing_the_same_recipe_twice_keeps_one_row() {
        let pool = crate::db::connect("sqlite://:memory:").await.unwrap();
        put(&pool, &entry(7, "sam", "chili", "Chili"))
            .await
            .unwrap();
        put(&pool, &entry(7, "sam", "chili", "Chili sin Carne"))
            .await
            .unwrap();

        assert_eq!(count(&pool).await.unwrap(), 1);
        assert_eq!(
            get(&pool, "sam", "chili").await.unwrap().unwrap().title,
            "Chili sin Carne"
        );
    }

    #[tokio::test]
    async fn a_renamed_recipe_keeps_one_row_and_frees_its_old_name() {
        let pool = crate::db::connect("sqlite://:memory:").await.unwrap();
        put(&pool, &entry(7, "sam", "chili", "Chili"))
            .await
            .unwrap();
        put(&pool, &entry(7, "sam", "chili-con-carne", "Chili"))
            .await
            .unwrap();

        assert_eq!(count(&pool).await.unwrap(), 1);
        assert!(get(&pool, "sam", "chili").await.unwrap().is_none());
        assert!(
            get(&pool, "sam", "chili-con-carne")
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn a_new_repository_may_take_a_name_that_an_old_row_still_holds() {
        let pool = crate::db::connect("sqlite://:memory:").await.unwrap();
        put(&pool, &entry(7, "sam", "chili", "Old")).await.unwrap();
        put(&pool, &entry(8, "sam", "chili", "New")).await.unwrap();

        assert_eq!(count(&pool).await.unwrap(), 1);
        assert_eq!(
            get(&pool, "sam", "chili").await.unwrap().unwrap().title,
            "New"
        );
    }

    #[tokio::test]
    async fn forgetting_removes_the_row() {
        let pool = crate::db::connect("sqlite://:memory:").await.unwrap();
        put(&pool, &entry(7, "sam", "chili", "Chili"))
            .await
            .unwrap();

        assert_eq!(forget(&pool, "sam", "chili").await.unwrap(), 1);
        assert_eq!(count(&pool).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn a_public_sweep_never_removes_a_private_recipe() {
        // A sweep with no credential cannot see a private Recipe, so a row
        // that says private is no evidence of anything.
        let pool = crate::db::connect("sqlite://:memory:").await.unwrap();

        let mut hidden = entry(7, "sam", "secret", "Secret");
        hidden.private = true;
        put(&pool, &hidden).await.unwrap();
        put(&pool, &entry(8, "sam", "chili", "Chili"))
            .await
            .unwrap();

        let removed = prune_missing(&pool, &Prune::Public, &[]).await.unwrap();

        assert_eq!(removed, 1, "only the public row goes");
        assert!(get(&pool, "sam", "secret").await.unwrap().is_some());
        assert!(get(&pool, "sam", "chili").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_sweep_of_one_person_leaves_everybody_else_alone() {
        let pool = crate::db::connect("sqlite://:memory:").await.unwrap();
        put(&pool, &entry(7, "sam", "chili", "Chili"))
            .await
            .unwrap();
        put(&pool, &entry(8, "alex", "stew", "Stew")).await.unwrap();

        let removed = prune_missing(&pool, &Prune::Owner("sam".to_string()), &[])
            .await
            .unwrap();

        assert_eq!(removed, 1);
        assert!(get(&pool, "alex", "stew").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn a_sweep_keeps_what_forgejo_named() {
        let pool = crate::db::connect("sqlite://:memory:").await.unwrap();
        put(&pool, &entry(7, "sam", "chili", "Chili"))
            .await
            .unwrap();
        put(&pool, &entry(8, "sam", "stew", "Stew")).await.unwrap();

        let removed = prune_missing(&pool, &Prune::Owner("sam".to_string()), &[7])
            .await
            .unwrap();

        assert_eq!(removed, 1);
        assert!(get(&pool, "sam", "chili").await.unwrap().is_some());
        assert!(get(&pool, "sam", "stew").await.unwrap().is_none());
    }
}
