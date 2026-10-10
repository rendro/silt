//! A long entry that is pasted into the REPL is read once.
//!
//! The REPL asks after each line whether the entry is finished. It
//! lexed the whole entry for each line: a list of 8,000 lines took 21 s
//! (the character scanner before it 6 s). A line that holds no closer
//! and no quote leaves an open entry open and is not lexed at all, so
//! such a list costs one lex, at its last line: twice the lines are
//! about twice the time.

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long `silt repl` takes for a list of `lines` lines, one number
/// each, pasted as one entry, and its length asked behind it.
fn paste_time(lines: usize) -> Duration {
    let mut input = String::from("import list\nlet xs = [\n");
    for i in 0..lines {
        input.push_str(&format!("  {i},\n"));
    }
    input.push_str("]\nlist.length(xs)\n:quit\n");
    let started = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("repl")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("silt repl starts");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(input.as_bytes())
        .expect("the entry is written");
    let output = child.wait_with_output().expect("silt repl ends");
    let elapsed = started.elapsed();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.lines().any(|line| line == lines.to_string()),
        "the list has {lines} elements: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    elapsed
}

#[test]
fn twice_the_pasted_lines_take_about_twice_as_long() {
    // A machine that was busy during one measurement is measured again;
    // a ratio that is over the cap because the entry is lexed for each
    // line (four times the time for twice the lines) is over it every
    // time.
    let mut over = Vec::new();
    for _ in 0..3 {
        let small = (0..3).map(|_| paste_time(8_000)).min().unwrap();
        let large = (0..3).map(|_| paste_time(16_000)).min().unwrap();
        let ratio = large.as_secs_f64() / small.as_secs_f64();
        if ratio <= 2.6 {
            return;
        }
        over.push(format!("{large:?} against {small:?} ({ratio:.2})"));
    }
    panic!(
        "a pasted list of 16,000 lines took more than 2.6 times as long as one of 8,000 in \
         each of three measurements ({}): the entry is lexed again for each line",
        over.join(", ")
    );
}
