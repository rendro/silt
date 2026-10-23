//! End-to-end tests for `silt remove <name>`: the dependency leaves
//! silt.toml (whose other text is kept) and silt.lock (with what only
//! it needed), and the mistakes are refused with nothing changed.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

fn fresh_workspace(prefix: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("silt_remove_tests_{prefix}_{n}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A package at `dir` with `manifest` as its silt.toml and the source
/// file `file` (`main.silt` or `lib.silt`).
fn write_package(dir: &Path, manifest: &str, file: &str, body: &str) {
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(dir.join("silt.toml"), manifest).unwrap();
    fs::write(dir.join("src").join(file), body).unwrap();
}

fn silt(dir: &Path, args: &[&str]) -> Output {
    silt_cmd().args(args).current_dir(dir).output().unwrap()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// The names of a lockfile's packages, in its order.
fn locked_names(lock: &str) -> Vec<String> {
    lock.lines()
        .filter_map(|line| line.strip_prefix("name = \""))
        .map(|rest| rest.trim_end_matches('"').to_string())
        .collect()
}

/// app depends on `calc` and `util`; `calc` depends on `deep`, which
/// nothing else needs.
fn workspace_with_two_deps(prefix: &str) -> (PathBuf, PathBuf) {
    let ws = fresh_workspace(prefix);
    let app = ws.join("app");
    write_package(
        &ws.join("deep"),
        "[package]\nname = \"deep\"\nversion = \"0.1.0\"\n",
        "lib.silt",
        "pub fn one() { 1 }\n",
    );
    write_package(
        &ws.join("calc"),
        "[package]\nname = \"calc\"\nversion = \"0.1.0\"\n\n[dependencies]\ndeep = { path = \"../deep\" }\n",
        "lib.silt",
        "import deep\npub fn two() { deep.one() + 1 }\n",
    );
    write_package(
        &ws.join("util"),
        "[package]\nname = \"util\"\nversion = \"0.1.0\"\n",
        "lib.silt",
        "pub fn three() { 3 }\n",
    );
    write_package(
        &app,
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         # the deps of the app\n[dependencies]\n\
         calc = { path = \"../calc\" } # math\n\
         util = { path = \"../util\" }\n",
        "main.silt",
        "import util\nfn main() { println(util.three()) }\n",
    );
    let out = silt(&app, &["run"]);
    assert!(out.status.success(), "setup run failed: {}", stderr(&out));
    (ws, app)
}

#[test]
fn test_remove_drops_the_dependency_from_silt_toml_and_silt_lock() {
    let (ws, app) = workspace_with_two_deps("drops");
    let lock_before = fs::read_to_string(app.join("silt.lock")).unwrap();
    assert_eq!(locked_names(&lock_before), ["app", "calc", "deep", "util"]);

    let out = silt(&app, &["remove", "calc"]);
    assert!(out.status.success(), "silt remove failed: {}", stderr(&out));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "Removed dependency 'calc'\n"
    );

    // The rest of silt.toml is as it was, its comment included.
    assert_eq!(
        fs::read_to_string(app.join("silt.toml")).unwrap(),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         # the deps of the app\n[dependencies]\n\
         util = { path = \"../util\" }\n"
    );
    // `deep`, which only `calc` needed, is gone from the lock too.
    let lock = fs::read_to_string(app.join("silt.lock")).unwrap();
    assert_eq!(locked_names(&lock), ["app", "util"], "lock: {lock}");
    let out = silt(&app, &["run"]);
    assert!(
        out.status.success(),
        "run after remove failed: {}",
        stderr(&out)
    );
    let _ = fs::remove_dir_all(&ws);
}

#[test]
fn test_remove_of_an_undeclared_name_changes_nothing() {
    let (ws, app) = workspace_with_two_deps("undeclared");
    let manifest = fs::read_to_string(app.join("silt.toml")).unwrap();
    let lock = fs::read_to_string(app.join("silt.lock")).unwrap();

    let out = silt(&app, &["remove", "deep"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stderr(&out),
        "error: silt remove: no dependency 'deep' in silt.toml\n"
    );
    assert_eq!(fs::read_to_string(app.join("silt.toml")).unwrap(), manifest);
    assert_eq!(fs::read_to_string(app.join("silt.lock")).unwrap(), lock);
    let _ = fs::remove_dir_all(&ws);
}

#[test]
fn test_remove_takes_exactly_one_name() {
    let (ws, app) = workspace_with_two_deps("usage");
    for args in [
        &["remove"][..],
        &["remove", "calc", "util"],
        &["remove", "--path"],
    ] {
        let out = silt(&app, args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        assert_eq!(
            stderr(&out),
            "error: silt remove takes the name of one dependency: silt remove <name>\n\
             Run 'silt remove --help' for usage.\n",
            "{args:?}"
        );
    }
    let _ = fs::remove_dir_all(&ws);
}

#[test]
fn test_remove_outside_a_package_is_refused() {
    let ws = fresh_workspace("outside");
    let out = silt(&ws, &["remove", "calc"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stderr(&out),
        "error: silt remove must be run inside a silt package\n"
    );
    let _ = fs::remove_dir_all(&ws);
}

#[test]
fn test_remove_help_and_the_top_level_help_list_it() {
    let out = silt_cmd().args(["remove", "--help"]).output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("Usage: silt remove <name>\n"));
    let out = silt_cmd().arg("--help").output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("silt remove <name>"));
}
