//! Turning a parsed Recipe into something a cook can read.
//!
//! The output of this module is plain text in a small view model. It never
//! contains markup. The template escapes every value, so a Recipe that
//! contains `<script>` shows those characters and cannot run.
//!
//! Nothing here writes. Reading a Recipe never changes the stored file, so
//! a later parser release changes what a person sees and never what Git
//! holds. The same is true of the view options: a cook can scale the
//! servings and convert the units, and the stored Cooklang source keeps
//! every byte.

use cooklang::convert::{ConvertTo, ConvertUnit, ConvertValue, Converter};
use cooklang::quantity::{Quantity, Value};
use cooklang::{Content, Item, Recipe};

use crate::scale::{self, Units, View};

/// The unit that a countdown counts in.
const SECOND: &str = "s";

/// The longest timer that gets a countdown, in seconds.
///
/// A timer longer than one day is a note to the cook and not something to
/// watch on a screen, so it stays plain text.
const MAX_TIMER_SECONDS: f64 = 24.0 * 60.0 * 60.0;

/// What kind of thing a piece of a step is.
///
/// One color per Cooklang entity, the same mapping that CookCLI uses, so a
/// cook who knows that interface can read this one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PieceKind {
    Text,
    Ingredient,
    Cookware,
    Timer,
}

impl PieceKind {
    /// The class that carries the color of this entity.
    ///
    /// These are the CookCLI class names, so the badges are styled by the
    /// CookCLI stylesheet without a change.
    pub fn css_class(&self) -> &'static str {
        match self {
            PieceKind::Text => "",
            PieceKind::Ingredient => "ingredient-badge",
            PieceKind::Cookware => "cookware-badge",
            PieceKind::Timer => "timer-badge",
        }
    }

    /// What a screen reader says before the value.
    pub fn label(&self) -> &'static str {
        match self {
            PieceKind::Text => "",
            PieceKind::Ingredient => "Ingredient",
            PieceKind::Cookware => "Cookware",
            PieceKind::Timer => "Time",
        }
    }

    pub fn is_text(&self) -> bool {
        *self == PieceKind::Text
    }
}

/// One run of a step: either plain words or a marked entity.
#[derive(Debug, Clone)]
pub struct Piece {
    pub kind: PieceKind,
    pub text: String,
    /// The amount, when the Recipe gives one.
    pub quantity: Option<String>,
    /// How long a timer runs, in whole seconds.
    ///
    /// Set only for a timer that gives a plain number and a time unit. The
    /// page needs the number for the countdown, and the badge reads as
    /// plain text when the script does not run.
    pub timer_seconds: Option<u32>,
}

impl Piece {
    fn text(value: impl Into<String>) -> Self {
        Self {
            kind: PieceKind::Text,
            text: value.into(),
            quantity: None,
            timer_seconds: None,
        }
    }

    /// The words in the badge: the name and the amount together.
    ///
    /// The amount sits in the badge on purpose. CookCLI repeats every
    /// amount in a list below the step, which makes a cook look away from
    /// the sentence. See CLAUDE.md.
    pub fn badge_text(&self) -> String {
        match (&self.quantity, self.text.is_empty()) {
            (Some(quantity), true) => quantity.clone(),
            (Some(quantity), false) => format!("{} {quantity}", self.text),
            (None, _) => self.text.clone(),
        }
    }
}

/// A numbered step, or a paragraph that carries no instruction.
#[derive(Debug, Clone)]
pub struct Block {
    /// Zero for a paragraph that is not a step.
    pub number: u32,
    pub pieces: Vec<Piece>,
}

impl Block {
    pub fn is_step(&self) -> bool {
        self.number > 0
    }
}

#[derive(Debug, Clone)]
pub struct RenderedSection {
    pub name: Option<String>,
    pub blocks: Vec<Block>,
}

/// One thing a cook needs before starting, with its amount.
#[derive(Debug, Clone)]
pub struct Component {
    pub name: String,
    pub quantity: Option<String>,
    pub note: Option<String>,
}

