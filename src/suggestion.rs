//! Suggestions: a change that somebody proposes to a Recipe they cannot
//! change themselves.
//!
//! A Suggestion is a Forgejo pull request, and that is the whole of it.
//! Forgejo holds the proposal, its state, and its conversation. This module
//! adds no second store, no marker in Git, and no state of its own, so
//! there is nothing here that can disagree with Forgejo.
//!
//! The proposal reaches Forgejo through AGit. The application offers a
//! Version on `refs/for/<branch>/<topic>` and Forgejo makes the Suggestion
//! out of it. Nothing is copied: the person needs no Variation of the
//! Recipe, and Forgejo takes the Version from anybody it gives read access
//! to. That is why a Reader can suggest a change to a Recipe that they
//! cannot write to.
//!
//! The topic carries the person and nothing else, so one person has one
//! Suggestion for one Recipe. Every save goes to the same topic, and
//! Forgejo adds the Version to the Suggestion that is already there.
//!
//! A save is built on the Version that the page started from, and Forgejo
//! takes it only while the Suggestion still holds that Version. A second
//! tab that wrote first therefore keeps what it wrote, and the later save
//! is refused with a reason. The application never joins two texts together
//! and never picks a winner.
//!
//! The two states a person reads are the WIP convention of Forgejo. A
//! Suggestion whose title begins with a work-in-progress prefix is
//! **Editing in progress**, and one without it is **Ready for review**.
//! Forgejo alone holds that title.

use crate::forgejo::{ForgejoClient, ForgejoError, ForgejoUser, PullRequest};
use crate::git::{GitAdapter, GitError, Identity, PushSuggestion};
use crate::secret::Secret;

/// Where a Suggestion lives. One person has one Suggestion for one Recipe,
/// so the name carries the person and nothing else.
const TOPIC_PREFIX: &str = "suggestion-";

/// The longest login this application will build a topic from.
const MAX_LOGIN_CHARS: usize = 64;

/// How many Suggestions one page shows.
pub const MAX_SUGGESTIONS: u32 = 50;

/// The prefixes that Forgejo reads as work in progress.
///
/// These are the Forgejo defaults. A person never reads one: the
/// application takes the prefix off before it draws a title, and shows the
/// state in words instead.
const WIP_PREFIXES: [&str; 2] = ["WIP:", "[WIP]"];

/// The prefix that this application writes.
const WIP_PREFIX: &str = "WIP:";

/// The description that every Suggestion Version carries.
const SUGGESTION_MESSAGE: &str = "Suggestion";

/// Shown while a person writes, after each save.
pub const SAVED_MESSAGE: &str = "CookLangHub saved your Suggestion.";

/// Shown when a second tab, or another device, saved a newer text.
pub const STALE_MESSAGE: &str = "CookLangHub did not save this text. Your Suggestion changed in a different tab or on a different device. Copy your text. Then open the Suggestion again to get the newest text.";

/// Shown when the save did not reach Forgejo.
pub const UNSAVED_MESSAGE: &str = "CookLangHub cannot save your Suggestion at the moment. Your text is still on this page. Try again.";

/// Shown on the editor while a Suggestion is open.
pub const NOTICE_MESSAGE: &str = "This is your Suggestion. CookLangHub saves it while you write. It does not change the Recipe until an Editor accepts it.";

/// Shown before the first save, when the person has no Suggestion yet.
pub const NEW_MESSAGE: &str = "Change the text below. CookLangHub makes a Suggestion from your first change, and it saves your work while you write. The Recipe does not change until an Editor accepts your Suggestion.";

/// Shown when this application cannot make a Suggestion name for this
/// person.
pub const NO_TOPIC_MESSAGE: &str = "CookLangHub cannot keep a Suggestion for this Recipe. Your text is still on this page. Copy your text, and write to the owner of the Recipe.";

