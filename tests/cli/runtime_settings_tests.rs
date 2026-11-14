//! The runtime's settings in the environment (`SILT_IO_TIMEOUT`,
//! `SILT_TIME_SLICE`): a value silt cannot read refuses the start of
//! `silt run`, `silt test` and `silt repl`, naming the variable, the
//! value and the forms it takes. (They were ignored without a word:
//! a safety setting with a typo was silently not there.)

use std::process::{Command, Stdio};

/// Run `silt <args>` in a fresh directory holding `main.silt` (a
/// program that prints "ran") and `main_test.silt` (a test that
/// prints "tested"), with `vars` set and the two settings otherwise
/// unset: (status, stdout, stderr).
fn silt(name: &str, args: &[&str], vars: &[(&str, &str)]) -> (Option<i32>, String, String) {
    let dir = std::env::temp_dir().join(format!(
        "silt_runtime_settings_{}_{name}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("main.silt"),
        "fn main() {\n  println(\"ran\")\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main_test.silt"),
        "fn test_it() {\n  println(\"tested\")\n}\n",
    )
    .unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_silt"));
    command
        .args(args)
        .current_dir(&dir)
        .env_remove("SILT_IO_TIMEOUT")
        .env_remove("SILT_TIME_SLICE")
        .stdin(Stdio::null());
    for (var, value) in vars {
        command.env(var, value);
    }
    let output = command.output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

const IO_FORMS: &str =
    "is not a duration: write a whole number above 0 with a unit, as in 500ms, 5s, 2m or 1h";
const SLICE_FORMS: &str = "is not a number of steps: write a whole number above 0, as in 500";

#[test]
fn an_io_timeout_silt_cannot_read_refuses_the_start() {
    for (i, value) in ["200", "0s", "0ms", "-5s", "abc", "5 parsecs", "none"]
        .into_iter()
        .enumerate()
    {
        let (code, out, err) = silt(
            &format!("io{i}"),
            &["run", "main.silt"],
            &[("SILT_IO_TIMEOUT", value)],
        );
        assert_eq!(code, Some(1), "{value}: {err}");
        assert_eq!(out, "", "{value}: the program did not run");
        assert_eq!(
            err,
            format!("error: silt run: SILT_IO_TIMEOUT='{value}' {IO_FORMS}\n"),
            "{value}"
        );
    }
}

#[test]
fn a_time_slice_silt_cannot_read_refuses_the_start() {
    for (i, value) in ["abc", "0", "-5", "1.5", "500ms"].into_iter().enumerate() {
        let (code, out, err) = silt(
            &format!("slice{i}"),
            &["run", "main.silt"],
            &[("SILT_TIME_SLICE", value)],
        );
        assert_eq!(code, Some(1), "{value}: {err}");
        assert_eq!(out, "", "{value}: the program did not run");
        assert_eq!(
            err,
            format!("error: silt run: SILT_TIME_SLICE='{value}' {SLICE_FORMS}\n"),
            "{value}"
        );
    }
}

#[test]
fn silt_test_and_the_repl_refuse_the_same() {
    let (code, out, err) = silt(
        "test",
        &["test", "main_test.silt"],
        &[("SILT_IO_TIMEOUT", "200")],
    );
    assert_eq!(code, Some(1), "{err}");
    assert!(!out.contains("tested"), "no test ran: {out}");
    assert_eq!(
        err,
        format!("error: silt test: SILT_IO_TIMEOUT='200' {IO_FORMS}\n")
    );
    // Without the setting the same test runs.
    let (code, out, err) = silt("test-ok", &["test", "main_test.silt"], &[]);
    assert_eq!(code, Some(0), "{err}");
    assert!(out.contains("tested"), "{out}");
    if cfg!(feature = "repl") {
        let (code, _, err) = silt("repl", &["repl"], &[("SILT_TIME_SLICE", "0")]);
        assert_eq!(code, Some(1), "{err}");
        assert_eq!(
            err,
            format!("error: silt repl: SILT_TIME_SLICE='0' {SLICE_FORMS}\n")
        );
    }
}

#[test]
fn values_silt_reads_and_empty_ones_start() {
    for (i, vars) in [
        &[("SILT_IO_TIMEOUT", "200ms"), ("SILT_TIME_SLICE", "500")][..],
        &[("SILT_IO_TIMEOUT", " 5s "), ("SILT_TIME_SLICE", "1")],
        &[("SILT_IO_TIMEOUT", "2m")],
        &[("SILT_IO_TIMEOUT", ""), ("SILT_TIME_SLICE", "")],
    ]
    .into_iter()
    .enumerate()
    {
        let (code, out, err) = silt(&format!("ok{i}"), &["run", "main.silt"], vars);
        assert_eq!(
            (code, out.as_str(), err.as_str()),
            (Some(0), "ran\n", ""),
            "{vars:?}"
        );
    }
}
