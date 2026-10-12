//! The reference pages under `docs/stdlib/` agree with the builtin
//! registry: each page is rendered again from the rows
//! (`silt::builtins::registry::docs::render_page`: the summary tables'
//! signature and description cells, the signature block of each
//! function's section) and compared with the file.
//!
//! After a change of a row, `SILT_BLESS=1 cargo nextest run --test meta
//! stdlib_reference` rewrites the pages.

use std::path::PathBuf;

use silt::builtins::registry::docs::{pages, render_page, signature_block};

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