/// Shown when Forgejo holds more than one open Suggestion of this person.
pub const TOO_MANY_MESSAGE: &str = "You have more than one open Suggestion for this Recipe. CookLangHub cannot tell which one to write in, and it will not guess. Open the Recipe in Forgejo to see them.";

/// Shown when the Suggestion on the page is closed now.
pub const GONE_MESSAGE: &str = "This Suggestion is not open any more. Somebody accepted it or declined it. Your text is still on this page. Open the Recipe again to start a new Suggestion.";

/// Shown when Forgejo refuses an action, or cannot answer.
pub const REFUSED_MESSAGE: &str =
    "Forgejo did not accept this action. Open the Recipe in Forgejo to see what is possible there.";

/// Shown when a Suggestion was made somewhere else.
pub const ELSEWHERE_MESSAGE: &str = "Somebody made this Suggestion outside CookLangHub, so it cannot be changed here. Open the Recipe in Forgejo to work on it.";

#[derive(Debug, thiserror::Error)]
pub enum SuggestionError {
    /// The login of this person cannot become a name that Git accepts.
    #[error("CookLangHub cannot keep a Suggestion for this Recipe")]
    NoTopic,
    /// Forgejo holds more than one open Suggestion of this person for this
    /// Recipe. The application does not guess which one to write in.
    #[error("you have more than one open Suggestion for this Recipe")]
    TooMany,
    /// The Suggestion that the page carried is closed now.
    #[error("this Suggestion is not open any more")]
    Gone,
    /// Somebody wrote first, so nothing was saved and the Suggestion keeps
    /// exactly what it held.
    #[error("your Suggestion changed somewhere else")]
    Stale,
    #[error(transparent)]
    Forgejo(#[from] ForgejoError),
    #[error(transparent)]
    Git(#[from] GitError),
}

/// What a person reads about a Suggestion.
///
/// Every state comes from Forgejo. Nothing here is stored, and nothing here
/// is worked out from a record that this application keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// The person is still writing. Forgejo marks it work in progress.
    Editing,
    /// The person finished, and an Editor can read it.
    Ready,
    /// An Editor accepted the Suggestion, and it is part of the Recipe.
    Accepted,
    /// An Editor declined the Suggestion.
    Declined,
}

impl State {
    /// The words a person reads.
    pub fn label(self) -> &'static str {
        match self {
            Self::Editing => "Editing in progress",
            Self::Ready => "Ready for review",
            Self::Accepted => "Accepted",
            Self::Declined => "Declined",
        }
    }

    /// Whether somebody can still write in this Suggestion.
    pub fn is_open(self) -> bool {
        matches!(self, Self::Editing | Self::Ready)
    }
}

/// The state of a Suggestion, as Forgejo reports it.
pub fn state_of(pull: &PullRequest) -> State {
    if pull.merged {
        State::Accepted
    } else if !pull.is_open() {
        State::Declined
    } else if is_editing(&pull.title) {
        State::Editing
    } else {
        State::Ready
    }
}

/// The topic that carries the Suggestion of one person.
///
/// Gives `None` when the login cannot become a safe name. Forgejo does not
/// hand out a login like that, but the value arrives from outside this
/// application, so it is checked rather than trusted.
pub fn topic(login: &str) -> Option<String> {
    if !is_safe_login(login) {
        return None;
    }
    Some(format!("{TOPIC_PREFIX}{login}"))
}

/// Whether a login can become part of a name that Git accepts.
fn is_safe_login(login: &str) -> bool {
    if login.is_empty() || login.chars().count() > MAX_LOGIN_CHARS {
        return false;
    }

    if !login
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return false;
    }

    // A leading dash reads as an option to a command. A dot at either end,
    // a doubled dot, and the `.lock` ending are names that Git refuses.
    !(login.starts_with('-')
        || login.starts_with('.')
        || login.ends_with('.')
        || login.contains("..")
        || login.ends_with(".lock"))
}

