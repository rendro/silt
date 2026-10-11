//! Text from outside that is not Unicode is not a crash: an argument of
//! the command line is a usage error before anything runs, and an
//! environment variable is left out of `env.vars()` (as `env.get` gives
//! `None` for it).
//!
//! A golden case cannot pass such an argument or variable, hence tests
//! of their own.

use std::ffi::OsString;
use std::process::Command;

/// Text that is not Unicode: bytes that are no UTF-8 on Unix, a lone
/// surrogate on Windows.
#[cfg(unix)]
fn not_unicode() -> OsString {
    use std::os::unix::ffi::OsStringExt;
    OsString::from_vec(vec![b'a', 0xff, 0xfe])
}

#[cfg(windows)]
fn not_unicode() -> OsString {
    use std::os::windows::ffi::OsStringExt;
    OsString::from_wide(&[u16::from(b'a'), 0xd800])
}

fn program(name: &str, source: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("silt_not_unicode_{}_{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.silt");
    std::fs::write(&file, source).unwrap();
    file
}

/// What `silt` with `args` prints on stderr, and its exit code.
fn refused(args: &[OsString]) -> (String, Option<i32>) {
    let out = Command::new(env!("CARGO_BIN_EXE_silt"))
        .args(args)
        .output()
        .unwrap();
    assert!(out.stdout.is_empty(), "nothing runs: {out:?}");
    (
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code(),
    )
}

/// An argument of the program behind `--`, a file to run, a file to
/// test: each is refused as a usage error, with the place of the
/// argument, and the program does not start.
#[test]
fn an_argument_that_is_not_unicode_is_a_usage_error() {
    let file = program("args", "import io\nfn main() {\n  println(io.args())\n}\n");
    let cases: [Vec<OsString>; 3] = [
        vec![
            "run".into(),
            file.clone().into(),
            "--".into(),
            not_unicode(),
        ],
        vec!["run".into(), not_unicode()],
        vec!["test".into(), not_unicode()],
    ];
    for args in cases {
        let (stderr, code) = refused(&args);
        let at = args.len();
        assert!(
            stderr.starts_with(&format!("silt: argument {at} is not valid Unicode: ")),
            "{stderr}"
        );
        assert!(!stderr.contains("panicked"), "{stderr}");
        assert_eq!(code, Some(1), "{stderr}");
    }
}

/// `env.vars()` leaves out a variable whose value or name is not
/// Unicode, and gives the others; `env.get` of it is `None`.
#[test]
fn a_variable_that_is_not_unicode_is_left_out_of_env_vars() {
    let file = program(
        "vars",
        r#"import env
import list

fn ours(pair: (String, String)) -> Bool {
  let (name, _) = pair
  name == "SILT_NOT_UNICODE_VALUE" || name == "SILT_UNICODE"
}

fn main() {
  println(list.filter(env.vars(), ours))
  println(env.get("SILT_NOT_UNICODE_VALUE"))
}
"#,
    );
    let mut command = Command::new(env!("CARGO_BIN_EXE_silt"));
    command
        .arg("run")
        .arg(&file)
        .env("SILT_NOT_UNICODE_VALUE", not_unicode())
        .env("SILT_UNICODE", "plain");
    // A name that is not Unicode: Unix has them; on Windows a process
    // cannot be started with one.
    #[cfg(unix)]
    command.env(not_unicode(), "value");
    let out = command.output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "[(SILT_UNICODE, plain)]\nNone\n",
        "{stderr}"
    );
}
