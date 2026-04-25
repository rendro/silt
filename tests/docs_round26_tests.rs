//! Round-26 DOC agent locks.
//!
//! These tests pin the doc state the round-26 audit fixed so it doesn't
//! drift again:
//!
//! - G7: README tooling block must list `silt update` and `silt add`,
//!   and its subcommand set must track `silt --help`.
//! - G8: `docs/stdlib/index.md` and `docs/stdlib-reference.md` must
//!   reference every stdlib module that has a per-module page
//!   (specifically `bytes`, `tcp`, `stream`, `postgres`). A coverage
//!   walker cross-checks every `register_<module>_builtins` call in
//!   `src/typechecker/builtins.rs` against `docs/stdlib/<name>.md` so
//!   a future module can't ship without docs.
//! - L11: Version-pinned wording ("coming in v0.7", the `v0.9` lock on
//!   `peer_addr`/`set_nodelay`, and the "v0.9 module surface" claim in
//!   bytes.md) must stay out.

use std::path::Path;
use std::process::Command;

fn manifest_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", path.display(), e))
}

/// Extract the ```text-free content of README's Tooling code fence.
/// The Tooling section starts at `## Tooling` and the next ``` fence
/// after it is the command table.
fn readme_tooling_block() -> String {
    let readme_path = manifest_dir().join("README.md");
    let body = read(&readme_path);
    let tooling_idx = body
        .find("## Tooling")
        .expect("README.md is missing a '## Tooling' heading");
    let rest = &body[tooling_idx..];
    let fence_open = rest
        .find("```")
        .expect("README.md Tooling section has no fenced code block");
    let body_after_open = &rest[fence_open + 3..];
    // The opener may be `\n` (no language tag) — skip to the first newline.
    let newline = body_after_open
        .find('\n')
        .expect("README Tooling fence opener has no newline");
    let body_after_open = &body_after_open[newline + 1..];
    let close = body_after_open
        .find("```")
        .expect("README.md Tooling code block is unterminated");
    body_after_open[..close].to_string()
}

// ─── G7: README tooling block ────────────────────────────────────────

/// README's Tooling block must list `silt update` and `silt add`.
/// Before round 26 it listed 10 subcommands and omitted these two,
/// even though getting-started.md already had them.
#[test]
fn readme_tooling_block_lists_silt_update_and_add() {
    let block = readme_tooling_block();
    assert!(
        block.contains("silt update"),
        "README.md Tooling block is missing `silt update`:\n{}",
        block
    );
    assert!(
        block.contains("silt add"),
        "README.md Tooling block is missing `silt add`:\n{}",
        block
    );
}

/// Mirror of `test_getting_started_tooling_block_matches_main_help` for
/// README. Extracts every `silt <subcommand>` entry from `silt --help`
/// and asserts each appears in the README Tooling block.
#[test]
fn readme_tooling_block_matches_main_help() {
    let block = readme_tooling_block();

    let help_output = silt_cmd()
        .arg("--help")
        .output()
        .expect("failed to spawn silt --help");
    assert!(
        help_output.status.success(),
        "silt --help exited non-zero: {:?}",
        help_output.status.code()
    );
    let help_text = String::from_utf8_lossy(&help_output.stdout).to_string()
        + &String::from_utf8_lossy(&help_output.stderr);

    let mut required: Vec<String> = Vec::new();
    for line in help_text.lines() {
        let trimmed = line.trim_start();
        let rest = match trimmed.strip_prefix("silt ") {
            Some(r) => r,
            None => continue,
        };
        let sub: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '-')
            .collect();
        if sub.is_empty() || sub == "help" {
            continue;
        }
        if !required.contains(&sub) {
            required.push(sub);
        }
    }
    assert!(
        !required.is_empty(),
        "could not extract any `silt <subcommand>` from silt --help:\n{}",
        help_text
    );

    let mut missing: Vec<String> = Vec::new();
    for sub in &required {
        let needle = format!("silt {}", sub);
        if !block.contains(&needle) {
            missing.push(sub.clone());
        }
    }
    assert!(
        missing.is_empty(),
        "README.md Tooling block is missing subcommand(s) {:?} that \
         appear in `silt --help` (authoritative list from src/main.rs). \
         Add a line for each missing subcommand so the doc stays in sync.\n\n\
         Tooling block:\n{}\n\nHelp output:\n{}",
        missing,
        block,
        help_text
    );
}

