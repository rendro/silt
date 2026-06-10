//! Round 93 (fix G1): behavioral lock for the enriched
//! "cannot load module" diagnostic.
//!
//! PRE-fix, a failed `import nosuchmod` surfaced only the raw OS error:
//!
//!   cannot load module 'nosuchmod': No such file or directory (os error 2)
//!
//! — no mention of WHICH path was attempted (`nosuchmod.silt` /
//! `src/nosuchmod.silt`), no did-you-mean against near-miss sibling
//! `.silt` files, and no pointer at the `silt.toml [dependencies]` /
//! `silt add` channel, the other way an import can resolve.
//!
//! POST-fix (`module::format_module_load_error`, called from the two
//! read_to_string sites in src/compiler/mod.rs) the diagnostic carries:
//!   = note: looked for `<attempted path>`
//!   = help: did you mean `<sibling>`? ...        (when a near-miss exists)
//!   = help: if '<name>' is a separate package, declare it under
//!           `[dependencies]` in silt.toml (e.g. `silt add <name>`)
//!
//! Each test runs the compiled `silt` binary against a fresh temp dir.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Unique temp dir per call so parallel tests never share state.
fn temp_dir(prefix: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("silt_round93_load_hint_{prefix}_{pid}_{n}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Run `silt <subcommand> <file>` with `cwd` as the working directory
/// (paths in diagnostics render CWD-relative).
fn silt_in(cwd: &Path, subcommand: &str, file: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg(subcommand)
        .arg(file)
        .current_dir(cwd)
        .output()
        .expect("failed to spawn silt binary")
}

fn stderr_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

// ── Attempted path + dependency-channel help ────────────────────────

#[test]
fn missing_module_shows_attempted_path_and_dependency_help() {
    let dir = temp_dir("adhoc");
    fs::write(
        dir.join("t.silt"),
        "import nosuchmod\n\nfn main() {\n  println(\"x\")\n}\n",
    )
    .unwrap();

    let out = silt_in(&dir, "run", "t.silt");
    let stderr = stderr_of(&out);

    assert!(
        !out.status.success(),
        "run must fail on a missing module, stderr:\n{stderr}"
    );
    // First line keeps the historical shape (locked elsewhere too).
    assert!(
        stderr.contains("cannot load module 'nosuchmod'"),
        "expected the historical first line, got:\n{stderr}"
    );
    // The attempted path is now named (ad-hoc script → sibling file).
    assert!(
        stderr.contains("looked for") && stderr.contains("nosuchmod.silt"),
        "expected `looked for ... nosuchmod.silt` note, got:\n{stderr}"
    );
    // The manifest dependency channel is mentioned with the add command.
    assert!(
        stderr.contains("[dependencies]") && stderr.contains("silt add nosuchmod"),
        "expected the `[dependencies]` / `silt add` help, got:\n{stderr}"
    );
}

#[test]
fn missing_module_inside_package_names_src_relative_path() {
    let dir = temp_dir("pkg");
    fs::write(
        dir.join("silt.toml"),
        "[package]\nname = \"hintpkg\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("src").join("main.silt"),
        "import nosuchmod\n\nfn main() {\n  println(\"x\")\n}\n",
    )
    .unwrap();

    let out = silt_in(&dir, "run", "src/main.silt");
    let stderr = stderr_of(&out);

    assert!(!out.status.success(), "run must fail, stderr:\n{stderr}");
    assert!(
        stderr.contains("cannot load module 'nosuchmod'"),
        "expected the load error, got:\n{stderr}"
    );
    // Inside a package, imports resolve against `<root>/src`, and the
    // note should render that path CWD-relative.
    assert!(
        stderr.contains("looked for")
            && stderr.contains(&format!("src{}nosuchmod.silt", std::path::MAIN_SEPARATOR)),
        "expected `looked for ... src/nosuchmod.silt`, got:\n{stderr}"
    );
}

// ── Did-you-mean against sibling .silt files ────────────────────────

#[test]
fn near_miss_sibling_module_gets_did_you_mean() {
    let dir = temp_dir("nearmiss");
    fs::write(dir.join("util.silt"), "pub fn answer() = 41\n").unwrap();
    fs::write(
        dir.join("t.silt"),
        "import utl\n\nfn main() {\n  println(utl.answer())\n}\n",
    )
    .unwrap();

    let out = silt_in(&dir, "run", "t.silt");
    let stderr = stderr_of(&out);

    assert!(!out.status.success(), "run must fail, stderr:\n{stderr}");
    assert!(
        stderr.contains("cannot load module 'utl'"),
        "expected the load error for the typo'd name, got:\n{stderr}"
    );
    assert!(
        stderr.contains("did you mean `util`?"),
        "expected a did-you-mean hint for the sibling util.silt, got:\n{stderr}"
    );
}

#[test]
fn no_did_you_mean_when_no_sibling_is_close() {
    let dir = temp_dir("nosibling");
    fs::write(dir.join("completely_unrelated.silt"), "pub fn f() = 1\n").unwrap();
    fs::write(
        dir.join("t.silt"),
        "import zzz\n\nfn main() {\n  println(\"x\")\n}\n",
    )
    .unwrap();

    let out = silt_in(&dir, "run", "t.silt");
    let stderr = stderr_of(&out);

    assert!(!out.status.success(), "run must fail, stderr:\n{stderr}");
    assert!(
        !stderr.contains("did you mean"),
        "no sibling is anywhere close to `zzz`; got spurious hint:\n{stderr}"
    );
    // The dependency-channel help still appears.
    assert!(
        stderr.contains("[dependencies]"),
        "expected the dependency-channel help, got:\n{stderr}"
    );
}

// ── Valid imports unaffected ────────────────────────────────────────

#[test]
fn valid_import_still_runs_clean() {
    let dir = temp_dir("valid");
    fs::write(dir.join("util.silt"), "pub fn answer() = 41\n").unwrap();
    fs::write(
        dir.join("t.silt"),
        "import util\n\nfn main() {\n  println(util.answer() + 1)\n}\n",
    )
    .unwrap();

    let out = silt_in(&dir, "run", "t.silt");
    let stderr = stderr_of(&out);
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        out.status.success(),
        "valid import must run clean; stderr:\n{stderr}"
    );
    assert!(
        stdout.contains("42"),
        "expected program output `42`, got:\n{stdout}"
    );
    assert!(
        !stderr.contains("cannot load module") && !stderr.contains("did you mean"),
        "no load diagnostics expected on the happy path, got:\n{stderr}"
    );
}
