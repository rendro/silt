//! Round 94 (BROKEN): module names took precedence over local bindings
//! in dotted-name resolution.
//!
//! When an identifier was BOTH a local binding (fn param, lambda param,
//! let, pattern binder) AND an imported module name, `x.member`
//! resolved to the MODULE, hijacking field access on the local:
//!
//!   1. USER PARAM: `import other` + `fn f(other: P) { other.year }`
//!      → `error[type]: unknown function 'year' on module 'other'`.
//!   2. BUILTIN MODULE: `import list` + `fn f(list: P) { list.map }`
//!      (P has field `map: Int`) → resolved to builtin `list.map`'s fn
//!      type → bogus type mismatch.
//!   3. SYNTHESIZED DERIVES: builtin auto-derive bodies use a parameter
//!      literally named `other` (`fn compare(self, other)` /
//!      `fn equal(self, other)` in `builtin_trait_decls`), so importing
//!      ANY user module named `other` broke `==` / `<` on EVERY record
//!      and on builtin Date comparisons.
//!
//! Required semantics: lexical shadowing. A value binding shadows a
//! same-named imported module within its scope; at top level with no
//! binding in scope, `other.double(21)` stays a module call.
//!
//! Fixes under test:
//!   * typechecker: `TypeChecker::value_binding_shadows_module` gates
//!     the FieldAccess module-resolution block, the Call arm's
//!     module-call detection and `callee_module_is_in_scope`
//!     (src/typechecker/inference.rs);
//!   * qualified type paths: a shadowed qualifier in record literals /
//!     variant patterns errors clearly instead of silently picking the
//!     module (`lookup_qualified_record` /
//!     `resolve_pattern_ctor_qualifier`);
//!   * compiler: top-level `let` binders are tracked in
//!     `top_level_value_globals` so dotted access on them compiles as
//!     field access, matching the typechecker (locals/upvalues were
//!     already resolved first by codegen);
//!   * effects walker: a shadowed/rebound dotted-callee base no longer
//!     charges the same-named MODULE fn's declared effects
//!     (src/typechecker/effects_infer.rs).

//!
//! Only the `--strict-effects` cases remain here; they are listed for
//! stage 4 (effect annotations are removed). Every other case is a golden
//! case `tests/golden/lang/modules/round94_module_shadowing__*`.

use std::path::PathBuf;
use std::process::Command;

// ── Helpers ─────────────────────────────────────────────────────────

fn rand_u64() -> u64 {
    use std::time::SystemTime;
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}

/// Create a fresh tempdir holding the supplied module files plus a
/// `main.silt` containing `main_source`. Returns the dir path.
fn setup_dir(label: &str, files: &[(&str, &str)], main_source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "silt_r94_shadow_{label}_{}_{}",
        std::process::id(),
        rand_u64()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    for (name, content) in files {
        std::fs::write(dir.join(name), content).expect("write module");
    }
    std::fs::write(dir.join("main.silt"), main_source).expect("write main");
    dir
}

/// Run `silt <args> main.silt` in `dir`; return (stdout, stderr, ok).
fn silt_in_dir(dir: &PathBuf, args: &[&str]) -> (String, String, bool) {
    let bin = env!("CARGO_BIN_EXE_silt");
    let out = Command::new(bin)
        .args(args)
        .arg(dir.join("main.silt"))
        .output()
        .expect("spawn silt");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

// ── Effects ─────────────────────────────────────────────────────────

/// A pure fn whose param shadows the builtin `io` module: pure FIELD
/// access through the shadowed name must not charge io effects under
/// --strict-effects. Before the fix this errored at typecheck time
/// ("unknown function 'size' on module 'io'").
#[test]
fn strict_effects_shadowed_io_param_field_access_is_pure() {
    let dir = setup_dir(
        "strict_pure",
        &[],
        r#"
import io
type Wrapper { size: Int }
fn size_of(io: Wrapper) -> Int { io.size }
fn main() !{io} { println(size_of(Wrapper { size: 3 })) }
"#,
    );
    let (stdout, stderr, ok) = silt_in_dir(&dir, &["run", "--strict-effects"]);
    assert!(
        ok,
        "pure field access through a shadowed module name must pass --strict-effects; stdout={stdout}, stderr={stderr}"
    );
    assert_eq!(stdout.trim(), "3");
}

/// Vice versa: when NOT shadowed, a real module call's effects must
/// still be charged under --strict-effects.
#[test]
fn strict_effects_unshadowed_module_call_still_charged() {
    let dir = setup_dir(
        "strict_charged",
        &[],
        r#"
import io
fn bad() -> Result(String, IoError) { io.read_line() }
fn main() !{io} {
  match bad() {
    Ok(s) -> println(s),
    Err(_) -> println("err")
  }
}
"#,
    );
    let (stdout, stderr, ok) = silt_in_dir(&dir, &["check", "--strict-effects"]);
    assert!(
        !ok,
        "unshadowed io call in an unannotated fn must fail --strict-effects; stdout={stdout}, stderr={stderr}"
    );
    let combined = format!("{stdout}\n{stderr}");
    assert!(
        combined.contains("io"),
        "diagnostic should mention the io effect; got {combined}"
    );
}

/// Effects walker soundness lock: a dotted CALL through a SHADOWING
/// param must NOT silently adopt the same-named module fn's (pure)
/// declared effects. Phase A can't see through a fn-typed FIELD, so
/// the conservative answer is TOP — the unannotated fn fails
/// --strict-effects instead of letting a potentially effectful field
/// fn slip through as "pure like the module fn".
#[test]
fn strict_effects_shadowed_dotted_call_is_conservative_not_module_pure() {
    let module = ("other.silt", "pub fn double(x: Int) -> Int !{} { x * 2 }\n");
    // Unshadowed baseline: calling the pure-annotated module fn from an
    // unannotated fn passes --strict-effects (module effects honored).
    let dir_ok = setup_dir(
        "strict_unshadowed_pure",
        &[module],
        r#"
import other
fn fine() -> Int { other.double(3) }
fn main() !{io} { println(fine()) }
"#,
    );
    let (stdout, stderr, ok) = silt_in_dir(&dir_ok, &["check", "--strict-effects"]);
    assert!(
        ok,
        "unshadowed pure module call must pass --strict-effects; stdout={stdout}, stderr={stderr}"
    );
    // Shadowed: same dotted spelling, but `other` is a param whose
    // field is a fn value — the module's `!{}` must not transfer.
    let dir_bad = setup_dir(
        "strict_shadowed_conservative",
        &[module],
        r#"
import other
type P { double: Fn(Int) -> Int }
fn sneaky(other: P) -> Int { other.double(3) }
fn main() !{io} { println(sneaky(P { double: fn(x) { x + 1 } })) }
"#,
    );
    let (stdout, stderr, ok) = silt_in_dir(&dir_bad, &["check", "--strict-effects"]);
    assert!(
        !ok,
        "shadowed dotted call must be charged conservatively (TOP), not the module fn's !{{}}; stdout={stdout}, stderr={stderr}"
    );
    let combined = format!("{stdout}\n{stderr}");
    assert!(
        combined.contains("'sneaky'"),
        "the conservative charge should land on 'sneaky'; got {combined}"
    );
}
