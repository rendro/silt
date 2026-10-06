//! `silt fmt` replaces a file as a whole: the formatted text is written
//! beside the file and then takes its place, so an interrupted run
//! leaves the source as it was. A file it may not write is an error in
//! silt's own words, the same on every platform.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

const UNFORMATTED: &str = "fn  main( ) {\nprintln(\"hello\")\n}\n";
const FORMATTED: &str = "fn main() {\n  println(\"hello\")\n}\n";

/// A fresh directory for one test.
fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("silt_cli_fmt_write_tests")
        .join(format!("{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn names_in(dir: &PathBuf) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn a_read_only_file_is_not_replaced() {
    let dir = temp_dir("read_only");
    let path = dir.join("main.silt");
    fs::write(&path, UNFORMATTED).unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&path, permissions.clone()).unwrap();

    let output = silt_cmd().arg("fmt").arg(&path).output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "stderr:\n{stderr}");
    assert_eq!(
        stderr.trim_end(),
        format!("error writing {}: permission denied", path.display())
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), UNFORMATTED);
    assert_eq!(names_in(&dir), ["main.silt"]);

    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    fs::set_permissions(&path, permissions).unwrap();
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn the_file_is_replaced_and_nothing_is_left_beside_it() {
    let dir = temp_dir("replaced");
    let path = dir.join("main.silt");
    fs::write(&path, UNFORMATTED).unwrap();
    let before = fs::metadata(&path).unwrap().permissions();

    let output = silt_cmd().arg("fmt").arg(&path).output().unwrap();
    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), FORMATTED);
    assert_eq!(names_in(&dir), ["main.silt"]);
    assert_eq!(fs::metadata(&path).unwrap().permissions(), before);
    let _ = fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_link_stays_a_link_and_the_file_keeps_its_mode() {
    use std::os::unix::fs::PermissionsExt;
    let dir = temp_dir("link");
    let real = dir.join("real.silt");
    let link = dir.join("link.silt");
    fs::write(&real, UNFORMATTED).unwrap();
    fs::set_permissions(&real, fs::Permissions::from_mode(0o640)).unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let output = silt_cmd().arg("fmt").arg(&link).output().unwrap();
    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read_to_string(&real).unwrap(), FORMATTED);
    assert_eq!(
        fs::metadata(&real).unwrap().permissions().mode() & 0o777,
        0o640
    );
    assert_eq!(names_in(&dir), ["link.silt", "real.silt"]);
    let _ = fs::remove_dir_all(&dir);
}
