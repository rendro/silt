//! Round-87 BROKEN-DOC-1 walker lock.
//!
//! `docs/language/modules.md` ships a "Built-in modules" table whose
//! row purposes inline call out specific module functions in backticks
//! (`http.get`, `io.read_file`, `task.spawn`, …). A pre-fix round-87
//! audit found a row that named `http.post` even though there is no
//! such builtin — `src/typechecker/builtins/http.rs` registers
//! `http.get`, `http.request`, `http.serve`, `http.serve_all`,
//! `http.segments`, `http.parse_query` only. The audit recommended
//! catching this class of drift via an end-to-end walker:
//!
//! 1. Scrape every `<module>.<function>(` reference out of the table.
//! 2. For each pair, run `silt check` on a tiny program that imports
//!    the module and calls the function with no arguments.
//! 3. Assert the resulting diagnostic (if any) is NOT
//!    "unknown function '<fn>' on module '<module>'" — argument-count
//!    and type errors are allowed; the goal is to catch the case where
//!    the function name itself is bogus.
//!
//! Running `silt check` as a subprocess (`CARGO_BIN_EXE_silt`) means
//! the lock is end-to-end: the entire pipeline (parser → resolver →
//! typechecker) gets a chance to flag the bogus name.

use std::collections::BTreeSet;
use std::process::Command;

const MODULES_MD: &str = include_str!("../docs/language/modules.md");

/// Slice the "Built-in modules" table out of `modules.md`. The table
/// is the first one preceded by the `## Built-in modules` heading, and
/// it ends at the next blank-line-then-prose paragraph. We just take
/// from the heading to the next `## ` header to stay tolerant of minor
/// editorial churn.
fn builtin_modules_table() -> &'static str {
    let start = MODULES_MD
        .find("## Built-in modules")
        .expect("modules.md must contain a `## Built-in modules` heading");
    let rest = &MODULES_MD[start..];
    let end_rel = rest[1..].find("\n## ").map(|i| i + 1).unwrap_or(rest.len());
    &rest[..end_rel]
}

/// Scrape every `<module>.<function>` reference inside the table.
/// The table writes callables as `` `module.fn` `` — a backticked
/// dotted name. We require the reference to be enclosed in
/// backticks (or to immediately precede a `(`) so prose like
/// `silt.toml` or filesystem-path-like fragments don't sneak in,
/// and so plain English module names without a function suffix are
/// ignored.
fn scrape_module_function_refs(table: &str) -> BTreeSet<(String, String)> {
    let mut found = BTreeSet::new();
    let bytes = table.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Anchor on a leading backtick — every callable in the
        // table is written `` `module.fn` ``.
        if bytes[i] != b'`' {
            i += 1;
            continue;
        }
        let after_tick = i + 1;
        if after_tick >= bytes.len() || !is_ident_start(bytes[after_tick]) {
            i += 1;
            continue;
        }
        let mod_start = after_tick;
        let mut j = mod_start;
        while j < bytes.len() && is_ident_continue(bytes[j]) {
            j += 1;
        }
        let mod_name = &table[mod_start..j];
        if j >= bytes.len() || bytes[j] != b'.' {
            i = j;
            continue;
        }
        let after_dot = j + 1;
        if after_dot >= bytes.len() || !is_ident_start(bytes[after_dot]) {
            i = after_dot;
            continue;
        }
        let fn_start = after_dot;
        let mut k = fn_start;
        while k < bytes.len() && is_ident_continue(bytes[k]) {
            k += 1;
        }
        let fn_name = &table[fn_start..k];
        // Require a closing backtick or `(` immediately after the
        // function name (allowing optional `()` between).
        let mut close = k;
        if close + 1 < bytes.len() && bytes[close] == b'(' && bytes[close + 1] == b')' {
            close += 2;
        }
        if close >= bytes.len() || bytes[close] != b'`' {
            i = k;
            continue;
        }
        i = close + 1;
        // Filter to known builtin modules so prose like
        // `silt.toml` doesn't sneak in.
        if silt::module::BUILTIN_MODULES.contains(&mod_name) {
            found.insert((mod_name.to_string(), fn_name.to_string()));
        }
    }
    found
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

#[test]
fn modules_doc_function_refs_exist_per_silt_check() {
    let table = builtin_modules_table();
    let pairs = scrape_module_function_refs(table);
    assert!(
        !pairs.is_empty(),
        "expected to scrape at least one `<module>.<fn>(` reference \
         out of the Built-in modules table; got none — scraper bug?"
    );

    // For each pair, run `silt check` on a tiny program. We write to
    // a per-pair temp file so failures point at the offending name.
    let tmp_root = std::env::temp_dir().join("silt-round87-modules-doc");
    let _ = std::fs::create_dir_all(&tmp_root);

    let silt_bin = env!("CARGO_BIN_EXE_silt");

    let mut bogus: Vec<(String, String, String)> = Vec::new();
    for (module, func) in &pairs {
        let src = format!(
            "import {module}\nfn main() {{ {module}.{func}() }}\n",
            module = module,
            func = func
        );
        let path = tmp_root.join(format!("{}_{}.silt", module, func));
        std::fs::write(&path, &src)
            .unwrap_or_else(|e| panic!("failed to write {}: {e}", path.display()));

        let output = Command::new(silt_bin)
            .arg("check")
            .arg(&path)
            .output()
            .unwrap_or_else(|e| panic!("failed to invoke silt check on {}: {e}", path.display()));

        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        let unknown_fn_marker = format!("unknown function '{func}' on module '{module}'");
        if combined.contains(&unknown_fn_marker) {
            bogus.push((module.clone(), func.clone(), combined.clone()));
        }
    }

    assert!(
        bogus.is_empty(),
        "docs/language/modules.md references {n} module function(s) that do not exist:\n{detail}",
        n = bogus.len(),
        detail = bogus
            .iter()
            .map(|(m, f, _)| format!("  - {m}.{f}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}