/// Whether Forgejo marks this title as work in progress.
pub fn is_editing(title: &str) -> bool {
    without_prefix(title).is_some()
}

/// The title without the Forgejo prefix, which no person reads.
pub fn plain_title(title: &str) -> &str {
    without_prefix(title).unwrap_or_else(|| title.trim())
}

/// The title of a Suggestion that somebody is still writing.
pub fn editing_title(title: &str) -> String {
    format!("{WIP_PREFIX} {}", plain_title(title))
}

/// The title of a Suggestion that an Editor can read.
pub fn ready_title(title: &str) -> String {
    plain_title(title).to_string()
}

/// The rest of a title, when it begins with a work-in-progress prefix.
fn without_prefix(title: &str) -> Option<&str> {
    let trimmed = title.trim();

    WIP_PREFIXES.iter().find_map(|prefix| {
        // `get` gives nothing when the cut would fall inside a letter, so a
        // title that begins with a multi-byte letter cannot break this.
        trimmed
            .get(..prefix.len())
            .filter(|start| start.eq_ignore_ascii_case(prefix))
            .map(|_| trimmed[prefix.len()..].trim_start())
    })
}

/// The name a page shows for the Suggestion of a Recipe.
pub fn title_for(recipe_title: &str) -> String {
    format!("Suggestion for {recipe_title}")
}

/// What Forgejo holds as the open Suggestion of one person.
#[derive(Debug, Clone)]
pub enum Mine {
    /// This person has no open Suggestion for this Recipe.
    None,
    /// The one open Suggestion of this person.
    One(Box<PullRequest>),
    /// Forgejo holds more than one. The application does not guess.
    Several,
}

/// Read the open Suggestion of one person for one Recipe.
///
/// Only a Suggestion that this application can write to counts. A pull
/// request that somebody made from a copy of the Recipe is a Suggestion as
/// well, and it appears in the list, but it is changed in Forgejo and not
/// here.
pub async fn mine(
    forgejo: &ForgejoClient,
    token: &Secret<String>,
    owner: &str,
    slug: &str,
    login: &str,
) -> Result<Mine, SuggestionError> {
    let open = forgejo
        .list_pull_requests(Some(token), owner, slug, "open", MAX_SUGGESTIONS)
        .await?;

    let mut found: Vec<PullRequest> = open
        .into_iter()
        .filter(|pull| pull.is_agit() && pull.author() == login)
        .collect();

    match found.len() {
        0 => Ok(Mine::None),
        1 => Ok(Mine::One(Box::new(found.remove(0)))),
        _ => Ok(Mine::Several),
    }
}

/// Read the Suggestions of a Recipe, newest first.
///
/// Forgejo names them and decides who may see them. This application keeps
/// no copy, so a Suggestion that somebody makes in Forgejo appears here at
/// once.
pub async fn list(
    forgejo: &ForgejoClient,
    token: Option<&Secret<String>>,
    owner: &str,
    slug: &str,
) -> Result<Vec<PullRequest>, ForgejoError> {
    let mut found = forgejo
        .list_pull_requests(token, owner, slug, "all", MAX_SUGGESTIONS)
        .await?;

    // The newest Suggestion is first, so the number counts backwards.
    found.sort_by_key(|pull| std::cmp::Reverse(pull.number));
    Ok(found)
}

/// A request to save the work of one person into their Suggestion.
pub struct Save<'a> {
    pub forgejo: &'a ForgejoClient,
    pub git: &'a dyn GitAdapter,
    pub token: &'a Secret<String>,
    pub user: &'a ForgejoUser,
    /// Who the Version belongs to.
    pub identity: &'a Identity,
    pub owner: &'a str,
    pub slug: &'a str,
    /// The branch that carries the published Recipe.
    pub branch: &'a str,
    /// The Cooklang the person wrote.
    pub source: &'a str,
    /// The published Version the person started from.
    pub base_version: &'a str,
    /// The Version the Suggestion held when the page opened. `None` says
    /// the person had no Suggestion then.
    pub expected: Option<&'a str>,
    /// The title a cook reads for the Recipe. It names a new Suggestion.
    pub recipe_title: &'a str,
}

