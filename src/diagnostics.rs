//! What an administrator reads to find the cause of a fault.
//!
//! Six things can fail on their own, and each one fails differently: the
//! application, Forgejo, the webhook, the reconciliation, the automation,
//! and the parser. One combined answer hides which of them stopped, so this
//! module reports each of them separately, says what it is for, and says
//! what to do when it is not working.
//!
//! Two rules hold here.
//!
//! 1. **Nothing is repaired.** Every probe reads. A state that this
//!    application cannot handle is named and handed to **Open in Forgejo**.
//! 2. **No credential reaches a page.** The automation token, the webhook
//!    secret, the session secret, and the credential of the person reading
//!    the page are all used to ask a question and none of them is ever
//!    rendered or logged. `no_secret_reaches_the_diagnostics_page` holds that.

use serde::Serialize;
use sqlx::sqlite::SqlitePool;

use crate::crypto::Cipher;
use crate::forgejo::ForgejoClient;
use crate::secret::Secret;
use crate::session::now;

/// The Forgejo major release that this application is built and tested
/// against. A different major is a state the administrator has to know
/// about, because the adapter was never exercised there.
pub const TESTED_FORGEJO_MAJOR: &str = "15";

/// The Cooklang that the parser self-check reads.
///
/// It carries one ingredient, one cookware, and one timer in a German unit
/// name. The German names come from `units/german.toml`, and without them
/// the timer is an error, so this source proves the converter as well as
/// the parser.
const SELF_CHECK: &str =
    "---\ntitle: Parser self-check\n---\n\nChop the @onion{1} in a #pan{} for ~{8%Min.}.\n";

// ------------------------------------------------------------- sweep state

/// The name that the Recipe index sweep records itself under.
pub const RECIPE_INDEX: &str = "recipe_index";
/// The name that the Cookbook index sweep records itself under.
pub const COOKBOOK_INDEX: &str = "cookbook_index";
/// The name that the automation run records itself under.
pub const AUTOMATION: &str = "automation";

/// What one recorded run of a sweep says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sweep {
    /// When the run finished, in seconds since the epoch.
    pub ran_at: i64,
    pub scanned: i64,
    pub changed: i64,
    pub removed: i64,
    pub failures: i64,
}

/// Record what one run of a sweep did.
///
/// A fault never stops the sweep that called this. The report is operational
/// state, so losing it costs the page a line and costs the product nothing.
pub async fn record_sweep(
    pool: &SqlitePool,
    name: &str,
    scanned: i64,
    changed: i64,
    removed: i64,
    failures: i64,
) {
    let written = sqlx::query(
        "INSERT INTO sweep (name, ran_at, scanned, changed, removed, failures)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(name) DO UPDATE SET
             ran_at   = excluded.ran_at,
             scanned  = excluded.scanned,
             changed  = excluded.changed,
             removed  = excluded.removed,
             failures = excluded.failures",
    )
    .bind(name)
    .bind(now())
    .bind(scanned)
    .bind(changed)
    .bind(removed)
    .bind(failures)
    .execute(pool)
    .await;

    if let Err(error) = written {
        tracing::warn!(%error, %name, "cannot record what the sweep did");
    }
}

/// Read what one sweep last did, when it has run at all.
pub async fn last_sweep(pool: &SqlitePool, name: &str) -> Option<Sweep> {
    let row: Option<(i64, i64, i64, i64, i64)> = sqlx::query_as(
        "SELECT ran_at, scanned, changed, removed, failures FROM sweep WHERE name = ?",
    )
    .bind(name)
    .fetch_optional(pool)
    .await
    .unwrap_or_else(|error| {
        tracing::warn!(%error, %name, "cannot read what the sweep did");
        None
    });

    row.map(|(ran_at, scanned, changed, removed, failures)| Sweep {
        ran_at,
        scanned,
        changed,
        removed,
        failures,
    })
}

/// Record that a message from Forgejo arrived and was accepted.
///
/// Only a message with a signature that matches gets here, so this moment
/// says that Forgejo reaches this application and that the shared secret is
/// the same on both sides.
pub async fn record_webhook_message(pool: &SqlitePool) {
    let written = sqlx::query("UPDATE webhook SET last_message_at = ? WHERE id = 1")
        .bind(now())
        .execute(pool)
        .await;

    if let Err(error) = written {
        tracing::warn!(%error, "cannot record the moment a webhook message arrived");
    }
}