/// One line of the Recipe metadata that is worth showing.
#[derive(Debug, Clone)]
pub struct Fact {
    pub label: String,
    pub value: String,
    /// Set only when the value is an address that is safe to follow.
    pub link: Option<String>,
    /// The CookCLI pill class for this kind of fact. CookCLI gives a time
    /// a different colour from a difficulty, and the stylesheet copied
    /// from CookCLI already carries every one of these classes.
    pub pill: &'static str,
    /// The mark CookCLI puts in front of this kind of fact.
    pub icon: Option<&'static str>,
}

/// What the serving and units control shows.
#[derive(Debug, Clone, Default)]
pub struct ScaleState {
    /// The serving count that the Recipe itself gives, when the page can
    /// scale it. Nothing here means the Recipe gives no usable number.
    pub base: Option<u32>,
    /// The serving count on screen.
    pub current: Option<u32>,
    /// The units on screen.
    pub units: Units,
    /// True when the page shows something other than the Recipe as written.
    pub changed: bool,
    /// Set when the page cannot do what the address asks for.
    pub note: Option<String>,
}

impl ScaleState {
    /// True when the Recipe gives a serving count that the page can scale.
    pub fn can_scale(&self) -> bool {
        self.base.is_some()
    }

    /// The value the number control starts with.
    pub fn value(&self) -> u32 {
        self.current.or(self.base).unwrap_or(scale::MIN_SERVINGS)
    }

    pub fn min(&self) -> u32 {
        scale::MIN_SERVINGS
    }

    pub fn max(&self) -> u32 {
        scale::MAX_SERVINGS
    }

    /// The unit choices, in the order the control shows them.
    pub fn choices(&self) -> [Units; 3] {
        Units::all()
    }
}

/// A Recipe ready for a template.
#[derive(Debug, Clone, Default)]
pub struct RenderedRecipe {
    pub servings: Option<String>,
    pub facts: Vec<Fact>,
    pub tags: Vec<String>,
    pub ingredients: Vec<Component>,
    pub cookware: Vec<Component>,
    pub sections: Vec<RenderedSection>,
    /// The state of the serving and units control.
    pub scale: ScaleState,
}

impl RenderedRecipe {
    pub fn is_empty(&self) -> bool {
        self.sections.iter().all(|s| s.blocks.is_empty())
            && self.ingredients.is_empty()
            && self.cookware.is_empty()
    }
}

/// What to gather, with a thing named twice brought together once.
///
/// A Recipe that uses oil in two steps names it twice. A cook reading the
/// list before starting needs one line with the total, not two lines to add
/// up. CookCLI does the same.
///
/// The amounts are added only when every mention writes the same unit. The
/// unit a person wrote is kept exactly: `2 EL` and `2 EL` make `4 EL`, never
/// `4 tbsp`. Mentions in units that do not match stay side by side, because
/// this application must not decide that a cup of oil and 50 g of oil are
/// the same amount.
fn gather<'a>(
    items: impl Iterator<Item = (&'a str, Option<&'a Quantity>, Option<&'a str>)>,
) -> Vec<Component> {
    let mut out: Vec<Component> = Vec::new();
    let mut amounts: Vec<Vec<&Quantity>> = Vec::new();

    for (name, quantity, note) in items {
        match out.iter().position(|held| held.name == name) {
            Some(at) => {
                if let Some(quantity) = quantity {
                    amounts[at].push(quantity);
                }
                // The first note that a Recipe gives is the one shown. A
                // later mention rarely repeats it and never contradicts it.
                if out[at].note.is_none() {
                    out[at].note = note.map(str::to_string);
                }
            }
            None => {
                out.push(Component {
                    name: name.to_string(),
                    quantity: None,
                    note: note.map(str::to_string),
                });
                amounts.push(quantity.into_iter().collect());
            }
        }
    }

    for (component, amounts) in out.iter_mut().zip(amounts) {
        component.quantity = total(&amounts);
    }

    out
}

