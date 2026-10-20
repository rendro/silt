//! The reference pages under `docs/stdlib/` agree with the builtin
//! registry: each page is rendered again from the rows
//! (`silt::builtins::registry::docs::render_page`: the summary tables'
//! signature and description cells, the signature block of each
//! function's section, the cells of the tables of type arguments) and
//! compared with the file.
//!
//! After a change of a row, `SILT_BLESS=1 cargo nextest run --test meta
//! stdlib_reference` rewrites the pages.

use std::collections::HashSet;
use std::path::PathBuf;

use silt::builtins::registry::decoding::{Family, Leaf};
use silt::builtins::registry::docs::{pages, render_page, signature_block};
use silt::builtins::registry::registry;

#[test]
fn the_reference_pages_are_what_the_registry_renders() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/stdlib");
    let bless = std::env::var_os("SILT_BLESS").is_some();
    let mut stale = Vec::new();
    for (file, page) in pages() {
        let rendered = render_page(page)
            .unwrap_or_else(|problems| panic!("docs/stdlib/{file}:\n  {}", problems.join("\n  ")));
        if rendered != page {
            if bless {
                std::fs::write(dir.join(&file), rendered).expect("write the page");
            }
            stale.push(file);
        }
    }
    assert!(
        stale.is_empty() || bless,
        "these pages of docs/stdlib/ differ from what the builtin registry renders: \
         {stale:?}. Run `SILT_BLESS=1 cargo nextest run --test meta stdlib_reference` \
         and review the diff."
    );
}

/// Every signature a page states in a plain code block is the
/// registry's: a line `module.name(...)` or `module.name: T` of a block
/// without a language, wherever on the page it stands and whatever
/// heading it is under, is the line the registry renders for that row.
/// (The renderer rewrites the block that opens a function's section;
/// this also reads the blocks it does not own, so a signature that a
/// page states twice cannot go stale in one place.)
#[test]
fn every_signature_block_of_a_page_is_the_registry_s() {
    let mut stale = Vec::new();
    let mut compared = 0;
    for (file, page) in pages() {
        let mut in_plain = false;
        let mut in_other = false;
        for (number, line) in page.lines().enumerate() {
            let fence = line.trim_start().strip_prefix("```");
            if let Some(language) = fence {
                match (in_plain, in_other) {
                    (false, false) if language.trim().is_empty() => in_plain = true,
                    (false, false) => in_other = true,
                    _ => (in_plain, in_other) = (false, false),
                }
                continue;
            }
            if !in_plain {
                continue;
            }
            // `module.name(` or `module.name:` at the start of the line.
            let Some((module, rest)) = line.split_once('.') else {
                continue;
            };
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_')
                .collect();
            if !matches!(rest[name.len()..].chars().next(), Some('(' | ':')) {
                continue;
            }
            let Some(expected) = signature_block(module, &name) else {
                continue;
            };
            compared += 1;
            if line != expected {
                stale.push(format!(
                    "docs/stdlib/{file}:{}:\n    page:     {line}\n    registry: {expected}",
                    number + 1
                ));
            }
        }
    }
    assert!(compared > 200, "{compared} signature lines compared");
    assert!(
        stale.is_empty(),
        "signature blocks that are not the registry's:\n  {}",
        stale.join("\n  ")
    );
}

/// Every page the registry reads is a file of `docs/stdlib/`, and the
/// directory holds no other page.
#[test]
fn docs_stdlib_holds_exactly_the_registry_s_pages() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/stdlib");
    let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
        .expect("docs/stdlib")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    on_disk.sort();
    let mut read: Vec<String> = pages().into_iter().map(|(file, _)| file).collect();
    read.sort();
    assert_eq!(on_disk, read);
}

/// The field types a page's "Decodable types" section says a record's
/// fields may have are the ones its module's decoders decode: the
/// field types that hold no other (`Int`, `Bytes`, `json.Value`, ...)
/// named in the bullet list after "may have these types:" are the
/// leaves of the decoders' family in the registry
/// (`silt::builtins::registry::decoding`), and the list names the
/// containers every family decodes.
#[test]
fn the_decodable_field_types_of_a_page_are_the_registry_s() {
    let mut checked = Vec::new();
    for module in &registry().modules {
        let families: HashSet<Family> = module
            .rows
            .iter()
            .filter_map(|row| row.decoding)
            .map(|decoding| decoding.family)
            .collect();
        let Some(family) = families.iter().next() else {
            continue;
        };
        assert_eq!(
            families.len(),
            1,
            "the decoders of {} are of one family",
            module.name
        );
        let Some((_, section)) = module.page.split_once("## Decodable types") else {
            continue;
        };
        let Some((_, after)) = section.split_once("these types:") else {
            panic!(
                "{}: its Decodable types section lists no field types",
                module.page_file
            );
        };
        let list: Vec<&str> = after
            .lines()
            .skip_while(|line| line.trim().is_empty())
            .take_while(|line| line.starts_with("- "))
            .collect();
        // The backticked names of the list.
        let named: Vec<&str> = list
            .iter()
            .flat_map(|line| line.split('`').skip(1).step_by(2))
            .collect();
        let leaves: Vec<&str> = named
            .iter()
            .copied()
            .filter(|name| Leaf::ALL.iter().any(|leaf| leaf.name() == *name))
            .collect();
        let expected: Vec<&str> = family.leaves().iter().map(|leaf| leaf.name()).collect();
        assert_eq!(
            leaves, expected,
            "{}: the field types of its list",
            module.page_file
        );
        for container in ["List(T)", "Option(T)", "Map(String, T)"] {
            assert!(
                named.contains(&container),
                "{}: its list names no {container}",
                module.page_file
            );
        }
        // Enum types, where the family decodes them: an item of the list
        // and a section of the page.
        let enums = list
            .iter()
            .any(|line| line.contains("non-generic enum types"));
        assert_eq!(
            enums,
            family.decodes_enums(),
            "{}: its list names enum types",
            module.page_file
        );
        assert_eq!(
            module.page.contains("\n## Enums\n"),
            family.decodes_enums(),
            "{}: its section on enums",
            module.page_file
        );
        checked.push(module.name);
    }
    assert_eq!(checked, ["json", "toml"]);
}