// ------------------------------------------------------------ the report

/// How one subsystem is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// It is doing its work.
    Working,
    /// It is not doing anything, and that is correct here. An installation
    /// with no Cookbook that follows a Recipe needs no automation.
    Quiet,
    /// It cannot do its work.
    Fault,
}

impl State {
    /// The words on the badge.
    pub fn words(&self) -> &'static str {
        match self {
            State::Working => "Working",
            State::Quiet => "Not in use",
            State::Fault => "Fault",
        }
    }

    /// The badge class.
    ///
    /// Green is the live state and grey is the quiet one, through the
    /// CookCLI classes. A fault carries the grey badge and the fault card
    /// below it, because the words and the card say it, not the colour.
    pub fn badge(&self) -> &'static str {
        match self {
            State::Working => "metadata-cuisine",
            State::Quiet | State::Fault => "metadata-servings",
        }
    }

    pub fn is_fault(&self) -> bool {
        matches!(self, State::Fault)
    }
}

/// One thing an administrator reads off the page.
#[derive(Debug, Clone, Serialize)]
pub struct Fact {
    pub label: String,
    pub value: String,
}

fn fact(label: &str, value: impl Into<String>) -> Fact {
    Fact {
        label: label.to_string(),
        value: value.into(),
    }
}

/// One of the six subsystems.
#[derive(Debug, Clone, Serialize)]
pub struct Subsystem {
    /// What it is called. An administrator page names the real subsystem.
    pub name: &'static str,
    /// What it does, in one sentence.
    pub purpose: &'static str,
    pub state: State,
    /// The words on the badge.
    pub summary: String,
    pub facts: Vec<Fact>,
    /// What is wrong, and what to do about it. Empty when nothing is wrong.
    pub problems: Vec<String>,
}

impl Subsystem {
    fn new(name: &'static str, purpose: &'static str, state: State) -> Self {
        Self {
            name,
            purpose,
            state,
            summary: state.words().to_string(),
            facts: Vec::new(),
            problems: Vec::new(),
        }
    }

    fn with(mut self, facts: Vec<Fact>) -> Self {
        self.facts = facts;
        self
    }

    fn stop(mut self, problem: impl Into<String>) -> Self {
        self.state = State::Fault;
        self.summary = State::Fault.words().to_string();
        self.problems.push(problem.into());
        self
    }

    /// Add something to act on without changing the state.
    fn note(mut self, problem: impl Into<String>) -> Self {
        self.problems.push(problem.into());
        self
    }
}

/// The whole report.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub subsystems: Vec<Subsystem>,
    /// Whether Forgejo answered while this page was built. The page says
    /// which numbers are then a cache and can be old.
    pub forgejo_answers: bool,
}

impl Report {
    /// Whether every subsystem is doing its work or is correctly quiet.
    pub fn is_healthy(&self) -> bool {
        !self.subsystems.iter().any(|part| part.state.is_fault())
    }
}

/// Build the whole report.
///
/// Each probe runs even when an earlier one failed, so one fault never hides
/// another. `admin_token` is the credential of the administrator who is
/// reading the page: Forgejo answers the webhook question for an
/// administrator only, and the page asks with the credential of the person
/// who is already allowed to see the answer.
pub async fn report(
    pool: &SqlitePool,
    cipher: &Cipher,
    forgejo: &ForgejoClient,
    admin_token: &Secret<String>,
) -> Report {
    let release = forgejo.version().await;
    let forgejo_answers = release.is_ok();

    let subsystems = vec![
        application(pool).await,
        forgejo_state(forgejo, &release, true),
        webhook(pool, forgejo, admin_token, forgejo_answers).await,
        reconciliation(pool, forgejo_answers).await,
        automation(pool, cipher, forgejo, forgejo_answers).await,
        parser(),
    ];

    Report {
        subsystems,
        forgejo_answers,
    }
}

