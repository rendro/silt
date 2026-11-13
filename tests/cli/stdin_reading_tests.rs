//! Reading stdin to its end: `io.read_stdin`, `io.read_stdin_bytes`,
//! `stream.stdin_lines` and the `None` that `io.read_line` gives at the
//! end. What a golden case cannot feed is here: `\r\n` line ends, bytes
//! that are no UTF-8, more than one read of input, and a stdin held open
//! (a line typed at a terminal comes before the input ends).

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// A program of `src` in a directory of its own named after `name`.
fn program(name: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("silt_stdin_{name}_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.silt");
    std::fs::write(&file, src).unwrap();
    (dir, file)
}

/// What the program `src` prints with `input` on its stdin, and whether
/// it ended well.
fn run_with(name: &str, src: &str, input: &[u8]) -> (String, String, bool) {
    let (dir, file) = program(name, src);
    let mut child = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("run")
        .arg(&file)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let input = input.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let out = child.wait_with_output().unwrap();
    writer.join().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

const LINES: &str = r#"import stream

fn main() {
  stream.stdin_lines() |> stream.collect |> println
}
"#;

#[test]
fn stdin_lines_drops_crlf_line_ends() {
    let (out, err, ok) = run_with("crlf", LINES, b"a\r\nb\r\n\r\nc\r");
    assert!(ok, "{err}");
    assert_eq!(out, "[Ok(\"a\"), Ok(\"b\"), Ok(\"\"), Ok(\"c\")]\n");
}

#[test]
fn stdin_lines_ends_with_an_error_at_a_line_that_is_no_utf8() {
    let (out, err, ok) = run_with("not_utf8", LINES, b"one\ntwo\n\xff\xfe\nfour\n");
    assert!(ok, "{err}");
    assert_eq!(
        out,
        "[Ok(\"one\"), Ok(\"two\"), Err(invalid input: line 3 of stdin is not UTF-8)]\n"
    );
    // A last line with no `\n` that is no UTF-8 is the same error.
    let (out, err, ok) = run_with("not_utf8_last", LINES, b"one\n\xc3");
    assert!(ok, "{err}");
    assert_eq!(
        out,
        "[Ok(\"one\"), Err(invalid input: line 2 of stdin is not UTF-8)]\n"
    );
}

#[test]
fn stdin_lines_reads_input_of_many_reads_whole() {
    // Lines of growing length, a few of them longer than one read, so
    // that lines and characters are cut by the edges of the reads.
    let mut input = String::new();
    let mut expected = Vec::new();
    for i in 0..2000 {
        let line = "é".repeat(i % 97) + &"x".repeat(i * 37 % 1500) + &i.to_string();
        input.push_str(&line);
        input.push('\n');
        expected.push(line);
    }
    let long = "z".repeat(200_000);
    input.push_str(&long);
    expected.push(long);
    let src = r#"import list
import stream
import string

fn main() {
  let lines = stream.stdin_lines() |> stream.collect
  println(list.length(lines))
  list.each(lines) { line ->
    match line {
      Ok(text) -> println("{string.length(text)} {string.slice(text, string.length(text) - 4, string.length(text))}")
      Err(e) -> println("error: {e}")
    }
  }
}
"#;
    let (out, err, ok) = run_with("many_reads", src, input.as_bytes());
    assert!(ok, "{err}");
    let mut want = format!("{}\n", expected.len());
    for line in &expected {
        let chars: Vec<char> = line.chars().collect();
        let tail: String = chars[chars.len().saturating_sub(4)..].iter().collect();
        want.push_str(&format!("{} {tail}\n", chars.len()));
    }
    assert_eq!(out, want);
}

#[test]
fn read_stdin_takes_crlf_as_it_is_and_refuses_no_utf8() {
    let src = r#"import bytes
import io

fn main() {
  println(io.inspect(io.read_stdin()))
}
"#;
    let (out, err, ok) = run_with("read_stdin", src, b"a\r\nb");
    assert!(ok, "{err}");
    assert_eq!(out, "Ok(\"a\\r\\nb\")\n");
    let (out, err, ok) = run_with("read_stdin_bad", src, b"ab\xffc");
    assert!(ok, "{err}");
    assert_eq!(out, "Err(InvalidInput(\"stdin is not UTF-8 at byte 2\"))\n");
    let bytes_src = r#"import bytes
import io

fn main() {
  match io.read_stdin_bytes() {
    Ok(b) -> println(bytes.to_list(b))
    Err(e) -> println("error: {e}")
  }
}
"#;
    let (out, err, ok) = run_with("read_stdin_bytes", bytes_src, b"ab\xff\r\n");
    assert!(ok, "{err}");
    assert_eq!(out, "[97, 98, 255, 13, 10]\n");
}

#[test]
fn input_that_is_no_utf8_is_invalid_input_for_all_three_readers() {
    // io.read_line says it as io.read_stdin and stream.stdin_lines do: an
    // `InvalidInput`, which one match arm takes for all three.
    let src = r#"import io

fn main() {
  println(io.inspect(io.read_line()))
  match io.read_line() {
    Err(io.InvalidInput(why)) -> println("invalid input: {why}")
    other -> println("not InvalidInput: {io.inspect(other)}")
  }
}
"#;
    let (out, err, ok) = run_with("read_line_bad", src, b"ok\n\xff\xfe\n");
    assert!(ok, "{err}");
    assert_eq!(
        out,
        "Ok(Some(\"ok\"))\ninvalid input: a line of stdin is not UTF-8\n"
    );
}

#[test]
fn a_line_on_an_open_stdin_comes_before_the_input_ends() {
    // `stream.stdin_lines` does not wait for a full read: the line it is
    // given is printed while stdin is still open.
    let src = r#"import io
import stream

fn main() {
  stream.stdin_lines() |> stream.take(1) |> stream.collect |> println
  match io.read_line() {
    Ok(Some(line)) -> println("then {line}")
    Ok(None) -> println("then the end")
    Err(e) -> println("error: {e}")
  }
}
"#;
    let (dir, file) = program("open", src);
    let mut child = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("run")
        .arg(&file)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(b"first\n").unwrap();
    stdin.flush().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut lines = BufReader::new(stdout).lines();
        while let Some(Ok(line)) = lines.next() {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let first = rx.recv_timeout(Duration::from_secs(20));
    // Then the end of the input: `read_line` sees it as `None`.
    drop(stdin);
    let then = rx.recv_timeout(Duration::from_secs(20));
    let status = child.wait().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        first.as_deref(),
        Ok("[Ok(\"first\")]"),
        "the line came only after stdin closed, or not at all"
    );
    assert_eq!(then.as_deref(), Ok("then the end"));
    assert!(status.success(), "{status:?}");
}
