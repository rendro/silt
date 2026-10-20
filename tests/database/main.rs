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
//!
//! A case whose first lines are `-- env: NAME=value` runs with those
//! variables set; one whose first lines have `-- exit: N` ends with that
//! status (else 0), and its stderr is `<case>.stderr` if that exists
//! (else empty).
//!
//! The example of docs/stdlib/postgres.md is a case too: the program and
//! the output the page shows (`the_docs_example`).

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
    let source = std::fs::read_to_string(cases_dir().join(&file)).expect("read the program");
    let header = source.lines().take_while(|line| line.starts_with("-- "));
    let vars: Vec<(String, String)> = header
        .clone()
        .filter_map(|line| line.strip_prefix("-- env: "))
        .map(|var| var.split_once('=').expect("`-- env: NAME=value`"))
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect();
    let mut header = header.filter_map(|line| line.strip_prefix("-- exit: "));
    let exit = header
        .next()
        .map_or(0, |n| n.parse().expect("`-- exit: N`"));
    let stderr = std::fs::read_to_string(cases_dir().join(format!("{name}.stderr")));
    let ending = Ending {
        exit,
        stderr: stderr.unwrap_or_default(),
    };
    run(name, &source, &vars, &expected, &ending);
}

/// How a case ends: its exit status and its stderr.
#[derive(Default)]
struct Ending {
    exit: i32,
    stderr: String,
}

/// The example of docs/stdlib/postgres.md runs, and prints what the
/// page says it prints. It takes the server's URL from `DATABASE_URL`.
#[test]
fn the_docs_example() {
    if !server_is_named() {
        return;
    }
    let page = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/stdlib/postgres.md");
    let page = std::fs::read_to_string(page).expect("read docs/stdlib/postgres.md");
    let example = page
        .split("\n## Example\n")
        .nth(1)
        .expect("an Example section");
    let mut blocks = example.split("```text\n").skip(1);
    let mut block = || {
        let block = blocks.next().expect("a ```text block");
        block.split("```").next().expect("the block").to_string()
    };
    let (source, expected) = (block(), block());
    let url = std::env::var(URL).expect("the server's URL");
    run(
        "the_docs_example",
        &source,
        &[("DATABASE_URL".to_string(), url)],
        &expected,
        &Ending::default(),
    );
}

/// Run the program `source` as `<name>.silt`, with the environment
/// variables `vars`, and hold its output against `expected` and its end
/// against `ending`.
fn run(name: &str, source: &str, vars: &[(String, String)], expected: &str, ending: &Ending) {
    let file = format!("{name}.silt");
    // The program runs in a directory of its own, so nothing `silt`
    // writes lands in the source tree.
    let scratch = std::env::temp_dir().join(format!("silt-database-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).expect("create scratch dir");
    std::fs::write(scratch.join(&file), source).expect("write the program");

    let mut child = Command::new(env!("CARGO_BIN_EXE_silt"))
        .args(["run", &file])
        .envs(vars.iter().map(|(name, value)| (name, value)))
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
    assert_eq!(
        status.code(),
        Some(ending.exit),
        "{file}: exit status\n{shown}"
    );
    assert_eq!(stderr, ending.stderr, "{file}: stderr\n{shown}");
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

cases!(
    select_one,
    cancel_of_a_statement_in_flight,
    cancelled_task_starts_no_statement,
    deadline_over_a_statement_in_flight,
    stream_closed_by_its_receiver,
    errors_of_statements,
    connect_errors,
    connect_to_a_server_that_never_answers,
    stream_left_unread,
    stream_of_a_cancelled_reader,
    stream_left_unread_in_a_transaction,
    cancel_of_a_statement_in_a_transaction,
    deadline_over_a_statement_in_a_transaction,
    deadline_while_waiting_for_a_connection,
    cancel_request_reaches_no_other_statement,
    io_timeout_leaves_a_commit_alone,
    transaction_used_after_its_end,
    aborted_transaction_is_not_committed,
    callback_deadline_aborts_its_transaction,
    rows_of_every_decoded_type,
    rows_that_do_not_fit,
    a_field_with_bytes_a_document_would_hold_is_refused,
    an_enum_field_is_no_row_field_at_run_time_either,
    parameters_of_every_type,
    execute_counts_and_returning,
    stream_and_cursor_of_a_record_type,
    a_parameter_that_cannot_be_sent,
    dates_go_back_as_they_were_read,
    a_pool_opens_connections_when_it_needs_them,
    a_channel_is_notified_under_the_name_it_was_listened_on,
    a_connection_the_server_ended_is_not_taken,
    a_statement_that_never_got_the_connection_gives_up,
    a_statement_behind_its_own_unread_stream_fails,
    a_cursor_ends_at_a_batch_that_fails,
    a_statement_right_after_the_server_ends_its_sessions_runs,
);
