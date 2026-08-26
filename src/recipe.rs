//! Cooklang parsing and the Recipe repository convention.
//!
//! `cooklang-rs` is the parser. The application never writes its own
//! Cooklang reader, and it never reformats a source that a person wrote.
//!
//! An error stops the friendly interface from publishing. A warning does
//! not: the person decides whether it matters.

use std::sync::LazyLock;

use cooklang::convert::UnitsFile;
use cooklang::{Converter, CooklangParser, Extensions};

/// The one canonical Recipe file in a Recipe repository.
pub const RECIPE_FILE: &str = "recipe.cook";

/// The friendly limit for a Cooklang source, in bytes.
pub const MAX_SOURCE_BYTES: usize = 1024 * 1024;

/// The topics that mark a Forgejo repository as a Recipe.
pub const RECIPE_TOPICS: [&str; 2] = ["cooklang", "recipe"];

/// German unit names, added to the bundled English ones.
const GERMAN_UNITS: &str = include_str!("../units/german.toml");

/// Building a parser is expensive, and the first parse is slower still, so
/// the application builds one and reuses it.
///
/// All canonical extensions are on. The converter knows the bundled units
/// plus the German names, because an unknown timer unit is an error and
/// would otherwise stop a German Recipe from being created.
static PARSER: LazyLock<CooklangParser> = LazyLock::new(|| {
    let converter = build_converter();
    CooklangParser::new(Extensions::all(), converter)
});

fn build_converter() -> Converter {
    let german = match toml::from_str::<UnitsFile>(GERMAN_UNITS) {
        Ok(file) => file,
        Err(error) => {
            tracing::error!(%error, "the German units file is not valid; continuing without it");
            return Converter::bundled();
        }
    };

    match Converter::builder()
        .with_units_file(UnitsFile::bundled())
        .and_then(|builder| builder.with_units_file(german))
        .and_then(|builder| builder.finish())
    {
        Ok(converter) => converter,
        Err(error) => {
            tracing::error!(%error, "cannot add the German units; continuing without them");
            Converter::bundled()
        }
    }
}

/// One message from the parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub message: String,
}

/// What the application learned from a Cooklang source.
#[derive(Debug, Clone)]
pub struct Parsed {
    /// The title from the Cooklang metadata, if the source names one.
    pub title: Option<String>,
    /// Messages that stop the friendly interface from publishing.
    pub errors: Vec<Diagnostic>,
    /// Messages that the person sees but that do not stop publishing.
    pub warnings: Vec<Diagnostic>,
}

