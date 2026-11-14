//! Round-26 DOC agent locks: the doc state the round-26 audit fixed.
//!
//! - G7: README tooling block's subcommand set must track `silt --help`.
//! - G8: the postgres page says that the module is an opt-in feature
//!   and how to build silt with it. (That a page lists every function
//!   of its module is `stdlib_reference_tests`; that every builtin name
//!   has a doc is `lsp_builtin_doc_extraction_tests`.)

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

// ─── G8: the postgres page ───────────────────────────────────────────

#[test]
fn postgres_doc_says_it_is_opt_in_and_how_to_enable_it() {
    // The module's page, whether or not this build has the feature.
    let body = silt::builtins::registry::registry()
        .module("postgres")
        .expect("the postgres module")
        .page;

    // Opt-in feature header — mirror the precedent at tcp's
    // "TLS (opt-in feature)" section.
    assert!(
        body.contains("opt-in feature") || body.contains("opt-in"),
        "docs/stdlib/postgres.md must flag the module as opt-in"
    );
    assert!(
        body.contains("--features postgres"),
        "docs/stdlib/postgres.md must show how to enable the feature \
         (e.g. `--features postgres`)"
    );
}