/// The Forgejo card alone, for a page that has no administrator credential.
///
/// Forgejo says who administers the installation, so during an outage this
/// application cannot tell. It still has to say why the page is empty, and
/// "Forgejo does not answer at this address" is not a secret: every page
/// already carries a link to that address. Everything that could be a
/// secret stays behind the administrator check.
///
/// Gives nothing back while Forgejo answers, because then the page can ask
/// who is reading it.
pub async fn forgejo_outage(forgejo: &ForgejoClient) -> Option<Subsystem> {
    let release = forgejo.version().await;
    if release.is_ok() {
        return None;
    }

    Some(forgejo_state(forgejo, &release, false))
}

/// 1. The application itself.
async fn application(pool: &SqlitePool) -> Subsystem {
    let part = Subsystem::new(
        "The application",
        "This is the CookLangHub server. It draws every page, and it holds \
         operational state only. Every row of that state is rebuildable.",
        State::Working,
    );

    let sessions: Result<i64, sqlx::Error> = sqlx::query_scalar("SELECT count(*) FROM session")
        .fetch_one(pool)
        .await;

    let installation: Result<String, sqlx::Error> =
        sqlx::query_scalar("SELECT installation_id FROM installation WHERE id = 1")
            .fetch_one(pool)
            .await;

    match (sessions, installation) {
        (Ok(sessions), Ok(installation)) => part.with(vec![
            fact("Release", env!("CARGO_PKG_VERSION")),
            fact("Installation", installation),
            fact("Operational database", "answers queries"),
            fact("Open sessions", sessions.to_string()),
        ]),
        (sessions, installation) => {
            let reason = sessions
                .err()
                .map(|error| error.to_string())
                .or_else(|| installation.err().map(|error| error.to_string()))
                .unwrap_or_default();

            part.with(vec![
                fact("Release", env!("CARGO_PKG_VERSION")),
                fact("Operational database", "does not answer queries"),
            ])
            .stop(format!(
                "The operational database does not answer: {reason}. The database holds \
                 operational state only, so you can delete the file and start the \
                 application again. The reconciliation then builds the indexes from \
                 Forgejo and Git."
            ))
        }
    }
}

/// 2. Forgejo, the authority.
///
/// `for_administrator` decides whether the address that this application
/// reaches Forgejo on appears. It is the name of a host on the internal
/// network, and every other page of this product shows the public address
/// only, so it stays behind the administrator check.
fn forgejo_state(
    forgejo: &ForgejoClient,
    release: &Result<String, crate::forgejo::ForgejoError>,
    for_administrator: bool,
) -> Subsystem {
    let part = Subsystem::new(
        "Forgejo",
        "Forgejo holds the accounts, the permissions, and the repositories \
         that carry every Recipe and every Cookbook. It is the authority for \
         all of them.",
        State::Working,
    );

    let mut addresses = vec![fact(
        "Address for a browser",
        forgejo.public_url().to_string(),
    )];
    if for_administrator {
        addresses.push(fact(
            "Address for this application",
            forgejo.api_url().to_string(),
        ));
    }
    addresses.push(fact(
        "Tested release",
        format!("Forgejo {TESTED_FORGEJO_MAJOR}"),
    ));

    // The fault names one address, and it is the one the reader can act on.
    let unreachable_at = if for_administrator {
        forgejo.api_url()
    } else {
        forgejo.public_url()
    };

    match release {
        Ok(release) => {
            let mut facts = vec![fact("Release", format!("Forgejo {release}"))];
            facts.extend(addresses);

            let part = part.with(facts);

            if major(release) == Some(TESTED_FORGEJO_MAJOR.to_string()) {
                part
            } else {
                part.note(format!(
                    "This installation runs Forgejo {release}. CookLangHub was tested \
                     against Forgejo {TESTED_FORGEJO_MAJOR}. A major release can change \
                     the Forgejo API, and CookLangHub was never exercised against this \
                     one. Read `docs/operations.md` before you continue."
                ))
            }
        }
        Err(error) => {
            let mut facts = vec![fact("Release", "Forgejo does not answer")];
            facts.extend(addresses);

            // The reason carries the address that this application asked,
            // so only an administrator reads it.
            let reason = if for_administrator {
                format!(": {error}")
            } else {
                String::new()
            };

            part.with(facts).stop(format!(
                "Forgejo does not answer at {unreachable_at}{reason}. CookLangHub \
                 refuses every edit while this continues, and it shows no Recipe and no \
                 Cookbook from its own store, because that copy can be old. Start \
                 Forgejo again, then start a reconciliation on this page."
            ))
        }
    }
}

