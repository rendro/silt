//! Silt VM benchmarks.
//!
//! Run with: cargo bench --bench vm_bench
//!
//! Each benchmark compiles a Silt program once, then runs it `ITERATIONS` times
//! and reports the average. This measures VM execution, not compilation.

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;
use silt::value::Value;
use silt::vm::Vm;
use std::sync::Arc;
use std::time::{Duration, Instant};

const ITERATIONS: u32 = 100;

// ── Helpers ─────────────────────────────────────────────────────────

fn compile(source: &str) -> Arc<silt::bytecode::Function> {
    let tokens = Lexer::new(source).tokenize().expect("lex error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = compiler.compile_program(&program).expect("compile error");
    Arc::new(functions.into_iter().next().unwrap())
}

fn run_once(script: &Arc<silt::bytecode::Function>) -> Value {
    let mut vm = Vm::new();
    vm.run(Arc::clone(script)).expect("runtime error")
}

fn bench(name: &str, source: &str) -> Duration {
    let script = compile(source);

    // Warmup
    for _ in 0..3 {
        run_once(&script);
    }

    let start = Instant::now();
    for _ in 0..ITERATIONS {
        run_once(&script);
    }
    let elapsed = start.elapsed();
    let per_iter = elapsed / ITERATIONS;

    println!("{name:40} {per_iter:>10.2?}  ({ITERATIONS} iterations)");
    per_iter
}

fn bench_compile(name: &str, source: &str) -> Duration {
    // Warmup
    for _ in 0..3 {
        compile(source);
    }

    let start = Instant::now();
    for _ in 0..ITERATIONS {
        compile(source);
    }
    let elapsed = start.elapsed();
    let per_iter = elapsed / ITERATIONS;

    println!("{name:40} {per_iter:>10.2?}  ({ITERATIONS} iterations)");
    per_iter
}

// ── Main ────────────────────────────────────────────────────────────

fn main() {
    println!();
    println!("═══════════════════════════════════════════════════════════════");
    println!("  Silt VM Benchmarks");
    println!("═══════════════════════════════════════════════════════════════");
    println!();

    // ── Compilation ─────────────────────────────────────────────────
    println!("── Compilation ────────────────────────────────────────────────");

    bench_compile(
        "compile: hello world",
        r#"
fn main() { println("hello") }
    "#,
    );

    bench_compile("compile: 100-line program", &generate_large_program(100));

    println!();

    // ── Recursion & TCO ─────────────────────────────────────────────
    println!("── Recursion & TCO ────────────────────────────────────────────");

    bench(
        "tco: countdown 1M",
        r#"
fn countdown(n) {
  match n {
    0 -> 0
    _ -> countdown(n - 1)
  }
}
fn main() { countdown(1000000) }
    "#,
    );

    bench(
        "tco: sum 100K",
        r#"
fn sum(n, acc) {
  match n {
    0 -> acc
    _ -> sum(n - 1, acc + n)
  }
}
fn main() { sum(100000, 0) }
    "#,
    );

    bench(
        "recursion: fibonacci(25)",
        r#"
fn fib(n) {
  match n {
    0 -> 0
    1 -> 1
    _ -> fib(n - 1) + fib(n - 2)
  }
}
fn main() { fib(25) }
    "#,
    );

    bench(
        "loop: sum 100K",
        r#"
fn main() {
  loop n = 0, acc = 0 {
    match n {
      100000 -> acc
      _ -> loop(n + 1, acc + n)
    }
  }
}
    "#,
    );

    println!();

    // ── Collections ─────────────────────────────────────────────────
    println!("── Collections ────────────────────────────────────────────────");

    bench(
        "list: map 10K",
        r#"
import list
fn main() {
  1..10001
  |> list.map { x -> x * 2 }
  |> list.length
}
    "#,
    );

    bench(
        "list: filter 10K",
        r#"
import list
fn main() {
  1..10001
  |> list.filter { x -> x % 2 == 0 }
  |> list.length
}
    "#,
    );

    bench(
        "list: fold 10K",
        r#"
import list
fn main() {
  1..10001
  |> list.fold(0) { acc, x -> acc + x }
}
    "#,
    );

    bench(
        "list: map+filter+fold 10K",
        r#"
import list
fn main() {
  1..10001
  |> list.map { x -> x * 2 }
  |> list.filter { x -> x % 3 == 0 }
  |> list.fold(0) { acc, x -> acc + x }
}
    "#,
    );

    bench(
        "list: sort_by 5K",
        r#"
import list
fn main() {
  let xs = list.reverse(1..5001)
  list.sort_by(xs, fn(x) { x })
  |> list.length
}
    "#,
    );

    bench(
        "list: flatten nested",
        r#"
import list
fn main() {
  let xs = list.map(1..101) { i -> list.map(1..101) { j -> i * 100 + j } }
  list.flatten(xs) |> list.length
}
    "#,
    );

    println!();

    // ── String operations ───────────────────────────────────────────
    println!("── Strings ────────────────────────────────────────────────────");

    bench(
        "string: split+join 1K",
        r#"
import string
import list
fn main() {
  let s = string.join(list.map(1..1001) { n -> "{n}" }, ",")
  string.split(s, ",") |> list.length
}
    "#,
    );

    bench(
        "string: interpolation 10K",
        r#"
import list
fn main() {
  list.map(1..10001) { n ->
    "item-{n}"
  } |> list.length
}
    "#,
    );

    // ── Regex operations ────────────────────────────────────────────
    println!("── Regex ──────────────────────────────────────────────────────");

    bench(
        "regex: is_match 1K (cached)",
        r#"
import regex
import list
fn main() {
  list.fold(1..1001, 0) { acc, n ->
    match regex.is_match("\\d+", "{n}") {
      true -> acc + 1
      false -> acc
    }
  }
}
    "#,
    );

    bench(
        "regex: find_all 1K",
        r#"
import regex
import list
fn main() {
  list.map(1..1001) { n ->
    regex.find_all("[0-9]+", "abc{n}def{n}ghi") |> list.length
  } |> list.fold(0) { acc, x -> acc + x }
}
    "#,
    );

    bench(
        "regex: replace_all 1K",
        r#"
import regex
import list
fn main() {
  list.map(1..1001) { n ->
    regex.replace_all("[aeiou]", "hello world {n}", "_")
  } |> list.length
}
    "#,
    );

    println!();

    // ── Pattern matching ────────────────────────────────────────────
    println!("── Pattern matching ───────────────────────────────────────────");

    bench(
        "match: simple 100K",
        r#"
import list
fn classify(n) {
  match n % 4 {
    0 -> "a"
    1 -> "b"
    2 -> "c"
    _ -> "d"
  }
}
fn main() {
  list.map(1..100001) { n -> classify(n) } |> list.length
}
    "#,
    );

    bench(
        "match: nested ADT 50K",
        r#"
import list
type Expr { Num(Int), Add(Expr, Expr) }
fn eval(e) {
  match e {
    Num(n) -> n
    Add(a, b) -> eval(a) + eval(b)
  }
}
fn main() {
  list.map(1..50001) { n ->
    eval(Add(Num(n), Add(Num(1), Num(2))))
  } |> list.fold(0) { acc, x -> acc + x }
}
    "#,
    );

    bench(
        "match: or-pattern 100K",
        r#"
import list
fn is_vowel(s) {
  match s {
    "a" | "e" | "i" | "o" | "u" -> true
    _ -> false
  }
}
fn main() {
  list.map(1..100001) { n ->
    match n % 5 {
      0 -> is_vowel("a")
      1 -> is_vowel("b")
      2 -> is_vowel("e")
      3 -> is_vowel("x")
      _ -> is_vowel("i")
    }
  } |> list.length
}
    "#,
    );

    println!();

    // ── Closures & higher-order ─────────────────────────────────────
    println!("── Closures & higher-order ─────────────────────────────────────");

    bench(
        "closure: capture + call 100K",
        r#"
import list
fn make_adder(n) = fn(x) { x + n }
fn main() {
  let add5 = make_adder(5)
  list.map(1..100001) { n -> add5(n) }
  |> list.fold(0) { acc, x -> acc + x }
}
    "#,
    );

    bench(
        "closure: nested composition",
        r#"
import list
fn compose(f, g) = fn(x) { f(g(x)) }
fn main() {
  let double = fn(x) { x * 2 }
  let inc = fn(x) { x + 1 }
  let double_then_inc = compose(inc, double)
  list.map(1..50001) { n -> double_then_inc(n) }
  |> list.fold(0) { acc, x -> acc + x }
}
    "#,
    );

    println!();

    // ── Maps & sets ─────────────────────────────────────────────────
    println!("── Maps & sets ────────────────────────────────────────────────");

    bench(
        "map: build+lookup 1K",
        r#"
import list
import map
fn main() {
  let m = list.fold(1..1001, #{}) { acc, n ->
    map.set(acc, "{n}", n)
  }
  list.fold(1..1001, 0) { acc, n ->
    match map.get(m, "{n}") {
      Some(v) -> acc + v
      None -> acc
    }
  }
}
    "#,
    );

    println!();

    // ── Concurrency ─────────────────────────────────────────────────
    println!("── Concurrency ────────────────────────────────────────────────");

    // Concurrency benchmarks use fewer iterations due to thread overhead
    {
        let source = r#"
import channel
import task
import list
fn main() {
  let ch = channel.new(10000)
  -- Pre-fill the channel, then read back (avoids cross-thread blocking)
  list.each(1..10001) { n -> channel.send(ch, n) }
  channel.close(ch)
  let sum = loop acc = 0 {
    match channel.receive(ch) {
      Message(n) -> loop(acc + n)
      Closed -> acc
    }
  }
  sum
}
        "#;
        bench("channel: send/receive 10K", source);
    }

    {
        let source = r#"
import task
import list
fn main() {
  let tasks = list.map(1..11) { n ->
    task.spawn(fn() { n * n })
  }
  list.map(tasks) { t -> task.join(t) }
  |> list.fold(0) { acc, x -> acc + x }
}
        "#;
        bench("task: spawn+join 10", source);
    }

    {
        // Pre-fill channels, then select — no cross-thread blocking
        let source = r#"
import channel
import list
fn main() {
  let ch1 = channel.new(1000)
  let ch2 = channel.new(1000)
  list.each(1..501) { n -> channel.send(ch1, n) }
  list.each(1..501) { n -> channel.send(ch2, n * 10) }
  channel.close(ch1)
  channel.close(ch2)

  let sum = loop acc = 0, done = 0 {
    match done {
      2 -> acc
      _ -> {
        let result = channel.select([ch1, ch2])
        match result {
          (_, Message(n)) -> loop(acc + n, done)
          (_, Closed) -> loop(acc, done + 1)
        }
      }
    }
  }
  sum
}
        "#;
        bench("channel.select: 2ch x 500 msg", source);
    }

    println!();

    // ── Arithmetic ──────────────────────────────────────────────────
    println!("── Arithmetic ─────────────────────────────────────────────────");

    bench(
        "int: arithmetic 1M ops",
        r#"
fn compute(n, acc) {
  match n {
    0 -> acc
    _ -> compute(n - 1, acc + n * n - n / 2)
  }
}
fn main() { compute(1000000, 0) }
    "#,
    );

    bench(
        "float: arithmetic 100K ops",
        r#"
import float
fn compute(n, acc) {
  match n == 0 {
    true -> acc
    false -> compute(n - 1, acc + 1.5 * 2.3 + 0.7)
  }
}
fn main() { compute(100000, 0.0) }
    "#,
    );

    println!();
    println!("═══════════════════════════════════════════════════════════════");
    println!("  Done.");
    println!("═══════════════════════════════════════════════════════════════");
}

// ── Helpers ─────────────────────────────────────────────────────────

fn generate_large_program(num_fns: usize) -> String {
    let mut s = String::new();
    for i in 0..num_fns {
        s.push_str(&format!("fn f{i}(x) = x + {i}\n"));
    }
    s.push_str("fn main() { f0(1) }\n");
    s
}
