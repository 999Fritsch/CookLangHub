//! The data behind Cook mode.
//!
//! Cook mode is the CookCLI screen for the kitchen: one card at a time, big
//! enough to read from a step away, with the things to gather before each
//! part and a step you move through by swiping, scrolling, or pressing a
//! key. The script and the stylesheet come from CookCLI. See NOTICE.
//!
//! CookCLI hands its script the Recipe as JSON in the page and lets the
//! script read the already rendered steps out of the page itself. This
//! module builds that same JSON from the same view model the page uses, so
//! Cook mode and the Recipe page can never disagree about a Recipe.
//!
//! Nothing here is written anywhere. Cook mode reads.

use serde::Serialize;

use crate::render::{Component, PieceKind, RenderedRecipe};

/// One thing to gather, in the shape the CookCLI script reads.
#[derive(Debug, Clone, Serialize)]
pub struct CookingIngredient {
    pub name: String,
    /// The amount as the Recipe writes it.
    ///
    /// CookCLI keeps the number and the unit apart and joins them again in
    /// the script. This project already holds them joined, so the unit stays
    /// empty and the join in the script gives the same words.
    pub quantity: String,
    pub unit: String,
    pub note: String,
}

impl From<&Component> for CookingIngredient {
    fn from(item: &Component) -> Self {
        Self {
            name: item.name.clone(),
            quantity: item.quantity.clone().unwrap_or_default(),
            unit: String::new(),
            note: item.note.clone().unwrap_or_default(),
        }
    }
}

/// One step, without its words.
///
/// The words are not here on purpose. The script takes the rendered step out
/// of the page, so a badge keeps its colour and an amount keeps its place
/// inside the badge.
#[derive(Debug, Clone, Serialize)]
pub struct CookingStep {
    pub number: u32,
    /// CookCLI shows a picture for a step. This project has none yet, and
    /// the script leaves the card without one when this is absent.
    pub image: Option<String>,
    pub ingredients: Vec<CookingIngredient>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CookingSection {
    pub name: Option<String>,
    /// What to gather for this part of the Recipe.
    pub ingredients: Vec<CookingIngredient>,
    pub steps: Vec<CookingStep>,
}

/// A Recipe as Cook mode needs it.
#[derive(Debug, Clone, Serialize, Default)]
pub struct CookingRecipe {
    pub name: String,
    pub sections: Vec<CookingSection>,
}

/// Build the Cook mode data from the Recipe the page is showing.
///
/// The serving count and the units already sit in the view model, so a
/// Recipe scaled to eight goes into Cook mode scaled to eight.
pub fn data(title: &str, cooked: &RenderedRecipe) -> CookingRecipe {
    let sections = cooked
        .sections
        .iter()
        .map(|section| {
            let steps: Vec<CookingStep> = section
                .blocks
                .iter()
                .filter(|block| block.is_step())
                .map(|block| CookingStep {
                    number: block.number,
                    image: None,
                    ingredients: block
                        .pieces
                        .iter()
                        .filter(|piece| piece.kind == PieceKind::Ingredient)
                        .map(|piece| CookingIngredient {
                            name: piece.text.clone(),
                            quantity: piece.quantity.clone().unwrap_or_default(),
                            unit: String::new(),
                            note: String::new(),
                        })
                        .collect(),
                })
                .collect();

            CookingSection {
                name: section.name.clone(),
                // A Recipe with one part gathers everything on the first
                // card. A Recipe with several parts has no per-part list in
                // the view model, so the whole list goes on the first card
                // and the later cards carry their steps only.
                ingredients: Vec::new(),
                steps,
            }
        })
        .collect::<Vec<_>>();

    let mut sections = sections;
    if let Some(first) = sections.first_mut() {
        first.ingredients = cooked
            .ingredients
            .iter()
            .map(CookingIngredient::from)
            .collect();
    }

    CookingRecipe {
        name: title.to_string(),
        sections,
    }
}

/// The data as the JSON that the page carries.
///
/// The value goes inside a `<script type="application/json">`, which a
/// browser does not run, so this is data and not code. `</script>` inside a
/// string would still close that element early, so the slash is escaped the
/// way JSON allows.
pub fn json(title: &str, cooked: &RenderedRecipe) -> String {
    serde_json::to_string(&data(title, cooked))
        .unwrap_or_else(|_| "{\"name\":\"\",\"sections\":[]}".to_string())
        .replace("</", "<\\/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recipe;
    use crate::render;

    fn cook(source: &str) -> RenderedRecipe {
        render::render(&recipe::parse_recipe(source).expect("the Recipe must parse"))
    }

    #[test]
    fn the_first_card_gathers_what_the_recipe_needs() {
        let data = data("Chili", &cook("Cook @beans{400%g} and @rice{1%cup}."));

        assert_eq!(data.name, "Chili");
        let gather = &data.sections[0].ingredients;
        assert_eq!(gather.len(), 2);
        assert_eq!(gather[0].name, "beans");
        assert_eq!(gather[0].quantity, "400 g");
    }

    #[test]
    fn a_step_names_the_things_it_uses() {
        let data = data(
            "Chili",
            &cook("Chop @onion{1}.\n\nAdd @beans{400%g} and @salt{}."),
        );

        let steps = &data.sections[0].steps;
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].number, 1);
        assert_eq!(steps[0].ingredients[0].name, "onion");
        assert_eq!(steps[1].ingredients.len(), 2);
        assert_eq!(steps[1].ingredients[1].name, "salt");
        // A thing with no amount still belongs on the card.
        assert_eq!(steps[1].ingredients[1].quantity, "");
    }

    #[test]
    fn a_paragraph_that_is_not_a_step_gets_no_card() {
        let data = data(
            "Chili",
            &cook("> A note about the Recipe.\n\nChop @onion{1}."),
        );

        let steps = &data.sections[0].steps;
        assert_eq!(steps.len(), 1, "only the step becomes a card");
        assert_eq!(steps[0].ingredients[0].name, "onion");
    }

    #[test]
    fn cook_mode_follows_the_serving_count_on_the_page() {
        let parsed = recipe::parse_recipe("---\nservings: 2\n---\nCook @rice{200%g}.")
            .expect("the Recipe must parse");
        let view = crate::scale::View::from_query(Some("4"), None);
        let scaled = render::render_with(&parsed, &view, recipe::converter());

        let data = data("Rice", &scaled);
        assert_eq!(
            data.sections[0].ingredients[0].quantity, "400 g",
            "Cook mode shows the amounts the page shows"
        );
    }

    #[test]
    fn a_script_end_cannot_escape_the_data_block() {
        let data = json(
            "</script><img src=x onerror=alert(1)>",
            &cook("Chop @onion{1}."),
        );

        assert!(
            !data.contains("</script"),
            "the data block must not be closeable from inside: {data}"
        );
        assert!(data.contains("<\\/script"));
    }
}
