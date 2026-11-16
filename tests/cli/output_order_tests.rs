//! What a program prints to stdout and to stderr arrives in the order of
//! the prints where both go to one place (`2>&1`), also a stdout line that
//! has no end yet: stdout held it until its end (or the exit), and a
//! later stderr text came before it.

use std::process::{Command, Stdio};

#[test]
fn a_stdout_line_with_no_end_yet_comes_before_a_later_stderr_text() {
    let dir = std::env::temp_dir().join(format!("silt_output_order_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a directory");
    let program = dir.join("main.silt");
    std::fs::write(
        &program,
        "import io\nfn main() {\n  print(\"out \")\n  io.eprint(\"err \")\n  print(\"out2 \")\n  io.eprintln(\"err2\")\n  println(\"end\")\n}\n",
    )
    .expect("the program is written");
    let both = dir.join("both");
    let file = std::fs::File::create(&both).expect("the output file");
    let status = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("run")
        .arg(&program)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(file.try_clone().expect("a second handle"))
        .stderr(file)
        .status()
        .expect("silt runs");
    let written = std::fs::read_to_string(&both).expect("the output is read");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(status.success(), "{written}");
    assert_eq!(written, "out err out2 err2\nend\n");
}

/// `silt run` of a program that runs `writes` (lines of code) and then
/// fails, with stdout and stderr into one file (`one`) or into two: what
/// the file holds, or the two files.
fn run_failing(tag: &str, writes: &str, one: bool) -> (Option<i32>, String, String) {
    let dir = std::env::temp_dir().join(format!(
        "silt_output_order_fail_{tag}_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("a directory");
    let program = dir.join("main.silt");
    std::fs::write(
        &program,
        format!(
            "import io\nimport list\nfn main() {{\n  io.eprint(\"\")\n  {writes}\n  let n = 1 / (list.length([1]) - 1)\n  println(n)\n}}\n"
        ),
    )
    .expect("the program is written");
    let (out, err) = (dir.join("out"), dir.join("err"));
    let out_file = std::fs::File::create(&out).expect("the output file");
    let err_file = match one {
        true => out_file.try_clone().expect("a second handle"),
        false => std::fs::File::create(&err).expect("the error file"),
    };
    let status = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("run")
        .arg(&program)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(out_file)
        .stderr(err_file)
        .status()
        .expect("silt runs");
    let read = |path: &std::path::Path| std::fs::read_to_string(path).unwrap_or_default();
    let written = (status.code(), read(&out), read(&err));
    let _ = std::fs::remove_dir_all(&dir);
    written
}

/// A stdout line with no end yet comes before the report of the runtime
/// error that ends the program, where both go to one place, and the
/// report starts on a line of its own; a program whose output ended its
/// line gets no blank line, and where stdout and stderr are two files
/// nothing is added to either. FAIL on the base: the report was written
/// first and "partial " at the exit; on 478805d4 the report followed the
/// partial text on its line.
#[test]
fn a_stdout_line_with_no_end_yet_comes_before_the_report_of_a_failure() {
    let report = "error[runtime]: division by zero\n";
    let (code, both, _) = run_failing("partial", "print(\"partial \")", true);
    assert_eq!(code, Some(1), "{both}");
    assert!(both.starts_with(&format!("partial \n{report}")), "{both}");

    let (code, both, _) = run_failing("line", "print(\"whole\\n\")", true);
    assert_eq!(code, Some(1), "{both}");
    assert!(both.starts_with(&format!("whole\n{report}")), "{both}");

    let (code, out, err) = run_failing("apart", "print(\"partial \")", false);
    assert_eq!(code, Some(1), "{err}");
    assert_eq!(out, "partial ");
    assert!(err.starts_with(report), "{err}");
}

/// The line the report starts on is the last line of both outputs where
/// they are one place: a line end is added only where the last text the
/// program wrote, to stdout or to stderr, left a partial line. FAIL on
/// e6b2f85e: only stdout's last text was looked at, so `print("out ")`
/// and then `io.eprintln("err")` got an empty line before the report.
#[test]
fn the_report_follows_the_last_text_of_either_output() {
    let report = "error[runtime]: division by zero\n";
    let cases = [
        (
            "out_err",
            "print(\"out \")\n  io.eprintln(\"err\")",
            "out err\n".to_string(),
        ),
        (
            "err_out",
            "io.eprint(\"err \")\n  println(\"out\")",
            "err out\n".to_string(),
        ),
        (
            "both",
            "print(\"out \")\n  io.eprint(\"err \")",
            "out err \n".to_string(),
        ),
        (
            "neither",
            "println(\"out\")\n  io.eprintln(\"err\")",
            "out\nerr\n".to_string(),
        ),
    ];
    for (tag, writes, before) in cases {
        let (code, both, _) = run_failing(tag, writes, true);
        assert_eq!(code, Some(1), "{tag}: {both}");
        assert!(
            both.starts_with(&format!("{before}{report}")),
            "{tag}: {both:?}"
        );
    }
}
