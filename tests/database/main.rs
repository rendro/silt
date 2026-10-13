//! Test suite: the `postgres` module against a PostgreSQL server.
//!
//! A case is a silt program in this directory with the output it must
//! write, `<case>.silt` and `<case>.stdout`, run through the built `silt`
//! binary as a user would run it. The program takes the server's URL
//! from the environment variable `SILT_TEST_POSTGRES_URL`.
//!
//! Without the variable there is no server to ask: a case then says on
//! stderr that it was skipped, and passes. In CI (`CI` is set) it fails
//! instead: there the job that runs this suite starts the server and
//! sets the variable, and a run that tested nothing must not be green.
//! `tests/README.md` says how to run the suite.

#![cfg(feature = "postgres")]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The variable that holds the server's URL.
const URL: &str = "SILT_TEST_POSTGRES_URL";

/// A case that has not exited after this long has failed.
const CASE_TIMEOUT: Duration = Duration::from_secs(60);

fn cases_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/database")
}

/// Whether there is a server to run a case against. Without one the
/// case is skipped, with a line on stderr, except in CI, where that is
/// a failure.
fn server_is_named() -> bool {
    if std::env::var_os(URL).is_some_and(|url| !url.is_empty()) {
        return true;
    }
    assert!(
        std::env::var_os("CI").is_none(),
        "{URL} is not set: in CI a database case is never skipped"
    );
    eprintln!("database case SKIPPED: {URL} is not set (see tests/README.md)");
    false
}

/// Run the case `name` and hold its output against `<name>.stdout`.
fn case(name: &str) {
    if !server_is_named() {
        return;
    }
    let file = format!("{name}.silt");
    let expected = std::fs::read_to_string(cases_dir().join(format!("{name}.stdout")))
        .unwrap_or_else(|e| panic!("{name}.stdout: {e}"));
    // The program runs in a directory of its own, so nothing `silt`
    // writes lands in the source tree.
    let scratch = std::env::temp_dir().join(format!("silt-database-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).expect("create scratch dir");
    std::fs::copy(cases_dir().join(&file), scratch.join(&file)).expect("copy the program");

    let mut child = Command::new(env!("CARGO_BIN_EXE_silt"))
        .args(["run", &file])
        .current_dir(&scratch)
        .env("NO_COLOR", "1")
        .env_remove("FORCE_COLOR")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn silt");
    // Read the pipes on their own threads so a large output cannot block
    // the child while this thread waits for it.
    let mut out_pipe = child.stdout.take().expect("stdout");
    let mut err_pipe = child.stderr.take().expect("stderr");
    let out_reader = std::thread::spawn(move || {
        let mut s = Vec::new();
        let _ = std::io::Read::read_to_end(&mut out_pipe, &mut s);
        s
    });
    let err_reader = std::thread::spawn(move || {
        let mut s = Vec::new();
        let _ = std::io::Read::read_to_end(&mut err_pipe, &mut s);
        s
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => break Some(status),
            None if started.elapsed() >= CASE_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    };
    let stdout = String::from_utf8_lossy(&out_reader.join().unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&err_reader.join().unwrap_or_default()).into_owned();
    let _ = std::fs::remove_dir_all(&scratch);

    let shown = format!("--- stdout\n{stdout}--- stderr\n{stderr}");
    let Some(status) = status else {
        panic!("{file} did not exit within {CASE_TIMEOUT:?}\n{shown}");
    };
    assert_eq!(status.code(), Some(0), "{file}: exit status\n{shown}");
    assert_eq!(stderr, "", "{file}: stderr\n{shown}");
    assert_eq!(stdout, expected, "{file}: stdout\n{shown}");
}

/// One test for each listed case, and one that holds the list against
/// the directory.
macro_rules! cases {
    ($($name:ident),* $(,)?) => {
        $(
            #[test]
            fn $name() {
                case(stringify!($name));
            }
        )*

        /// Every program in the directory is a case above, and every
        /// case has its program: a file that nobody runs holds nothing.
        #[test]
        fn every_program_is_a_case() {
            let mut listed = vec![$(format!("{}.silt", stringify!($name))),*];
            listed.sort();
            let mut found: Vec<String> = std::fs::read_dir(cases_dir())
                .expect("read tests/database")
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|file| file.ends_with(".silt"))
                .collect();
            found.sort();
            assert_eq!(found, listed);
        }
    };
}

cases!(select_one);
