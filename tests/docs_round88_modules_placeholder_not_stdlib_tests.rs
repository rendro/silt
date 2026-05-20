//! Round 88 LATENT lock + J finding: doc placeholders for user-defined
//! modules must NOT collide with stdlib module names.
//!
//! Pre-fix: `docs/language/modules.md`'s "Imports" section used
//! `import math ...` as the placeholder for a user-defined module,
//! and `docs/getting-started.md`'s "Modules and imports" section used
//! `import list ...`. Both `math` and `list` are stdlib modules
//! registered in `silt::module::BUILTIN_MODULES`. Any reader who
//! copy-pasted the snippet to learn the syntax — or to seed their own
//! module file under `src/math.silt` / `src/list.silt` — would shadow
//! a stdlib name, producing surprising "name already in scope" /
//! resolution errors that the doc itself did not warn about.
//!
//! Post-fix: both sections use `geometry`, a name that is NOT in
//! `BUILTIN_MODULES`. This test locks both files to that invariant by
//! parsing the `import <name>` lines in the targeted sections and
//! asserting every extracted module-root name is absent from
//! `silt::module::BUILTIN_MODULES`. Future drift (renaming back to a
//! stdlib name, or adding a new stdlib name with the same string)
//! flips the assertion.

use silt::module::BUILTIN_MODULES;

const MODULES_DOC: &str = include_str!("../docs/language/modules.md");
const GETTING_STARTED_DOC: &str = include_str!("../docs/getting-started.md");

/// Slice a doc string between the first occurrence of `start_marker`
/// and the first occurrence of `end_marker` *strictly after* it
/// (i.e. past the start marker's own bytes). Returns `None` if
/// either marker is missing. We use this to constrain the scan to
/// the "Imports" / "Modules and imports" snippet — we do NOT want
/// to flag the stdlib examples like `import list` in the
/// "Built-in modules" section that follow.
fn slice_between<'a>(src: &'a str, start_marker: &str, end_marker: &str) -> Option<&'a str> {
    let start = src.find(start_marker)?;
    let body_start = start + start_marker.len();
    let after_start = &src[body_start..];
    let end_in_after = after_start.find(end_marker)?;
    Some(&src[start..body_start + end_in_after])
}

/// Extract the first identifier following each `import ` token in
/// `slice`. Handles brace forms (`import foo.{ a, b }`), aliased
/// forms (`import foo as f`), and dotted forms (`import foo.bar`) —
/// in every case we want the *root* module name (`foo`).
fn import_roots(slice: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in slice.lines() {
        let trimmed = line.trim();
        let rest = match trimmed.strip_prefix("import ") {
            Some(r) => r,
            None => continue,
        };
        // The root is the run of [A-Za-z0-9_] characters before the
        // first separator (`.`, ` `, `\t`, `{`).
        let mut end = 0;
        for (i, ch) in rest.char_indices() {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                end = i + ch.len_utf8();
            } else {
                break;
            }
        }
        if end > 0 {
            out.push(rest[..end].to_string());
        }
    }
    out
}

fn assert_no_stdlib_collision(slice: &str, section_label: &str) {
    let roots = import_roots(slice);
    assert!(
        !roots.is_empty(),
        "expected to find at least one `import <name>` line in the {section_label} \
         section. Did the doc section markers move? Update this test."
    );
    let mut bad: Vec<(String, &'static str)> = Vec::new();
    for root in &roots {
        if let Some(stdlib) = BUILTIN_MODULES.iter().find(|m| **m == root.as_str()) {
            bad.push((root.clone(), *stdlib));
        }
    }
    assert!(
        bad.is_empty(),
        "{section_label} uses stdlib module name(s) as the user-module placeholder: \
         {bad:?}. Rename to a non-stdlib name (e.g. `geometry`, `shapes`, `mymath`) \
         so copy-pasting the snippet does not collide with a stdlib module. \
         Full BUILTIN_MODULES list: {BUILTIN_MODULES:?}"
    );
}

#[test]
fn modules_doc_imports_section_uses_non_stdlib_placeholder() {
    // Constrain to the "## Imports" section — i.e. from that heading
    // up to the next `## ` heading. The stdlib-example block in the
    // "Built-in modules" section legitimately imports stdlib names
    // (`io`, `list`, `channel`) so we explicitly avoid scanning it.
    let slice = slice_between(MODULES_DOC, "## Imports", "## ")
        .or_else(|| MODULES_DOC.find("## Imports").map(|i| &MODULES_DOC[i..]))
        .expect("docs/language/modules.md must contain an `## Imports` section");
    assert_no_stdlib_collision(slice, "docs/language/modules.md `## Imports`");
}

#[test]
fn getting_started_doc_modules_section_uses_non_stdlib_placeholder() {
    // The Modules section in getting-started.md is "## 5. Modules and
    // imports" and runs until the next `## ` heading ("## 6.
    // Errors as values"). Constrain to that slice — later sections
    // may legitimately import stdlib modules in examples.
    let slice = slice_between(
        GETTING_STARTED_DOC,
        "## 5. Modules and imports",
        "## 6.",
    )
    .expect(
        "docs/getting-started.md must contain a `## 5. Modules and imports` section \
         followed by a `## 6.` section. If headings were renumbered, update this test.",
    );
    // The section also shows a `geometry.silt` user-defined module
    // example after the placeholder snippet. `geometry` is NOT in
    // BUILTIN_MODULES, so it's safe — the assertion below catches
    // any stdlib name in the whole section.
    assert_no_stdlib_collision(
        slice,
        "docs/getting-started.md `## 5. Modules and imports`",
    );
}

#[test]
fn helper_import_roots_extracts_root_name_only() {
    // Self-test the parser so a future change to the extractor
    // doesn't silently weaken the lock.
    let sample = "\
        import geometry                    -- qualified\n\
        import geometry.{ area, perim }    -- direct\n\
        import geometry as g               -- aliased\n\
        import net.http                     -- dotted\n\
    ";
    let roots = import_roots(sample);
    assert_eq!(
        roots,
        vec![
            "geometry".to_string(),
            "geometry".to_string(),
            "geometry".to_string(),
            "net".to_string(),
        ],
        "import_roots must extract the root module name only — \
         pre-`.`, pre-`{{`, pre-` as`. Got: {roots:?}"
    );
}