/// 3. The system webhook.
async fn webhook(
    pool: &SqlitePool,
    forgejo: &ForgejoClient,
    admin_token: &Secret<String>,
    forgejo_answers: bool,
) -> Subsystem {
    let part = Subsystem::new(
        "The webhook",
        "Forgejo reports each repository change to CookLangHub with one \
         system webhook. It makes the indexes current within a moment \
         instead of within a restart.",
        State::Working,
    );

    // The row holds the shared secret as well. Only these three columns are
    // read, so no credential can reach the page.
    let row: Option<(i64, String, Option<i64>)> = sqlx::query_as(
        "SELECT forgejo_hook_id, target_url, last_message_at FROM webhook WHERE id = 1",
    )
    .fetch_optional(pool)
    .await
    .unwrap_or_default();

    let Some((hook_id, target_url, last_message_at)) = row else {
        return Subsystem::new(part.name, part.purpose, State::Fault)
            .with(vec![fact("Registered", "no")])
            .stop(
                "No webhook is registered. CookLangHub then learns about a change only \
                 when it starts, or when an administrator starts a reconciliation. Run \
                 `cooklanghub bootstrap --forgejo-admin-token TOKEN` to register one.",
            );
    };

    let mut facts = vec![
        fact("Registered", "yes"),
        fact("Address that Forgejo posts to", target_url.clone()),
        fact("Events", crate::webhook::EVENTS.join(", ")),
        fact("Identifier in Forgejo", hook_id.to_string()),
        fact(
            "Last message",
            match last_message_at {
                Some(at) => ago(at),
                None => "no message has arrived yet".to_string(),
            },
        ),
    ];

    // Ask Forgejo whether it still holds the webhook. Somebody can remove it
    // there, and nothing else in the system shows that.
    let held = if forgejo_answers {
        match forgejo.system_hook(admin_token, hook_id).await {
            Ok(Some(hook)) => {
                facts.push(fact(
                    "Forgejo holds it",
                    if hook.active {
                        "yes"
                    } else {
                        "yes, but it is off"
                    },
                ));
                Some(hook.active)
            }
            Ok(None) => {
                facts.push(fact("Forgejo holds it", "no"));
                Some(false)
            }
            Err(error) => {
                facts.push(fact("Forgejo holds it", format!("cannot ask: {error}")));
                None
            }
        }
    } else {
        facts.push(fact(
            "Forgejo holds it",
            "cannot ask while Forgejo does not answer",
        ));
        None
    };

    let part = part.with(facts);

    if held == Some(false) {
        return part.stop(
            "Forgejo does not hold this webhook any more. CookLangHub then learns about \
             a change only when it starts, or when an administrator starts a \
             reconciliation. Run `cooklanghub bootstrap --forgejo-admin-token TOKEN` to \
             register it again.",
        );
    }

    if last_message_at.is_none() {
        return Subsystem {
            state: State::Quiet,
            summary: State::Quiet.words().to_string(),
            ..part
        }
        .note(format!(
            "No message has arrived yet. Forgejo must be able to reach {target_url}. \
             Inside the bundled stack a browser and Forgejo use different addresses, so \
             set COOKLANGHUB_INTERNAL_URL to the address that Forgejo uses. The \
             reconciliation keeps the indexes correct while this continues."
        ));
    }

    part
}