/// The words for the amounts of one thing.
fn total(amounts: &[&Quantity]) -> Option<String> {
    match amounts {
        [] => None,
        // One mention is written out exactly as it stands, so a fraction
        // stays a fraction and a word stays a word.
        [only] => Some(only.to_string()),
        many => {
            let unit = many[0].unit();
            let same_unit = many.iter().all(|q| q.unit() == unit);

            let numbers: Option<Vec<f64>> = many
                .iter()
                .map(|q| match q.value() {
                    Value::Number(number) => Some(number.value()),
                    _ => None,
                })
                .collect();

            match (same_unit, numbers) {
                (true, Some(numbers)) => {
                    let sum: f64 = numbers.iter().sum();
                    Some(match unit {
                        Some(unit) => format!("{} {unit}", number_text(sum)),
                        None => number_text(sum),
                    })
                }
                // Units that do not match, or an amount that is not a
                // number. Show what the Recipe says and add nothing up.
                _ => {
                    let mut seen: Vec<String> = Vec::new();
                    for quantity in many {
                        let text = quantity.to_string();
                        if !seen.contains(&text) {
                            seen.push(text);
                        }
                    }
                    Some(seen.join(", "))
                }
            }
        }
    }
}

/// A number as a cook would write it: `4`, not `4.0`.
fn number_text(value: f64) -> String {
    if (value - value.round()).abs() < f64::EPSILON {
        return format!("{}", value.round() as i64);
    }
    let text = format!("{value:.3}");
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Metadata keys that the page shows on its own, so the fact list skips them.
const HANDLED_KEYS: [&str; 3] = ["title", "tags", "servings"];

/// Build the view model from a parsed Recipe, exactly as it is stored.
pub fn render(recipe: &Recipe) -> RenderedRecipe {
    render_with(recipe, &View::default(), crate::recipe::converter())
}

/// Build the view model with the options that the address asks for.
///
/// This is a pure function of what it is given, which is what makes the
/// serving counts and the unit conversions testable without Forgejo. It
/// works on a copy, so the Recipe that came from Git is never touched.
pub fn render_with(recipe: &Recipe, view: &View, converter: &Converter) -> RenderedRecipe {
    let scaled = scale::apply(recipe, view, converter);
    let recipe = &scaled.recipe;

    // A Recipe that uses the same thing in two steps names it twice. What a
    // cook needs before starting is one line with the total, not two lines
    // to add up, so the amounts of every mention are grouped here. This is
    // what CookCLI shows, and `cooklang-rs` does the adding, including the
    // part this application must not guess: two amounts in units that do
    // not combine stay side by side rather than becoming a wrong total.
    let ingredients = gather(
        recipe
            .ingredients
            .iter()
            .map(|i| (i.name.as_str(), i.quantity.as_ref(), i.note.as_deref())),
    );

    let cookware = gather(
        recipe
            .cookware
            .iter()
            .map(|c| (c.name.as_str(), c.quantity.as_ref(), c.note.as_deref())),
    );

    let sections = recipe
        .sections
        .iter()
        .map(|section| RenderedSection {
            name: section.name.clone(),
            blocks: section
                .content
                .iter()
                .map(|content| match content {
                    Content::Step(step) => Block {
                        number: step.number,
                        pieces: step
                            .items
                            .iter()
                            .map(|item| piece(recipe, item, converter))
                            .collect(),
                    },
                    Content::Text(text) => Block {
                        number: 0,
                        pieces: vec![Piece::text(text.clone())],
                    },
                })
                .collect(),
        })
        .collect();

    RenderedRecipe {
        servings: servings(recipe),
        facts: facts(recipe),
        tags: recipe
            .metadata
            .tags()
            .map(|tags| tags.iter().map(|t| t.to_string()).collect())
            .unwrap_or_default(),
        ingredients,
        cookware,
        sections,
        scale: ScaleState {
            base: scaled.base_servings,
            current: scaled.servings,
            units: scaled.units,
            changed: scaled.changed,
            note: scaled.note,
        },
    }
}

fn piece(recipe: &Recipe, item: &Item, converter: &Converter) -> Piece {
    match item {
        Item::Text { value } => Piece::text(value.clone()),
        Item::Ingredient { index } => match recipe.ingredients.get(*index) {
            Some(ingredient) => Piece {
                kind: PieceKind::Ingredient,
                text: ingredient.name.clone(),
                quantity: ingredient.quantity.as_ref().map(ToString::to_string),
                timer_seconds: None,
            },
            None => Piece::text(String::new()),
        },
        Item::Cookware { index } => match recipe.cookware.get(*index) {
            Some(cookware) => Piece {
                kind: PieceKind::Cookware,
                text: cookware.name.clone(),
                quantity: cookware.quantity.as_ref().map(ToString::to_string),
                timer_seconds: None,
            },
            None => Piece::text(String::new()),
        },
        Item::Timer { index } => match recipe.timers.get(*index) {
            Some(timer) => Piece {
                kind: PieceKind::Timer,
                text: timer.name.clone().unwrap_or_default(),
                quantity: timer.quantity.as_ref().map(ToString::to_string),
                timer_seconds: timer
                    .quantity
                    .as_ref()
                    .and_then(|quantity| timer_seconds(quantity, converter)),
            },
            None => Piece::text(String::new()),
        },
        // An inline quantity is a plain amount inside the text.
        other => Piece::text(inline_text(other)),
    }
}

/// How long a timer runs, in whole seconds.
///
/// The converter does the arithmetic, so `~{1%hour}` and `~{8%Min.}` both
/// give a number. A range such as `~{5-10%minutes}`, a word such as
/// `~{a while}`, and a unit that is not a time give nothing, and the badge
/// then stays plain text.
fn timer_seconds(quantity: &Quantity, converter: &Converter) -> Option<u32> {
    let Value::Number(number) = quantity.value() else {
        return None;
    };
    let unit = quantity.unit()?;

    let (converted, _) = converter
        .convert(
            ConvertValue::Number(number.value()),
            ConvertUnit::Key(unit),
            ConvertTo::Unit(ConvertUnit::Key(SECOND)),
        )
        .ok()?;

    let ConvertValue::Number(seconds) = converted else {
        return None;
    };

    // A value that is not finite, shorter than a second, or longer than a
    // day is not a countdown a cook can use.
    if !seconds.is_finite() || !(1.0..=MAX_TIMER_SECONDS).contains(&seconds) {
        return None;
    }

    Some(seconds.round() as u32)
}

fn inline_text(item: &Item) -> String {
    match item {
        Item::InlineQuantity { .. } => String::new(),
        Item::Text { value } => value.clone(),
        _ => String::new(),
    }
}

fn servings(recipe: &Recipe) -> Option<String> {
    // Read the written value rather than the parsed one, so `4` shows as
    // `4` and a range such as `4-6` keeps its own shape.
    let value = recipe.metadata.map.get("servings")?;
    let text = scalar(value);
    (!text.trim().is_empty()).then_some(text)
}

/// The CookCLI pill and mark for one metadata key.
///
/// CookCLI gives each kind of fact its own colour, and the stylesheet taken
/// from CookCLI holds all of these classes already. Without this map every
/// fact fell into `metadata-custom`, so the whole row showed as one purple.
/// That is not how CookCLI looks.
///
/// A key that CookCLI has no colour for keeps `metadata-custom`, which is
/// what CookCLI does with it as well.
fn pill_for(key: &str) -> (&'static str, Option<&'static str>) {
    let key = key.trim().to_ascii_lowercase().replace(['_', '-'], " ");

    match key.as_str() {
        "difficulty" => ("metadata-difficulty", Some("📊")),
        "prep time" | "preptime" | "prep" => ("metadata-prep", Some("⏱️")),
        "cook time" | "cooktime" | "cooking time" => ("metadata-cook", Some("🔥")),
        "time" | "total time" | "duration" => ("metadata-time", Some("⏰")),
        "course" | "category" | "meal" => ("metadata-course", Some("🍽️")),
        "cuisine" => ("metadata-cuisine", Some("🌍")),
        "diet" => ("metadata-diet", Some("🥗")),
        "author" | "source author" => ("metadata-author", Some("✍️")),
        // CookCLI writes this class and gives it no colour of its own, so
        // the pill shows plain. Kept the same on purpose.
        "source" => ("metadata-source", Some("📖")),
        _ => ("metadata-custom", None),
    }
}

/// Collect the metadata worth showing, as text.
fn facts(recipe: &Recipe) -> Vec<Fact> {
    let mut facts = Vec::new();

    // `map_filtered` gives only the keys that are not standard, and the
    // interesting ones here are standard. Read the whole map instead and
    // skip what the page already shows in its own place.
    for (key, value) in &recipe.metadata.map {
        let Some(key) = key.as_str() else { continue };
        if HANDLED_KEYS.contains(&key) {
            continue;
        }

        let text = scalar(value);
        if text.trim().is_empty() {
            continue;
        }

        let (pill, icon) = pill_for(key);

        facts.push(Fact {
            label: humanize(key),
            link: safe_link(&text),
            value: text,
            pill,
            icon,
        });
    }

    facts.sort_by(|a, b| a.label.cmp(&b.label));
    facts
}

/// Render a YAML value as one line of text. A structure becomes its own
/// compact form rather than debug output.
fn scalar(value: &serde_yaml::Value) -> String {
    match value {
        serde_yaml::Value::String(s) => s.clone(),
        serde_yaml::Value::Number(n) => n.to_string(),
        serde_yaml::Value::Bool(b) => b.to_string(),
        serde_yaml::Value::Sequence(items) => items
            .iter()
            .map(scalar)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(", "),
        _ => String::new(),
    }
}

/// Turn a metadata key into a label a person reads.
fn humanize(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    for (index, word) in key.split(['_', '-', ' ']).enumerate() {
        if word.is_empty() {
            continue;
        }
        if index > 0 {
            out.push(' ');
        }
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
        }
    }
    out
}

