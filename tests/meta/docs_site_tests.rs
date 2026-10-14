//! What the website needs of `docs/`. silt-lang.com builds a page from
//! every file under `docs/`, at the path of the file, and lists the
//! pages of the sections `SECTIONS` in its navigation, each section in
//! the order of its pages' `order:`.
//!
//! - Every page has a `title:`.
//! - A listed page has an `order:` that no other page of its section
//!   has: two pages with one number have no order between them.
//! - Every relative link of a page leads to a file of the repository
//!   (a page, an example), not to a directory: the site has a page for
//!   a file, and sends a link that leaves `docs/` to the file on GitHub.
//!
//! Its front page is made of `README.md`: the program above the first
//! heading, then each `##` section above "Install" as one feature (its
//! heading, its text, its program), and the table of "Reference".

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The sections the site lists.
const SECTIONS: [&str; 3] = ["Guide", "Language", "Standard Library"];

fn pages() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "md") {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("docs"),
        &mut out,
    );
    out.sort();
    assert!(!out.is_empty(), "docs/ holds no page");
    out
}

/// The value of the frontmatter's `key`, without its quotes.
fn field(text: &str, key: &str) -> Option<String> {
    let rest = text.strip_prefix("---\n")?;
    let (frontmatter, _) = rest.split_once("\n---\n")?;
    frontmatter.lines().find_map(|line| {
        let value = line.strip_prefix(key)?.strip_prefix(':')?.trim();
        Some(value.trim_matches('"').to_string())
    })
}

fn shown(path: &Path) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let relative = path.strip_prefix(root).unwrap();
    relative.to_string_lossy().replace('\\', "/")
}

#[test]
fn every_page_has_a_title_and_a_place_of_its_own_in_its_section() {
    let mut places: BTreeMap<(String, u32), Vec<String>> = BTreeMap::new();
    let mut failures = Vec::new();
    for path in pages() {
        let text = std::fs::read_to_string(&path)
            .unwrap()
            .replace("\r\n", "\n");
        if field(&text, "title").is_none_or(|title| title.is_empty()) {
            failures.push(format!("{}: no `title:`", shown(&path)));
        }
        let Some(section) = field(&text, "section") else {
            failures.push(format!("{}: no `section:`", shown(&path)));
            continue;
        };
        if !SECTIONS.contains(&section.as_str()) {
            continue;
        }
        match field(&text, "order").and_then(|order| order.parse::<u32>().ok()) {
            Some(order) => places
                .entry((section, order))
                .or_default()
                .push(shown(&path)),
            None => failures.push(format!("{}: no `order:` that is a number", shown(&path))),
        }
    }
    for ((section, order), pages) in places {
        if pages.len() > 1 {
            failures.push(format!(
                "section \"{section}\", order {order}: {}",
                pages.join(" and ")
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The targets of the markdown links of `text` outside its code: what
/// stands in the parentheses of `[...](...)`.
fn links(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_fence = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        // Code spans hold no links.
        let prose: String = line.split('`').step_by(2).collect::<Vec<_>>().join(" ");
        let mut rest = prose.as_str();
        while let Some(at) = rest.find("](") {
            rest = &rest[at + 2..];
            if let Some(end) = rest.find(')') {
                out.push(rest[..end].to_string());
            }
        }
    }
    out
}

#[test]
fn every_relative_link_leads_to_a_file() {
    let mut failures = Vec::new();
    for path in pages() {
        let text = std::fs::read_to_string(&path).unwrap();
        for link in links(&text) {
            let target = link.split('#').next().unwrap_or_default();
            if target.is_empty() || target.contains("://") || target.starts_with("mailto:") {
                continue;
            }
            let file = path.parent().unwrap().join(target);
            if !file.is_file() {
                failures.push(format!("{}: ({link}) is not a file", shown(&path)));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn the_readme_has_what_the_front_page_shows() {
    let readme = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md"))
        .unwrap()
        .replace("\r\n", "\n");
    let mut sections = readme.split("\n## ");
    let top = sections.next().unwrap();
    assert!(
        top.contains("\n```silt\n"),
        "README.md has no `silt` block above its first `##` heading"
    );
    let sections: Vec<&str> = sections.collect();
    let install = sections
        .iter()
        .position(|section| section.starts_with("Install\n"))
        .expect("README.md has no \"## Install\" section");
    assert!(install > 0, "README.md has no section above \"## Install\"");
    for feature in &sections[..install] {
        let heading = feature.lines().next().unwrap_or_default();
        assert!(
            feature.contains("\n```silt\n"),
            "the section \"{heading}\" of README.md has no `silt` block"
        );
    }
    let reference = sections
        .iter()
        .find(|section| section.starts_with("Reference\n"))
        .expect("README.md has no \"## Reference\" section");
    assert!(
        reference.contains("\n| **keywords** |"),
        "the \"## Reference\" section of README.md has no table with a keywords row"
    );
}