/// 4. The reconciliation.
async fn reconciliation(pool: &SqlitePool, forgejo_answers: bool) -> Subsystem {
    let part = Subsystem::new(
        "The reconciliation",
        "The reconciliation reads Forgejo and Git again and makes the \
         indexes match. It writes to neither of them. It runs when the \
         application starts, and whenever an administrator asks for it here.",
        State::Working,
    );

    let recipes = crate::index::count(pool).await.unwrap_or_default();
    let cookbooks = crate::cookbook::count(pool).await.unwrap_or_default();

    let recipe_sweep = last_sweep(pool, RECIPE_INDEX).await;
    let cookbook_sweep = last_sweep(pool, COOKBOOK_INDEX).await;

    let mut facts = vec![
        fact("Recipes in the index", recipes.to_string()),
        fact("Cookbooks in the index", cookbooks.to_string()),
        fact(
            "Recipe index, last run",
            describe(recipe_sweep.as_ref(), "written"),
        ),
        fact(
            "Cookbook index, last run",
            describe(cookbook_sweep.as_ref(), "written"),
        ),
    ];

    if !forgejo_answers {
        facts.push(fact(
            "These counts",
            "come from the index, which is a cache. Forgejo does not answer now, so \
             they can be old.",
        ));
    }

    let part = part.with(facts);

    let failures: i64 = [recipe_sweep.as_ref(), cookbook_sweep.as_ref()]
        .into_iter()
        .flatten()
        .map(|sweep| sweep.failures)
        .sum();

    if recipe_sweep.is_none() && cookbook_sweep.is_none() {
        return Subsystem {
            state: State::Quiet,
            summary: State::Quiet.words().to_string(),
            ..part
        }
        .note(
            "No reconciliation has finished yet. Select Start a reconciliation, above, \
             to build both indexes from Forgejo and Git.",
        );
    }

    if failures > 0 {
        return part.stop(format!(
            "The last reconciliation could not answer {failures} questions, so an index \
             can be behind. Forgejo is the usual cause. Read the Forgejo state above, \
             then start a reconciliation again."
        ));
    }

    part
}

/// 5. The automation account.
async fn automation(
    pool: &SqlitePool,
    cipher: &Cipher,
    forgejo: &ForgejoClient,
    forgejo_answers: bool,
) -> Subsystem {
    let part = Subsystem::new(
        "The automation",
        "A Cookbook can follow a Recipe. It then moves to each new Version \
         of that Recipe, and one Forgejo account is the author of those \
         Versions, so that nobody has their name on a change they did not \
         make.",
        State::Working,
    );

    let run = last_sweep(pool, AUTOMATION).await;

    // The credential is read to ask Forgejo a question with it. It is never
    // put into a fact and never written to a log.
    let Some(account) = crate::automation::of(pool, cipher).await else {
        return Subsystem {
            state: State::Quiet,
            summary: State::Quiet.words().to_string(),
            ..part
        }
        .with(vec![fact("Account", "none is registered")])
        .note(
            "This installation has no automation account. A Cookbook that follows a \
             Recipe then stays where it is, and the Cookbook page says so. An \
             installation with no Cookbook that follows a Recipe needs none of this. To \
             register one, read the README.",
        );
    };

    let mut facts = vec![
        fact("Account", account.login.clone()),
        fact("Name in History", account.name.clone()),
        fact("Identifier in Forgejo", account.forgejo_user_id.to_string()),
        fact("Last run", describe(run.as_ref(), "moved")),
    ];

    if !forgejo_answers {
        return part.with(facts).stop(
            "Forgejo does not answer, so CookLangHub cannot check the credential of the \
             automation account, and no Cookbook moves to a new Version. Read the \
             Forgejo state above.",
        );
    }

    match forgejo.current_user(&account.token).await {
        Ok(user) => {
            facts.push(fact(
                "Credential",
                format!("Forgejo accepts it for {}", user.login),
            ));
            let part = part.with(facts);

            match run.as_ref() {
                Some(run) if run.failures > 0 => part.stop(format!(
                    "The last run could not move {} Cookbooks. Somebody can have taken \
                     the write access away in Forgejo. CookLangHub never gives that \
                     access again on its own. Open the Cookbook in Forgejo and give the \
                     access again.",
                    run.failures
                )),
                _ => part,
            }
        }
        Err(error) => {
            facts.push(fact("Credential", "Forgejo refuses it"));
            part.with(facts).stop(format!(
                "Forgejo refuses the credential of the automation account: {error}. A \
                 Cookbook that follows a Recipe stays where it is. Make a new access \
                 token for {} in Forgejo, put it in COOKLANGHUB_AUTOMATION_TOKEN, and \
                 start the application again.",
                account.login
            ))
        }
    }
}

