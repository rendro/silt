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
//! Conservatism controls for the parser side (trailing closures,
//! next-line braces, match bodies, record update) live in
//! tests/round93_qualified_record_literal_tests.rs.

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

/// `silt check` then `silt run`, asserting both succeed; returns the
/// run's stdout with all whitespace stripped (print/println agnostic).
fn check_and_run(label: &str, files: &[(&str, &str)]) -> String {
    let dir = temp_project(label, files);
    let main = dir.join("main.silt");
    let (stdout, stderr, ok) = run_silt("check", &main);
    assert!(
        ok,
        "[{label}] silt check must pass; stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let (stdout, stderr, ok) = run_silt("run", &main);
    assert!(
        ok,
        "[{label}] silt run must pass; stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let _ = std::fs::remove_dir_all(&dir);
    stdout.replace(char::is_whitespace, "")
}

/// `silt check`, asserting it FAILS; returns stderr.
fn check_fails(label: &str, files: &[(&str, &str)]) -> String {
    let dir = temp_project(label, files);
    let main = dir.join("main.silt");
    let (stdout, stderr, ok) = run_silt("check", &main);
    assert!(
        !ok,
        "[{label}] silt check must fail; stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let _ = std::fs::remove_dir_all(&dir);
    stderr
}

const UTIL_PT: &str = "pub type Pt { x: Int, y: Int }\n";
const SHAPES: &str = "pub type Shape {\n  Circle(Float),\n  Rect(Float, Float)\n}\n";

// ────────────────────────────────────────────────────────────────────
// Qualified record construction (plain `import util`)
// ────────────────────────────────────────────────────────────────────

/// Semantics requirement 1: `util.Pt { ... }` with `import util`
/// constructs exactly the value `Pt { ... }` builds after
/// `import util.{ Pt }` — field access, ==, and Display all agree.
#[test]
fn qualified_record_construction_runs() {
    let out = check_and_run(
        "construct",
        &[
            ("util.silt", UTIL_PT),
            (
                "main.silt",
                "import util\n\nfn main() {\n  let a = util.Pt { x: 1, y: 2 }\n  print(a.x)\n  print(a.y)\n  print(a == util.Pt { x: 1, y: 2 })\n  print(a == util.Pt { x: 9, y: 9 })\n}\n",
            ),
        ],
    );
    assert_eq!(out, "12truefalse");
}

/// The qualified and bare spellings build INTERCHANGEABLE values: a
/// selectively-imported `Pt { ... }` compares equal to `util.Pt { ... }`.
#[test]
fn qualified_and_bare_construction_are_the_same_value() {
    let out = check_and_run(
        "interchange",
        &[
            ("util.silt", UTIL_PT),
            (
                "main.silt",
                "import util\nimport util.{ Pt }\n\nfn main() {\n  let a = util.Pt { x: 1, y: 2 }\n  let b = Pt { x: 1, y: 2 }\n  print(a == b)\n  print(\"{a}\" == \"{b}\")\n}\n",
            ),
        ],
    );
    assert_eq!(out, "truetrue");
}

/// Alias imports qualify with the alias, exactly like fn calls do.
#[test]
fn qualified_record_construction_via_alias() {
    let out = check_and_run(
        "alias",
        &[
            ("util.silt", UTIL_PT),
            (
                "main.silt",
                "import util as u\n\nfn main() {\n  let p = u.Pt { x: 5, y: 6 }\n  print(p.x + p.y)\n}\n",
            ),
        ],
    );
    assert_eq!(out, "11");
}

/// Typecheck parity with the bare form: missing and unknown fields in
/// a QUALIFIED literal get the same quality of diagnostics.
#[test]
fn qualified_record_field_errors_match_bare_quality() {
    let stderr = check_fails(
        "fielderr",
        &[
            ("util.silt", UTIL_PT),
            (
                "main.silt",
                "import util\n\nfn main() {\n  let a = util.Pt { x: 1 }\n  let b = util.Pt { x: 1, y: 2, z: 3 }\n  print(a == b)\n}\n",
            ),
        ],
    );
    assert!(
        stderr.contains("missing field") && stderr.contains("y"),
        "missing-field diagnostic must fire through the qualifier; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("unknown field 'z'"),
        "unknown-field diagnostic must fire through the qualifier; stderr:\n{stderr}"
    );
}

// ────────────────────────────────────────────────────────────────────
// Manifest dependency
// ────────────────────────────────────────────────────────────────────

/// Qualified construction works across a `silt.toml` path dependency —
/// the qualified mirrors must be populated by the dep-export
/// registration path, not just the sibling-module path.
#[test]
fn qualified_record_construction_from_manifest_dep() {
    let ws = tempdir("manifestdep");
    let dep_root = ws.join("dep");
    let app_root = ws.join("app");
    std::fs::create_dir_all(dep_root.join("src")).expect("create dep src");
    std::fs::create_dir_all(app_root.join("src")).expect("create app src");
    std::fs::write(
        dep_root.join("silt.toml"),
        "[package]\nname = \"geom\"\nversion = \"0.1.0\"\n",
    )
    .expect("write dep silt.toml");
    std::fs::write(
        dep_root.join("src/lib.silt"),
        "pub type Pt { x: Int, y: Int }\npub type Shape {\n  Circle(Float),\n  Rect(Float, Float)\n}\n",
    )
    .expect("write dep lib.silt");
    std::fs::write(
        app_root.join("silt.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\ngeom = { path = \"../dep\" }\n",
    )
    .expect("write app silt.toml");
    std::fs::write(
        app_root.join("src/main.silt"),
        "import geom\n\nfn main() {\n  let p = geom.Pt { x: 3, y: 4 }\n  print(p.x + p.y)\n  let s = match geom.Circle(2.0) {\n    geom.Circle(r) -> \"circle\",\n    geom.Rect(w, h) -> \"rect\",\n  }\n  print(s)\n}\n",
    )
    .expect("write app main.silt");
    let main = app_root.join("src/main.silt");

    let (stdout, stderr, ok) = run_silt("check", &main);
    assert!(
        ok,
        "dep-qualified construction must pass check; stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let (stdout, stderr, ok) = run_silt("run", &main);
    assert!(
        ok,
        "dep-qualified construction must run; stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert_eq!(stdout.replace(char::is_whitespace, ""), "7circle");
    let _ = std::fs::remove_dir_all(&ws);
}

// ────────────────────────────────────────────────────────────────────
// Disambiguation: two modules, same type name
// ────────────────────────────────────────────────────────────────────

/// The motivating case: two modules each export a `Pt` with DIFFERENT
/// fields. The qualifier picks the right declaration for both the
/// literal and the record pattern — no alias import needed.
#[test]
fn conflicting_record_names_disambiguated_by_qualifier() {
    let out = check_and_run(
        "conflict",
        &[
            ("a.silt", "pub type Pt { x: Int, y: Int }\n"),
            ("b.silt", "pub type Pt { label: String, v: Int }\n"),
            (
                "main.silt",
                "import a\nimport b\n\nfn main() {\n  let p = a.Pt { x: 1, y: 2 }\n  let q = b.Pt { label: \"hi\", v: 3 }\n  print(p.x + p.y)\n  print(q.label)\n  let b.Pt { label, .. } = q\n  print(label)\n}\n",
            ),
        ],
    );
    assert_eq!(out, "3hihi");
}

/// Disambiguation must hold in BOTH import orders: the bare-name maps
/// keep the first import's type, so the second module's qualified
/// spelling is exactly the case the qualified mirrors exist for.
#[test]
fn conflicting_record_names_second_module_wins_when_qualified() {
    for (label, imports) in [
        ("order_ab", "import a\nimport b\n"),
        ("order_ba", "import b\nimport a\n"),
    ] {
        let main = format!(
            "{imports}\nfn main() {{\n  let q = b.Pt {{ label: \"hi\", v: 3 }}\n  let p = a.Pt {{ x: 1, y: 2 }}\n  print(p.x + q.v)\n}}\n"
        );
        let out = check_and_run(
            label,
            &[
                ("a.silt", "pub type Pt { x: Int, y: Int }\n"),
                ("b.silt", "pub type Pt { label: String, v: Int }\n"),
                ("main.silt", &main),
            ],
        );
        assert_eq!(out, "4", "import order {label} must not matter");
    }
}

/// Using the WRONG module's fields through a qualifier is an error —
/// the literal is checked against the named module's declaration.
#[test]
fn qualifier_checks_against_named_modules_fields() {
    let stderr = check_fails(
        "wrongfields",
        &[
            ("a.silt", "pub type Pt { x: Int, y: Int }\n"),
            ("b.silt", "pub type Pt { label: String, v: Int }\n"),
            (
                "main.silt",
                "import a\nimport b\n\nfn main() {\n  let q = b.Pt { x: 1, y: 2 }\n  print(q)\n}\n",
            ),
        ],
    );
    assert!(
        stderr.contains("unknown field") || stderr.contains("missing field"),
        "b.Pt must be checked against b's declaration; stderr:\n{stderr}"
    );
}

// ────────────────────────────────────────────────────────────────────
// Qualified variant patterns, in every position
// ────────────────────────────────────────────────────────────────────

/// Match arms, `when let`, plain `let` (irrefutable case via a
/// single-variant enum), or-patterns, and nesting inside tuple/list
/// patterns all accept the qualified spelling.
#[test]
fn qualified_variant_patterns_all_positions() {
    let out = check_and_run(
        "patterns",
        &[
            ("shapes.silt", SHAPES),
            ("single.silt", "pub type One {\n  Only(Int)\n}\n"),
            (
                "main.silt",
                concat!(
                    "import shapes\nimport single\n\n",
                    "fn main() {\n",
                    // match arm
                    "  let c = shapes.Circle(2.0)\n",
                    "  let m = match c {\n    shapes.Circle(r) -> 1,\n    shapes.Rect(w, h) -> 2,\n  }\n",
                    "  print(m)\n",
                    // when let
                    "  when let shapes.Circle(r) = c else { return () }\n",
                    "  print(r)\n",
                    // plain let (irrefutable: single-variant enum)
                    "  let single.Only(n) = single.Only(7)\n",
                    "  print(n)\n",
                    // or-pattern with mixed spellings
                    "  let o = match shapes.Rect(3.0, 4.0) {\n    shapes.Circle(x) | Rect(x, _) -> x,\n  }\n",
                    "  print(o)\n",
                    // nested inside a tuple pattern
                    "  let t = match (c, 5) {\n    (shapes.Circle(x), n) -> n,\n    (shapes.Rect(_, _), n) -> 0 - n,\n  }\n",
                    "  print(t)\n",
                    "}\n",
                ),
            ),
        ],
    );
    // m=1, r=2 (floats print without a trailing .0), n=7, o=3, t=5
    assert_eq!(out, "12735");
}

/// Match-arm GUARDS see bindings introduced by qualified patterns.
#[test]
fn qualified_variant_pattern_with_guard() {
    let out = check_and_run(
        "guard",
        &[
            ("shapes.silt", SHAPES),
            (
                "main.silt",
                "import shapes\n\nfn main() {\n  let m = match shapes.Circle(2.0) {\n    shapes.Circle(r) when r > 1.0 -> \"big\",\n    shapes.Circle(_) -> \"small\",\n    shapes.Rect(_, _) -> \"rect\",\n  }\n  print(m)\n}\n",
            ),
        ],
    );
    assert_eq!(out, "big");
}

/// The enum-NAME qualifier spelling (`Shape.Circle(r)`) keeps working
/// and is now validated rather than silently discarded.
#[test]
fn enum_name_qualified_pattern_still_works() {
    let out = check_and_run(
        "enumqual",
        &[
            ("shapes.silt", SHAPES),
            (
                "main.silt",
                "import shapes.{ Shape }\n\nfn main() {\n  let m = match shapes.Circle(2.0) {\n    Shape.Circle(r) -> \"circle\",\n    Shape.Rect(w, h) -> \"rect\",\n  }\n  print(m)\n}\n",
            ),
        ],
    );
    assert_eq!(out, "circle");
}

/// Refutable qualified constructor in `let` is rejected like the bare
/// spelling (multi-variant enums need `match` / `when let`).
#[test]
fn qualified_refutable_let_rejected() {
    let stderr = check_fails(
        "refutable",
        &[
            ("shapes.silt", SHAPES),
            (
                "main.silt",
                "import shapes\n\nfn main() {\n  let shapes.Circle(r) = shapes.Circle(1.0)\n  print(r)\n}\n",
            ),
        ],
    );
    assert!(
        stderr.contains("refutable pattern"),
        "qualified spelling must hit the same refutability check; stderr:\n{stderr}"
    );
}

// ────────────────────────────────────────────────────────────────────
// Exhaustiveness: qualified == bare constructor identity
// ────────────────────────────────────────────────────────────────────

/// Mixed qualified+unqualified arms covering all variants: accepted.
#[test]
fn mixed_spelling_exhaustive_match_accepted() {
    let out = check_and_run(
        "exhaustive",
        &[
            ("shapes.silt", SHAPES),
            (
                "main.silt",
                "import shapes\n\nfn main() {\n  let m = match shapes.Rect(1.0, 2.0) {\n    shapes.Circle(r) -> \"circle\",\n    Rect(w, h) -> \"rect\",\n  }\n  print(m)\n}\n",
            ),
        ],
    );
    assert_eq!(out, "rect");
}

/// Incomplete match using the qualified spelling: rejected, and the
/// witness names the missing variant.
#[test]
fn qualified_incomplete_match_rejected_with_witness() {
    let stderr = check_fails(
        "witness",
        &[
            ("shapes.silt", SHAPES),
            (
                "main.silt",
                "import shapes\n\nfn main() {\n  let m = match shapes.Circle(1.0) {\n    shapes.Circle(r) -> 1,\n  }\n  print(m)\n}\n",
            ),
        ],
    );
    assert!(
        stderr.contains("non-exhaustive") && stderr.contains("Rect"),
        "exhaustiveness must treat the qualified arm as covering only Circle \
         and name Rect as the witness; stderr:\n{stderr}"
    );
}

/// A qualified arm COVERS its bare twin: writing the same constructor
/// in both spellings doesn't double-count coverage or break analysis
/// (the second arm is simply redundant, the match stays exhaustive).
#[test]
fn qualified_and_bare_same_ctor_are_one_constructor() {
    let out = check_and_run(
        "samector",
        &[
            ("shapes.silt", SHAPES),
            (
                "main.silt",
                "import shapes\n\nfn main() {\n  let m = match shapes.Circle(1.0) {\n    shapes.Circle(r) -> \"first\",\n    Circle(r) -> \"shadowed\",\n    Rect(w, h) -> \"rect\",\n  }\n  print(m)\n}\n",
            ),
        ],
    );
    assert_eq!(out, "first");
}

// ────────────────────────────────────────────────────────────────────
// Diagnostics: wrong module / unknown type / near misses
// ────────────────────────────────────────────────────────────────────

/// Unknown record type on a real module: names the module, suggests
/// the near-miss.
#[test]
fn unknown_qualified_record_names_module_and_suggests() {
    let stderr = check_fails(
        "recsuggest",
        &[
            ("util.silt", UTIL_PT),
            (
                "main.silt",
                "import util\n\nfn main() {\n  let p = util.Ptt { x: 1, y: 2 }\n  print(p)\n}\n",
            ),
        ],
    );
    assert!(
        stderr.contains("module 'util' has no record type 'Ptt'")
            && stderr.contains("did you mean `Pt`?"),
        "wrong-type diagnostic must name the module and suggest the near miss; stderr:\n{stderr}"
    );
}

/// Unknown variant on a real module in a PATTERN: same quality.
#[test]
fn unknown_qualified_variant_pattern_names_module_and_suggests() {
    let stderr = check_fails(
        "varsuggest",
        &[
            ("shapes.silt", SHAPES),
            (
                "main.silt",
                "import shapes\n\nfn main() {\n  let m = match shapes.Circle(1.0) {\n    shapes.Circl(r) -> 1,\n    _ -> 2,\n  }\n  print(m)\n}\n",
            ),
        ],
    );
    assert!(
        stderr.contains("module 'shapes' has no variant 'Circl'")
            && stderr.contains("did you mean `Circle`?"),
        "wrong-variant diagnostic must name the module and suggest; stderr:\n{stderr}"
    );
}

/// Wrong-MODULE qualifier (the variant lives elsewhere): rejected.
#[test]
fn wrong_module_qualifier_rejected() {
    let stderr = check_fails(
        "wrongmod",
        &[
            ("shapes.silt", SHAPES),
            ("util.silt", UTIL_PT),
            (
                "main.silt",
                "import shapes\nimport util\n\nfn main() {\n  let m = match shapes.Circle(1.0) {\n    util.Circle(r) -> 1,\n    _ -> 2,\n  }\n  print(m)\n}\n",
            ),
        ],
    );
    assert!(
        stderr.contains("module 'util' has no variant 'Circle'"),
        "naming the wrong module must be rejected; stderr:\n{stderr}"
    );
}

/// Wrong-ENUM qualifier (`Shape.Pt`-style) attributes the variant to
/// its real owner, mirroring the expression-side wording.
#[test]
fn wrong_enum_qualifier_names_real_owner() {
    let stderr = check_fails(
        "wrongenum",
        &[(
            "main.silt",
            "type Shape {\n  Circle(Int)\n}\ntype Color {\n  Red\n}\n\nfn main() {\n  let m = match Circle(1) {\n    Shape.Red -> 1,\n    _ -> 2,\n  }\n  print(m)\n}\n",
        )],
    );
    assert!(
        stderr.contains("'Red' is not a variant of enum 'Shape'")
            && stderr.contains("belongs to 'Color'"),
        "wrong-enum qualifier must name the real owner; stderr:\n{stderr}"
    );
}

/// Record types used with constructor-pattern syntax through a
/// qualifier get the record-syntax hint, like the bare form.
#[test]
fn qualified_record_with_ctor_syntax_gets_shape_hint() {
    let stderr = check_fails(
        "shapehint",
        &[
            ("util.silt", UTIL_PT),
            (
                "main.silt",
                "import util\n\nfn main() {\n  let m = match util.Pt { x: 1, y: 2 } {\n    util.Pt(a) -> a,\n  }\n  print(m)\n}\n",
            ),
        ],
    );
    assert!(
        stderr.contains("record type") && stderr.contains("record-pattern syntax"),
        "ctor syntax on a qualified record must hint the `{{ ... }}` shape; stderr:\n{stderr}"
    );
}

// ────────────────────────────────────────────────────────────────────
// Controls
// ────────────────────────────────────────────────────────────────────

/// Trailing-closure disambiguation must not regress: `util.f { x -> x }`
/// stays a trailing closure end-to-end (checks AND runs).
#[test]
fn trailing_closure_control_still_runs() {
    let out = check_and_run(
        "closure",
        &[
            ("helpers.silt", "pub fn f(g) = g(41)\n"),
            (
                "main.silt",
                "import helpers\n\nfn main() {\n  let r = helpers.f { x -> x + 1 }\n  print(r)\n}\n",
            ),
        ],
    );
    assert_eq!(out, "42");
}

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

/// Nested qualifier segments in PATTERNS are a clean parse error, not
/// a misparse.
#[test]
fn nested_pattern_qualifier_is_clean_parse_error() {
    let stderr = check_fails(
        "nestedqual",
        &[(
            "main.silt",
            "fn main() {\n  let m = match 1 {\n    a.B.Circle(r) -> 1,\n    _ -> 2,\n  }\n  print(m)\n}\n",
        )],
    );
    assert!(
        stderr.contains("nested qualifiers are not supported in patterns"),
        "deep pattern qualifiers must die with the targeted error; stderr:\n{stderr}"
    );
    // A lowercase second segment gets its own targeted message.
    let stderr = check_fails(
        "lowerqual",
        &[(
            "main.silt",
            "fn main() {\n  let m = match 1 {\n    a.b.Circle(r) -> 1,\n    _ -> 2,\n  }\n  print(m)\n}\n",
        )],
    );
    assert!(
        stderr.contains("expected a type or variant name after 'a.' in pattern"),
        "lowercase qualifier tails must die with the targeted error; stderr:\n{stderr}"
    );
}

/// Qualified construction without the import in scope: actionable
/// diagnostic naming the module.
#[test]
fn qualified_construction_without_import_errors() {
    let stderr = check_fails(
        "noimport",
        &[
            ("util.silt", UTIL_PT),
            (
                "main.silt",
                "fn main() {\n  let p = util.Pt { x: 1, y: 2 }\n  print(p)\n}\n",
            ),
        ],
    );
    assert!(
        stderr.contains("undefined type 'util.Pt'") && stderr.contains("import util"),
        "missing-import diagnostic must name the module and the fix; stderr:\n{stderr}"
    );
}
