//! Lock tests for the `SILT_IO_POOL_SIZE` environment knob.
//!
//! Mirrors the `SILT_TIME_SLICE` / `SILT_IO_TIMEOUT` precedent: a
//! process-wide env var that overrides the default I/O worker count at
//! `Vm::new` time. These tests exist so a future refactor cannot
//! silently revert `Vm::new` to the historical hardcoded
//! `min(cores, 4)` form; if that happens, every "env var set →
//! resolved size matches" case below will fail.
//!
//! Gated on the `test-hooks` Cargo feature so the introspection helpers
//! (`silt::vm::io_pool_worker_count`, `resolve_io_pool_size`,
//! `io_pool_size_cap`, `default_io_pool_size`) are exposed. Run with:
//!
//! ```sh
//! cargo test --features test-hooks --test io_pool_size_env_knob_tests
//! ```
//!
//! Without `test-hooks`, the file compiles to an empty crate (no tests).

#![cfg(feature = "test-hooks")]

use silt::vm::{
    Vm, default_io_pool_size, io_pool_size_cap, io_pool_worker_count, resolve_io_pool_size,
};

/// Serialize env-var manipulation across tests. `cargo test` runs
/// integration tests in parallel by default, and `SILT_IO_POOL_SIZE` is
/// process-global; without this lock two tests can stomp on each other
/// and observe a transient value the test that *set* it didn't intend.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// RAII guard: takes the global env lock and clears `SILT_IO_POOL_SIZE`
/// on construction and on drop. Tests that need to set the var do so
/// after acquiring the guard; the drop ensures even a panicking test
/// can't leak a stale value to its neighbours.
struct EnvGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl EnvGuard {
    fn acquire() -> Self {
        let lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: matches the convention in
        // src/scheduler.rs:1830 — Rust 1.80+ uses thread-local env
        // caches and these tests are serialized by ENV_LOCK so no
        // concurrent reader observes the transient remove.
        unsafe { std::env::remove_var("SILT_IO_POOL_SIZE") };
        EnvGuard { _lock: lock }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: see EnvGuard::acquire.
        unsafe { std::env::remove_var("SILT_IO_POOL_SIZE") };
    }
}

#[test]
fn silt_io_pool_size_env_var_overrides_default_to_8() {
    let _g = EnvGuard::acquire();
    // SAFETY: env-var mutation is serialized by ENV_LOCK; see
    // EnvGuard::acquire.
    unsafe { std::env::set_var("SILT_IO_POOL_SIZE", "8") };
    assert_eq!(resolve_io_pool_size(), 8);

    // End-to-end: the constructed Vm's I/O pool reflects the env var.
    let vm = Vm::new();
    assert_eq!(
        io_pool_worker_count(&vm),
        8,
        "Vm::new must read SILT_IO_POOL_SIZE — if this fails, someone \
         likely reverted Vm::new to the hardcoded min(cores, 4) form"
    );
}

#[test]
fn silt_io_pool_size_zero_falls_back_to_default() {
    let _g = EnvGuard::acquire();
    unsafe { std::env::set_var("SILT_IO_POOL_SIZE", "0") };
    assert_eq!(resolve_io_pool_size(), default_io_pool_size());

    let vm = Vm::new();
    assert_eq!(io_pool_worker_count(&vm), default_io_pool_size());
}

#[test]
fn silt_io_pool_size_invalid_falls_back_to_default() {
    let _g = EnvGuard::acquire();
    unsafe { std::env::set_var("SILT_IO_POOL_SIZE", "abc") };
    assert_eq!(resolve_io_pool_size(), default_io_pool_size());

    let vm = Vm::new();
    assert_eq!(io_pool_worker_count(&vm), default_io_pool_size());
}

#[test]
fn silt_io_pool_size_caps_at_upper_bound() {
    let _g = EnvGuard::acquire();
    unsafe { std::env::set_var("SILT_IO_POOL_SIZE", "100000") };
    let cap = io_pool_size_cap();
    assert_eq!(resolve_io_pool_size(), cap);

    let vm = Vm::new();
    assert_eq!(
        io_pool_worker_count(&vm),
        cap,
        "huge SILT_IO_POOL_SIZE must clamp at the documented cap"
    );
}

#[test]
fn silt_io_pool_size_unset_uses_min_cores_4_default() {
    let _g = EnvGuard::acquire();
    // EnvGuard::acquire already removed the var; assert the unset path.
    let resolved = resolve_io_pool_size();
    let default = default_io_pool_size();
    assert_eq!(
        resolved, default,
        "unset env var must resolve to default_io_pool_size()"
    );
    // Document the default's shape: min(available_parallelism, 4); the
    // fallback 2 applies only when available_parallelism() errors, NOT
    // as a floor — a single-core host legitimately yields 1.
    assert!(
        (1..=4).contains(&default),
        "default_io_pool_size() out of expected [1, 4]: {default}"
    );
}

#[test]
fn default_io_pool_remains_min_cores_4_when_env_var_absent() {
    // Regression lock: this is the test that fails if a future change
    // makes `resolve_io_pool_size` always read the env var path even
    // when the var is unset. The pre-knob behaviour must be preserved
    // bit-for-bit.
    let _g = EnvGuard::acquire();
    // No set_var here — the guard removed the var on entry.
    let vm = Vm::new();
    let count = io_pool_worker_count(&vm);
    let expected = default_io_pool_size();
    assert_eq!(
        count, expected,
        "absent SILT_IO_POOL_SIZE must produce the historical \
         min(cores, 4) sizing — if this fails, the env-knob default \
         path drifted"
    );
}

#[test]
fn silt_io_pool_size_at_cap_returns_cap() {
    let _g = EnvGuard::acquire();
    let cap = io_pool_size_cap();
    unsafe { std::env::set_var("SILT_IO_POOL_SIZE", &cap.to_string()) };
    assert_eq!(resolve_io_pool_size(), cap);
}

#[test]
fn silt_io_pool_size_one_is_honored() {
    // A user can legitimately want a single I/O thread (e.g. to
    // serialize blocking ops in a constrained environment). 1 is a
    // valid usize > 0 and below the cap, so it must pass through.
    let _g = EnvGuard::acquire();
    unsafe { std::env::set_var("SILT_IO_POOL_SIZE", "1") };
    assert_eq!(resolve_io_pool_size(), 1);

    let vm = Vm::new();
    assert_eq!(io_pool_worker_count(&vm), 1);
}