/// 6. The Cooklang parser.
fn parser() -> Subsystem {
    let part = Subsystem::new(
        "The parser",
        "`cooklang-rs` reads every Recipe. CookLangHub never writes its own \
         Cooklang reader, and it never reformats a source that a person wrote.",
        State::Working,
    );

    let facts = vec![
        fact("Reader", "cooklang-rs, with all canonical extensions"),
        fact(
            "Units",
            "the bundled English names, and the German names in units/german.toml",
        ),
    ];

    let read = crate::recipe::parse(SELF_CHECK);
    if !read.errors.is_empty() {
        let messages: Vec<String> = read.errors.iter().map(|e| e.message.clone()).collect();
        return part.with(facts).stop(format!(
            "The parser cannot read the self-check Recipe: {}. Every Recipe page shows a \
             broken state while this continues. This is a fault in the build of this \
             application, not in a Recipe.",
            messages.join("; ")
        ));
    }

    let Some(recipe) = crate::recipe::parse_recipe(SELF_CHECK) else {
        return part.with(facts).stop(
            "The parser cannot read the self-check Recipe. Every Recipe page shows a \
             broken state while this continues.",
        );
    };

    let counted = (
        recipe.ingredients.len(),
        recipe.cookware.len(),
        recipe.timers.len(),
    );

    if counted != (1, 1, 1) {
        return part.with(facts).stop(format!(
            "The parser read the self-check Recipe as {} ingredients, {} cookware, and \
             {} timers, and one of each is correct. A German unit name is the usual \
             cause: without units/german.toml a timer such as ~{{8%Min.}} is an error.",
            counted.0, counted.1, counted.2
        ));
    }

    let mut facts = facts;
    facts.push(fact(
        "Self-check",
        "the parser reads one ingredient, one cookware, and one timer in a German unit",
    ));

    part.with(facts)
}

// ---------------------------------------------------------------- helpers

/// The major part of a release name, for example `15` of `15.0.7`.
pub fn major(release: &str) -> Option<String> {
    let digits: String = release
        .trim()
        .trim_start_matches('v')
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();

    (!digits.is_empty()).then_some(digits)
}

/// How long ago a moment was, in words.
///
/// A day and a clock need a time zone, and an administrator wants to know
/// how old a thing is rather than when it happened.
fn ago(at: i64) -> String {
    let seconds = now() - at;

    if seconds < 0 {
        return "just now".to_string();
    }

    let (count, unit) = match seconds {
        0..=59 => return "less than a minute ago".to_string(),
        60..=3599 => (seconds / 60, "minute"),
        3600..=86_399 => (seconds / 3600, "hour"),
        _ => (seconds / 86_400, "day"),
    };

    if count == 1 {
        format!("1 {unit} ago")
    } else {
        format!("{count} {unit}s ago")
    }
}

