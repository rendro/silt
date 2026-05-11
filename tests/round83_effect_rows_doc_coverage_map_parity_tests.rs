//! Round-83 LATENT fix: the stdlib-coverage-map table in
//! `docs/proposals/effect-rows.md` Part 6 §(a) (lines ~457-461) had
//! drifted from the real stdlib. The proposal is `status:
//! implemented` so readers treat its mapping as normative, yet the
//! walker can't reach it (no `fn main` fences). This lock guards
//! against re-drift.
//!
//! Strategy (combined): a NARROW source-grep lock (the floor — the
//! exact strings that were wrong must stay absent) plus a richer
//! parity lock (module-name entries in the catch-all row must appear
//! in `BUILTIN_MODULES`, and function entries in the per-effect rows
//! must reference real registered builtins).

use std::collections::BTreeSet;

use silt::module::BUILTIN_MODULES;

const DOC: &str = include_str!("../docs/proposals/effect-rows.md");

/// Each `forbidden` substring identifies a known-wrong fragment that
/// existed before the round-83 fix. If any reappears, the doc has
/// drifted back.
#[test]
fn round83_doc_does_not_contain_known_wrong_strings() {
    let forbidden: &[&str] = &[
        // io.* functions that don't exist (they're fs.*):
        "io.exists",
        "io.is_file",
        "io.is_dir",
        // time function that doesn't exist (it's time.format / time.format_date):
        "time.format_now",
        // crypto wildcard that matches nothing:
        "crypto.gen_*",
        // bogus module-name tokens from the original catch-all row:
        ", env_var,",
        ", net,",
        ", random,",
        ", ffi |",
    ];

    for needle in forbidden {
        assert!(
            !DOC.contains(needle),
            "docs/proposals/effect-rows.md still contains the known-wrong string {:?}",
            needle
        );
    }
}

/// Extract the cell text of a row whose leading effect label is `label`
/// (e.g. ``"`!fs`"``). Returns the right-hand cell (the
/// "Modules / builtins contributing" column).
fn coverage_row(label: &str) -> String {
    // Find the row in the stdlib-coverage-map table. Rows look like:
    //   | `!fs` | `io.read_file`, ... |
    // The label match must be on a row directly under the coverage-map
    // header to avoid picking up other prose mentions of "!fs".
    let header_idx = DOC
        .find("#### Stdlib coverage map")
        .expect("Stdlib coverage map header missing");
    let after_header = &DOC[header_idx..];
    for line in after_header.lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with('|') {
            continue;
        }
        // Format: | <label> | <cells> |
        let parts: Vec<&str> = trimmed.splitn(3, '|').collect();
        if parts.len() < 3 {
            continue;
        }
        // parts[0] is empty (leading |), parts[1] is label, parts[2] is rest.
        if parts[1].trim() == label {
            // parts[2] is "<cells> |\n" — strip the trailing " |".
            let cells = parts[2].trim_end().trim_end_matches('|').trim();
            return cells.to_string();
        }
    }
    panic!(
        "row with label {:?} not found in Stdlib coverage map",
        label
    );
}

/// Extract every backtick-quoted token from a row, returning the
/// inner text (e.g. ``"`time.now`"`` -> "time.now").
fn extract_tokens(cells: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = cells.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() && bytes[j] != b'`' {
                j += 1;
            }
            if j < bytes.len() {
                out.push(String::from_utf8_lossy(&bytes[start..j]).into_owned());
                i = j + 1;
            } else {
                break;
            }
        } else {
            i += 1;
        }
    }
    out
}

/// The set of registered builtin `module.fn` names (i.e. names that
/// contain a dot), harvested from the typechecker's actual env.
fn registered_module_fns() -> BTreeSet<String> {
    silt::typechecker::iter_builtins_for_effects_audit()
        .into_iter()
        .map(|(name, _)| name)
        .filter(|name| name.contains('.'))
        .collect()
}

#[test]
fn round83_catch_all_row_lists_only_real_modules_or_functions() {
    // The `!io` row is the catch-all. Module-bare entries (no dot)
    // must be in BUILTIN_MODULES. Dotted entries must be real
    // `module.fn` names.
    let cells = coverage_row("`!io` (catch-all)");
    let tokens = extract_tokens(&cells);
    assert!(
        !tokens.is_empty(),
        "expected at least one backtick-quoted token in catch-all row, got: {:?}",
        cells
    );
    let module_names: BTreeSet<&str> = BUILTIN_MODULES.iter().copied().collect();
    let fns = registered_module_fns();
    for tok in &tokens {
        if tok.contains('.') {
            assert!(
                fns.contains(tok),
                "catch-all row references {:?} which is not a registered builtin function",
                tok
            );
        } else {
            assert!(
                module_names.contains(tok.as_str()),
                "catch-all row references module {:?} which is not in BUILTIN_MODULES",
                tok
            );
        }
    }
}

#[test]
fn round83_fs_row_lists_only_real_functions() {
    let cells = coverage_row("`!fs`");
    let tokens = extract_tokens(&cells);
    let fns = registered_module_fns();
    assert!(!tokens.is_empty(), "expected !fs row to list functions");
    for tok in &tokens {
        // Each token in the !fs row should be a real registered function.
        assert!(
            fns.contains(tok),
            "!fs row references {:?} which is not a registered builtin function",
            tok
        );
    }
}

#[test]
fn round83_time_row_lists_only_real_functions() {
    let cells = coverage_row("`!time`");
    let tokens = extract_tokens(&cells);
    let fns = registered_module_fns();
    assert!(!tokens.is_empty(), "expected !time row to list functions");
    for tok in &tokens {
        assert!(
            fns.contains(tok),
            "!time row references {:?} which is not a registered builtin function",
            tok
        );
    }
}

#[test]
fn round83_random_row_lists_only_real_functions() {
    let cells = coverage_row("`!random`");
    let tokens = extract_tokens(&cells);
    let fns = registered_module_fns();
    assert!(!tokens.is_empty(), "expected !random row to list functions");
    for tok in &tokens {
        assert!(
            fns.contains(tok),
            "!random row references {:?} which is not a registered builtin function",
            tok
        );
    }
}
