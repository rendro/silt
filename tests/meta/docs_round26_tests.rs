//! Round-26 DOC agent locks.
//!
//! These tests pin the doc state the round-26 audit fixed so it doesn't
//! drift again:
//!
//! - G7: README tooling block's subcommand set must track `silt --help`.
//! - G8: `docs/stdlib/index.md` and `docs/stdlib-reference.md` must
//!   reference every stdlib module that has a per-module page
//!   (specifically `bytes`, `tcp`, `stream`, `postgres`). A coverage
//!   walker cross-checks every `BUILTIN_MODULES` entry against the
//!   registered builtin docs so a future module can't ship without docs.

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

#[test]
fn postgres_doc_exists_with_frontmatter_and_documents_every_builtin() {
    let docs = silt::builtins::registry::docs::builtin_docs();
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

/// Coverage walker: every builtin module (`silt::module::builtin_modules()`)
/// must have at least one `<module>.*` binding with a registered doc
/// string, and the bare-name error-variant constructors registered by
/// the typechecker's errors pass must be documented too. A new module
/// then cannot ship without docs.
#[test]
fn every_builtin_module_has_a_per_module_doc() {
    let docs = silt::builtins::registry::docs::builtin_docs();

    let mut missing: Vec<String> = Vec::new();
    for name in silt::module::builtin_modules() {
        let dot = format!("{name}.");
        let has_any_doc = docs
            .iter()
            .any(|(k, v)| k.starts_with(&dot) && !v.trim().is_empty());
        if !has_any_doc {
            missing.push(format!(
                "module `{name}` has no inlined docs — no `{name}.*` binding \
                 has a non-empty `super::docs::*_MD` section attached. Add one \
                 and call `attach_module_docs` (or `attach_module_overview` \
                 for module-level prose) from its register function."
            ));
        }
    }
    // The errors pass registers bare-name variant constructors, each
    // attached the same body via `attach_enum_variant_docs`.
    for variant in ["IoNotFound", "JsonSyntax", "TomlSyntax", "ParseEmpty"] {
        if !docs.get(variant).is_some_and(|d| !d.trim().is_empty()) {
            missing.push(format!("error variant `{variant}` has no registered doc"));
        }
    }

    assert!(
        missing.is_empty(),
        "{} stdlib module(s) ship without a per-module doc:\n{}",
        missing.len(),
        missing.join("\n")
    );
}
