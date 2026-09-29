//! Round-75 DX-5 GAP regression tests: `silt init` must reject package
//! names that collide with a builtin module (`io`, `string`, `http`, …).
//!
//! Background: `mkdir io && cd io && silt init` previously silently
//! produced a `silt.toml` whose `name = "io"` shadowed the stdlib `io`
//! module. Sibling `silt add` already correctly rejected
//! builtin-colliding dep names via `silt::module::is_builtin_module` (see
//! `src/cli/add.rs:273-279`), so init was the outlier.
//!
//! Fix: a single `validate_package_name` lives in `src/manifest.rs` and
//! is consulted by:
//!   - `silt::cli::init` before writing `silt.toml`
//!   - `silt::manifest::Manifest::load` (rejects on-disk manifests with
//!     builtin-colliding package names too)
//! so the rule is enforced at every entry point — one way to do things.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

/// Create a fresh empty temp directory whose final segment is exactly
/// `name`. The dirname matters because `silt init` derives the package
/// name from it; we keep the per-test scratch root distinct via a counter
/// so we can reuse a builtin-colliding name like `io` across tests.
fn temp_dir_named(name: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let root = std::env::temp_dir().join(format!("silt_round75_init_collision_{n}"));
    let dir = root.join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

// ─── 1. Source-grep lock ────────────────────────────────────────────────
//
// Pin that the package-name validator in `src/manifest.rs` consults
// `is_builtin_module`. The audit's contract: if anyone removes the
// builtin-collision check from the manifest module, this test screams.

#[test]
fn round75_manifest_validator_consults_is_builtin_module() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/manifest.rs");
    let src = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));

    // Two assertions:
    //   (a) `validate_package_name` exists.
    //   (b) somewhere in manifest.rs, `is_builtin_module` is called
    //       (the validator delegates here, and Manifest::load also
    //       reuses the same predicate for the on-disk path).
    assert!(
        src.contains("pub fn validate_package_name"),
        "src/manifest.rs is missing pub fn validate_package_name — the \
         single source of truth for package-name acceptance \
         (round-75 DX-5 GAP fix)."
    );
    assert!(
        src.contains("is_builtin_module"),
        "src/manifest.rs's package-name validator must consult \
         is_builtin_module so the builtin-collision check matches the \
         one silt add already enforces (round-75 DX-5 GAP fix)."
    );
}

// ─── 2. Behavioral: dir named `io` is rejected ──────────────────────────

#[test]
fn round75_init_in_dir_named_io_is_rejected() {
    let dir = temp_dir_named("io");
    let out = silt_cmd()
        .arg("init")
        .current_dir(&dir)
        .output()
        .expect("failed to invoke silt init");
    assert!(
        !out.status.success(),
        "silt init should refuse a package whose name collides with builtin `io`; \
         stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("io"),
        "stderr should name the offending package name `io`: {stderr}"
    );
    assert!(
        stderr.contains("builtin module"),
        "stderr should describe the collision as a builtin module clash, \
         mirroring `silt add`'s wording: {stderr}"
    );
    assert!(
        stderr.contains("pick a different name"),
        "stderr should suggest the user pick a different name (canonical \
         shape from src/cli/add.rs:273-279): {stderr}"
    );
    // No partial state left behind: neither manifest nor src/ should exist.
    assert!(
        !dir.join("silt.toml").exists(),
        "silt.toml must not be written when init refuses",
    );
    assert!(
        !dir.join("src").exists(),
        "src/ must not be created when init refuses",
    );
}

// ─── 3. Behavioral: dir named `http` is rejected (a different builtin) ──

#[test]
fn round75_init_in_dir_named_http_is_rejected() {
    let dir = temp_dir_named("http");
    let out = silt_cmd()
        .arg("init")
        .current_dir(&dir)
        .output()
        .expect("failed to invoke silt init");
    assert!(
        !out.status.success(),
        "silt init should refuse a package whose name collides with builtin `http`; \
         stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("http"),
        "stderr should name the offending package name `http`: {stderr}"
    );
    assert!(
        stderr.contains("builtin module"),
        "stderr should describe the collision as a builtin module clash: {stderr}"
    );
}

// ─── 4. Behavioral: every builtin module name is rejected ──────────────
//
// Drives the end-to-end CLI for every name in `BUILTIN_MODULES`. If
// someone adds a new builtin without re-running the audit, this catches
// the gap automatically (init starts producing colliding manifests for
// the new name and this test fires).

#[test]
fn round75_init_rejects_every_builtin_module_name() {
    for name in silt::module::BUILTIN_MODULES {
        let dir = temp_dir_named(name);
        let out = silt_cmd()
            .arg("init")
            .current_dir(&dir)
            .output()
            .expect("failed to invoke silt init");
        assert!(
            !out.status.success(),
            "silt init must refuse builtin-colliding name `{name}` \
             (stdout={}, stderr={})",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("builtin module"),
            "stderr for `{name}` should describe a builtin-module collision: {stderr}"
        );
        assert!(
            !dir.join("silt.toml").exists(),
            "silt.toml must not be written when init refuses (`{name}`)"
        );
    }
}

// ─── 5. Negative: non-builtin name succeeds (no false positive) ────────

