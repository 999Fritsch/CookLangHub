//! The view options of a Recipe: how many servings, and which units.
//!
//! Everything in this module changes the view and nothing else. The
//! application never writes a scaled Recipe back, so the stored Cooklang
//! source stays byte-identical and no Version appears.
//!
//! `cooklang-rs` does the arithmetic. It knows which quantity can scale and
//! which unit can convert, so a quantity that it cannot handle keeps the
//! words that the author wrote. The application never guesses an amount.
//!
//! The options travel in the address, for example
//! `?servings=4&units=metric`. This keeps one implementation, keeps the page
//! usable when scripts are blocked, and makes a scaled view shareable.

use cooklang::Recipe;
use cooklang::convert::{Converter, System};

/// The smallest serving count the page accepts.
pub const MIN_SERVINGS: u32 = 1;

/// The largest serving count the page accepts.
///
/// A larger number is almost always a mistake or an attack, and the page
/// falls back to the serving count of the Recipe.
pub const MAX_SERVINGS: u32 = 1000;

/// Shown when the address asks for a serving count that the Recipe cannot
/// give.
pub const NO_SERVINGS_MESSAGE: &str =
    "This Recipe does not give a serving count. The application cannot scale it.";

/// Which units the cook wants to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Units {
    /// The units of the author, untouched. This is what a visitor gets.
    #[default]
    AsWritten,
    Metric,
    Imperial,
}

impl Units {
    pub fn as_str(&self) -> &'static str {
        match self {
            Units::AsWritten => "as-written",
            Units::Metric => "metric",
            Units::Imperial => "imperial",
        }
    }

    /// What a person reads on the control.
    pub fn label(&self) -> &'static str {
        match self {
            Units::AsWritten => "As written",
            Units::Metric => "Metric",
            Units::Imperial => "Imperial",
        }
    }

    /// The choices, in the order the control shows them.
    pub fn all() -> [Units; 3] {
        [Units::AsWritten, Units::Metric, Units::Imperial]
    }

    fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "as-written" => Some(Units::AsWritten),
            "metric" => Some(Units::Metric),
            "imperial" => Some(Units::Imperial),
            _ => None,
        }
    }

    /// The unit system to convert to, or nothing when the author decides.
    fn system(&self) -> Option<System> {
        match self {
            Units::AsWritten => None,
            Units::Metric => Some(System::Metric),
            Units::Imperial => Some(System::Imperial),
        }
    }

    /// Whether this is the given choice. The template cannot compare values
    /// of an enum, so it asks with a name.
    pub fn is(&self, name: &str) -> bool {
        self.as_str() == name
    }
}

/// What the address asks the page to show.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct View {
    /// The wanted serving count, when the address gives a usable one.
    pub servings: Option<u32>,
    pub units: Units,
}

impl View {
    /// Read the view options from the values in the address.
    ///
    /// A serving count that is absent, empty, not a number, zero, negative,
    /// fractional, or larger than [`MAX_SERVINGS`] gives no target. The page
    /// then shows the Recipe as its author wrote it. A unit name that is not
    /// known does the same. Nothing here can fail, because a strange address
    /// must give a page and not an error.
    pub fn from_query(servings: Option<&str>, units: Option<&str>) -> Self {
        Self {
            servings: servings.and_then(parse_servings),
            units: units.and_then(Units::parse).unwrap_or_default(),
        }
    }

    /// True when the address asks for the Recipe exactly as it is stored.
    pub fn is_as_written(&self) -> bool {
        self.servings.is_none() && self.units == Units::AsWritten
    }
}

/// Read a serving count from the address.
fn parse_servings(value: &str) -> Option<u32> {
    // `u32` refuses a negative number, a fraction, and a value that is too
    // large to hold. The range check refuses zero and a count that no
    // kitchen can cook.
    let count: u32 = value.trim().parse().ok()?;
    (MIN_SERVINGS..=MAX_SERVINGS)
        .contains(&count)
        .then_some(count)
}

