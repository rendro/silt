//! What `silt run` makes of the path it is given: the directory of a
//! package runs the package's `main`, as `silt run` in it does, and a
//! file that is not a `.silt` file is refused with how to give it to
//! the program instead (it is not read as silt).

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

fn fresh_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("silt_cli_run_paths_tests")
        .join(format!("{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A package `app` in `dir`, whose `main` prints its arguments.
fn package(dir: &std::path::Path) -> PathBuf {
    let root = dir.join("app");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("silt.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(
        root.join("src").join("main.silt"),
        "import io\n\nfn main() {\n  println(\"app {io.args()}\")\n}\n",
    )
    .unwrap();
    root
}

#[test]
fn a_package_directory_runs_its_main() {
    let dir = fresh_dir("package_dir");
    package(&dir);
    let output = (silt_cmd().args(["run", "app", "--", "x"]))
        .current_dir(&dir)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "stderr:\n{stderr}");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "app [x]\n");
    let _ = fs::remove_dir_all(&dir);
}

/// A directory that is no package is still no file to run.
#[test]
fn a_directory_that_is_no_package_is_not_read() {
    let dir = fresh_dir("plain_dir");
    fs::create_dir_all(dir.join("plain")).unwrap();
    let output = (silt_cmd().args(["run", "plain"]))
        .current_dir(&dir)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "error reading plain: it is a directory\n"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// A path that names nothing is a file that cannot be read (a misspelt
/// name, most likely), whatever it ends in: it is not refused as a file
/// meant for the program.
#[test]
fn a_path_that_does_not_exist_is_no_such_file() {
    let dir = fresh_dir("no_such");
    for name in ["nosuch", "nosuch.silt"] {
        let output = (silt_cmd().args(["run", name]))
            .current_dir(&dir)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            format!("error reading {name}: no such file\n")
        );
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_file_that_is_not_silt_is_refused_with_how_to_give_it_to_the_program() {
    let dir = fresh_dir("not_silt");
    fs::write(dir.join("cfg.toml"), "# cfg\nport = 1\n").unwrap();
    let output = (silt_cmd().args(["run", "cfg.toml"]))
        .current_dir(&dir)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "error: silt run: cfg.toml is not a .silt file; to give it to a program, write it \
         after `--`: `silt run <file>.silt -- cfg.toml`\n"
    );
    // In a package, the program is the package's: no file is named.
    let root = package(&dir);
    fs::write(root.join("cfg.toml"), "# cfg\n").unwrap();
    let output = (silt_cmd().args(["run", "cfg.toml"]))
        .current_dir(&root)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "error: silt run: cfg.toml is not a .silt file; to give it to the package's program \
         (src/main.silt), write it after `--`: `silt run -- cfg.toml`\n"
    );
    let _ = fs::remove_dir_all(&dir);
}
