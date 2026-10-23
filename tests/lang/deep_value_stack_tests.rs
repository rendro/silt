//! Values nested a million and a half levels deep are compared,
//! ordered, hashed, made members of a set, shown, inspected and
//! dropped: none of these goes down a value on the native stack (the
//! key of a value in src/value/key.rs, its text in src/value/fmt.rs,
//! its end in src/value/deep.rs). Each overflowed it at some depth,
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

/// The text of a value of this test has eight characters for each level,
/// and four more: `Node([` and `])` around `Leaf`.
fn text_length(depth: usize) -> usize {
    depth * 8 + 4
}

/// The deepest value of this test whose text a string holds (at most
/// 10,000,000 bytes): `DEPTH` in a debug build, 1,249,999 levels in a
/// release build.
const SHOWN_DEPTH: usize = match DEPTH * 8 + 4 > 10_000_000 {
    true => (10_000_000 - 4) / 8,
    false => DEPTH,
};

/// Two values `DEPTH` levels deep, a list in a variant at each level,
/// and what `uses` prints of them.
fn program(uses: &[&str]) -> String {
    program_of(DEPTH, DEPTH, uses)
}

/// [`program`] with `a` and `b` of the depths given.
fn program_of(depth_a: usize, depth_b: usize, uses: &[&str]) -> String {
    let uses: String = uses
        .iter()
        .map(|used| format!("  println({used})\n"))
        .collect();
    format!(
        "import io\nimport set\nimport string\n\
         type T {{\n  Leaf,\n  Node(List(T))\n}}\n\
         fn build(n) {{\n  loop i = 0, acc = Leaf {{\n    match i >= n {{\n      true -> acc\n      false -> loop(i + 1, Node([acc]))\n    }}\n  }}\n}}\n\
         fn main() {{\n  let a = build({depth_a})\n  let b = build({depth_b})\n{uses}}}\n"
    )
}

fn run(name: &str, uses: &[&str]) -> String {
    run_program(name, &program(uses))
}

fn run_program(name: &str, program: &str) -> String {
    let out = ended(name, program);
    assert!(
        out.status.success(),
        "{name}: exit {:?}\n{}\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// How `silt run` of `program` ended.
fn ended(name: &str, program: &str) -> std::process::Output {
    let dir = std::env::temp_dir().join(format!("silt_deep_{}_{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.silt");
    std::fs::write(&file, program).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("run")
        .arg(&file)
        .output()
        .expect("spawn silt");
    let _ = std::fs::remove_dir_all(&dir);
    out
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

/// A value is shown as deep as a string can hold its text (`a`), and
/// inspected at the full depth (`b`, and a tuple that holds both). A
/// text that a string cannot hold is refused as it is written: a
/// release build's `DEPTH` is that deep, and showing it stops with the
/// error, not by overflowing the stack.
#[test]
fn deep_values_are_shown_inspected_and_dropped() {
    let (shown, deep) = (text_length(SHOWN_DEPTH), text_length(DEPTH));
    assert_eq!(
        run_program(
            "text",
            &program_of(
                SHOWN_DEPTH,
                DEPTH,
                &[
                    "string.length(\"{a}\")",
                    "string.length(io.inspect(b))",
                    "string.length(io.inspect((a, [b])))",
                ]
            )
        ),
        format!("{shown}\n{deep}\n{}", shown + deep + 6)
    );
    let out = ended("too_long", &program(&["string.length(\"{b}\")"]));
    let (stdout, stderr) = (
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    match deep > 10_000_000 {
        true => {
            assert_eq!(out.status.code(), Some(1), "{stdout}\n{stderr}");
            assert!(
                stderr.contains("string interpolation: result would exceed maximum string size"),
                "{stderr}"
            );
        }
        false => assert_eq!(stdout.trim(), deep.to_string(), "{stderr}"),
    }
}

/// A value is nested without a type that holds itself too: a function
/// that calls itself with its argument inside a list, a tuple, a map
/// or a set makes one of them a level deeper at each call, and one
/// that wraps a closure in a closure, or sends a channel into a new
/// channel, does the same. Such a value is dropped like any other,
/// when the last call returns. (Each of these overflowed the native
/// stack: in a release build at the depths of this test, lists,
/// tuples and closures between two and five million levels, maps,
/// sets and channels between one and two.)
#[test]
fn values_nested_through_any_container_are_dropped() {
    let scale = match cfg!(debug_assertions) {
        true => 100_000,
        false => 1_000_000,
    };
    let polymorphic = |next: &str| {
        format!(
            "fn deep(x: a, n: Int) -> Int where a: Hash + Equal {{\n  match n <= 0 {{\n    \
             true -> 0\n    false -> deep({next}, n - 1)\n  }}\n}}\n"
        )
    };
    let closures = "fn deep(f: Fn() -> Int, n: Int) -> Int {\n  match n <= 0 {\n    \
                    true -> f()\n    false -> deep({ -> f() }, n - 1)\n  }\n}\n"
        .to_string();
    let channels = "import channel\n\
                    fn deep(x: a, n: Int) -> Int {\n  match n <= 0 {\n    true -> 0\n    \
                    false -> {\n      let c = channel.new(1)\n      channel.send(c, x)\n      \
                    deep(c, n - 1)\n    }\n  }\n}\n"
        .to_string();
    let shapes = [
        ("list", polymorphic("[x]"), "1", 5),
        ("tuple", polymorphic("(n, x)"), "1", 5),
        ("map_value", polymorphic("#{n: x}"), "1", 2),
        ("map_key", polymorphic("#{x: n}"), "1", 2),
        ("set", polymorphic("#[x]"), "1", 2),
        ("closure", closures, "{ -> 0 }", 5),
        ("channel", channels, "1", 2),
    ];
    for (name, deep, first, millions) in shapes {
        let depth = millions * scale;
        let program = format!(
            "{deep}fn main() {{\n  println(deep({first}, {depth}))\n  println(\"done\")\n}}\n"
        );
        assert_eq!(run_program(name, &program), "0\ndone", "{name}");
    }
}
