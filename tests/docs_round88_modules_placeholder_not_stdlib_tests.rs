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

/// Extract user-module placeholder names from `-- File: src/<name>.silt`
/// comment lines (the "## File = module" code block). Returns the bare
/// module name (`<name>`, without the `src/` prefix or `.silt` suffix,
/// and only the leaf if a subdirectory path is shown).
///
/// Round 89: the round-88 fix renamed the "## Imports" placeholder to
/// `geometry` but MISSED this earlier `-- File:` block, which still
/// said `src/math.silt` — `math` is a stdlib module, so a first-time
/// reader seeding `src/math.silt` from this snippet would be shadowed.
fn file_comment_module_names(slice: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in slice.lines() {
        let trimmed = line.trim();
        // Match `-- File: <path>` (whitespace-tolerant after `File:`).
        let rest = match trimmed.strip_prefix("-- File:") {
            Some(r) => r.trim(),
            None => continue,
        };
        // `rest` is a path like `src/geometry.silt` or `src/net/http.silt`.
        // We only care about a TOP-LEVEL user module placeholder: a file
        // directly under `src/` (i.e. `src/<name>.silt` with no further
        // `/`). A nested file like `src/net/http.silt` resolves to the
        // dotted module `net.http`, whose root is `net` — not a stdlib
        // collision — so we skip it here (the directory-tree check below
        // handles the root-name reasoning explicitly).
        let path = rest.strip_prefix("src/").unwrap_or(rest);
        let name = match path.strip_suffix(".silt") {
            Some(n) => n,
            None => continue,
        };
        if name.contains('/') {
            // Nested path: collision is governed by the dotted full name,
            // not the leaf; handled by the directory-tree check.
            continue;
        }
        out.push(name.to_string());
    }
    out
}

/// Extract the *root* module name from each directory-tree leaf line of
/// the form `<name>.silt -- imported as \`<dotted.path>\``. The root is
/// the first segment of the dotted import path, since that is the name a
/// reader's top-level `src/` file must avoid colliding with. For
/// `http.silt -- imported as \`net.http\`` the root is `net` (safe);
/// for `geometry.silt -- imported as \`geometry\`` the root is
/// `geometry` (safe). A pre-fix `math.silt -- imported as \`math\``
/// would yield root `math` (stdlib collision).
fn tree_import_roots(slice: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in slice.lines() {
        // Only consider lines that name a `.silt` file AND state how it
        // is imported, e.g. `  geometry.silt   -- imported as `geometry``.
        if !line.contains(".silt") || !line.contains("imported as") {
            continue;
        }
        // Pull the backtick-quoted dotted import path.
        let after = match line.split("imported as").nth(1) {
            Some(a) => a,
            None => continue,
        };
        let start = match after.find('`') {
            Some(i) => i + 1,
            None => continue,
        };
        let end = match after[start..].find('`') {
            Some(i) => start + i,
            None => continue,
        };
        let dotted = &after[start..end];
        let root = dotted.split('.').next().unwrap_or(dotted).trim();
        if !root.is_empty() {
            out.push(root.to_string());
        }
    }
    out
}

fn assert_names_no_stdlib_collision(names: &[String], section_label: &str) {
    assert!(
        !names.is_empty(),
        "expected to extract at least one user-module placeholder name from the \
         {section_label} section. Did the doc section markers or example format \
         move? Update this test."
    );
    let mut bad: Vec<(String, &'static str)> = Vec::new();
    for n in names {
        if let Some(stdlib) = BUILTIN_MODULES.iter().find(|m| **m == n.as_str()) {
            bad.push((n.clone(), *stdlib));
        }
    }
    assert!(
        bad.is_empty(),
        "{section_label} uses stdlib module name(s) as the user-module placeholder: \
         {bad:?}. A first-time reader who seeds their own `src/<name>.silt` from this \
         snippet would be shadowed by the stdlib module of the same name (e.g. \
         `import math; math.add(...)` yields `unknown function 'add' on module 'math'`). \
         Rename to a non-stdlib name (e.g. `geometry`). Full BUILTIN_MODULES: \
         {BUILTIN_MODULES:?}"
    );
}

#[test]
fn modules_doc_file_module_block_uses_non_stdlib_placeholder() {
    // Constrain to the "## File = module" section — from that heading up
    // to the next `## ` heading ("## Visibility"). This covers BOTH the
    // `-- File: src/<name>.silt` code block and the directory-tree
    // example that follows.
    let slice = slice_between(MODULES_DOC, "## File = module", "## Visibility")
        .expect(
            "docs/language/modules.md must contain a `## File = module` section \
             followed by a `## Visibility` section. If headings changed, update \
             this test.",
        );

    let file_names = file_comment_module_names(slice);
    assert_names_no_stdlib_collision(
        &file_names,
        "docs/language/modules.md `## File = module` `-- File:` code block",
    );

    let tree_roots = tree_import_roots(slice);
    assert_names_no_stdlib_collision(
        &tree_roots,
        "docs/language/modules.md `## File = module` directory-tree example",
    );
}

#[test]
fn helper_file_comment_and_tree_extractors_behave() {
    // Self-test the new extractors so a future change doesn't silently
    // weaken the lock.
    let sample = "\
        ```silt\n\
        -- File: src/geometry.silt\n\
        pub fn add(a, b) { a + b }\n\
        ```\n\
        src/\n\
          main.silt\n\
          geometry.silt      -- imported as `geometry`\n\
          net/\n\
            http.silt        -- imported as `net.http`\n\
    ";
    // `-- File:` extractor: only the top-level `geometry`.
    assert_eq!(
        file_comment_module_names(sample),
        vec!["geometry".to_string()],
        "file_comment_module_names must return the bare top-level module name",
    );
    // Tree extractor: roots are `geometry` and `net` (NOT `http`, since
    // the dotted full name `net.http` does not collide with stdlib `http`).
    assert_eq!(
        tree_import_roots(sample),
        vec!["geometry".to_string(), "net".to_string()],
        "tree_import_roots must return the root segment of the dotted import path",
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
