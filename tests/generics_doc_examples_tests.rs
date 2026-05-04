//! Lock the user-facing examples in `docs/language/generics.md` behind
//! a test so doc edits don't silently regress.
//!
//! These tests mirror the key examples verbatim. Adding a new example to
//! the doc should come with a new test here; the inverse — a test here
//! without a matching doc example — is also a drift signal.
//!
//! Round-71 upgrade: the original file used the in-process
//! `typechecker::check` API only, which produced a weak gate — bugs
//! like DOC-1 (a snippet calling `map.insert(...)` instead of the
//! actual `map.set`) survived because it was in `Cache` example which
//! the in-process walker happened not to reach with full cascade.
//! Each test now writes the snippet to a temp file and spawns
//! `silt run` (or `silt check` for declaration-only snippets).
//! This mirrors `tests/docs_round68_assoc_types_and_rows_walker_tests.rs::run_ok`
//! and gives a much stronger gate: parser + typechecker + compiler +
//! runtime all need to agree.

use std::path::{Path, PathBuf};
use std::process::Command;

fn silt_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

fn scratch_dir(suffix: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("silt_round71_generics_{suffix}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_main(dir: &Path, src: &str) -> PathBuf {
    let path = dir.join("main.silt");
    std::fs::write(&path, src).unwrap();
    path
}

/// Run a snippet that already contains `fn main` end-to-end through
/// `silt run`. The snippet must be a complete program.
fn run_ok(label: &str, src: &str) -> (String, String) {
    let dir = scratch_dir(label);
    let main = write_main(&dir, src);
    let output = silt_bin()
        .args(["run", main.to_str().unwrap()])
        .output()
        .expect("silt run");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "[{label}] silt run should succeed; stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    (stdout, stderr)
}

/// Run a declaration-only snippet through `silt check`. Used for
/// signatures / type definitions that have no observable runtime
/// behaviour but should still typecheck.
fn check_ok(label: &str, src: &str) -> (String, String) {
    let dir = scratch_dir(label);
    let main = write_main(&dir, src);
    let output = silt_bin()
        .args(["check", main.to_str().unwrap()])
        .output()
        .expect("silt check");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "[{label}] silt check should succeed; stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    (stdout, stderr)
}

/// Snippet that should be rejected by `silt check`. Asserts the
/// stderr contains the substring `needle`.
fn check_fails_with(label: &str, src: &str, needle: &str) -> String {
    let dir = scratch_dir(label);
    let main = write_main(&dir, src);
    let output = silt_bin()
        .args(["check", main.to_str().unwrap()])
        .output()
        .expect("silt check");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        !output.status.success(),
        "[{label}] silt check should reject the snippet; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains(needle),
        "[{label}] expected stderr to contain {needle:?}; got:\n{stderr}"
    );
    stderr
}

#[test]
fn identity() {
    check_ok(
        "identity",
        "fn identity(x: a) -> a { x }\n\nfn main() { let _ = identity(1) }\n",
    );
}

#[test]
fn swap() {
    check_ok(
        "swap",
        r#"
        fn swap(pair: (a, b)) -> (b, a) {
            let (x, y) = pair
            (y, x)
        }

        fn main() { let _ = swap((1, "a")) }
        "#,
    );
}

#[test]
fn generic_type_decls() {
    check_ok(
        "option_result_pair",
        r#"
        type Option(a) { Some(a), None }
        type Result(a, e) { Ok(a), Err(e) }
        type Pair(a, b) { first: a, second: b }

        fn main() { () }
        "#,
    );
}

#[test]
fn binding_rule_return_only_rejected() {
    check_fails_with(
        "return_only",
        "fn make() -> a { unreachable() }\n\nfn main() { () }\n",
        "type variable 'a' in return type is not introduced",
    );
}

#[test]
fn where_clause_multi_bound() {
    check_ok(
        "dedup",
        r#"
        fn dedup(xs: List(a)) -> List(a) where a: Equal + Hash {
            xs
        }

        fn main() { let _ = dedup([1, 2, 3]) }
        "#,
    );
}

#[test]
fn default_descriptor_dispatch_example() {
    check_ok(
        "default_via_type_descriptor",
        r#"
        trait Default {
            fn default() -> Self
        }

        trait Default for Int {
            fn default() -> Self { 0 }
        }

        fn default(type a) -> a where a: Default {
            a.default()
        }

        fn use_it() {
            let _ = default(Int)
        }

        fn main() { use_it() }
        "#,
    );
}

#[test]
fn parse_descriptor_dispatch_example() {
    // Phase 1 of the stdlib error redesign: `int.parse` now returns
    // `Result(Int, ParseError)` instead of `Result(Int, String)`, so
    // the Decode-for-Int impl propagates `ParseError` through the
    // trait. The parameterized trait still demonstrates the type
    // descriptor dispatch mechanism; only the error slot changed.
    check_ok(
        "parse_via_type_descriptor",
        r#"
        import int

        trait Decode {
            fn decode(body: String) -> Result(Self, ParseError)
        }

        trait Decode for Int {
            fn decode(body: String) -> Result(Int, ParseError) {
                int.parse(body)
            }
        }

        fn parse(body: String, type a) -> Result(a, ParseError) where a: Decode {
            a.decode(body)
        }

        fn use_it() {
            let _ = parse("42", Int)
        }

        fn main() { use_it() }
        "#,
    );
}

