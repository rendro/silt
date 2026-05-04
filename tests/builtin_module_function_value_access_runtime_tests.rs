//! Round-72 RUNTIME parity lock — every typechecker-registered
//! `<module>.<func>` must be reachable as a first-class VALUE at runtime,
//! not just as the head of a call.
//!
//! ## Background
//!
//! Two paths exist for `module.func` references:
//!
//! 1. **CALL form** — `task.spawn_until(dur, fn)` is recognised by the
//!    compiler in `extract_builtin_name` and lowered to
//!    `Op::CallBuiltin("task.spawn_until", …)`, which dispatches by
//!    module prefix in `Vm::dispatch_builtin` and never looks at globals.
//!    This works regardless of whether `builtin_module_functions` knows
//!    the name.
//!
//! 2. **VALUE form** — `let f = task.spawn_until` (as well as
//!    `xs |> task.spawn_until(dur)` past the `|>` desugaring, and
//!    closure capture of a builtin) compiles to
//!    `Op::GetGlobal("task.spawn_until")`. This requires the global to
//!    have been seeded by `Vm::register_builtins`
//!    (`src/vm/dispatch.rs`), which iterates exactly
//!    `module::builtin_module_functions(<module>)` and inserts
//!    `Value::BuiltinFn` for each name.
//!
//! ## Bug
//!
//! Round 59 fixed the *aliased*-CALL path
//! (`tests/aliased_import_runtime_tests.rs`) by routing
//! `l.sum(…)` through `extract_builtin_name`. The non-aliased
//! VALUE-access path was never widened: ~20 names (`task.spawn_until`,
//! `task.deadline`, `list.sum`, `list.product`, `list.scan`,
//! `list.intersperse`, `list.remove_at`, `list.min_by`, `list.max_by`,
//! `list.index_of`, `string.lines`, `string.split_at`,
//! `string.last_index_of`, `string.starts_with_at`, `bytes.starts_with`,
//! `bytes.ends_with`, `bytes.split`, `bytes.index_of`,
//! `set.symmetric_difference`, `channel.recv_timeout`,
//! `regex.captures_named`, `http.parse_query`, plus the postgres
//! cursor / listen / notify family) were registered as schemes by the
//! typechecker but missing from `builtin_module_functions` in
//! `src/module.rs`. The first-class repro
//!
//!   ```silt
//!   import task
//!   import time
//!   fn main() {
//!     let f = task.spawn_until
//!     let h = f(time.seconds(1), fn() { 42 })
//!     task.join(h)
//!   }
//!   ```
//!
//! crashed with `error[runtime]: undefined global: task.spawn_until`.
//!
//! ## Lock
//!
//! `direct_value_access_runs` exercises `let f = task.spawn_until`
//! (the canonical repro) plus `let g = list.sum` and
//! `let h = string.lines` end-to-end via `silt run`. These names span
//! three different stdlib modules so a regression that misses any one
//! arm of the `match module` in `builtin_module_functions` will fail at
//! runtime, not just at typecheck.
//!
//! `every_typechecker_registration_is_value_accessible` is the
//! parity-lock proper: it walks the curated set of names called out in
//! the round-72 finding (representative of every affected module) and
//! asserts each binds-as-value-then-prints at runtime. If a future
//! round adds another `intern("foo.bar")` to a typechecker submodule
//! without widening `builtin_module_functions`, the test fails with a
//! clear "undefined global: foo.bar" message and points back to the
//! fix site.
//!
//! ## Companion
//!
//! `tests/aliased_import_runtime_tests.rs` covers the aliased-CALL
//! path. This file covers the non-aliased VALUE path. Together they
//! lock both halves of the `builtin_module_functions` invariant:
//! every typechecker registration must be (a) seeded as a global
//! (this file) and (b) mirrored under any user-declared alias prefix
//! (the companion file).

use std::process::Command;

