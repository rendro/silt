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

use std::path::{Path, PathBuf};
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

const REQUIRED_INDEX_MODULES: &[&str] = &["bytes", "tcp", "stream", "postgres"];

#[test]
fn stdlib_index_references_all_per_module_pages() {
    let path = manifest_dir().join("docs").join("stdlib").join("index.md");
    let body = read(&path);
    for module in REQUIRED_INDEX_MODULES {
        // Reference either as a markdown link target `<module>.md` or
        // as a backticked name. The link form is the contract we want
        // to enforce — the module should be clickable from the index.
        let link = format!("({}.md)", module);
        assert!(
            body.contains(&link),
            "{} is missing a link to `{}.md` in the Module Index",
            path.display(),
            module
        );
    }
}

#[test]
fn stdlib_reference_table_references_all_per_module_pages() {
    let path = manifest_dir().join("docs").join("stdlib-reference.md");
    let body = read(&path);
    for module in REQUIRED_INDEX_MODULES {
        let link = format!("(stdlib/{}.md)", module);
        assert!(
            body.contains(&link),
            "{} is missing a link to `stdlib/{}.md` in the per-module table",
            path.display(),
            module
        );
    }
}

#[test]
fn postgres_doc_exists_with_frontmatter_and_documents_every_builtin() {
    let path = manifest_dir()
        .join("docs")
        .join("stdlib")
        .join("postgres.md");
    assert!(
        path.is_file(),
        "docs/stdlib/postgres.md does not exist: {}",
        path.display()
    );
    let body = read(&path);

    // Frontmatter must be present and match neighboring stdlib pages.
    assert!(
        body.starts_with("---\n"),
        "postgres.md must begin with YAML frontmatter (starting with '---')"
    );
    assert!(
        body.contains("title: \"postgres\""),
        "postgres.md frontmatter must include title: \"postgres\""
    );
    assert!(
        body.contains("section: \"Standard Library\""),
        "postgres.md frontmatter must include section: \"Standard Library\""
    );

    // Opt-in feature header — mirror the precedent at tcp.md's
    // "TLS (opt-in feature)" section.
    assert!(
        body.contains("opt-in feature") || body.contains("opt-in"),
        "postgres.md must flag itself as opt-in (mirror tcp.md TLS precedent)"
    );
    assert!(
        body.contains("--features postgres"),
        "postgres.md must show how to enable the feature (e.g. `--features postgres`)"
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
        // The table uses bare function names (e.g. `connect`) so we
        // accept either the full `postgres.connect` form or the bare
        // name. Require both a surface mention of the fully-qualified
        // name in the doc body *or* a table row for it.
        let bare = name.strip_prefix("postgres.").unwrap();
        let table_row = format!("`{}`", bare);
        if !body.contains(name) && !body.contains(&table_row) {
            missing.push(name);
        }
    }
    assert!(
        missing.is_empty(),
        "postgres.md is missing documentation for builtin(s): {:?}",
        missing
    );
}

/// Coverage walker: for every `register_<name>_builtins` definition in
/// `src/typechecker/builtins.rs`, assert that a matching
/// `docs/stdlib/<name>.md` exists (or that `<name>` is explicitly
/// documented on a combined page we know about).
///
/// This is the future-proofing lock: if a new `register_foo_builtins`
/// ships, this test fails until `docs/stdlib/foo.md` is added.
#[test]
fn every_register_builtins_has_a_per_module_doc() {
    let src_path = manifest_dir()
        .join("src")
        .join("typechecker")
        .join("builtins.rs");
    let src = read(&src_path);

    // Modules that share a combined page with neighboring modules.
    // The keys are `<name>` as it appears in `register_<name>_builtins`;
    // the values are the doc filename (without .md) that actually hosts
    // them. Keep this in sync with docs/stdlib/ layout.
    let combined = |name: &str| -> Option<&'static str> {
        match name {
            "int" | "float" => Some("int-float"),
            "io" | "fs" | "env" => Some("io-fs"),
            "result" | "option" => Some("result-option"),
            "channel" => Some("channel-task"),
            _ => None,
        }
    };

    let stdlib_dir = manifest_dir().join("docs").join("stdlib");

    let mut missing: Vec<String> = Vec::new();
    let mut seen_any = false;
    for line in src.lines() {
        let trimmed = line.trim_start();
        // Match definitions, not call sites, so we don't double-count.
        // The definition form is `fn register_<name>_builtins(`.
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

        let doc_stem = combined(name).unwrap_or(name);
        let doc_path = stdlib_dir.join(format!("{}.md", doc_stem));
        if !doc_path.is_file() {
            missing.push(format!(
                "register_{}_builtins has no docs (expected {})",
                name,
                doc_path.display()
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
    let path = manifest_dir().join("docs").join("stdlib").join("tcp.md");
    let body = read(&path);
    // The prior wording was "return Err in v0.9 (they require unwrapping ...)".
    // We allow `v0.9` elsewhere (e.g. historical notes), but not as a
    // version pin on the peer_addr / set_nodelay limitation.
    assert!(
        !body.contains("return Err in v0.9"),
        "docs/stdlib/tcp.md still contains the stale \"return Err in v0.9\" \
         version-pinned wording; drop the version pin or retarget to \
         current release."
    );
}

#[test]
fn bytes_doc_has_no_v0_9_module_surface_claim() {
    let path = manifest_dir().join("docs").join("stdlib").join("bytes.md");
    let body = read(&path);
    assert!(
        !body.contains("v0.9 module surface"),
        "docs/stdlib/bytes.md still contains the stale \"v0.9 module surface\" \
         forward-compat claim; generalize the wording."
    );
}

// ─── Supporting integrity checks ─────────────────────────────────────

/// Paranoia check: every module we claim to link from the indexes
/// must actually have a file on disk.
#[test]
fn required_index_modules_have_files() {
    let stdlib_dir: PathBuf = manifest_dir().join("docs").join("stdlib");
    for module in REQUIRED_INDEX_MODULES {
        let path = stdlib_dir.join(format!("{}.md", module));
        assert!(
            path.is_file(),
            "docs/stdlib/{}.md must exist (referenced by index)",
            module
        );
    }
}
