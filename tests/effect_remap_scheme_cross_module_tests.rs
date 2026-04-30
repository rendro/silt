//! Round 64 GAP lock: cross-module remap_scheme effects propagation.
//!
//! Round 62 commit 5b3f240 fixed `remap_scheme` (in
//! `src/typechecker/mod.rs`) to carry `scheme.effects` through
//! cross-module imports rather than hardcoding `EffectSet::TOP`. The
//! pre-existing locks in `tests/effect_round62_audit_tests.rs`
//! (`narrowed_fn_preserves_declared_effects` and
//! `narrowed_fn_strict_effects_preserves_inferred`) place callee + caller
//! in a single source file, so they exercise the in-module narrowing
//! path (B4) only — never the cross-module remap path (B5). The round-62
//! commit body explicitly noted "cross-module fixture deferred".
//!
//! These tests close the gap: each fixture spans two files, so importing
//! `producer` into `main` forces the typechecker through `remap_scheme`.
//! With the fix in place a producer's `pub fn read_file() !{io, fs}` must
//! arrive at the consumer carrying `!{io, fs}` — not the audit's broken
//! `EffectSet::TOP` widening.
//!
//! Lock semantics:
//!   - Test 1 stages `pub fn read_file() !{io, fs}` in the producer and
//!     `fn main() !{io, fs}` in the consumer. With the fix, `silt check
//!     --strict-effects` succeeds. Reverting `effects: scheme.effects`
//!     to `EffectSet::TOP` at the remap site widens the imported
//!     callee's effects to all five; consumer's `!{io, fs}` no longer
//!     supersets the body, and a "not declared" error fires.
//!   - Test 2 narrows the consumer to `!{io}`. With the fix, the body
//!     correctly carries `!{io, fs}` from the producer and the
//!     diagnostic names `fs` (the missing effect) — locking that
//!     `remap_scheme` doesn't drop `fs` to nothing or balloon to TOP.
//!   - Tests 3 / 4 repeat with a three-effect producer (`!{io, fs, net}`)
//!     so a regression that re-introduces TOP couldn't accidentally
//!     pass by collapsing all three onto `!{io, fs}` shaped bitsets.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

/// Stage a fresh tempdir holding a `producer.silt` and `main.silt` pair.
/// Returns the path to `main.silt` so the caller can pass it to
/// `silt check`.
///
/// We model this on the temp-dir bookkeeping in
/// `tests/cli_strict_effects_flag_tests.rs` and
/// `tests/cross_module_inference_tests.rs`: build a process-unique +
/// counter-unique directory under `std::env::temp_dir()`, defensively
/// clean any stale state from a prior run, and write the two files
/// without any enclosing `silt.toml` (so the compiler's no-manifest
/// fallback resolves `import producer` against the sibling
/// `producer.silt` — see `src/cli/package.rs::fallback_package_setup`).
///
/// Canonicalization matters on macOS where `/var` is a symlink to
/// `/private/var`: silt's import resolution may canonicalize the dir
/// once and then look up paths under the canonical form, so we
/// pre-canonicalize here to avoid a flake.
fn stage_two_file_project(prefix: &str, producer: &str, main_src: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let raw_dir = std::env::temp_dir().join(format!(
        "silt_remap_scheme_xmod_{}_{}_{}",
        prefix,
        std::process::id(),
        n
    ));
    let _ = fs::remove_dir_all(&raw_dir);
    fs::create_dir_all(&raw_dir).expect("mkdir tempdir");
    let dir = raw_dir.canonicalize().expect("canonicalize tempdir");
    fs::write(dir.join("producer.silt"), producer).expect("write producer.silt");
    let main_path = dir.join("main.silt");
    fs::write(&main_path, main_src).expect("write main.silt");
    main_path
}

// ── 1. Cross-module !{io, fs} round-trip succeeds under strict ────────

#[test]
fn cross_module_io_fs_pub_fn_round_trips_through_remap_scheme() {
    // Producer declares `pub fn read_file() !{io, fs}`. Consumer
    // imports producer, declares `fn main() !{io, fs}`, and calls
    // `producer.read_file()`. With the round-62 fix to `remap_scheme`,
    // the imported callee's scheme arrives at the consumer carrying
    // `!{io, fs}` (not `EffectSet::TOP` — the broken widening shape).
    // The body's union is `!{io, fs}`, the consumer's declared bound
    // is `!{io, fs}`, subset check passes, `silt check
    // --strict-effects` exits 0.
    //
    // If `remap_scheme` regressed to hardcoding `EffectSet::TOP`, the
    // consumer's body would see the call as carrying all five effects;
    // `!{io, fs}` would not superset that, and the strict-mode check
    // would emit `effect '!{net}' not declared in fn 'main'` (or a
    // similar over-wide diagnostic). This assertion is the lock.
    let main_path = stage_two_file_project(
        "io_fs_ok",
        r#"pub fn read_file() !{io, fs} {
  42
}
"#,
        r#"import producer
fn main() !{io, fs} {
  producer.read_file()
}
"#,
    );

    let output = silt_cmd()
        .arg("check")
        .arg("--strict-effects")
        .arg(&main_path)
        .output()
        .expect("failed to run silt check --strict-effects");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "silt check --strict-effects rejected a !{{io, fs}} consumer of an \
         imported !{{io, fs}} producer fn — `remap_scheme` likely dropped \
         the producer's declared effects.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // Belt-and-suspenders: even on a zero exit, no `error[` markers
    // should appear in stderr. Catches a future regression where the
    // CLI starts surfacing typecheck errors as warnings without
    // flipping the exit status.
    assert!(
        !stderr.contains("error["),
        "stderr unexpectedly contains an `error[...]` marker even though \
         the process exited 0:\n{stderr}"
    );
}