#[test]
fn round75_init_in_dir_named_mypkg_succeeds() {
    let dir = temp_dir_named("mypkg");
    let out = silt_cmd()
        .arg("init")
        .current_dir(&dir)
        .output()
        .expect("failed to invoke silt init");
    assert!(
        out.status.success(),
        "silt init should succeed for non-builtin name `mypkg`; \
         stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let manifest_path = dir.join("silt.toml");
    assert!(
        manifest_path.is_file(),
        "silt.toml should exist after successful init"
    );
    let manifest = fs::read_to_string(&manifest_path).unwrap();
    assert!(
        manifest.contains("name = \"mypkg\""),
        "manifest should contain non-colliding name `mypkg`: {manifest}"
    );
}

// ─── 6. Manifest loader also rejects builtin-colliding package names ───
//
// Belt-and-suspenders: even if a user hand-edits `silt.toml` to set
// `name = "io"` (bypassing init), the manifest loader rejects it. This
// pins that the validation is enforced at *every* entry point.

#[test]
fn round75_manifest_load_rejects_builtin_package_name() {
    let dir = temp_dir_named("hand_edited");
    let manifest_path = dir.join("silt.toml");
    fs::write(
        &manifest_path,
        "[package]\nname = \"io\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    let err = silt::manifest::Manifest::load(&manifest_path)
        .expect_err("Manifest::load must reject builtin-colliding package name `io`");
    let msg = format!("{err}");
    assert!(
        msg.contains("io"),
        "error should name the offending package: {msg}"
    );
    assert!(
        msg.contains("builtin module"),
        "error should describe a builtin-module collision: {msg}"
    );
    assert!(
        msg.contains("pick a different name"),
        "error should suggest picking a different name (canonical shape): {msg}"
    );
}

// ─── 7. validate_package_name unit-level acceptance ────────────────────
//
// Round-trip the public API directly, so we have a fast-feedback shape
// guard independent of the CLI process.

#[test]
fn round75_validate_package_name_rejects_builtins_accepts_others() {
    // Every BUILTIN_MODULES entry must be rejected.
    for name in silt::module::BUILTIN_MODULES {
        let result = silt::manifest::validate_package_name(name);
        assert!(
            result.is_err(),
            "validate_package_name should reject builtin `{name}`, got Ok"
        );
        let msg = result.unwrap_err();
        assert!(
            msg.contains("builtin module"),
            "rejection for `{name}` should describe a builtin-module collision: {msg}"
        );
    }

    // Every reserved keyword must be rejected too: a package named
    // after a keyword lexes as that keyword, so `import loop` can never
    // parse. Same import-shadowing footgun as the builtin collision.
    for name in silt::lexer::KEYWORDS
        .iter()
        .chain(silt::lexer::KEYWORD_LITERALS.iter())
    {
        let result = silt::manifest::validate_package_name(name);
        assert!(
            result.is_err(),
            "validate_package_name should reject reserved keyword `{name}`, got Ok"
        );
        let msg = result.unwrap_err();
        assert!(
            msg.contains("reserved silt keyword"),
            "rejection for keyword `{name}` should describe a keyword collision: {msg}"
        );
        assert!(
            msg.contains("pick a different name"),
            "rejection for keyword `{name}` should suggest picking a different name: {msg}"
        );
    }

    // A handful of non-builtin, non-keyword identifiers are accepted.
    for name in ["mypkg", "my_app", "calc", "_priv", "foo123"] {
        let result = silt::manifest::validate_package_name(name);
        assert!(
            result.is_ok(),
            "validate_package_name should accept non-builtin `{name}`, got {result:?}"
        );
    }

    // Identifier-rule failures are rejected too (the "invalid name"
    // arm), so init/add can lean on this one helper for both shape and
    // collision checks.
    for bad in ["Foo", "foo-bar", "1foo", ""] {
        let result = silt::manifest::validate_package_name(bad);
        assert!(
            result.is_err(),
            "validate_package_name should reject malformed name `{bad}`, got Ok"
        );
    }
}

// ─── 8. reserved-keyword GAP: `silt init` in a keyword-named dir ───────
//
// A directory named after a silt keyword (e.g. `loop`) would previously
// produce a `silt.toml` whose `name = "loop"`. The package runs as a
// single file, but `import loop` can never parse: `loop` lexes as a
// keyword token, not an `Ident`. Same import-shadowing footgun the
// builtin-collision check above guards against, now extended to keywords.

#[test]
fn keyword_init_in_dir_named_loop_is_rejected() {
    let dir = temp_dir_named("loop");
    let out = silt_cmd()
        .arg("init")
        .current_dir(&dir)
        .output()
        .expect("failed to invoke silt init");
    assert!(
        !out.status.success(),
        "silt init should refuse a package whose name is the reserved keyword `loop`; \
         stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("loop"),
        "stderr should name the offending package name `loop`: {stderr}"
    );
    assert!(
        stderr.contains("reserved silt keyword"),
        "stderr should describe the collision as a reserved-keyword clash: {stderr}"
    );
    assert!(
        stderr.contains("pick a different name"),
        "stderr should suggest picking a different name (canonical shape): {stderr}"
    );
    // No partial state left behind.
    assert!(
        !dir.join("silt.toml").exists(),
        "silt.toml must not be written when init refuses a keyword name"
    );
    assert!(
        !dir.join("src").exists(),
        "src/ must not be created when init refuses a keyword name"
    );
}

// ─── 9. reserved-keyword GAP: every keyword name is rejected ───────────

#[test]
fn keyword_init_rejects_every_reserved_keyword() {
    for name in silt::lexer::KEYWORDS
        .iter()
        .chain(silt::lexer::KEYWORD_LITERALS.iter())
    {
        let dir = temp_dir_named(name);
        let out = silt_cmd()
            .arg("init")
            .current_dir(&dir)
            .output()
            .expect("failed to invoke silt init");
        assert!(
            !out.status.success(),
            "silt init must refuse reserved-keyword name `{name}` \
             (stdout={}, stderr={})",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("reserved silt keyword"),
            "stderr for `{name}` should describe a reserved-keyword collision: {stderr}"
        );
        assert!(
            !dir.join("silt.toml").exists(),
            "silt.toml must not be written when init refuses (`{name}`)"
        );
    }
}