/// A Recipe as the view options ask for it, and what happened on the way.
#[derive(Debug, Clone)]
pub struct Scaled {
    /// A copy of the Recipe. Nothing here goes back to Git.
    pub recipe: Recipe,
    /// The serving count that the Recipe itself gives, when it gives one
    /// that the page can scale.
    pub base_servings: Option<u32>,
    /// The serving count on screen.
    pub servings: Option<u32>,
    /// The units on screen.
    pub units: Units,
    /// True when the page shows something other than the Recipe as written.
    pub changed: bool,
    /// Set when the page cannot do what the address asks for.
    pub note: Option<String>,
}

/// Apply the view options to a copy of the Recipe.
///
/// The input is never touched. The result is a copy that only this one page
/// view uses.
pub fn apply(recipe: &Recipe, view: &View, converter: &Converter) -> Scaled {
    let mut copy = recipe.clone();

    // A Recipe with `servings: 4-6`, `servings: a lot`, or `servings: 0`
    // gives no number to scale from. Zero would divide by zero, so it is
    // refused here and not in the parser.
    let base_servings = recipe
        .metadata
        .servings()
        .and_then(|servings| servings.as_number())
        .filter(|count| *count > 0);

    let mut servings = base_servings;
    let mut note = None;
    let mut changed = false;

    if let Some(target) = view.servings {
        match base_servings {
            Some(base) => {
                // The parser scales only what it can: a quantity with a
                // number and without a scaling lock. A quantity such as
                // `@salt{some}` keeps the word `some`.
                if copy.scale_to_servings(target, converter).is_ok() {
                    servings = Some(target);
                    changed = target != base;
                } else {
                    note = Some(NO_SERVINGS_MESSAGE.to_string());
                }
            }
            None => note = Some(NO_SERVINGS_MESSAGE.to_string()),
        }
    }

    if let Some(system) = view.units.system() {
        // Each quantity that the converter cannot handle stays as the author
        // wrote it. The parser reports these, and they are normal: an amount
        // without a unit, or a unit that no system knows, has nothing to
        // convert to.
        let _ = copy.convert(system, converter);
        changed = true;
    }

    Scaled {
        recipe: copy,
        base_servings,
        servings,
        units: view.units,
        changed,
        note,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recipe;

    fn parse(source: &str) -> Recipe {
        recipe::parse_recipe(source).expect("the fixture must parse")
    }

    /// The amount of one ingredient, as the page would print it.
    fn amount(recipe: &Recipe, name: &str) -> String {
        recipe
            .ingredients
            .iter()
            .find(|i| i.name == name)
            .unwrap_or_else(|| panic!("no ingredient named {name}"))
            .quantity
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default()
    }

    fn scaled(source: &str, view: View) -> Scaled {
        apply(&parse(source), &view, recipe::converter())
    }

    const CHILI: &str = "---\ntitle: Chili\nservings: 4\n---\n\n\
         Mix @flour{500%g} and @water{300%ml} with @salt{some}.";

    #[test]
    fn a_larger_serving_count_makes_larger_amounts() {
        let out = scaled(
            CHILI,
            View {
                servings: Some(8),
                units: Units::AsWritten,
            },
        );

        assert_eq!(amount(&out.recipe, "flour"), "1 kg");
        assert_eq!(amount(&out.recipe, "water"), "600 ml");
        assert_eq!(out.servings, Some(8));
        assert!(out.changed);
        assert_eq!(out.note, None);
    }

    #[test]
    fn a_smaller_serving_count_makes_smaller_amounts() {
        let out = scaled(
            CHILI,
            View {
                servings: Some(2),
                units: Units::AsWritten,
            },
        );

        assert_eq!(amount(&out.recipe, "flour"), "250 g");
        assert_eq!(amount(&out.recipe, "water"), "150 ml");
        assert_eq!(out.servings, Some(2));
    }

    #[test]
    fn a_result_that_is_not_whole_keeps_its_parts() {
        // 4 servings to 3 is three quarters of everything.
        let out = scaled(
            "---\ntitle: T\nservings: 4\n---\n\nUse @flour{100%g} and @egg{1}.",
            View {
                servings: Some(3),
                units: Units::AsWritten,
            },
        );

        assert_eq!(amount(&out.recipe, "flour"), "75 g");
        // The parser keeps the part of an egg rather than rounding it away.
        assert_eq!(amount(&out.recipe, "egg"), "0.75");
    }

    #[test]
    fn a_quantity_with_no_number_stays_as_written() {
        let out = scaled(
            CHILI,
            View {
                servings: Some(8),
                units: Units::AsWritten,
            },
        );
        assert_eq!(amount(&out.recipe, "salt"), "some");
    }

    #[test]
    fn a_recipe_without_a_serving_count_is_not_scaled() {
        let out = scaled(
            "---\ntitle: T\n---\n\nUse @flour{500%g}.",
            View {
                servings: Some(8),
                units: Units::AsWritten,
            },
        );

        assert_eq!(amount(&out.recipe, "flour"), "500 g");
        assert_eq!(out.base_servings, None);
        assert_eq!(out.note.as_deref(), Some(NO_SERVINGS_MESSAGE));
    }

    #[test]
    fn a_serving_count_that_is_words_is_not_scaled() {
        // `servings: 4-6` is a range that the parser reads as text.
        let out = scaled(
            "---\ntitle: T\nservings: 4-6\n---\n\nUse @flour{500%g}.",
            View {
                servings: Some(8),
                units: Units::AsWritten,
            },
        );

        assert_eq!(amount(&out.recipe, "flour"), "500 g");
        assert_eq!(out.base_servings, None);
        assert_eq!(out.note.as_deref(), Some(NO_SERVINGS_MESSAGE));
    }

    #[test]
    fn a_serving_count_of_zero_in_the_recipe_never_divides_by_zero() {
        let out = scaled(
            "---\ntitle: T\nservings: 0\n---\n\nUse @flour{500%g}.",
            View {
                servings: Some(8),
                units: Units::AsWritten,
            },
        );

        assert_eq!(amount(&out.recipe, "flour"), "500 g");
        assert_eq!(out.base_servings, None);
    }

    #[test]
    fn the_same_serving_count_changes_nothing() {
        let out = scaled(
            CHILI,
            View {
                servings: Some(4),
                units: Units::AsWritten,
            },
        );

        assert_eq!(amount(&out.recipe, "flour"), "500 g");
        assert!(!out.changed, "the view is the Recipe as written");
    }

    #[test]
    fn metric_units_convert_an_imperial_amount() {
        let out = scaled(
            "---\ntitle: T\n---\n\nAdd @butter{4%oz} and @milk{1%cup}.",
            View {
                servings: None,
                units: Units::Metric,
            },
        );

        assert_eq!(amount(&out.recipe, "butter"), "113.398 g");
        assert_eq!(amount(&out.recipe, "milk"), "236.588 ml");
        assert!(out.changed);
    }

    #[test]
    fn imperial_units_convert_a_metric_amount() {
        let out = scaled(
            "---\ntitle: T\n---\n\nAdd @flour{500%g}.",
            View {
                servings: None,
                units: Units::Imperial,
            },
        );

        assert_eq!(amount(&out.recipe, "flour"), "18 oz");
    }

    #[test]
    fn a_german_amount_still_converts() {
        // The converter carries the German unit names, so a German Recipe
        // is not left behind.
        let out = scaled(
            "---\ntitle: T\n---\n\nGib @Mehl{500%Gramm} und @Öl{2%EL} dazu.",
            View {
                servings: None,
                units: Units::Metric,
            },
        );

        assert_eq!(amount(&out.recipe, "Mehl"), "500 g");
        assert_eq!(amount(&out.recipe, "Öl"), "29.574 ml");
    }

    #[test]
    fn a_unit_with_no_conversion_stays_as_written() {
        // A count of eggs has no unit, so no system can hold it.
        let out = scaled(
            "---\ntitle: T\n---\n\nAdd @egg{2} and @yeast{1%pinch}.",
            View {
                servings: None,
                units: Units::Metric,
            },
        );

        assert_eq!(amount(&out.recipe, "egg"), "2");
        assert_eq!(amount(&out.recipe, "yeast"), "1 pinch");
    }

    #[test]
    fn scaling_and_converting_work_together() {
        let out = scaled(
            "---\ntitle: T\nservings: 2\n---\n\nAdd @butter{2%oz}.",
            View {
                servings: Some(4),
                units: Units::Metric,
            },
        );

        assert_eq!(amount(&out.recipe, "butter"), "113.398 g");
    }

    #[test]
    fn nothing_in_the_stored_recipe_changes() {
        let original = parse(CHILI);
        let before = original.clone();

        let _ = apply(
            &original,
            &View {
                servings: Some(64),
                units: Units::Imperial,
            },
            recipe::converter(),
        );

        assert_eq!(original, before, "the parsed Recipe must not be touched");
    }

    #[test]
    fn a_serving_count_that_is_not_a_number_is_refused() {
        for value in [
            "abc",
            "",
            "   ",
            "4.5",
            "-2",
            "1e9",
            "0x10",
            "٤",
            "NaN",
            "Infinity",
            "4; DROP TABLE",
            "4,8",
        ] {
            assert_eq!(
                View::from_query(Some(value), None).servings,
                None,
                "`{value}` must give no serving count"
            );
        }
    }

    #[test]
    fn a_number_with_room_around_it_still_counts() {
        // A person can paste a value with a space, and a form can send a
        // plus sign. Both are a plain number.
        assert_eq!(View::from_query(Some(" 4 "), None).servings, Some(4));
        assert_eq!(View::from_query(Some("+4"), None).servings, Some(4));
    }

    #[test]
    fn a_serving_count_outside_the_range_is_refused() {
        assert_eq!(View::from_query(Some("0"), None).servings, None);
        assert_eq!(View::from_query(Some("1001"), None).servings, None);
        assert_eq!(View::from_query(Some("4294967296"), None).servings, None);
        assert_eq!(
            View::from_query(Some("99999999999999999999"), None).servings,
            None
        );
        assert_eq!(View::from_query(Some("1"), None).servings, Some(1));
        assert_eq!(View::from_query(Some("1000"), None).servings, Some(1000));
    }

    #[test]
    fn an_unknown_unit_name_falls_back_to_the_author() {
        for value in ["", "kelvin", "METRIC", "../../etc", "<script>"] {
            assert_eq!(
                View::from_query(None, Some(value)).units,
                Units::AsWritten,
                "`{value}` must give the units of the author"
            );
        }

        assert_eq!(View::from_query(None, Some("metric")).units, Units::Metric);
        assert_eq!(
            View::from_query(None, Some("imperial")).units,
            Units::Imperial
        );
        assert_eq!(
            View::from_query(None, Some("as-written")).units,
            Units::AsWritten
        );
    }

    #[test]
    fn a_bad_address_gives_the_recipe_as_written() {
        let view = View::from_query(Some("-1"), Some("klingon"));
        assert!(view.is_as_written());

        let out = scaled(CHILI, view);
        assert_eq!(amount(&out.recipe, "flour"), "500 g");
        assert!(!out.changed);
        assert_eq!(out.note, None, "a bad address is not worth a message");
    }

    #[test]
    fn every_choice_has_a_name_and_a_label() {
        for units in Units::all() {
            assert!(!units.as_str().is_empty());
            assert!(!units.label().is_empty());
            assert!(units.is(units.as_str()));
        }
        assert!(!Units::Metric.is("imperial"));
    }
}