// ── 2. Cross-module narrower consumer surfaces missing-effect ─────────

#[test]
fn cross_module_consumer_missing_fs_errors_naming_fs() {
    // Same producer declaring `!{io, fs}`. Consumer narrows to `!{io}`
    // (deliberately omits `fs`). The body's effects (post-remap) must
    // be `!{io, fs}`, so the consumer's `!{io}` declaration is
    // insufficient. `silt check --strict-effects` must error and the
    // diagnostic must name `fs` as the missing effect.
    //
    // This test is dual-purpose: it confirms (a) the cross-module
    // remap propagates `fs` through (a regression that dropped `fs`
    // entirely would let this test pass with no error), and (b) the
    // diagnostic remains targeted (a regression that widened to TOP
    // would name `net` or `random` instead of `fs`).
    let main_path = stage_two_file_project(
        "io_fs_narrowed_missing_fs",
        r#"pub fn read_file() !{io, fs} {
  42
}
"#,
        r#"import producer
fn main() !{io} {
  producer.read_file()
}
"#,
    );

    let output = silt_cmd()
        .arg("check")
        .arg("--strict-effects")
        .arg(&main_path)
        .output()
        .expect("failed to run silt check --strict-effects");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stderr}\n{stdout}");

    assert!(
        !output.status.success(),
        "silt check --strict-effects accepted a !{{io}} consumer of a \
         !{{io, fs}} producer fn — the missing `fs` effect was not \
         flagged.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        combined.contains("not declared") && combined.contains("'main'"),
        "expected `effect ... not declared in fn 'main'` diagnostic, got:\n{combined}"
    );
    // The targeted-effect lock: the diagnostic must name `fs` as the
    // missing effect. A regression that dropped to EMPTY would not
    // mention any effect; a regression that widened to TOP would name
    // `net` or `random` (the effects the producer never declared)
    // instead.
    assert!(
        combined.contains("fs"),
        "diagnostic must name `fs` as the missing effect, got:\n{combined}"
    );
    // Anti-lock: no spurious `net`/`random`/`time` mention should
    // appear in the diagnostic (those effects aren't on the producer
    // and shouldn't surface in a clean cross-module remap). The
    // pre-fix bug would widen to TOP and surface every missing
    // effect, often picking `net` as the first representative.
    assert!(
        !combined.contains("'!{net}'") && !combined.contains("'!{random}'"),
        "diagnostic mentions effects (`net`/`random`) that the producer \
         never declared — `remap_scheme` likely widened to TOP:\n{combined}"
    );
}

// ── 3. Cross-module three-effect !{io, fs, net} round-trip ────────────

#[test]
fn cross_module_io_fs_net_pub_fn_round_trips_through_remap_scheme() {
    // A three-effect producer guards against a regression that happens
    // to collapse-pass the two-effect case by accident. With
    // `!{io, fs, net}` declared on both sides, the strict check must
    // still pass — the remap must carry every declared effect across,
    // not silently coalesce them.
    let main_path = stage_two_file_project(
        "io_fs_net_ok",
        r#"pub fn read_file() !{io, fs, net} {
  42
}
"#,
        r#"import producer
fn main() !{io, fs, net} {
  producer.read_file()
}
"#,
    );

    let output = silt_cmd()
        .arg("check")
        .arg("--strict-effects")
        .arg(&main_path)
        .output()
        .expect("failed to run silt check --strict-effects");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "silt check --strict-effects rejected a !{{io, fs, net}} consumer \
         of an imported !{{io, fs, net}} producer fn — `remap_scheme` \
         likely dropped one or more declared effects.\nstdout:\n{stdout}\n\
         stderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("error["),
        "stderr unexpectedly contains an `error[...]` marker on a \
         success exit:\n{stderr}"
    );
}

// ── 4. Cross-module three-effect consumer missing `net` ───────────────

#[test]
fn cross_module_consumer_missing_net_errors_naming_net() {
    // Producer declares `!{io, fs, net}`; consumer drops `net` to
    // `!{io, fs}`. The strict check must error and name `net` as the
    // missing effect — a precise lock that the remap carries `net`
    // through without dropping it. A regression that widened to TOP
    // would name `random` or `time` first; a regression that dropped
    // effects would either pass silently or name a different effect.
    let main_path = stage_two_file_project(
        "io_fs_net_narrowed_missing_net",
        r#"pub fn read_file() !{io, fs, net} {
  42
}
"#,
        r#"import producer
fn main() !{io, fs} {
  producer.read_file()
}
"#,
    );

    let output = silt_cmd()
        .arg("check")
        .arg("--strict-effects")
        .arg(&main_path)
        .output()
        .expect("failed to run silt check --strict-effects");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stderr}\n{stdout}");

    assert!(
        !output.status.success(),
        "silt check --strict-effects accepted a !{{io, fs}} consumer of a \
         !{{io, fs, net}} producer fn — the missing `net` effect was not \
         flagged.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        combined.contains("not declared") && combined.contains("'main'"),
        "expected `effect ... not declared in fn 'main'` diagnostic, got:\n{combined}"
    );
    assert!(
        combined.contains("net"),
        "diagnostic must name `net` as the missing effect, got:\n{combined}"
    );
    // Anti-lock for the round-62 regression shape: a TOP widening
    // would also name `random`/`time` — neither belongs in the
    // diagnostic for this fixture.
    assert!(
        !combined.contains("'!{random}'") && !combined.contains("'!{time}'"),
        "diagnostic mentions effects (`random`/`time`) that the producer \
         never declared — `remap_scheme` likely widened to TOP:\n{combined}"
    );
}