impl Parsed {
    pub fn is_valid(&self) -> bool {
        self.errors.is_empty()
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RecipeError {
    #[error("the Recipe source is larger than 1 MB")]
    TooLarge,
    #[error("the Recipe needs a title")]
    NoTitle,
}

/// Parse a source and give back the Recipe model itself.
///
/// Returns nothing when the source has an error, because a Recipe that the
/// parser refused cannot be shown as a Recipe.
pub fn parse_recipe(source: &str) -> Option<cooklang::Recipe> {
    let result = PARSER.parse(source);
    if result.report().has_errors() {
        return None;
    }
    result.into_output()
}

/// Read a Cooklang source.
pub fn parse(source: &str) -> Parsed {
    let result = PARSER.parse(source);
    let report = result.report();

    let collect = |diagnostics: &mut dyn Iterator<Item = &cooklang::error::SourceDiag>| {
        diagnostics
            .map(|diagnostic| Diagnostic {
                message: diagnostic.to_string(),
            })
            .collect::<Vec<_>>()
    };

    let errors = collect(&mut report.errors());
    let warnings = collect(&mut report.warnings());

    let title = result
        .output()
        .and_then(|recipe| recipe.metadata.title())
        .map(str::to_string)
        .filter(|title| !title.trim().is_empty());

    Parsed {
        title,
        errors,
        warnings,
    }
}

/// Put a title into the Cooklang metadata of a source.
///
/// The user-facing title lives in the Cooklang source and nowhere else, so
/// the application stores no second copy of it. A source that already names
/// a title keeps its own layout, and only the value changes.
pub fn set_title(source: &str, title: &str) -> String {
    let title = title.trim();

    if let Some(rest) = source.strip_prefix("---") {
        // A YAML frontmatter block. Replace the title line inside it, or add
        // one, and leave every other line as the person wrote it.
        if let Some(end) = rest.find("\n---") {
            let (front, tail) = rest.split_at(end);
            let mut lines: Vec<String> = front.lines().map(str::to_string).collect();

            let existing = lines
                .iter()
                .position(|line| line.trim_start().starts_with("title:"));

            match existing {
                Some(index) => lines[index] = format!("title: {title}"),
                None => lines.insert(
                    // Line 0 is empty, because the source begins with `---`.
                    usize::from(!lines.is_empty()),
                    format!("title: {title}"),
                ),
            }

            return format!("---{}{}", lines.join("\n"), tail);
        }
    }

    // No frontmatter: add one above whatever the person wrote.
    let body = source.trim_start_matches(['\n', '\r']);
    if body.is_empty() {
        format!("---\ntitle: {title}\n---\n")
    } else {
        format!("---\ntitle: {title}\n---\n\n{body}")
    }
}

/// Make a repository slug from a Recipe title.
///
/// Forgejo needs a name that is safe in a URL. The slug is technical and
/// stays fixed, so renaming a Recipe later changes the Cooklang title but
/// not this value.
pub fn slug(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    let mut last_dash = true;

    for character in title.chars() {
        let mapped = match character {
            'a'..='z' | '0'..='9' => Some(character),
            'A'..='Z' => Some(character.to_ascii_lowercase()),
            'ä' | 'Ä' => {
                out.push_str("ae");
                last_dash = false;
                continue;
            }
            'ö' | 'Ö' => {
                out.push_str("oe");
                last_dash = false;
                continue;
            }
            'ü' | 'Ü' => {
                out.push_str("ue");
                last_dash = false;
                continue;
            }
            'ß' => {
                out.push_str("ss");
                last_dash = false;
                continue;
            }
            _ => None,
        };

        match mapped {
            Some(value) => {
                out.push(value);
                last_dash = false;
            }
            None if !last_dash => {
                out.push('-');
                last_dash = true;
            }
            None => {}
        }
    }

    let slug = out.trim_matches('-').to_string();

    // Forgejo limits a repository name to 100 characters.
    let slug: String = slug.chars().take(100).collect();
    let slug = slug.trim_end_matches('-').to_string();

    if slug.is_empty() {
        "recipe".to_string()
    } else {
        slug
    }
}

/// Add a suffix to a slug so that a second Recipe with the same title can
/// exist. `attempt` counts from 2, because the first try uses the plain slug.
pub fn slug_attempt(base: &str, attempt: u32) -> String {
    if attempt <= 1 {
        return base.to_string();
    }
    let suffix = format!("-{attempt}");
    let room = 100usize.saturating_sub(suffix.len());
    let trimmed: String = base.chars().take(room).collect();
    format!("{}{suffix}", trimmed.trim_end_matches('-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_title_only_recipe_is_valid() {
        let parsed = parse("---\ntitle: Toast\n---\n");
        assert!(parsed.is_valid());
        assert_eq!(parsed.title.as_deref(), Some("Toast"));
    }

    #[test]
    fn a_real_recipe_parses_without_an_error() {
        let source = "---\ntitle: Chili\nservings: 4\n---\n\n\
             Chop the @onion{1} in a #pan{} for ~{5%minutes}.";
        let parsed = parse(source);

        assert!(parsed.is_valid(), "errors: {:?}", parsed.errors);
        assert_eq!(parsed.title.as_deref(), Some("Chili"));
    }

    #[test]
    fn the_german_units_are_embedded_in_the_binary() {
        // include_str! puts this file in the binary, so the container build
        // has to copy the folder that holds it. A missing COPY line breaks
        // the image build while `cargo test` still passes, so assert that
        // the content really arrived.
        assert!(
            GERMAN_UNITS.contains("[extend.units]"),
            "the German units file is empty or missing"
        );
        assert!(GERMAN_UNITS.contains("Min."));
    }

    #[test]
    fn german_timer_units_do_not_stop_a_recipe() {
        // A real German collection writes `Min.`, not `minutes`. The bundled
        // units are English, and an unknown timer unit is an error, so
        // without the German units file this Recipe could not be created.
        for unit in ["Min.", "Minuten", "Min", "Std.", "Stunden", "Sek."] {
            let source = format!(
                "---
title: Test
---

Wait ~{{5%{unit}}}."
            );
            let parsed = parse(&source);
            assert!(
                parsed.is_valid(),
                "`{unit}` must be a known time unit, got {:?}",
                parsed.errors
            );
        }
    }

    #[test]
    fn german_measures_are_known_units() {
        let source = "---
title: Test
---

@Öl{2%EL} and @Salz{1%TL} and @Mehl{500%g}.";
        let parsed = parse(source);
        assert!(parsed.is_valid(), "errors: {:?}", parsed.errors);
    }

    #[test]
    fn an_unknown_timer_unit_is_still_an_error() {
        // The German units widen what is known. They must not turn the check
        // off, or a real mistake would pass unnoticed.
        let parsed = parse(
            "---
title: Test
---

Wait ~{5%bananas}.",
        );
        assert!(!parsed.is_valid(), "an unknown unit must still be an error");
    }

    #[test]
    fn a_source_without_a_title_reports_none() {
        let parsed = parse("Chop the @onion{1}.");
        assert_eq!(parsed.title, None);
    }

    #[test]
    fn set_title_adds_frontmatter_when_there_is_none() {
        let out = set_title("Chop the @onion{1}.", "Onion Soup");

        assert!(out.starts_with("---\ntitle: Onion Soup\n---\n"));
        assert!(out.contains("Chop the @onion{1}."));
        assert_eq!(parse(&out).title.as_deref(), Some("Onion Soup"));
    }

    #[test]
    fn set_title_replaces_an_existing_title_and_keeps_the_rest() {
        let source = "---\ntitle: Old\nservings: 4\ntags: [vegan]\n---\n\nStep one.";
        let out = set_title(source, "New");

        assert_eq!(parse(&out).title.as_deref(), Some("New"));
        assert!(out.contains("servings: 4"), "other metadata must survive");
        assert!(out.contains("tags: [vegan]"));
        assert!(out.contains("Step one."));
        assert!(!out.contains("title: Old"));
    }

    #[test]
    fn set_title_adds_a_title_to_frontmatter_that_has_none() {
        let source = "---\nservings: 4\n---\n\nStep one.";
        let out = set_title(source, "Stew");

        assert_eq!(parse(&out).title.as_deref(), Some("Stew"));
        assert!(out.contains("servings: 4"));
    }

    #[test]
    fn set_title_on_an_empty_source_gives_a_title_only_recipe() {
        let out = set_title("", "Toast");
        assert_eq!(parse(&out).title.as_deref(), Some("Toast"));
        assert!(parse(&out).is_valid());
    }

    #[test]
    fn a_slug_comes_from_the_title() {
        assert_eq!(slug("Chili Sin Carne"), "chili-sin-carne");
        assert_eq!(slug("Green Goddess Salad"), "green-goddess-salad");
    }

    #[test]
    fn a_slug_transliterates_german_letters() {
        // The corpus that this was tested against is German, so these are
        // ordinary titles rather than an edge case.
        assert_eq!(slug("Pfannekuchen für Gäste"), "pfannekuchen-fuer-gaeste");
        assert_eq!(slug("Grieß mit Öl"), "griess-mit-oel");
    }

    #[test]
    fn a_slug_never_ends_up_empty_or_ragged() {
        assert_eq!(slug("!!!"), "recipe");
        assert_eq!(slug("  --Spaced--  "), "spaced");
        assert_eq!(slug(""), "recipe");
    }

    #[test]
    fn a_slug_stays_within_the_forgejo_limit() {
        let long = "a".repeat(300);
        assert_eq!(slug(&long).len(), 100);
        assert_eq!(slug_attempt(&slug(&long), 2).len(), 100);
    }

    #[test]
    fn a_second_attempt_adds_a_suffix() {
        assert_eq!(slug_attempt("chili", 1), "chili");
        assert_eq!(slug_attempt("chili", 2), "chili-2");
        assert_eq!(slug_attempt("chili", 10), "chili-10");
    }
}
