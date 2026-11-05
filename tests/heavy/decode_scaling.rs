//! A decoder reads a document in time and memory that do not grow with
//! how deep its variants are nested.
//!
//! A variant's `fields` that come before its name are kept as read until
//! the name says how to decode them. Each variant nested in them kept
//! its own fields again, a copy of the copy: a document of one list of
//! 300,000 numbers inside 60 variants took 0.9 s and 640 MB where the
//! decoder before had taken 45 ms and 46 MB. Now the fields are decoded as
//! they are read where the name comes first, and kept once where it
//! comes after them: a variant in what is kept is read from it, shared.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

const PROGRAM: &str = "import io
import json

type E { Wrap(E), Leaf(List(Int)) }

fn depth(e: E, n: Int) -> Int {
  match e {
    Wrap(inner) -> depth(inner, n + 1)
    Leaf(_) -> n
  }
}

fn main() {
  let text = match io.read_file(\"doc.json\") {
    Ok(t) -> t
    Err(e) -> panic(\"{e}\")
  }
  match json.parse(text, E) {
    Ok(e) -> println(\"depth {depth(e, 0)}\")
    Err(e) -> println(\"{e}\")
  }
}
";

/// The JSON document of a `Leaf` of `numbers` numbers inside `levels`
/// `Wrap`s, each variant's name before its fields or after them.
fn document(numbers: usize, levels: usize, name_first: bool) -> String {
    let variant = |name: &str, fields: String| match name_first {
        true => format!(r#"{{"variant":"{name}","fields":[{fields}]}}"#),
        false => format!(r#"{{"fields":[{fields}],"variant":"{name}"}}"#),
    };
    let list: Vec<String> = (0..numbers).map(|n| n.to_string()).collect();
    let mut doc = variant("Leaf", format!("[{}]", list.join(",")));
    for _ in 0..levels {
        doc = variant("Wrap", doc);
    }
    doc
}

/// How long the program takes to decode the document in `dir`, which
/// holds `levels` variants around the leaf.
fn decode_time(dir: &Path, levels: usize) -> Duration {
    let started = Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("run")
        .arg("main.silt")
        .current_dir(dir)
        .output()
        .expect("silt runs");
    let elapsed = started.elapsed();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.trim(),
        format!("depth {levels}"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    elapsed
}

#[test]
fn a_document_of_variants_nested_eight_times_as_deep_takes_about_as_long() {
    const NUMBERS: usize = 100_000;
    const SHALLOW: usize = 6;
    const DEEP: usize = 48;
    let base = std::env::temp_dir().join(format!("silt_decode_scaling_{}", std::process::id()));
    let mut slow = Vec::new();
    for name_first in [true, false] {
        let [shallow, deep] = [SHALLOW, DEEP].map(|levels| {
            let dir = base.join(format!("{name_first}_{levels}"));
            std::fs::create_dir_all(&dir).expect("a temporary directory");
            std::fs::write(dir.join("main.silt"), PROGRAM).expect("the program is written");
            let doc = document(NUMBERS, levels, name_first);
            std::fs::write(dir.join("doc.json"), doc).expect("the document is written");
            dir
        });
        // A machine that was busy during one measurement is measured
        // again; a ratio over the cap because of what the decoder does
        // is over it every time.
        let mut over = Vec::new();
        for _ in 0..3 {
            let mut best = (Duration::MAX, Duration::MAX);
            for _ in 0..3 {
                best.0 = best.0.min(decode_time(&deep, DEEP));
                best.1 = best.1.min(decode_time(&shallow, SHALLOW));
            }
            let ratio = best.0.as_secs_f64() / best.1.as_secs_f64();
            if ratio <= 2.0 {
                over.clear();
                break;
            }
            over.push(format!("{:?} against {:?} ({ratio:.2})", best.0, best.1));
        }
        if !over.is_empty() {
            let order = match name_first {
                true => "the name first",
                false => "the fields first",
            };
            slow.push(format!("{order}: {}", over.join(", ")));
        }
    }
    let _ = std::fs::remove_dir_all(&base);
    assert!(
        slow.is_empty(),
        "{DEEP} nested variants took more than twice as long to decode as {SHALLOW}, in \
         each of three measurements:\n{}",
        slow.join("\n")
    );
}