/// What a save left behind.
#[derive(Debug, Clone)]
pub struct Saved {
    /// The number of the Suggestion in Forgejo.
    pub number: i64,
    /// The Version the Suggestion holds now. The next save sends it back.
    pub version: String,
    /// Whether this save made the Suggestion.
    pub created: bool,
}

/// Save the work of one person into their Suggestion.
///
/// The first save makes the Suggestion. Every save after it adds one
/// Version to the same Suggestion, and no save makes a second one.
///
/// Nothing here writes to the published Recipe. The Recipe changes when an
/// Editor accepts the Suggestion, and at no other moment.
pub async fn save(request: Save<'_>) -> Result<Saved, SuggestionError> {
    let Some(topic) = topic(&request.user.login) else {
        tracing::warn!(login = %request.user.login, "this login cannot carry a Suggestion");
        return Err(SuggestionError::NoTopic);
    };

    let held = mine(
        request.forgejo,
        request.token,
        request.owner,
        request.slug,
        &request.user.login,
    )
    .await?;

    // What to build the new Version on, and what to name the Suggestion
    // with when Forgejo has to make one.
    let (parent_reference, parent_version, number, title) = match &held {
        Mine::Several => return Err(SuggestionError::TooMany),
        Mine::One(pull) => {
            // The page carries the Version it started from. When the
            // Suggestion no longer holds it, somebody wrote first.
            match request.expected {
                Some(expected) if expected == pull.head.sha => {}
                // A second tab made the Suggestion while this page was
                // open. Nothing here may write over what it saved.
                _ => return Err(SuggestionError::Stale),
            }

            (
                pull.head.reference.clone(),
                pull.head.sha.clone(),
                Some(pull.number),
                None,
            )
        }
        Mine::None => {
            // The page carries a Suggestion that Forgejo no longer holds
            // open, so it was accepted or declined while the person wrote.
            if request.expected.is_some() {
                return Err(SuggestionError::Gone);
            }

            (
                request.branch.to_string(),
                request.base_version.to_string(),
                None,
                Some(editing_title(&title_for(request.recipe_title))),
            )
        }
    };

    let mut files = std::collections::BTreeMap::new();
    files.insert(
        crate::recipe::RECIPE_FILE.to_string(),
        request.source.as_bytes().to_vec(),
    );

    let version = request
        .git
        .push_suggestion(PushSuggestion {
            remote_url: &request
                .forgejo
                .git_url(&format!("{}/{}", request.owner, request.slug)),
            token: request.token,
            identity: request.identity,
            branch: request.branch,
            topic: &topic,
            parent_reference: &parent_reference,
            parent_version: &parent_version,
            message: SUGGESTION_MESSAGE,
            title: title.as_deref(),
            description: None,
            files,
        })
        .await
        .map_err(|error| match error {
            GitError::Stale => SuggestionError::Stale,
            other => SuggestionError::Git(other),
        })?;

    // Forgejo gives the Suggestion its number, so a Suggestion that was
    // just made is read back rather than guessed at.
    let number = match number {
        Some(number) => number,
        None => match mine(
            request.forgejo,
            request.token,
            request.owner,
            request.slug,
            &request.user.login,
        )
        .await?
        {
            Mine::One(pull) => pull.number,
            _ => return Err(SuggestionError::TooMany),
        },
    };

    Ok(Saved {
        number,
        version,
        created: title.is_some(),
    })
}

