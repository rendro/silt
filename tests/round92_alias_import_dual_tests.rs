//! Round 92 regression: an aliased USER-module import must register its
//! `alias.name` globals even when the same module was already imported
//! another way.
//!
//! `src/compiler/mod.rs::compile_file_module` returns the module's
//! public export names so the `ImportTarget::Alias` arm can emit
//! `GetGlobal("mod.name") + SetGlobal("alias.name")` pairs. Before the
//! fix, the already-compiled cache hit returned `Ok(vec![])`, so an
//! alias import following ANY prior import of the same module
//! registered nothing. The typechecker mirrors the alias
//! unconditionally, so `silt check` exited 0 while `silt run` died
//! with `error[runtime]: undefined global: g.area`.
//!
//! The exact stacked-import shape is documented in
//! docs/getting-started.md and docs/language/modules.md:
//!
//! ```silt
//! import geometry
//! import geometry as g
//! ```
//!
//! Builtin modules (`import list as l`) route through a different
//! (alias-mirror) path and were never affected — that path is locked
//! by tests/aliased_import_runtime_tests.rs.
//!
//! These are full RUN-path tests: each builds a real package on disk
//! (silt.toml + src/) and executes it through the CLI binary.

use std::path::PathBuf;
use std::process::Command;

/// Fresh per-test temp directory so parallel test runs don't collide.
fn tempdir(label: &str) -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "silt_round92_alias_{label}_{}_{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(dir.join("src")).expect("create temp package dirs");
    dir
}

/// Build a package whose src/geometry.silt exports `area`, write the
/// given main.silt body, and `silt run` it. Returns (stdout, stderr, ok).
fn run_package(label: &str, main_src: &str) -> (String, String, bool) {
    let root = tempdir(label);
    std::fs::write(
        root.join("silt.toml"),
        "[package]\nname = \"aliasdual\"\nversion = \"0.1.0\"\n",
    )
    .expect("write silt.toml");
    std::fs::write(
        root.join("src/geometry.silt"),
        "pub fn area(w: Int, h: Int) -> Int = w * h\n",
    )
    .expect("write geometry.silt");
    let main_path = root.join("src/main.silt");
    std::fs::write(&main_path, main_src).expect("write main.silt");

    let bin = env!("CARGO_BIN_EXE_silt");
    let out = Command::new(bin)
        .arg("run")
        .arg(&main_path)
        .output()
        .expect("spawn silt run");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let ok = out.status.success();
    let _ = std::fs::remove_dir_all(&root);
    (stdout, stderr, ok)
}

/// The documented stacked shape: plain import first, then an alias of
/// the same module. Before the fix `g.area` was an undefined global at
/// runtime (while `silt check` passed).
#[test]
fn plain_import_then_alias_runs() {
    let (stdout, stderr, ok) = run_package(
        "plain_then_alias",
        "import geometry\nimport geometry as g\n\nfn main() {\n  println(geometry.area(2, 5))\n  println(g.area(3, 4))\n}\n",
    );
    assert!(
        ok,
        "silt run should succeed for plain-then-alias; stderr={stderr}"
    );
    assert!(
        !stderr.contains("undefined global"),
        "no undefined-global runtime error expected; stderr={stderr}"
    );
    assert_eq!(
        stdout.lines().collect::<Vec<_>>(),
        vec!["10", "12"],
        "both the plain and aliased call must work"
    );
}

/// Reverse order: alias first, plain second. The alias arm compiles
/// the module fresh (this direction already worked); the plain import
/// then hits the cache. Locks both names stay usable.
#[test]
fn alias_then_plain_import_runs() {
    let (stdout, stderr, ok) = run_package(
        "alias_then_plain",
        "import geometry as g\nimport geometry\n\nfn main() {\n  println(g.area(3, 4))\n  println(geometry.area(2, 5))\n}\n",
    );
    assert!(
        ok,
        "silt run should succeed for alias-then-plain; stderr={stderr}"
    );
    assert_eq!(
        stdout.lines().collect::<Vec<_>>(),
        vec!["12", "10"],
        "both the aliased and plain call must work"
    );
}

/// Two distinct aliases of the same module: the second alias is a
/// cache hit and previously registered nothing, so `g2.area` died with
/// an undefined-global runtime error.
#[test]
fn two_aliases_of_same_module_run() {
    let (stdout, stderr, ok) = run_package(
        "two_aliases",
        "import geometry as g1\nimport geometry as g2\n\nfn main() {\n  println(g1.area(3, 4))\n  println(g2.area(2, 5))\n}\n",
    );
    assert!(
        ok,
        "silt run should succeed for two aliases of one module; stderr={stderr}"
    );
    assert!(
        !stderr.contains("undefined global"),
        "no undefined-global runtime error expected; stderr={stderr}"
    );
    assert_eq!(
        stdout.lines().collect::<Vec<_>>(),
        vec!["12", "10"],
        "both aliases must resolve to the module's exports"
    );
}