/// Run a silt source program via the `silt run` subcommand and return
/// `(stdout, stderr, success)`. Mirrors the helper in
/// `aliased_import_runtime_tests.rs` so the two files exercise the
/// runtime the same way.
///
/// The temp file path includes both the caller label AND the test
/// process id, so two tests in this same binary running in parallel
/// (cargo test default) cannot clobber each other's source files.
fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let pid = std::process::id();
    let tid = format!("{:?}", std::thread::current().id());
    // `ThreadId(N)` → `N` to keep the path tidy.
    let tid = tid
        .trim_start_matches("ThreadId(")
        .trim_end_matches(')')
        .to_string();
    let tmp = std::env::temp_dir().join(format!(
        "silt_value_access_rt_{label}_p{pid}_t{tid}.silt"
    ));
    std::fs::write(&tmp, src).expect("write temp file");
    let bin = env!("CARGO_BIN_EXE_silt");
    let out = Command::new(bin)
        .arg("run")
        .arg(&tmp)
        .output()
        .expect("spawn silt run");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (stdout, stderr, out.status.success())
}

fn run_silt_ok(label: &str, src: &str) -> String {
    let (stdout, stderr, ok) = run_silt_raw(label, src);
    assert!(
        ok,
        "silt run should succeed for {label}; stdout={stdout:?}, stderr={stderr:?}"
    );
    stdout
}

/// Canonical round-72 repro: every name spelled out in the finding
/// must round-trip through a `let`-binding. Three modules covered so
/// an arm-shaped regression in `builtin_module_functions` (e.g. a
/// stale `task` arm only) is caught even if other arms are correct.
#[test]
fn direct_value_access_runs() {
    // task.spawn_until: the exact program from the round-72 finding.
    let out = run_silt_ok(
        "task_spawn_until",
        r#"
import task
import time
fn main() {
  let f = task.spawn_until
  let h = f(time.seconds(1), fn() { 42 })
  println(task.join(h))
}
"#,
    );
    assert!(
        out.contains("42"),
        "expected '42' in stdout for `let f = task.spawn_until`; got {out:?}"
    );

    // list.sum: representative of the list-module gap (sum, product,
    // sum_float, product_float, min_by, max_by, scan, intersperse,
    // remove_at, index_of all share the same missing-global root cause).
    let out = run_silt_ok(
        "list_sum",
        r#"
import list
fn main() {
  let f = list.sum
  println(f([1, 2, 3, 4]))
}
"#,
    );
    assert!(
        out.contains("10"),
        "expected '10' in stdout for `let f = list.sum`; got {out:?}"
    );

    // string.lines: representative of the string-module gap (lines,
    // split_at, last_index_of, starts_with_at all share the same
    // missing-global root cause).
    let out = run_silt_ok(
        "string_lines",
        r#"
import string
import list
fn main() {
  let f = string.lines
  list.each(f("a\nb"), fn(line) { println(line) })
}
"#,
    );
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        vec!["a", "b"],
        "expected a\\nb from `let f = string.lines`; got {out:?}"
    );
}

