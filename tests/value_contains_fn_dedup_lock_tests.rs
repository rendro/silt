//! Dedup lock — the contains-a-fn walker must have exactly ONE
//! definition: `Vm::value_contains_fn` (src/vm/mod.rs), the single
//! runtime-side oracle for every execution-site Compare/Equal/Hash
//! gate.
//!
//! FINDING (LATENT): src/builtins/collections.rs carried a private
//! free-fn copy of the walker, byte-for-byte identical to
//! `Vm::value_contains_fn`, with no dedup or sync lock tying the two
//! together. The two copies happened to enumerate the same arms
//! (VmClosure / BuiltinFn / VariantConstructor leaves; List / Tuple /
//! Variant / Set / Map / Record recursion) — but a new container
//! `Value` variant added to one walker and not the other would have
//! silently split gate behavior between the operator/dispatch surface
//! and the collection-builtin surface. Exact parallel-drift class
//! locked elsewhere by tests/builtin_variant_seed_parity_lock_tests.rs
//! (the value.rs / module.rs 115-line duplicate).
//!
//! FIX: the free copy is deleted; `ensure_no_fn` in
//! src/builtins/collections.rs delegates to `Vm::value_contains_fn`
//! (a `pub` associated fn with no receiver — the free copy was pure
//! duplication).
//!
//! Locks:
//!   1. SOURCE-SCAN — walk every `.rs` file under src/ and assert
//!      exactly one `fn value_contains_fn(` definition exists,
//!      and that it lives in src/vm/mod.rs. FAILS pre-fix (two
//!      definitions), PASSES post-fix.
//!   2. DELEGATION — collections.rs must consult the oracle via
//!      `Vm::value_contains_fn`, so the builtin gates cannot quietly
//!      stop consulting the single walker.
//!   3. EXECUTION no-op proof — the deletion must not change gate
//!      behavior: a Fn-bearing container reaching a collection builtin
//!      still errors with the canonical wording (positive control:
//!      the same builtin on comparable values still succeeds). The
//!      broader behavioral matrix stays pinned by
//!      tests/collection_builtin_fn_gate_tests.rs.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Recursively collect every `.rs` file under `dir`.
fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

// ── Lock 1: single definition across src/ ──────────────────────────────

#[test]
fn value_contains_fn_has_exactly_one_definition_in_src() {
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    assert!(
        src_dir.is_dir(),
        "expected src directory at {}",
        src_dir.display()
    );

    let mut files = Vec::new();
    collect_rs_files(&src_dir, &mut files);
    assert!(
        files.len() >= 10,
        "expected to scan at least 10 .rs files under src/, found {}",
        files.len()
    );

    let mut defining_files: Vec<String> = Vec::new();
    let mut total_defs = 0usize;
    for path in &files {
        let source = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        // A DEFINITION is `fn value_contains_fn(` — call sites are
        // `value_contains_fn(v)` / `Self::value_contains_fn(...)` /
        // `Vm::value_contains_fn(...)` and never match the `fn ` prefix.
        let defs = source.matches("fn value_contains_fn(").count();
        if defs > 0 {
            total_defs += defs;
            defining_files.push(format!("{} ({defs})", path.display()));
        }
    }

    assert_eq!(
        total_defs, 1,
        "expected exactly ONE `fn value_contains_fn(` definition across \
         src/ — `Vm::value_contains_fn` in src/vm/mod.rs is the single \
         runtime-side oracle for the Compare/Equal/Hash gates. A second \
         copy (the pre-fix state had a byte-identical private free fn in \
         src/builtins/collections.rs) re-opens parallel-drift: a new \
         container Value variant added to one walker silently splits gate \
         behavior. Delegate to the oracle instead. Definitions found in: \
         {defining_files:?}"
    );
    assert!(
        defining_files[0].contains("vm") && defining_files[0].contains("mod.rs"),
        "the single `fn value_contains_fn(` definition must live in \
         src/vm/mod.rs (the documented oracle), found it in: \
         {defining_files:?}"
    );
}

// ── Lock 2: collections.rs delegates to the oracle ─────────────────────

const COLLECTIONS_RS: &str = include_str!("../src/builtins/collections.rs");

#[test]
fn collections_gate_delegates_to_vm_oracle() {
    assert!(
        !COLLECTIONS_RS.contains("fn value_contains_fn("),
        "src/builtins/collections.rs re-declares `fn value_contains_fn(` \
         — the walker was deduped into `Vm::value_contains_fn` \
         (src/vm/mod.rs); call the oracle instead of re-inlining a copy"
    );
    assert!(
        COLLECTIONS_RS.contains("Vm::value_contains_fn"),
        "src/builtins/collections.rs no longer consults \
         `Vm::value_contains_fn` — the collection-builtin Fn gate \
         (`ensure_no_fn`) must use the single runtime-side oracle so all \
         Compare/Equal gate surfaces agree on which values contain a fn"
    );
}

// ── Lock 3: execution no-op proof for the dedup refactor ───────────────

/// Drive the full pipeline via the `silt run` CLI.
fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_contains_fn_dedup_{label}.silt"));
    std::fs::write(&tmp, src).expect("write temp file");
    let bin = env!("CARGO_BIN_EXE_silt");
    let out = Command::new(bin)
        .arg("run")
        .arg(&tmp)
        .output()
        .expect("spawn silt run");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (stdout, stderr, out.status.success())
}

/// Post-dedup, the builtin gate still walks nested containers down to a
/// fn-shaped leaf via the oracle: `list.sort` on a list of records whose
/// field holds a closure is rejected with the canonical wording.
#[test]
fn dedup_keeps_nested_fn_rejection_through_builtin_gate() {
    let src = r#"
import list
type Box {
  item: Fn(Int) -> Int,
}
fn main() {
  let a = Box { item: fn(y){ y } }
  let b = Box { item: fn(z){ z + 1 } }
  let sorted = list.sort([a, b])
  println(list.length(sorted))
}
"#;
    let (stdout, stderr, ok) = run_silt_raw("nested_fn_sort", src);
    assert!(
        !ok,
        "expected list.sort of fn-bearing records to fail at the runtime \
         gate, got success with stdout: {stdout:?}"
    );
    assert!(
        stderr.contains("does not implement Compare"),
        "expected canonical gate wording in stderr, got: {stderr:?}"
    );
}

/// Positive control: the delegated walk must not over-reject — sorting
/// plain comparable values through the same gated builtin still works.
#[test]
fn dedup_keeps_comparable_sort_working() {
    let src = r#"
import list
fn main() {
  println(list.sort([3, 1, 2]))
}
"#;
    let (stdout, stderr, ok) = run_silt_raw("comparable_sort", src);
    assert!(
        ok,
        "expected clean run for list.sort of ints, got failure with \
         stderr: {stderr:?}"
    );
    assert!(
        stdout.contains("[1, 2, 3]"),
        "expected sorted output, got: {stdout:?}"
    );
}