#[test]
fn concrete_type_path_example() {
    check_ok(
        "Int_default",
        r#"
        trait Default {
            fn default() -> Self
        }

        trait Default for Int {
            fn default() -> Self { 0 }
        }

        fn use_it() {
            let _ = Int.default()
        }

        fn main() { use_it() }
        "#,
    );
}

#[test]
fn value_receiver_on_no_self_rejected() {
    check_fails_with(
        "5_default_rejected",
        r#"
        trait Default {
            fn default() -> Self
        }

        trait Default for Int {
            fn default() -> Self { 0 }
        }

        fn use_it() {
            let _ = 5.default()
        }

        fn main() { use_it() }
        "#,
        "takes no `self`",
    );
}

#[test]
fn pipe_composition_with_type_arg() {
    check_ok(
        "pipe",
        r#"
        import json

        type Todo { id: Int, title: String }

        fn use_it(body: String) {
            let _ = body |> json.parse(Todo)
        }

        fn main() { use_it("") }
        "#,
    );
}

#[test]
fn group_by_with_trait_bound() {
    check_ok(
        "group_by",
        r#"
        import list
        import map

        fn group_by(xs: List(a), key: Fn(a) -> k) -> Map(k, List(a))
            where k: Hash + Equal
        {
            #{}
        }

        fn main() { () }
        "#,
    );
}

#[test]
fn user_defined_generic_container() {
    // `get` half of the Cache example.
    check_ok(
        "Cache_get",
        r#"
        import map

        type Cache(k, v) {
            store: Map(k, v),
            capacity: Int,
        }

        fn get(c: Cache(k, v), key: k) -> Option(v) where k: Hash + Equal {
            map.get(c.store, key)
        }

        fn main() { () }
        "#,
    );
}

#[test]
fn f_put_generic_container_runs() {
    // ROUND-71 DOC-1 lock: the `fn put` half of the Cache example
    // mirrors the snippet at `docs/language/generics.md:675-679`
    // verbatim. Pre-fix this called `map.insert(...)`, which doesn't
    // exist (the actual builtin is `map.set`); the snippet failed
    // typecheck with "unknown function 'insert' on module 'map'" but
    // the in-tree `examples_check` walker skipped fences without a
    // `fn main`, so the bug shipped. This test restores end-to-end
    // coverage.
    check_ok(
        "Cache_put",
        r#"
        import map

        type Cache(k, v) {
            store: Map(k, v),
            capacity: Int,
        }

        fn get(c: Cache(k, v), key: k) -> Option(v) where k: Hash + Equal {
            map.get(c.store, key)
        }

        fn put(c: Cache(k, v), key: k, value: v) -> Cache(k, v)
            where k: Hash + Equal
        {
            c.{ store: map.set(c.store, key, value) }
        }

        fn main() { () }
        "#,
    );
}

#[test]
fn f_traits_default_method_show_runs() {
    // ROUND-71 DOC-2 lock: the doc previously redeclared the built-in
    // `Display` trait with `fn show` / `fn debug` methods, which the
    // typechecker rejects as a builtin redefinition. The trait has
    // been renamed to `Show` in `docs/language/traits.md:114-129`
    // and `:139-144`. This test mirrors both halves end-to-end so
    // the renamed snippets actually exercise the runtime dispatch
    // paths the doc claims they do.
    let (stdout, _) = run_ok(
        "trait_show_default",
        r#"
        trait Show {
          fn show(self) -> String { "default" }
          fn debug(self) -> String
        }

        type Item { v: Int }

        trait Show for Item {
          fn debug(self) -> String { "item-debug" }
        }

        fn main() {
          println(Item { v: 1 }.show())
          println(Item { v: 1 }.debug())
        }
        "#,
    );
    assert!(
        stdout.contains("default") && stdout.contains("item-debug"),
        "expected `default` and `item-debug` in stdout; got {stdout:?}"
    );
}

#[test]
fn f_traits_show_override_runs() {
    let (stdout, _) = run_ok(
        "trait_show_override",
        r#"
        trait Show {
          fn show(self) -> String { "default" }
          fn debug(self) -> String
        }

        type Item { v: Int }

        trait Show for Item {
          fn show(self) -> String { "explicit-show" }
          fn debug(self) -> String { "item-debug" }
        }

        fn main() {
          println(Item { v: 1 }.show())
          println(Item { v: 1 }.debug())
        }
        "#,
    );
    assert!(
        stdout.contains("explicit-show") && stdout.contains("item-debug"),
        "expected `explicit-show` and `item-debug` in stdout; got {stdout:?}"
    );
}
