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
