//! Compare parser configurations against a folder of real Cooklang files.
//! `cargo run --example corpus_check -- <folder>`
use cooklang::{Converter, CooklangParser, Extensions};

fn main() {
    let dir = std::env::args().nth(1).expect("give a folder");
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .expect("cannot read the folder")
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "cook"))
        .collect();
    entries.sort_by_key(|e| e.path());

    let sources: Vec<(String, String)> = entries
        .iter()
        .map(|e| {
            (
                e.file_name().to_string_lossy().to_string(),
                std::fs::read_to_string(e.path()).unwrap_or_default(),
            )
        })
        .collect();

    let configs: Vec<(&str, CooklangParser)> = vec![
        (
            "all() (extended)",
            CooklangParser::new(Extensions::all(), Converter::bundled()),
        ),
        (
            "COMPAT",
            CooklangParser::new(Extensions::COMPAT, Converter::bundled()),
        ),
        (
            "COMPAT - ADVANCED_UNITS",
            CooklangParser::new(
                Extensions::COMPAT & !Extensions::ADVANCED_UNITS,
                Converter::bundled(),
            ),
        ),
        (
            "all() - ADVANCED_UNITS",
            CooklangParser::new(
                Extensions::all() & !Extensions::ADVANCED_UNITS,
                Converter::bundled(),
            ),
        ),
    ];

    println!("{:<26} {:>8} {:>10} {:>10}", "config", "errors", "warnings", "no title");
    println!("{}", "-".repeat(58));

    // The application's own parser, with the German units loaded.
    {
        let (mut errs, mut warns, mut notitle) = (0, 0, 0);
        let mut kinds: std::collections::BTreeMap<String, usize> = Default::default();
        for (_f, source) in &sources {
            let p = cooklanghub::recipe::parse(source);
            if !p.errors.is_empty() {
                errs += 1;
                for d in &p.errors {
                    let first = d.message.lines().next().unwrap_or("").trim().to_string();
                    let key = first.split(':').next().unwrap_or(&first).to_string();
                    *kinds.entry(key).or_default() += 1;
                }
            }
            if !p.warnings.is_empty() { warns += 1; }
            if p.title.is_none() { notitle += 1; }
        }
        println!("{:<26} {errs:>8} {warns:>10} {notitle:>10}", "CookLangHub (de units)");
        for (k, v) in kinds.iter().take(4) {
            println!("      {v:>3}x {k}");
        }
    }

    for (name, parser) in &configs {
        let (mut errs, mut warns, mut notitle) = (0, 0, 0);
        let mut kinds: std::collections::BTreeMap<String, usize> = Default::default();
        for (_file, source) in &sources {
            let r = parser.parse(source);
            let rep = r.report();
            if rep.has_errors() {
                errs += 1;
                for d in rep.errors() {
                    let m = d.to_string();
                    let first = m.lines().next().unwrap_or("").trim().to_string();
                    let key = first.split(':').next().unwrap_or(&first).to_string();
                    *kinds.entry(key).or_default() += 1;
                }
            }
            if rep.has_warnings() {
                warns += 1;
            }
            if r.output().and_then(|x| x.metadata.title()).is_none() {
                notitle += 1;
            }
        }
        println!("{name:<26} {errs:>8} {warns:>10} {notitle:>10}");
        for (k, v) in kinds.iter().take(4) {
            println!("      {v:>3}x {k}");
        }
    }
}