/// Return the value as a followable address, or nothing.
///
/// Only `http` and `https` pass. A scheme such as `javascript:` or `data:`
/// can run code or carry a payload, so it never becomes a link.
pub fn safe_link(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let lower = trimmed.to_ascii_lowercase();

    if lower.starts_with("http://") || lower.starts_with("https://") {
        // A control character or a space would let the value break out of
        // the attribute in a client that parses loosely.
        if trimmed.chars().any(|c| c.is_control() || c.is_whitespace()) {
            return None;
        }
        return Some(trimmed.to_string());
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recipe;

    fn parse(source: &str) -> Recipe {
        recipe::parse_recipe(source).expect("the fixture must parse")
    }

    /// The one piece of a step that is of this kind.
    fn only(recipe: &RenderedRecipe, kind: PieceKind) -> Piece {
        recipe.sections[0].blocks[0]
            .pieces
            .iter()
            .find(|p| p.kind == kind)
            .unwrap_or_else(|| panic!("no {kind:?} in the step"))
            .clone()
    }

    /// Render with the given view options.
    fn view(recipe: &Recipe, servings: Option<u32>, units: Units) -> RenderedRecipe {
        render_with(
            recipe,
            &View { servings, units },
            crate::recipe::converter(),
        )
    }

    #[test]
    fn a_step_keeps_its_words_and_marks_its_entities() {
        let r = render(&parse(
            "---\ntitle: T\n---\n\nChop the @onion{2} in a #pan{} for ~{5%minutes}.",
        ));

        let block = &r.sections[0].blocks[0];
        assert_eq!(block.number, 1);

        let kinds: Vec<PieceKind> = block.pieces.iter().map(|p| p.kind).collect();
        assert!(kinds.contains(&PieceKind::Ingredient));
        assert!(kinds.contains(&PieceKind::Cookware));
        assert!(kinds.contains(&PieceKind::Timer));

        let onion = block
            .pieces
            .iter()
            .find(|p| p.kind == PieceKind::Ingredient)
            .unwrap();
        assert_eq!(onion.text, "onion");
        assert_eq!(onion.quantity.as_deref(), Some("2"));
    }

    #[test]
    fn ingredients_and_cookware_are_collected_for_the_cook() {
        let r = render(&parse(
            "---\ntitle: T\n---\n\nMix @flour{500%g} and @water{300%ml} in a #bowl{}.",
        ));

        let names: Vec<&str> = r.ingredients.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["flour", "water"]);
        assert_eq!(r.ingredients[0].quantity.as_deref(), Some("500 g"));
        assert_eq!(r.cookware[0].name, "bowl");
    }

    #[test]
    fn a_thing_used_twice_is_gathered_once_with_the_total() {
        // The fault this fixes: a person shopping from the list saw `2 EL`
        // twice and bought half the oil the Recipe needs.
        let r = render(&parse(
            "Heat @oil{2%EL}.

Add more @oil{2%EL}.",
        ));

        assert_eq!(r.ingredients.len(), 1, "one line, not two");
        assert_eq!(r.ingredients[0].name, "oil");
        assert_eq!(r.ingredients[0].quantity.as_deref(), Some("4 EL"));
    }

    #[test]
    fn adding_up_never_rewrites_the_unit_a_person_wrote() {
        // `EL` is a German spoon. It must not become `tbsp` on the way.
        let r = render(&parse(
            "Nimm @Öl{2%EL}.

Nimm noch @Öl{1%EL}.",
        ));
        assert_eq!(r.ingredients[0].quantity.as_deref(), Some("3 EL"));

        let r = render(&parse(
            "Nimm @Mehl{250%g}.

Nimm @Mehl{250%g}.",
        ));
        assert_eq!(r.ingredients[0].quantity.as_deref(), Some("500 g"));
    }

    #[test]
    fn amounts_that_do_not_match_are_shown_and_not_added() {
        // A cup of oil and 50 g of oil are not the same amount, and this
        // application must not decide that they are.
        let r = render(&parse(
            "Heat @oil{1%cup}.

Add @oil{50%g}.",
        ));
        let shown = r.ingredients[0].quantity.as_deref().unwrap();

        assert!(shown.contains("1 cup"), "got `{shown}`");
        assert!(shown.contains("50 g"), "got `{shown}`");
    }

    #[test]
    fn one_mention_is_written_out_exactly_as_it_stands() {
        // A single amount goes through untouched, so a fraction stays one.
        let r = render(&parse("Add @salt{1/2%TL}."));
        assert_eq!(r.ingredients[0].quantity.as_deref(), Some("1/2 TL"));

        let r = render(&parse("Add @pepper{}."));
        assert_eq!(r.ingredients[0].quantity, None);
    }

    #[test]
    fn a_thing_with_no_amount_beside_one_with_an_amount_still_gathers() {
        let r = render(&parse(
            "Add @salt{1%TL}.

Add more @salt{}.",
        ));
        assert_eq!(r.ingredients.len(), 1);
        assert_eq!(r.ingredients[0].quantity.as_deref(), Some("1 TL"));
    }

    #[test]
    fn cookware_named_twice_is_gathered_too() {
        let r = render(&parse(
            "Use a #pot{1}.

Use another #pot{1}.",
        ));
        assert_eq!(r.cookware.len(), 1);
        assert_eq!(r.cookware[0].quantity.as_deref(), Some("2"));
    }

    #[test]
    fn a_total_reads_as_a_cook_would_write_it() {
        assert_eq!(number_text(4.0), "4");
        assert_eq!(number_text(0.5), "0.5");
        assert_eq!(number_text(1.25), "1.25");
        assert_eq!(number_text(500.0), "500");
    }

    #[test]
    fn german_units_render_as_written() {
        let r = render(&parse(
            "---\ntitle: T\n---\n\nGib @Öl{2%EL} dazu und warte ~{8%Min.}.",
        ));

        assert_eq!(r.ingredients[0].name, "Öl");
        assert_eq!(r.ingredients[0].quantity.as_deref(), Some("2 EL"));
    }

    #[test]
    fn each_kind_of_fact_gets_the_colour_cookcli_gives_it() {
        // The stylesheet from CookCLI carries a colour for each of these.
        // Before this map every fact was `metadata-custom`, so the row of
        // pills was one purple instead of CookCLI's several colours.
        for (key, expected) in [
            ("difficulty", "metadata-difficulty"),
            ("Prep Time", "metadata-prep"),
            ("prep_time", "metadata-prep"),
            ("cook time", "metadata-cook"),
            ("Total Time", "metadata-time"),
            ("cuisine", "metadata-cuisine"),
            ("diet", "metadata-diet"),
            ("author", "metadata-author"),
            ("course", "metadata-course"),
        ] {
            assert_eq!(pill_for(key).0, expected, "`{key}` has the wrong pill");
        }
    }

    #[test]
    fn a_fact_cookcli_has_no_colour_for_stays_the_plain_one() {
        for key in ["calories", "protein", "whatever a person wrote"] {
            assert_eq!(pill_for(key), ("metadata-custom", None));
        }
    }

    #[test]
    fn metadata_becomes_readable_facts() {
        let r = render(&parse(
            "---\ntitle: T\nprep time: 25 minutes\ndifficulty: Einfach\ntags: [vegan, quick]\n---\n\nStep.",
        ));

        let labels: Vec<&str> = r.facts.iter().map(|f| f.label.as_str()).collect();
        assert!(labels.contains(&"Prep Time"), "got {labels:?}");
        assert!(labels.contains(&"Difficulty"), "got {labels:?}");
        // The title and the tags have their own places on the page.
        assert!(!labels.contains(&"Title"));
        assert!(!labels.contains(&"Tags"));
        assert_eq!(r.tags, vec!["vegan", "quick"]);
    }

    #[test]
    fn an_http_source_becomes_a_link() {
        let r = render(&parse(
            "---\ntitle: T\nsource: https://example.test/recipe\n---\n\nStep.",
        ));
        let source = r.facts.iter().find(|f| f.label == "Source").unwrap();
        assert_eq!(source.link.as_deref(), Some("https://example.test/recipe"));
    }

    #[test]
    fn a_dangerous_scheme_never_becomes_a_link() {
        for value in [
            "javascript:alert(1)",
            "JavaScript:alert(1)",
            "data:text/html;base64,PHNjcmlwdD4=",
            "vbscript:msgbox",
            "file:///etc/passwd",
            "  javascript:alert(1)  ",
        ] {
            assert_eq!(safe_link(value), None, "`{value}` must not become a link");
        }
    }

    #[test]
    fn a_link_with_whitespace_is_refused() {
        // A space lets a value break out of an attribute in a loose parser.
        assert_eq!(safe_link("https://example.test/a b"), None);
        assert_eq!(safe_link("https://example.test/a\nb"), None);
    }

    #[test]
    fn markup_in_a_recipe_stays_text() {
        // The view model holds text. The template escapes it, so the marks
        // below can never become elements.
        let r = render(&parse(
            "---\ntitle: T\n---\n\nAdd @<script>alert(1)</script>{1} to the pot.",
        ));

        let piece = r.sections[0].blocks[0]
            .pieces
            .iter()
            .find(|p| p.kind == PieceKind::Ingredient)
            .unwrap();

        assert!(piece.text.contains("script"));
        // It is still text, not an element: nothing parsed it as markup.
        assert!(piece.text.contains('<'));
    }

    #[test]
    fn a_title_only_recipe_renders_as_empty() {
        let r = render(&parse("---\ntitle: Just A Title\n---\n"));
        assert!(r.is_empty());
    }

    #[test]
    fn each_entity_carries_its_own_class_and_label() {
        assert_eq!(PieceKind::Ingredient.css_class(), "ingredient-badge");
        assert_eq!(PieceKind::Cookware.css_class(), "cookware-badge");
        assert_eq!(PieceKind::Timer.css_class(), "timer-badge");
        assert_eq!(PieceKind::Ingredient.label(), "Ingredient");
        assert!(PieceKind::Text.is_text());
    }

    #[test]
    fn the_badge_holds_the_name_and_the_amount_together() {
        let piece = Piece {
            kind: PieceKind::Ingredient,
            text: "onion".to_string(),
            quantity: Some("2".to_string()),
            timer_seconds: None,
        };
        assert_eq!(piece.badge_text(), "onion 2");

        // A timer with no name is only its length.
        let timer = Piece {
            kind: PieceKind::Timer,
            text: String::new(),
            quantity: Some("5 minutes".to_string()),
            timer_seconds: Some(300),
        };
        assert_eq!(timer.badge_text(), "5 minutes");

        // Cookware usually carries no amount.
        let pan = Piece {
            kind: PieceKind::Cookware,
            text: "pan".to_string(),
            quantity: None,
            timer_seconds: None,
        };
        assert_eq!(pan.badge_text(), "pan");
    }

    #[test]
    fn a_timer_carries_its_length_in_seconds() {
        for (source, seconds) in [
            ("Wait ~{5%minutes}.", 300),
            ("Wait ~{1%hour}.", 3600),
            ("Wait ~{90%s}.", 90),
            ("Wait ~{8%Min.}.", 480),
            ("Wait ~cooking{2%Stunden}.", 7200),
        ] {
            let r = view(
                &parse(&format!("---\ntitle: T\n---\n\n{source}")),
                None,
                Units::AsWritten,
            );
            assert_eq!(
                only(&r, PieceKind::Timer).timer_seconds,
                Some(seconds),
                "`{source}` gave the wrong length"
            );
        }
    }

    #[test]
    fn a_timer_that_cannot_be_counted_stays_plain_text() {
        for source in [
            // A range has no single length.
            "Wait ~{5-10%minutes}.",
            // No unit at all.
            "Wait ~{5}.",
            // Shorter than a second.
            "Wait ~{0.5%s}.",
            // Longer than a day.
            "Rest ~{3%days}.",
        ] {
            let r = view(
                &parse(&format!("---\ntitle: T\n---\n\n{source}")),
                None,
                Units::AsWritten,
            );
            assert_eq!(
                only(&r, PieceKind::Timer).timer_seconds,
                None,
                "`{source}` must not get a countdown"
            );
        }
    }

    #[test]
    fn scaling_the_view_changes_what_the_page_shows() {
        let recipe = parse("---\ntitle: T\nservings: 4\n---\n\nMix @flour{500%g}.");

        let doubled = view(&recipe, Some(8), Units::AsWritten);
        assert_eq!(doubled.ingredients[0].quantity.as_deref(), Some("1 kg"));
        assert_eq!(
            only(&doubled, PieceKind::Ingredient).quantity.as_deref(),
            Some("1 kg")
        );
        assert_eq!(doubled.servings.as_deref(), Some("8"));
        assert_eq!(doubled.scale.current, Some(8));
        assert_eq!(doubled.scale.base, Some(4));
        assert!(doubled.scale.changed);
        assert!(doubled.scale.can_scale());

        // The Recipe as stored is untouched.
        let written = view(&recipe, None, Units::AsWritten);
        assert_eq!(written.ingredients[0].quantity.as_deref(), Some("500 g"));
        assert_eq!(written.servings.as_deref(), Some("4"));
        assert!(!written.scale.changed);
    }

    #[test]
    fn converting_the_view_changes_the_units() {
        let recipe = parse("---\ntitle: T\n---\n\nAdd @butter{4%oz}.");

        let metric = view(&recipe, None, Units::Metric);
        assert_eq!(metric.ingredients[0].quantity.as_deref(), Some("113.398 g"));
        assert!(metric.scale.units.is("metric"));
        assert!(metric.scale.changed);
    }

    #[test]
    fn a_recipe_with_no_serving_count_cannot_be_scaled() {
        let recipe = parse("---\ntitle: T\n---\n\nMix @flour{500%g}.");

        let asked = view(&recipe, Some(8), Units::AsWritten);
        assert!(!asked.scale.can_scale());
        assert_eq!(asked.ingredients[0].quantity.as_deref(), Some("500 g"));
        assert_eq!(
            asked.scale.note.as_deref(),
            Some(scale::NO_SERVINGS_MESSAGE)
        );
    }

    #[test]
    fn the_control_starts_at_a_number_it_can_use() {
        let recipe = parse("---\ntitle: T\nservings: 4\n---\n\nMix @flour{500%g}.");

        assert_eq!(view(&recipe, None, Units::AsWritten).scale.value(), 4);
        assert_eq!(view(&recipe, Some(6), Units::AsWritten).scale.value(), 6);

        // With no number anywhere, the control still has a usable value.
        let plain = parse("---\ntitle: T\n---\n\nMix @flour{500%g}.");
        let state = view(&plain, None, Units::AsWritten).scale;
        assert_eq!(state.value(), state.min());
        assert_eq!(state.max(), scale::MAX_SERVINGS);
    }

    #[test]
    fn render_shows_the_recipe_as_written() {
        // The plain entry point is the stored Recipe and nothing else.
        let recipe = parse("---\ntitle: T\nservings: 4\n---\n\nMix @flour{500%g}.");
        let r = render(&recipe);

        assert_eq!(r.ingredients[0].quantity.as_deref(), Some("500 g"));
        assert!(!r.scale.changed);
        assert!(r.scale.units.is("as-written"));
    }
}