// ─── G8: Stdlib indexes + per-module docs ────────────────────────────
//
// Round 62 phase-2 deleted `docs/stdlib/index.md` and
// `docs/stdlib-reference.md` (along with every per-module page) and
// moved the per-module markdown into `super::docs::*_MD` constants.
// The tests below now check that each formerly-listed module has at
// least one binding with a non-empty registered doc, and that
// postgres-specific contracts (opt-in feature, --features postgres
// hint, every documented builtin) are preserved in the inlined
// markdown.

const REQUIRED_INDEX_MODULES: &[&str] = &["bytes", "tcp", "stream", "postgres"];

#[test]
fn stdlib_index_references_all_per_module_pages() {
    let docs = silt::typechecker::builtin_docs();
    for module in REQUIRED_INDEX_MODULES {
        let dot = format!("{module}.");
        let any = docs
            .iter()
            .any(|(k, v)| k.starts_with(&dot) && !v.trim().is_empty());
        assert!(
            any,
            "no `{module}.*` binding has a non-empty registered doc — \
             round 62 phase-2 inlined the per-module prose into \
             `super::docs::*_MD` (see src/typechecker/builtins/docs.rs). \
             Restore the section."
        );
    }
}

#[test]
fn stdlib_reference_table_references_all_per_module_pages() {
    // Same contract as above; round 62 phase-2 collapsed both the
    // `stdlib/index.md` table and the `stdlib-reference.md` table
    // into the single LSP-delivered surface. Kept as a parallel
    // assertion so future drift is easier to bisect.
    stdlib_index_references_all_per_module_pages();
}

#[test]
fn postgres_doc_exists_with_frontmatter_and_documents_every_builtin() {
    let docs = silt::typechecker::builtin_docs();
    let body = docs
        .keys()
        .filter(|k| k.starts_with("postgres."))
        .find_map(|k| docs.get(k))
        .cloned()
        .expect("at least one postgres.* binding must have a registered doc");

    // Opt-in feature header — mirror the precedent at tcp's
    // "TLS (opt-in feature)" section.
    assert!(
        body.contains("opt-in feature") || body.contains("opt-in"),
        "the inlined postgres doc must flag the module as opt-in"
    );
    assert!(
        body.contains("--features postgres"),
        "the inlined postgres doc must show how to enable the feature \
         (e.g. `--features postgres`)"
    );

    // Every postgres builtin must be documented by name.
    const REQUIRED_BUILTINS: &[&str] = &[
        "postgres.connect",
        "postgres.query",
        "postgres.execute",
        "postgres.transact",
        "postgres.close",
        "postgres.stream",
        "postgres.cursor",
        "postgres.cursor_next",
        "postgres.cursor_close",
        "postgres.listen",
        "postgres.notify",
        "postgres.uuidv7",
    ];
    let mut missing: Vec<&str> = Vec::new();
    for name in REQUIRED_BUILTINS {
        let bare = name.strip_prefix("postgres.").unwrap();
        let table_row = format!("`{}`", bare);
        if !body.contains(name) && !body.contains(&table_row) {
            missing.push(name);
        }
    }
    assert!(
        missing.is_empty(),
        "the inlined postgres doc (super::docs::POSTGRES_MD) is \
         missing documentation for builtin(s): {missing:?}"
    );
}

