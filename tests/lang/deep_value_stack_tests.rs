//! Values nested a million and a half levels deep are compared,
//! ordered, hashed, made members of a set, shown, inspected and
//! dropped: none of these goes down a value on the native stack (the
//! key of a value in src/value/key.rs, its text in src/value/fmt.rs,
//! its end in src/value/obj.rs). Each overflowed it at some depth,
//! which was another in a debug build, in a release build, and from
//! one compiler to the next.
//!
//! CI runs this in a release build as a job of its own (`cargo test
//! --release --all-features --test lang deep_value_stack_tests`). A
//! debug build runs a program ten times slower, and takes a tenth of
//! the depth: far more than its native stack ever held.

use std::process::Command;

const DEPTH: usize = match cfg!(debug_assertions) {
    true => 150_000,
    false => 1_500_000,
};

/// Two values `DEPTH` levels deep, a list in a variant at each level,
/// and what `uses` prints of them.
fn program(uses: &[&str]) -> String {
    let uses: String = uses
        .iter()
        .map(|used| format!("  println({used})\n"))
        .collect();
    format!(
        "import io\nimport set\nimport string\n\
         type T {{\n  Leaf,\n  Node(List(T))\n}}\n\
         fn build(n) {{\n  loop i = 0, acc = Leaf {{\n    match i >= n {{\n      true -> acc\n      false -> loop(i + 1, Node([acc]))\n    }}\n  }}\n}}\n\
         fn main() {{\n  let a = build({DEPTH})\n  let b = build({DEPTH})\n{uses}}}\n"
    )
}

fn run(name: &str, uses: &[&str]) -> String {
    let dir = std::env::temp_dir().join(format!("silt_deep_{}_{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.silt");
    std::fs::write(&file, program(uses)).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("run")
        .arg(&file)
        .output()
        .expect("spawn silt");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{name}: exit {:?}\n{}\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn deep_values_compare_order_and_hash() {
    assert_eq!(
        run(
            "key",
            &[
                "a == b",
                "a < b",
                "a.hash() == b.hash()",
                "set.length(set.from_list([a, b]))",
            ]
        ),
        "true\nfalse\ntrue\n1"
    );
}

/// The text of such a value has eight characters for each level, and
/// four more: `Node([` and `])` around `Leaf`.
#[test]
fn deep_values_are_shown_inspected_and_dropped() {
    let length = (DEPTH * 8 + 4).to_string();
    assert_eq!(
        run(
            "text",
            &[
                "string.length(\"{a}\")",
                "string.length(io.inspect(b))",
                "string.length(\"{(a, [b])}\")",
            ]
        ),
        format!("{length}\n{length}\n{}", DEPTH * 16 + 8 + 6)
    );
}
