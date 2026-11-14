//! The reference pages of the standard library are reached from the
//! Language Guide: its "Standard Library" section links every page of
//! `docs/stdlib/`, and with them the page of every builtin module.

use std::path::Path;

fn standard_library_section() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/language-guide.md");
    let guide = std::fs::read_to_string(&path).expect("read docs/language-guide.md");
    let start = guide
        .find("\n## Standard Library\n")
        .expect("docs/language-guide.md has a `## Standard Library` section");
    let rest = &guide[start + 1..];
    let end = rest[3..].find("\n## ").map_or(rest.len(), |i| i + 3);
    rest[..end].to_string()
}

#[test]
fn the_guide_links_every_stdlib_page() {
    let section = standard_library_section();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/stdlib");
    let mut missing = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("read docs/stdlib") {
        let name = entry.expect("dir entry").file_name();
        let name = name.to_string_lossy();
        if name.ends_with(".md") && !section.contains(&format!("(stdlib/{name})")) {
            missing.push(name.to_string());
        }
    }
    missing.sort();
    assert!(
        missing.is_empty(),
        "the Standard Library section of docs/language-guide.md links no \
         `stdlib/<page>.md` for: {missing:?}"
    );
}

#[test]
fn every_builtin_module_has_its_page_in_the_guide() {
    let section = standard_library_section();
    let registry = silt::builtins::registry::registry();
    let mut missing = Vec::new();
    for module in &registry.modules {
        if !section.contains(&format!("(stdlib/{})", module.page_file)) {
            missing.push(format!("{} ({})", module.name, module.page_file));
        }
    }
    assert!(
        missing.is_empty(),
        "the Standard Library section of docs/language-guide.md does not \
         link the page of: {missing:?}"
    );
}