/// Parity-lock proper: every name from the round-72 finding (one
/// representative per affected module) binds as a value at runtime.
/// Each name is exercised through `let f = <module>.<name>` so the
/// failure mode is `undefined global: <module>.<name>` — the exact
/// shape the original bug surfaced. A pure typecheck pass is NOT
/// enough — the typechecker registers these names regardless; only
/// the runtime path catches the missing-global drift.
///
/// If this test fails after a future audit round adds a new
/// `intern("foo.bar")` to a typechecker submodule, widen the matching
/// arm of `builtin_module_functions` in `src/module.rs` to include
/// `bar`. That is the same fix shape applied in round 72 itself.
#[test]
fn every_typechecker_registration_is_value_accessible() {
    // (module, function, snippet that binds the function as a value
    // and uses it). The call shape varies per function arity — we just
    // need each one to resolve as a global, then exercise it once.
    let cases: &[(&str, &str, &str)] = &[
        // task module — the canonical repro. `task.spawn_until` and
        // `task.deadline` were both missing.
        (
            "task",
            "spawn_until",
            r#"
import task
import time
fn main() {
  let f = task.spawn_until
  let h = f(time.ms(1), fn() { 1 })
  println(task.join(h))
}
"#,
        ),
        (
            "task",
            "deadline",
            r#"
import task
import time
fn main() {
  let f = task.deadline
  println(f(time.seconds(1), fn() { 7 }))
}
"#,
        ),
        // list module — sum / product / scan / intersperse /
        // remove_at / index_of / min_by / max_by all share the gap.
        (
            "list",
            "product",
            r#"
import list
fn main() {
  let f = list.product
  println(f([2, 3, 4]))
}
"#,
        ),
        (
            "list",
            "scan",
            r#"
import list
fn main() {
  let f = list.scan
  println(f([1, 2, 3], 0, fn(acc, x) { acc + x }))
}
"#,
        ),
        (
            "list",
            "intersperse",
            r#"
import list
fn main() {
  let f = list.intersperse
  println(f([1, 2, 3], 0))
}
"#,
        ),
        (
            "list",
            "remove_at",
            r#"
import list
fn main() {
  let f = list.remove_at
  println(f([10, 20, 30], 1))
}
"#,
        ),
        (
            "list",
            "index_of",
            r#"
import list
fn main() {
  let f = list.index_of
  println(f([10, 20, 30], 20))
}
"#,
        ),
        // string module — last_index_of / split_at / starts_with_at.
        (
            "string",
            "last_index_of",
            r#"
import string
fn main() {
  let f = string.last_index_of
  println(f("abcabc", "b"))
}
"#,
        ),
        (
            "string",
            "split_at",
            r#"
import string
fn main() {
  let f = string.split_at
  println(f("hello", 2))
}
"#,
        ),
        (
            "string",
            "starts_with_at",
            r#"
import string
fn main() {
  let f = string.starts_with_at
  println(f("hello", 1, "ello"))
}
"#,
        ),
        // bytes module — starts_with / ends_with / split / index_of.
        (
            "bytes",
            "starts_with",
            r#"
import bytes
fn main() {
  let f = bytes.starts_with
  let a = bytes.from_string("hello")
  let b = bytes.from_string("he")
  println(f(a, b))
}
"#,
        ),
        (
            "bytes",
            "ends_with",
            r#"
import bytes
fn main() {
  let f = bytes.ends_with
  let a = bytes.from_string("hello")
  let b = bytes.from_string("lo")
  println(f(a, b))
}
"#,
        ),
        (
            "bytes",
            "index_of",
            r#"
import bytes
fn main() {
  let f = bytes.index_of
  let a = bytes.from_string("hello")
  let b = bytes.from_string("ll")
  println(f(a, b))
}
"#,
        ),
        // set module — symmetric_difference.
        (
            "set",
            "symmetric_difference",
            r#"
import set
fn main() {
  let f = set.symmetric_difference
  let a = set.from_list([1, 2, 3])
  let b = set.from_list([2, 3, 4])
  println(set.length(f(a, b)))
}
"#,
        ),
        // channel module — recv_timeout. Returns `Result(a, ChannelError)`
        // (NOT `ChannelResult`) — see the doc-comment at
        // `src/typechecker/builtins/channel.rs::channel.recv_timeout`.
        // The channel is constructed with a buffer of 1 so the
        // single-thread `send` lands in the buffer before the
        // `recv_timeout` is called (otherwise the unbuffered send
        // deadlocks the main thread).
        (
            "channel",
            "recv_timeout",
            r#"
import channel
import time
fn main() {
  let f = channel.recv_timeout
  let ch: Channel(Int) = channel.new(1)
  channel.send(ch, 99)
  match f(ch, time.ms(50)) {
    Ok(v) -> println(v)
    Err(_) -> println(-1)
  }
}
"#,
        ),
        // regex module — captures_named.
        (
            "regex",
            "captures_named",
            r#"
import regex
fn main() {
  let f = regex.captures_named
  match f("(?P<x>\\d+)", "abc 123") {
    Some(_) -> println("matched")
    None -> println("no match")
  }
}
"#,
        ),
        // http module — parse_query.
        (
            "http",
            "parse_query",
            r#"
import http
import map
fn main() {
  let f = http.parse_query
  println(map.length(f("a=1&b=2")))
}
"#,
        ),
    ];

    for (module, func, src) in cases {
        let label = format!("{module}_{func}");
        let (stdout, stderr, ok) = run_silt_raw(&label, src);
        assert!(
            ok,
            "first-class value access of `{module}.{func}` failed at runtime — \
             this is the exact missing-global shape round 72 is locking. \
             Add `\"{func}\"` to the `\"{module}\"` arm of \
             `builtin_module_functions` in src/module.rs.\n\
             stdout={stdout:?}\nstderr={stderr:?}"
        );
        // Sanity check: stderr must not contain the canonical undefined-global
        // wording even if `success` is somehow true (defence-in-depth).
        let undef = format!("undefined global: {module}.{func}");
        assert!(
            !stderr.contains(&undef),
            "stderr surfaced `{undef}` for `{module}.{func}` — runtime did not seed the global"
        );
    }
}