/// Mark a Suggestion **Ready for review**, or put it back to **Editing in
/// progress**.
///
/// The state is the title that Forgejo holds, so this writes the title and
/// keeps nothing. `note` replaces the words that go with the Suggestion; an
/// empty note leaves them as they are.
pub async fn set_state(
    forgejo: &ForgejoClient,
    token: &Secret<String>,
    owner: &str,
    slug: &str,
    pull: &PullRequest,
    ready: bool,
    note: &str,
) -> Result<(), SuggestionError> {
    let title = if ready {
        ready_title(&pull.title)
    } else {
        editing_title(&pull.title)
    };

    let body = (!note.trim().is_empty()).then(|| note.trim());

    forgejo
        .edit_pull_request(token, owner, slug, pull.number, Some(&title), body)
        .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The words of the forge, which no message here may say.
    const FORGE_WORDS: [&str; 13] = [
        "commit",
        "commits",
        "branch",
        "branches",
        "diff",
        "repository",
        "repo",
        "fork",
        "patch",
        "sha",
        "merge",
        "rebase",
        "git",
    ];

    /// Whether a text says one of the words of the forge.
    ///
    /// Whole words only. `Sharing` is an area of a Recipe and `share` is
    /// what a person does with one, so neither may be read as the
    /// identifier that Git uses.
    fn says_forge_word(text: &str) -> Option<&'static str> {
        let lower = text.to_lowercase();

        for phrase in ["pull request", "merge request"] {
            if lower.contains(phrase) {
                return Some("pull request");
            }
        }

        let spoken: Vec<&str> = lower
            .split(|c: char| !c.is_alphanumeric())
            .filter(|word| !word.is_empty())
            .collect();

        FORGE_WORDS.into_iter().find(|word| spoken.contains(word))
    }

    #[test]
    fn every_message_a_person_reads_uses_cooking_words() {
        for message in [
            SAVED_MESSAGE,
            STALE_MESSAGE,
            UNSAVED_MESSAGE,
            NOTICE_MESSAGE,
            NEW_MESSAGE,
            NO_TOPIC_MESSAGE,
            TOO_MANY_MESSAGE,
            GONE_MESSAGE,
            REFUSED_MESSAGE,
            ELSEWHERE_MESSAGE,
        ] {
            assert_eq!(
                says_forge_word(message),
                None,
                "a word of the forge must not reach the person: {message}"
            );
        }
    }

    #[test]
    fn every_refusal_a_person_reads_uses_cooking_words() {
        for message in [
            SuggestionError::NoTopic.to_string(),
            SuggestionError::TooMany.to_string(),
            SuggestionError::Gone.to_string(),
            SuggestionError::Stale.to_string(),
        ] {
            assert_eq!(
                says_forge_word(&message),
                None,
                "a word of the forge must not reach the person: {message}"
            );
        }
    }

    #[test]
    fn every_state_a_person_reads_uses_cooking_words() {
        for state in [
            State::Editing,
            State::Ready,
            State::Accepted,
            State::Declined,
        ] {
            assert_eq!(says_forge_word(state.label()), None);
        }
    }

    #[test]
    fn one_person_gets_one_suggestion_name_for_one_recipe() {
        // The name carries the person and nothing else, so a second
        // Suggestion for the same Recipe cannot exist.
        assert_eq!(topic("sam"), Some("suggestion-sam".to_string()));
        assert_eq!(topic("sam"), topic("sam"));
        assert_ne!(topic("sam"), topic("kim"));
        assert_eq!(topic("a-b_c.d"), Some("suggestion-a-b_c.d".to_string()));
    }

    #[test]
    fn a_login_that_git_would_refuse_gets_no_suggestion_name() {
        for login in [
            "",
            "   ",
            "-sam",
            ".sam",
            "sam.",
            "sa..m",
            "sam.lock",
            "sam/kim",
            "sam kim",
            "sam~1",
            "sam^",
            "sam:kim",
            "sam?",
            "sam*",
            "sam[1]",
            "sam\\kim",
            "../../etc",
            "--upload-pack=touch",
        ] {
            assert!(
                topic(login).is_none(),
                "`{login}` must not name a Suggestion"
            );
        }

        assert!(topic(&"a".repeat(MAX_LOGIN_CHARS + 1)).is_none());
    }

    #[test]
    fn the_two_states_are_the_forgejo_work_in_progress_convention() {
        assert!(is_editing("WIP: Suggestion for Chili"));
        assert!(is_editing("[WIP] Suggestion for Chili"));
        // Forgejo reads the prefix whatever the letters look like.
        assert!(is_editing("wip: Suggestion for Chili"));
        assert!(!is_editing("Suggestion for Chili"));
        assert!(!is_editing("Wipe the pan"));
    }

    #[test]
    fn the_prefix_never_reaches_the_title_a_person_reads() {
        assert_eq!(
            plain_title("WIP: Suggestion for Chili"),
            "Suggestion for Chili"
        );
        assert_eq!(
            plain_title("[WIP] Suggestion for Chili"),
            "Suggestion for Chili"
        );
        assert_eq!(
            plain_title("  Suggestion for Chili  "),
            "Suggestion for Chili"
        );
    }

    #[test]
    fn a_title_of_multibyte_letters_does_not_break_the_prefix_check() {
        // Cutting by byte could fall inside a letter and stop the page.
        assert!(!is_editing("Über Chili"));
        assert_eq!(plain_title("Über Chili"), "Über Chili");
    }

    #[test]
    fn marking_ready_takes_the_prefix_off_and_marking_editing_puts_it_back() {
        let editing = editing_title(&title_for("Chili"));
        assert_eq!(editing, "WIP: Suggestion for Chili");
        assert!(is_editing(&editing));

        let ready = ready_title(&editing);
        assert_eq!(ready, "Suggestion for Chili");
        assert!(!is_editing(&ready));

        // Marking twice must not stack two prefixes.
        assert_eq!(editing_title(&editing), editing);
        assert_eq!(ready_title(&ready), ready);
    }

    /// One pull request, as Forgejo reports it.
    fn pull(title: &str, state: &str, merged: bool) -> PullRequest {
        serde_json::from_value(serde_json::json!({
            "number": 1,
            "title": title,
            "state": state,
            "merged": merged,
            "flow": 1,
        }))
        .expect("the answer must read")
    }

    #[test]
    fn the_state_of_a_suggestion_comes_from_forgejo_alone() {
        assert_eq!(
            state_of(&pull("WIP: Suggestion for Chili", "open", false)),
            State::Editing
        );
        assert_eq!(
            state_of(&pull("Suggestion for Chili", "open", false)),
            State::Ready
        );
        assert_eq!(
            state_of(&pull("Suggestion for Chili", "closed", true)),
            State::Accepted
        );
        assert_eq!(
            state_of(&pull("Suggestion for Chili", "closed", false)),
            State::Declined
        );
        // An accepted Suggestion that Forgejo still marks work in progress
        // is accepted. What happened to it counts for more than the title.
        assert_eq!(
            state_of(&pull("WIP: Suggestion for Chili", "closed", true)),
            State::Accepted
        );
    }

    #[test]
    fn only_an_open_suggestion_can_be_written_in() {
        assert!(State::Editing.is_open());
        assert!(State::Ready.is_open());
        assert!(!State::Accepted.is_open());
        assert!(!State::Declined.is_open());
    }

    #[test]
    fn a_suggestion_made_outside_this_application_is_not_written_to_here() {
        let ours: PullRequest = serde_json::from_value(serde_json::json!({
            "number": 1, "flow": 1, "state": "open",
        }))
        .expect("the answer must read");
        let theirs: PullRequest = serde_json::from_value(serde_json::json!({
            "number": 2, "flow": 0, "state": "open",
        }))
        .expect("the answer must read");

        assert!(ours.is_agit());
        assert!(!theirs.is_agit());
    }
}