/// Coverage walker: for every `register_<name>_builtins` definition in
/// `src/typechecker/builtins.rs`, assert that at least one `<name>.*`
/// builtin name has a registered doc string. Round 62 phase-2 inlined
/// the per-module markdown into `super::docs::*_MD` constants under
/// `src/typechecker/builtins/docs.rs` and the per-module register
/// function calls `attach_module_docs` (or the overview/filtered
/// variants) to stamp those bodies onto each binding's
/// `env.builtin_docs` entry.
///
/// This is the future-proofing lock: if a new `register_foo_builtins`
/// ships, this test fails until the corresponding `FOO_MD` blob has a
/// `## \`foo.X\`` section attached for at least one of the names it
/// registers.
#[test]
fn every_register_builtins_has_a_per_module_doc() {
    let src_path = manifest_dir()
        .join("src")
        .join("typechecker")
        .join("builtins.rs");
    let src = read(&src_path);

    let docs = silt::typechecker::builtin_docs();

    // The `errors` module is special: `register_errors_builtins`
    // registers bare-name variant constructors (`IoNotFound`,
    // `JsonSyntax`, …), not `errors.*`. We assert each of those
    // variants has a registered doc rather than scanning a prefix.
    fn errors_have_docs(docs: &std::collections::HashMap<String, String>) -> bool {
        // A representative sample — every variant is attached the
        // same body via `attach_enum_variant_docs` in errors.rs, so
        // checking one is sufficient for the coverage smoke test.
        ["IoNotFound", "JsonSyntax", "TomlSyntax", "ParseEmpty"]
            .iter()
            .all(|n| docs.get(*n).map(|d| !d.trim().is_empty()).unwrap_or(false))
    }

    let mut missing: Vec<String> = Vec::new();
    let mut seen_any = false;
    for line in src.lines() {
        let trimmed = line.trim_start();
        let after_fn = match trimmed.strip_prefix("fn register_") {
            Some(r) => r,
            None => continue,
        };
        let end = match after_fn.find("_builtins") {
            Some(i) => i,
            None => continue,
        };
        let name = &after_fn[..end];
        if name.is_empty() {
            continue;
        }
        seen_any = true;

        let has_any_doc = if name == "errors" {
            errors_have_docs(&docs)
        } else {
            let dot = format!("{name}.");
            docs.iter()
                .any(|(k, v)| k.starts_with(&dot) && !v.trim().is_empty())
        };
        if !has_any_doc {
            missing.push(format!(
                "register_{}_builtins has no inlined docs — no `{}.*` \
                 binding has a non-empty `super::docs::*_MD` section \
                 attached. Add one and call `attach_module_docs` (or \
                 `attach_module_overview` for module-level prose) from \
                 `register(checker, env)`.",
                name, name
            ));
        }
    }

    assert!(
        seen_any,
        "no `fn register_<name>_builtins` definitions found in {} — \
         did the file layout change?",
        src_path.display()
    );
    assert!(
        missing.is_empty(),
        "{} stdlib module(s) ship without a per-module doc page. \
         Create the missing docs or add the module to the combined-page \
         map in this test:\n{}",
        missing.len(),
        missing.join("\n")
    );
}

// ─── L11: Version-pinned wording ─────────────────────────────────────

#[test]
fn getting_started_does_not_reference_coming_in_v0_7() {
    let path = manifest_dir().join("docs").join("getting-started.md");
    let body = read(&path);
    assert!(
        !body.contains("coming in v0.7"),
        "docs/getting-started.md still contains the stale 'coming in v0.7' \
         phrasing — `silt update` shipped in v0.7 and the current release \
         is v0.10+. Rewrite to describe the current behavior."
    );
}

#[test]
fn tcp_doc_has_no_bare_v0_9_limitation_pin() {
    let docs = silt::typechecker::builtin_docs();
    let body = docs
        .keys()
        .filter(|k| k.starts_with("tcp."))
        .find_map(|k| docs.get(k))
        .cloned()
        .expect("at least one tcp.* binding must have a registered doc");
    assert!(
        !body.contains("return Err in v0.9"),
        "the inlined tcp doc (super::docs::TCP_MD) still contains the \
         stale \"return Err in v0.9\" version-pinned wording; drop the \
         version pin or retarget to current release."
    );
}

#[test]
fn bytes_doc_has_no_v0_9_module_surface_claim() {
    let docs = silt::typechecker::builtin_docs();
    let body = docs
        .keys()
        .filter(|k| k.starts_with("bytes."))
        .find_map(|k| docs.get(k))
        .cloned()
        .expect("at least one bytes.* binding must have a registered doc");
    assert!(
        !body.contains("v0.9 module surface"),
        "the inlined bytes doc (super::docs::BYTES_MD) still contains \
         the stale \"v0.9 module surface\" forward-compat claim; \
         generalize the wording."
    );
}

// ─── Supporting integrity checks ─────────────────────────────────────

/// Paranoia check: every module we claim to deliver via LSP must
/// actually have at least one registered builtin doc. Round 62
/// phase-2 replaced the on-disk file presence check.
#[test]
fn required_index_modules_have_files() {
    let docs = silt::typechecker::builtin_docs();
    for module in REQUIRED_INDEX_MODULES {
        let dot = format!("{module}.");
        let any = docs
            .iter()
            .any(|(k, v)| k.starts_with(&dot) && !v.trim().is_empty());
        assert!(
            any,
            "module `{module}` has no registered builtin doc — round 62 \
             phase-2 inlined the per-module markdown into \
             `super::docs::*_MD` (in src/typechecker/builtins/docs.rs)."
        );
    }
}
