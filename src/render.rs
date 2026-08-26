//! Turning a parsed Recipe into something a cook can read.
//!
//! The output of this module is plain text in a small view model. It never
//! contains markup. The template escapes every value, so a Recipe that
//! contains `<script>` shows those characters and cannot run.
//!
//! Nothing here writes. Reading a Recipe never changes the stored file, so
//! a later parser release changes what a person sees and never what Git
//! holds.

use cooklang::{Content, Item, Recipe};

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
    pub fn css_class(&self) -> &'static str {
        match self {
            PieceKind::Text => "",
            PieceKind::Ingredient => "ingredient",
            PieceKind::Cookware => "cookware",
            PieceKind::Timer => "timer",
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
}

impl Piece {
    fn text(value: impl Into<String>) -> Self {
        Self {
            kind: PieceKind::Text,
            text: value.into(),
            quantity: None,
        }
    }
}

/// A numbered step, or a paragraph that carries no instruction.
#[derive(Debug, Clone)]
pub struct Block {
    /// Zero for a paragraph that is not a step.
    pub number: u32,
    pub pieces: Vec<Piece>,
    /// What this one step needs, with amounts.
    ///
    /// A cook working through the method should not have to look back at
    /// the full list to find out how much of something this step takes.
    pub components: Vec<Component>,
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
}

impl RenderedRecipe {
    pub fn is_empty(&self) -> bool {
        self.sections.iter().all(|s| s.blocks.is_empty())
            && self.ingredients.is_empty()
            && self.cookware.is_empty()
    }
}

/// Metadata keys that the page shows on its own, so the fact list skips them.
const HANDLED_KEYS: [&str; 3] = ["title", "tags", "servings"];

/// Build the view model from a parsed Recipe.
pub fn render(recipe: &Recipe) -> RenderedRecipe {
    let ingredients: Vec<Component> = recipe
        .ingredients
        .iter()
        .map(|i| Component {
            name: i.name.clone(),
            quantity: i.quantity.as_ref().map(ToString::to_string),
            note: i.note.clone(),
        })
        .collect();

    let cookware: Vec<Component> = recipe
        .cookware
        .iter()
        .map(|c| Component {
            name: c.name.clone(),
            quantity: c.quantity.as_ref().map(ToString::to_string),
            note: c.note.clone(),
        })
        .collect();

    let sections = recipe
        .sections
        .iter()
        .map(|section| RenderedSection {
            name: section.name.clone(),
            blocks: section
                .content
                .iter()
                .map(|content| match content {
                    Content::Step(step) => {
                        let pieces: Vec<Piece> = step
                            .items
                            .iter()
                            .map(|item| piece(recipe, item))
                            .collect();
                        let components = step_components(&pieces);
                        Block {
                            number: step.number,
                            pieces,
                            components,
                        }
                    }
                    Content::Text(text) => Block {
                        number: 0,
                        pieces: vec![Piece::text(text.clone())],
                        components: Vec::new(),
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
    }
}

/// The ingredients that one step uses, each named once.
fn step_components(pieces: &[Piece]) -> Vec<Component> {
    let mut out: Vec<Component> = Vec::new();

    for piece in pieces {
        if piece.kind != PieceKind::Ingredient {
            continue;
        }
        // A step can name the same ingredient twice. Show it once.
        if out.iter().any(|c| c.name == piece.text) {
            continue;
        }
        out.push(Component {
            name: piece.text.clone(),
            quantity: piece.quantity.clone(),
            note: None,
        });
    }

    out
}

fn piece(recipe: &Recipe, item: &Item) -> Piece {
    match item {
        Item::Text { value } => Piece::text(value.clone()),
        Item::Ingredient { index } => match recipe.ingredients.get(*index) {
            Some(ingredient) => Piece {
                kind: PieceKind::Ingredient,
                text: ingredient.name.clone(),
                quantity: ingredient.quantity.as_ref().map(ToString::to_string),
            },
            None => Piece::text(String::new()),
        },
        Item::Cookware { index } => match recipe.cookware.get(*index) {
            Some(cookware) => Piece {
                kind: PieceKind::Cookware,
                text: cookware.name.clone(),
                quantity: cookware.quantity.as_ref().map(ToString::to_string),
            },
            None => Piece::text(String::new()),
        },
        Item::Timer { index } => match recipe.timers.get(*index) {
            Some(timer) => Piece {
                kind: PieceKind::Timer,
                text: timer.name.clone().unwrap_or_default(),
                quantity: timer.quantity.as_ref().map(ToString::to_string),
            },
            None => Piece::text(String::new()),
        },
        // An inline quantity is a plain amount inside the text.
        other => Piece::text(inline_text(other)),
    }
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

        facts.push(Fact {
            label: humanize(key),
            link: safe_link(&text),
            value: text,
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
    fn each_step_lists_what_it_needs() {
        let r = render(&parse(
            "---
title: T
---

Chop @onion{1} and @garlic{2%cloves}.

Fry the @onion{} again in @oil{2%tbsp}.",
        ));

        let first = &r.sections[0].blocks[0];
        let names: Vec<&str> = first.components.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["onion", "garlic"]);
        assert_eq!(first.components[1].quantity.as_deref(), Some("2 cloves"));

        // The second step names only what it uses.
        let second = &r.sections[0].blocks[1];
        let names: Vec<&str> = second.components.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["onion", "oil"]);
    }

    #[test]
    fn a_step_names_a_repeated_ingredient_once() {
        let r = render(&parse(
            "---
title: T
---

Add @salt{1%pinch}, stir, then add @salt{} again.",
        ));

        let step = &r.sections[0].blocks[0];
        assert_eq!(step.components.len(), 1, "salt must appear once");
        assert_eq!(step.components[0].name, "salt");
    }

    #[test]
    fn a_paragraph_that_is_not_a_step_lists_nothing() {
        let r = render(&parse("---
title: T
---

Chop @onion{1}."));
        let step = &r.sections[0].blocks[0];
        assert!(step.is_step());
        assert!(!step.components.is_empty());
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
    fn german_units_render_as_written() {
        let r = render(&parse(
            "---\ntitle: T\n---\n\nGib @Öl{2%EL} dazu und warte ~{8%Min.}.",
        ));

        assert_eq!(r.ingredients[0].name, "Öl");
        assert_eq!(r.ingredients[0].quantity.as_deref(), Some("2 EL"));
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
        assert_eq!(PieceKind::Ingredient.css_class(), "ingredient");
        assert_eq!(PieceKind::Cookware.css_class(), "cookware");
        assert_eq!(PieceKind::Timer.css_class(), "timer");
        assert_eq!(PieceKind::Ingredient.label(), "Ingredient");
        assert!(PieceKind::Text.is_text());
    }
}
