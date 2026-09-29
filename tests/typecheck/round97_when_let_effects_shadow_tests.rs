//! Round 97 (BROKEN, soundness — module-shadowing hole in `when let`):
//! The `Stmt::When` arm of `stmt_effects` ignored the `when let`
//! pattern's binders. Unlike `Stmt::Let` (and match-arm / loop bindings,
//! all fixed in round 94), it never called `aliases.mark_pattern_rebound`
//! and never invalidated stale alias entries.
//!
//! Consequence: a `when let` binder that shadows a same-named imported
//! module was still treated as a live module qualifier. A later
//! `name.fn(...)` resolved through the MODULE's qualified scheme via
//! `resolvable_callee_name` and charged the module fn's declared effects
//! (e.g. `!{}`) instead of conservative higher-order TOP. A genuinely
//! effectful field fn could then slip past `--strict-effects`.
//!
//! Fix: mirror the `Stmt::Let` arm in `src/typechecker/effects_infer.rs`
//! — resolve the aliasing source of the scrutinee first, then
//! `mark_pattern_rebound(pattern)` and invalidate stale alias entries for
//! the binders so a later dotted call is charged TOP, not the module's
//! scheme.

use std::path::PathBuf;
use std::process::Command;

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
        "silt_r97_when_let_{label}_{}_{}",
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

/// A pure-annotated module fn whose name collides with a record field.
const OTHER_MODULE: (&str, &str) = ("other.silt", "pub fn double(x: Int) -> Int !{} { x * 2 }\n");

/// SOUNDNESS LOCK: a dotted CALL through a `when let` binder that shadows
/// an imported module must NOT silently adopt the same-named module fn's
/// (pure) declared effects. Phase A can't see through a fn-typed FIELD, so
/// the conservative answer is TOP — the unannotated fn fails
/// --strict-effects instead of letting a potentially effectful field fn
/// slip through as "pure like the module fn".
#[test]
fn strict_effects_when_let_shadowed_dotted_call_is_conservative_not_module_pure() {
    let dir = setup_dir(
        "when_let_shadowed_conservative",
        &[OTHER_MODULE],
        r#"
import other
type P { double: Fn(Int) -> Int }
fn opt(p: P) -> Result(P, String) { Ok(p) }
fn sneaky(p: P) -> Int {
  when let Ok(other) = opt(p) else { return 0 }
  other.double(3)
}
fn main() !{io} { println(sneaky(P { double: fn(x) { x + 1 } })) }
"#,
    );
    let (stdout, stderr, ok) = silt_in_dir(&dir, &["check", "--strict-effects"]);
    assert!(
        !ok,
        "shadowed `when let` dotted call must be charged conservatively (TOP), \
         not the module fn's !{{}}; stdout={stdout}, stderr={stderr}"
    );
    let combined = format!("{stdout}\n{stderr}");
    assert!(
        combined.contains("'sneaky'"),
        "the conservative charge should land on 'sneaky'; got {combined}"
    );
}

/// CONTROL: an UNSHADOWED `when let` followed by a real pure-annotated
/// module call must still pass --strict-effects. The fix must not start
/// over-charging an honest module call just because it appears after a
/// `when let` binding. Here `m` is the `when let` binder and `other` is
/// still the live module — its `!{}` scheme is correctly honored.
#[test]
fn strict_effects_when_let_unshadowed_module_call_still_pure() {
    let dir = setup_dir(
        "when_let_unshadowed_pure",
        &[OTHER_MODULE],
        r#"
import other
fn opt() -> Result(Int, String) { Ok(7) }
fn fine() -> Int {
  when let Ok(m) = opt() else { return 0 }
  m + other.double(3)
}
fn main() !{io} { println(fine()) }
"#,
    );
    let (stdout, stderr, ok) = silt_in_dir(&dir, &["check", "--strict-effects"]);
    assert!(
        ok,
        "unshadowed pure module call after a `when let` must pass --strict-effects; \
         stdout={stdout}, stderr={stderr}"
    );
}

/// CONTROL: the `when let` binder leaves scope correctly — a binder that
/// is purely a local value (no dotted module collision) must not perturb
/// effect inference; the program runs and type/effect-checks cleanly.
#[test]
fn when_let_local_binder_runs_and_checks_clean() {
    let dir = setup_dir(
        "when_let_local_ok",
        &[],
        r#"
fn opt(n: Int) -> Result(Int, String) { Ok(n) }
fn use_it(n: Int) -> Int {
  when let Ok(v) = opt(n) else { return -1 }
  v + 1
}
fn main() !{io} { println(use_it(41)) }
"#,
    );
    let (stdout, stderr, ok) = silt_in_dir(&dir, &["run", "--strict-effects"]);
    assert!(
        ok,
        "local `when let` binding must run and pass --strict-effects; \
         stdout={stdout}, stderr={stderr}"
    );
    assert_eq!(stdout.trim(), "42");
}
