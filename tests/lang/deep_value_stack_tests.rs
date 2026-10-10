//! Comparing, ordering and hashing values nested a million and a half
//! levels deep, in a release build, as base did up to two million: the
//! checks the VM makes before it compares (`Value::contains_fn`) walk
//! a worklist, so the native stack they need does not depend on how the
//! compiler inlined them. A debug build's frames are far larger, so the
//! test runs only in release (`cargo test --release`).

use std::process::Command;

const DEPTH: usize = 1_500_000;

fn program(use_value: &str) -> String {
    format!(
        "import set\n\
         type T {{\n  Leaf,\n  Node(List(T))\n}}\n\
         fn build(n) {{\n  loop i = 0, acc = Leaf {{\n    match i >= n {{\n      true -> acc\n      false -> loop(i + 1, Node([acc]))\n    }}\n  }}\n}}\n\
         fn main() {{\n  let a = build({DEPTH})\n  let b = build({DEPTH})\n  println({use_value})\n}}\n"
    )
}

fn run(name: &str, use_value: &str) -> String {
    let dir = std::env::temp_dir().join(format!("silt_deep_{}_{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.silt");
    std::fs::write(&file, program(use_value)).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("run")
        .arg(&file)
        .output()
        .expect("spawn silt");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{name}: exit {:?}\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
#[cfg_attr(debug_assertions, ignore = "release only: debug frames are far larger")]
fn deep_values_compare_order_and_hash_in_release() {
    assert_eq!(run("eq", "a == b"), "true");
    assert_eq!(run("lt", "a < b"), "false");
    assert_eq!(run("set", "set.length(set.from_list([a, b]))"), "1");
}
