//! The reference pages under `docs/stdlib/` agree with the builtin
//! registry: each page is rendered again from the rows
//! (`silt::builtins::registry::docs::render_page`: the summary tables'
//! signature and description cells, the signature block of each
//! function's section) and compared with the file.
//!
//! After a change of a row, `SILT_BLESS=1 cargo nextest run --test meta
//! stdlib_reference` rewrites the pages.

use std::path::PathBuf;

use silt::builtins::registry::docs::{pages, render_page};

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
