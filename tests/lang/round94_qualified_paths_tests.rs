//! Round 94 — module-qualified type paths work in ALL positions.
//!
//! The approved design: `mod.Type` is explicit naming, not a second
//! construction form. Qualified enum-constructor CALLS
//! (`shapes.Circle(2.0)`) already worked; this round adds:
//!
//!   * qualified RECORD literals: `util.Pt { x: 1, y: 2 }` constructs
//!     exactly the value `Pt { x: 1, y: 2 }` builds after a selective
//!     import — same typechecking, same runtime, same trait dispatch;
//!   * qualified VARIANT patterns in every pattern position (match
//!     arms, `when let`, `let`, or-patterns, nested patterns), plus
//!     qualified RECORD patterns;
//!   * exhaustiveness treats `shapes.Circle(r)` and `Circle(r)` as the
//!     SAME constructor (mixed spellings across one match's arms);
//!   * disambiguation: two modules exporting same-named types are told
//!     apart by the qualifier, no alias import needed;
//!   * quality diagnostics for wrong-module / unknown-type, with
//!     near-miss suggestions.
//!
//! Every test of this file except the `silt fmt` round trip (a
//! multi-step CLI flow) is a golden case under
//! `tests/golden/lang/modules/round94_qualified_paths__*`.
//!
//! Conservatism controls for the parser side (trailing closures,
//! next-line braces, match bodies, record update) live in
//! tests/lang/round93_qualified_record_literal_tests.rs.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Fresh per-test temp directory so parallel test runs don't collide.
fn tempdir(label: &str) -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "silt_round94_qualpath_{label}_{}_{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Write `(name, contents)` files into a fresh temp project dir.
fn temp_project(label: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = tempdir(label);
    for (name, contents) in files {
        std::fs::write(dir.join(name), contents).expect("write project file");
    }
    dir
}

/// Run `silt <subcommand> <file>` and return (stdout, stderr, success).
fn run_silt(subcommand: &str, file: &Path) -> (String, String, bool) {
    let bin = env!("CARGO_BIN_EXE_silt");
    let out = Command::new(bin)
        .arg(subcommand)
        .arg(file)
        .output()
        .expect("spawn silt");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (stdout, stderr, out.status.success())
}

const UTIL_PT: &str = "pub type Pt { x: Int, y: Int }\n";
const SHAPES: &str = "pub type Shape {\n  Circle(Float),\n  Rect(Float, Float)\n}\n";

// ────────────────────────────────────────────────────────────────────
// Controls
// ────────────────────────────────────────────────────────────────────

/// `silt fmt` round-trips the new forms: formatting is idempotent and
/// the formatted file still runs with identical output.
#[test]
fn formatter_roundtrips_qualified_forms() {
    let dir = temp_project(
        "fmt",
        &[
            ("util.silt", UTIL_PT),
            ("shapes.silt", SHAPES),
            (
                "main.silt",
                "import shapes\nimport util\n\nfn main() {\n  let p = util.Pt { x: 1, y: 2 }\n  let m = match shapes.Circle(1.0) {\n    shapes.Circle(r) -> p.x,\n    Shape.Rect(w, h) -> p.y,\n  }\n  let n = match p {\n    util.Pt { x, .. } -> x,\n  }\n  print(m + n)\n}\n",
            ),
        ],
    );
    let main = dir.join("main.silt");
    let (_, stderr, ok) = run_silt("fmt", &main);
    assert!(ok, "silt fmt must succeed; stderr:\n{stderr}");
    let once = std::fs::read_to_string(&main).expect("read formatted");
    assert!(
        once.contains("util.Pt { x: 1, y: 2 }")
            && once.contains("shapes.Circle(r)")
            && once.contains("Shape.Rect(w, h)")
            && once.contains("util.Pt { x, .. }"),
        "fmt must preserve the qualified spellings; got:\n{once}"
    );
    let (_, stderr, ok) = run_silt("fmt", &main);
    assert!(ok, "second silt fmt must succeed; stderr:\n{stderr}");
    let twice = std::fs::read_to_string(&main).expect("read reformatted");
    assert_eq!(once, twice, "fmt must be idempotent on qualified forms");
    let (stdout, stderr, ok) = run_silt("run", &main);
    assert!(
        ok,
        "formatted file must still run; stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert_eq!(stdout.replace(char::is_whitespace, ""), "2");
    let _ = std::fs::remove_dir_all(&dir);
}
