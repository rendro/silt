//! Round-94 GAP (consistency/soundness): the language `==` operator applied
//! IEEE-754 NaN semantics (`NaN != NaN`) only at the TOP level.
//!
//! `language_eq` (`src/vm/execute.rs`) overrode the bitwise float comparison
//! for top-level floats but fell through to `PartialEq for Value` for nested
//! values. `PartialEq` on `ExtFloat` uses `to_bits()` equality (NaN is
//! self-equal — REQUIRED for Map/Set keying and dedup), so a NaN buried in a
//! List/Tuple/Map/Set/Record/Variant compared EQUAL, making `==` inconsistent:
//!
//!   nan == nan            -> false  (correct IEEE-754)
//!   [nan] == [nan]        -> true   (INCONSISTENT — should be false)
//!   (nan, 1) == (nan, 1)  -> true   (INCONSISTENT — should be false)
//!
//! The fix makes `language_eq` recurse through every container kind, applying
//! the NaN-aware float rule at every level, WITHOUT touching `PartialEq` /
//! `Hash` / `Ord` (which still back collection keying — a NaN key must stay
//! self-equal so it can be found again).
//!
//! These tests shell out to the compiled CLI binary and assert on the VM
//! execution path (`silt run`), which is where the bug lived — a typecheck-only
//! assertion would not exercise it.

use std::process::Command;

fn run_silt(label: &str, src: &str) -> String {
    let tmp = std::env::temp_dir().join(format!("silt_round94_nan_eq_{label}.silt"));
    std::fs::write(&tmp, src).expect("write temp file");
    let bin = env!("CARGO_BIN_EXE_silt");
    let out = Command::new(bin)
        .arg("run")
        .arg(&tmp)
        .output()
        .expect("spawn silt");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "silt run should succeed for {label}; stdout={stdout:?}, stderr={stderr:?}"
    );
    stdout
}

#[test]
fn top_level_nan_is_not_self_equal() {
    let out = run_silt(
        "top_level",
        r#"
fn main() {
    let nan = 0.0 / 0.0
    println(nan == nan)
    println(nan != nan)
}
"#,
    );
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines, vec!["false", "true"], "out={out:?}");
}

#[test]
fn nan_nested_in_list_is_not_equal() {
    let out = run_silt(
        "list",
        r#"
fn main() {
    let nan = 0.0 / 0.0
    println([nan] == [nan])
    println([nan] != [nan])
}
"#,
    );
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines, vec!["false", "true"], "out={out:?}");
}

#[test]
fn nan_nested_in_tuple_is_not_equal() {
    let out = run_silt(
        "tuple",
        r#"
fn main() {
    let nan = 0.0 / 0.0
    println((nan, 1) == (nan, 1))
    println((nan, 1) != (nan, 1))
}
"#,
    );
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines, vec!["false", "true"], "out={out:?}");
}

#[test]
fn normal_nested_equality_still_works() {
    let out = run_silt(
        "sanity",
        r#"
fn main() {
    println([1.0] == [1.0])
    println([1, 2] == [1, 2])
    println((1.0, 2) == (1.0, 2))
    println([1, 2] == [1, 3])
}
"#,
    );
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines, vec!["true", "true", "true", "false"], "out={out:?}");
}

#[test]
fn nan_keyed_collections_keying_path_intact() {
    // The Eq/Hash contract for collection keying must NOT be affected by the
    // `==` operator fix: a NaN inserted into a Set/Map must still be found.
    let out = run_silt(
        "keying",
        r#"
import set
import map

fn main() {
    let nan = 0.0 / 0.0
    let s = set.from_list([nan])
    println(set.contains(s, nan))
    let m = #{ nan: "x" }
    println(map.get(m, nan))
}
"#,
    );
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2, "out={out:?}");
    assert_eq!(
        lines[0], "true",
        "NaN set key must still be found via Hash/Ord keying; out={out:?}"
    );
    assert!(
        lines[1].contains("x"),
        "NaN map key must still resolve its value; out={out:?}"
    );
}
