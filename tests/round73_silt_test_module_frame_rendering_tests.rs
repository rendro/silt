//! Round-73 G1 lock: `silt test` setup-error call-stack rendering must
//! preserve `<module:...>` init frames just like `silt run` does.
//!
//! Bug the lock guards against: `vm/error.rs::render_call_stack` used to
//! drop every frame whose name starts with `<`, including the synthetic
//! `<module:foo>` init frames that mark the boundary between an imported
//! module's top-level statements and any user function it calls into.
//! `silt run` had its own copy of the filter that explicitly kept
//! `<module:...>` frames (`!name.starts_with('<') || name.starts_with("<module:")`);
//! `silt test` delegated to the shared helper and silently lost them.
//!
//! The fix updates the shared helper to keep `<module:...>` frames AND
//! has `silt run` delegate to the helper (instead of duplicating the
//! filter inline), so the two subcommands cannot drift again.
//!
//! Mutation reasoning: reverting the helper's filter back to
//! `!name.starts_with('<')` makes the `<module:` assertion below fail —
//! the rendered call-stack block no longer surfaces the module-init
//! frame that triggered the panic, so the user loses provenance for
//! "this top-level value initializer is what caused the crash, not a
//! call from main".

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

/// Build a self-contained tempdir for the test. Each call gets a unique
/// directory so parallel runs don't collide.
fn temp_dir(prefix: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("silt_round73_{prefix}_{n}"));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// `silt test` on a test file whose imported module crashes during
/// top-level initialization (`pub let value = helper.boom()` where
/// `boom` divides by zero) must emit a call-stack block that contains
/// the `<module:crasher>` init frame.
///
/// We deliberately route the panic through a helper function so the
/// post-filter stack has two meaningful frames (`<module:crasher>` +
/// `boom`) — enough to clear the `meaningful.len() >= 2` guard that
/// suppresses single-frame stacks. Without the round-73 fix, the
/// `<module:crasher>` frame is dropped and only `boom` remains, which
/// also drops below the threshold and the entire call-stack block is
/// suppressed — leaving the user with just the innermost div-by-zero
/// site and no path back to "this is happening because crasher's
/// top-level initializer ran".
#[test]
fn round73_silt_test_setup_error_keeps_module_init_frame() {
    let dir = temp_dir("module_init_frame");

    // Helper module: provides a function that panics. Defined in a
    // separate module so when `crasher`'s top-level calls it the call
    // stack records both the `<module:crasher>` init frame AND the
    // `boom` user-function frame.
    let helper_file = dir.join("helper.silt");
    fs::write(&helper_file, "pub fn boom() = 1 / 0\n").unwrap();

    // Crasher module: top-level binding that calls into the helper.
    // The crash happens *during* `<module:crasher>` initialization.
    let crasher_file = dir.join("crasher.silt");
    fs::write(
        &crasher_file,
        "import helper\npub let value = helper.boom()\n",
    )
    .unwrap();

    // Test file: importing `crasher` is enough to trigger the
    // top-level evaluation of crasher.silt during `silt test` setup.
    let test_file = dir.join("crasher_consumer_test.silt");
    fs::write(
        &test_file,
        "import crasher\n\
         import test\n\
         fn test_never_runs() {\n  \
           test.assert_eq(crasher.value, 0)\n\
         }\n",
    )
    .unwrap();

    let output = silt_cmd()
        .arg("test")
        .arg("crasher_consumer_test.silt")
        .current_dir(&dir)
        .output()
        .expect("failed to invoke silt");

    assert!(
        !output.status.success(),
        "expected setup error to exit non-zero, got status {:?}",
        output.status
    );
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Sanity: this is the setup-error path, not a per-test failure,
    // and the underlying VM error is the div-by-zero we expected.
    assert!(
        stderr.contains("setup error"),
        "expected 'setup error' marker in stderr, got: {stderr}"
    );
    assert!(
        stderr.contains("division by zero"),
        "expected 'division by zero' message in stderr, got: {stderr}"
    );

    // The call-stack block must be rendered (≥2 meaningful frames now
    // that `<module:crasher>` is preserved). Without the fix, only
    // `boom` survives the filter, the count drops to 1, and the entire
    // call-stack block is suppressed.
    assert!(
        stderr.contains("call stack:"),
        "expected 'call stack:' header in stderr — the block is being \
         suppressed because the <module:crasher> frame was filtered out, \
         dropping the meaningful-frame count below 2. Full stderr:\n{stderr}"
    );

    // The fix must surface the module-init frame in the rendered
    // stack. The literal `<module:crasher>` token is the canonical
    // signal — it tells the user "this came from initializing the
    // crasher module", which is otherwise indistinguishable from a
    // direct call into helper.boom from elsewhere.
    assert!(
        stderr.contains("<module:crasher>"),
        "expected '<module:crasher>' frame in the rendered call stack — \
         the module-init frame is being dropped by render_call_stack. \
         Full stderr:\n{stderr}"
    );

    // And the helper function we routed through should also appear,
    // confirming the stack contains both the module-init frame and
    // the user-function frame as expected.
    assert!(
        stderr.contains("boom"),
        "expected 'boom' frame in the rendered call stack, got: {stderr}"
    );
}
