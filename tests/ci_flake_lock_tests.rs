//! CI flake lock tests.
//!
//! These are source-grep regression locks against two narrow flakes that
//! recurred on GitHub-hosted runners and were fixed by the previous
//! commit. They do not exercise runtime behavior — they pin the exact
//! shape of the fix so a future refactor that strips the helper or
//! tightens the budget can't silently re-introduce the flake.
//!
//! Locks:
//! 1. tests/http_hardening_tests.rs — must contain a `connect_with_retry`
//!    helper (or equivalent retry-on-ConnectionRefused loop) at the
//!    test-side TCP connect site.
//! 2. tests/channel_timeout_tests.rs — harness timeout constant must be
//!    >= 8 seconds. Below that, Windows GitHub-hosted runners cold-start
//!    silt + scheduler slowly enough to flake.
//!
//! If you intentionally restructure either file, update the lock to
//! match the new shape; do not delete it.

use std::path::PathBuf;
use std::time::Duration;

fn read_test_file(name: &str) -> String {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("tests");
    path.push(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// http_hardening_tests.rs must keep the connect-retry helper and call
/// it from at least one test-side TCP connect site. Without it, the
/// brief race between `wait_for_bind`'s probe drop and the silt accept
/// loop reopening produced `ConnectionRefused` flakes on Linux CI.
#[test]
fn http_hardening_tests_define_connect_with_retry_helper() {
    let src = read_test_file("http_hardening_tests.rs");
    assert!(
        src.contains("fn connect_with_retry("),
        "tests/http_hardening_tests.rs must define `connect_with_retry` \
         to absorb the bind-probe -> real-connect race; the bare \
         TcpStream::connect form re-introduces the CI flake against \
         med1_handler_error_does_not_leak_vm_error_details",
    );
    assert!(
        src.contains("ConnectionRefused"),
        "tests/http_hardening_tests.rs `connect_with_retry` must treat \
         `ConnectionRefused` as transient; otherwise the retry loop \
         doesn't actually mask the flake it was added for",
    );
}

/// http_hardening_tests.rs must actually USE `connect_with_retry` at
/// the regressed test's connect site. Defining the helper without
/// calling it would silently regress the flake.
#[test]
fn http_hardening_tests_use_connect_with_retry_at_call_sites() {
    let src = read_test_file("http_hardening_tests.rs");
    // Strip the `fn connect_with_retry(` definition and count remaining
    // occurrences. >=1 means at least one test calls through it.
    let total = src.matches("connect_with_retry").count();
    // 1 occurrence in the helper signature, 1 in any doc comment that
    // mentions the helper name, plus one per call. Require >=3 so we
    // know there's at least one real call site.
    assert!(
        total >= 3,
        "tests/http_hardening_tests.rs mentions `connect_with_retry` \
         only {total} time(s); expected the helper definition plus at \
         least one call site",
    );
}

/// scheduler_deadlock_detector_tests.rs:
/// `test_main_watchdog_resets_when_channel_has_counterparty` must use
/// CI-aware iteration count + per-trial budget so CPU contention from
/// nextest's parallel test binaries doesn't trigger false-positive
/// deadlock reports against the strict 0/N assertion.
#[test]
fn test_main_watchdog_resets_when_channel_has_counterparty_is_ci_aware() {
    let src = read_test_file("scheduler_deadlock_detector_tests.rs");
    assert!(
        src.contains(r#"std::env::var("CI")"#) || src.contains(r#"env::var("CI")"#),
        "test_main_watchdog_resets_when_channel_has_counterparty must use \
         CI-aware iteration count to avoid CPU-contention flakes; if you \
         remove the `CI` env-var probe, the strict 0-false-positives \
         assertion will trip intermittently on Linux CI under nextest.",
    );
}

/// cross_module_inference_tests.rs `setup_dir` must canonicalize the
/// tempdir before returning it. On macOS `std::env::temp_dir()` lives
/// under `/var/folders/...` but `/var` is a symlink to `/private/var`;
/// without `.canonicalize()` the silt compiler's import resolution can
/// see paths that don't match what the test wrote, surfacing as
/// intermittent "undefined variable" errors when sibling-module files
/// like `a.silt` appear missing.
#[test]
fn cross_module_inference_setup_dir_canonicalizes_tempdir() {
    let src = read_test_file("cross_module_inference_tests.rs");
    assert!(
        src.contains(".canonicalize()"),
        "setup_dir must canonicalize the tempdir path so the silt \
         compiler's path resolution matches macOS symlink semantics; \
         dropping the canonicalize call re-introduces the macOS \
         intermittent flake where sibling-module imports fail with \
         'undefined variable' diagnostics.",
    );
}

/// channel_timeout_tests.rs harness budget must stay >= 8s. Windows
/// GitHub-hosted runners cold-start the in-process trial in ~2-4s and
/// the previous 2-second budget produced consistent timed-out flakes.
#[test]
fn channel_timeout_tests_harness_budget_at_least_8_seconds() {
    let src = read_test_file("channel_timeout_tests.rs");
    // Locate `const TEST_HARNESS_TIMEOUT: Duration = Duration::from_secs(N);`
    // and assert N >= 8. Done by string search rather than reflection
    // because we want the lock to fail loudly if the constant is
    // renamed or replaced with an inline literal.
    let needle = "const TEST_HARNESS_TIMEOUT: Duration = Duration::from_secs(";
    let start = src.find(needle).unwrap_or_else(|| {
        panic!(
            "tests/channel_timeout_tests.rs must declare \
             `const TEST_HARNESS_TIMEOUT: Duration = Duration::from_secs(N)` \
             with N >= 8; the previous 2-second budget flaked on Windows CI"
        )
    });
    let after = &src[start + needle.len()..];
    let end = after
        .find(')')
        .expect("malformed TEST_HARNESS_TIMEOUT declaration");
    let n: u64 = after[..end]
        .trim()
        .parse()
        .expect("TEST_HARNESS_TIMEOUT seconds must be an integer literal");
    assert!(
        Duration::from_secs(n) >= Duration::from_secs(8),
        "TEST_HARNESS_TIMEOUT regressed to {n}s; must stay >= 8s to \
         tolerate Windows GitHub-runner cold-start jitter",
    );
}