/// One line about the last run of a sweep.
fn describe(sweep: Option<&Sweep>, changed: &str) -> String {
    let Some(sweep) = sweep else {
        return "it has not run yet".to_string();
    };

    format!(
        "{}, {} read, {} {changed}, {} removed, {} not answered",
        ago(sweep.ran_at),
        sweep.scanned,
        sweep.changed,
        sweep.removed,
        sweep.failures
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_major_release_is_the_leading_number() {
        assert_eq!(major("15.0.7"), Some("15".to_string()));
        assert_eq!(major("v16.0.0"), Some("16".to_string()));
        assert_eq!(major("15"), Some("15".to_string()));
        assert_eq!(major("+dev"), None);
        assert_eq!(major(""), None);
    }

    #[test]
    fn the_tested_release_matches_the_bundled_one() {
        // The compose file names the release that a self-hoster gets, and
        // this constant is what the page compares a running Forgejo with.
        // They must never drift apart.
        let compose = include_str!("../docker-compose.yml");
        let line = compose
            .lines()
            .find(|line| line.contains("codeberg.org/forgejo/forgejo"))
            .expect("the compose file names no Forgejo image");
        let tag = line.rsplit(':').next().unwrap_or_default().trim();

        assert_eq!(
            major(tag),
            Some(TESTED_FORGEJO_MAJOR.to_string()),
            "the bundled Forgejo is `{tag}` and the tested major is `{TESTED_FORGEJO_MAJOR}`"
        );
    }

    #[test]
    fn the_bundled_forgejo_names_one_release_and_never_floats() {
        // A floating tag would upgrade Forgejo whenever the image is pulled,
        // and an upgrade must be something an administrator decides.
        let compose = include_str!("../docker-compose.yml");
        let line = compose
            .lines()
            .find(|line| line.contains("codeberg.org/forgejo/forgejo"))
            .expect("the compose file names no Forgejo image");
        let tag = line.rsplit(':').next().unwrap_or_default().trim();

        assert_ne!(tag, "latest", "`latest` upgrades across a major release");
        assert_eq!(
            tag.split('.').count(),
            3,
            "the tag must name one release, for example 15.0.7, and `{tag}` does not"
        );
    }

    #[test]
    fn a_moment_is_reported_as_an_age() {
        let at = now();
        assert_eq!(ago(at), "less than a minute ago");
        assert_eq!(ago(at - 60), "1 minute ago");
        assert_eq!(ago(at - 3600), "1 hour ago");
        assert_eq!(ago(at - 7200), "2 hours ago");
        assert_eq!(ago(at - 86_400 * 3), "3 days ago");
        // A clock that moved backwards must not print a negative age.
        assert_eq!(ago(at + 500), "just now");
    }

    #[test]
    fn a_sweep_that_never_ran_says_so() {
        assert_eq!(describe(None, "written"), "it has not run yet");
        assert!(
            describe(
                Some(&Sweep {
                    ran_at: now() - 120,
                    scanned: 4,
                    changed: 3,
                    removed: 1,
                    failures: 0,
                }),
                "written"
            )
            .contains("4 read, 3 written, 1 removed, 0 not answered")
        );
    }

    #[test]
    fn the_parser_self_check_passes_in_this_build() {
        // The self-check is what the page reports. If it fails here, the
        // German units did not reach the converter.
        let part = parser();
        assert_eq!(part.state, State::Working, "problems: {:?}", part.problems);
    }

    #[test]
    fn the_internal_forgejo_address_reaches_an_administrator_only() {
        // While Forgejo is away nobody can be named an administrator, so the
        // Forgejo card is the one card that anybody can reach. The address
        // this application talks to is a name on the internal network, and
        // every other page of this product shows the public address only.
        let internal = "http://forgejo:3000";
        let public = "https://forge.example.test";
        let forgejo = ForgejoClient::with_urls(internal, public).unwrap();
        let away: Result<String, crate::forgejo::ForgejoError> = Err(
            crate::forgejo::ForgejoError::Unreachable(format!("cannot connect to {internal}")),
        );

        let anybody = forgejo_state(&forgejo, &away, false);
        let written = format!("{:?}", (&anybody.facts, &anybody.problems));
        assert!(
            !written.contains(internal),
            "the internal address must not reach a visitor: {written}"
        );
        assert!(written.contains(public), "got {written}");
        assert_eq!(anybody.state, State::Fault);

        // An administrator has to know which address failed.
        let administrator = forgejo_state(&forgejo, &away, true);
        let written = format!("{:?}", (&administrator.facts, &administrator.problems));
        assert!(written.contains(internal), "got {written}");
    }

    #[test]
    fn a_badge_is_green_only_while_a_subsystem_works() {
        assert_eq!(State::Working.badge(), "metadata-cuisine");
        assert_eq!(State::Quiet.badge(), "metadata-servings");
        assert_eq!(State::Fault.badge(), "metadata-servings");
    }

    #[tokio::test]
    async fn a_sweep_report_survives_a_restart() {
        let pool = crate::db::connect("sqlite://:memory:").await.unwrap();

        assert_eq!(last_sweep(&pool, RECIPE_INDEX).await, None);

        record_sweep(&pool, RECIPE_INDEX, 7, 5, 2, 1).await;
        let read = last_sweep(&pool, RECIPE_INDEX).await.expect("no row");
        assert_eq!(read.scanned, 7);
        assert_eq!(read.changed, 5);
        assert_eq!(read.removed, 2);
        assert_eq!(read.failures, 1);

        // A second run replaces the first, so the page shows the last one.
        record_sweep(&pool, RECIPE_INDEX, 1, 1, 0, 0).await;
        let read = last_sweep(&pool, RECIPE_INDEX).await.expect("no row");
        assert_eq!(read.scanned, 1);
        assert_eq!(read.failures, 0);
    }
}
