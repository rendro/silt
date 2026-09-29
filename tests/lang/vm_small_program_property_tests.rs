//! # VM small-program property tests
//!
//! This file feeds randomly-generated, *type-correct* silt programs into
//! the lexer → parser → typechecker → compiler → VM pipeline and asserts
//! a handful of invariants that should hold for every program regardless
//! of its shape:
//!
//! - **No panic.**  The VM is invoked inside `std::panic::catch_unwind`
//!   on a worker thread; any Rust unwind fails the property test.  This
//!   catches `unreachable!()`, debug-assertion trips, and the like.
//!
//! - **Bounded runtime.**  The worker thread is given a 10-second wall
//!   clock via `mpsc::recv_timeout`.  The generator is deliberately
//!   constructed so every production terminates quickly (no open loops;
//!   all recursion carries an explicit fuel counter), so only a
//!   pathological program can exceed the cap.  A timeout is reported as
//!   a VM-layer failure.
//!
//! - **No internal-invariant errors.**  The VM distinguishes user-level
//!   errors (division by zero, list out-of-bounds, …) from internal
//!   invariant violations via an `"internal:"` prefix on the error
//!   message.  A user-level error is acceptable; an internal one fails
//!   the test.
//!
//! - **Generator health.**  Any program that fails to typecheck or
//!   compile is bucketed as a generator reject — it does NOT count
//!   against the VM.  This lets us iterate on the generator without
//!   contaminating VM-layer signal.
//!
//! - **Determinism (optional second test).**  Single-threaded
//!   Int-typed programs read no nondeterministic state, so two runs of
//!   the same source must produce byte-identical results.
//!
//! The generator is narrow by design: every expression form is
//! Int-typed, so typechecking well-formedness is statically preserved
//! across compositions.  This keeps the signal concentrated on VM
//! execution rather than type errors.
//!
//! Ten hand-crafted targeted programs used to sit beside the proptest
//! loop; they are now golden cases in
//! tests/golden/lang/vm/vm_small_program_property__*.silt.  Each one
//! exercises a region the audit protocol has flagged in rounds 22-27
//! (deep match, mutual recursion, long pipe chains, record-update
//! cascades, nested closures, tail-calls through match arms, or-patterns
//! over compound scrutinees, rest-patterns, shadowing destructure-chains,
//! pipe-into-returned-closure).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use proptest::prelude::*;
use proptest::test_runner::TestCaseError;

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;
use silt::types::Severity;
use silt::value::Value;
use silt::vm::Vm;

// ── Knobs ─────────────────────────────────────────────────────────────

/// Wall-clock cap for a single program's execution.  Well-formed
/// generator output finishes in under 100 ms; 10 seconds is a 100x
/// cushion only a genuinely broken (non-terminating) program can hit.
const VM_WALL_CLOCK_SECS: u64 = 10;

// ── Compile + run harness ────────────────────────────────────────────

/// The outcome of running a generated program.
#[derive(Debug)]
enum RunOutcome {
    /// Compiled, executed, and returned a value.
    Ok(Value),
    /// The VM surfaced a structured error (e.g. division by zero, list
    /// OOB).  Acceptable so long as the message does not start with
    /// `"internal:"`.
    UserError(String),
    /// Lex / parse / typecheck / compile failure.  Counts as a generator
    /// bug, not a VM failure.
    #[allow(dead_code)] // shown through `Debug` when a case is rejected
    GeneratorReject(String),
}

/// Compile `source` and run `main()` on a worker thread with a wall-clock
/// guard.  Any panic, any internal-invariant error, or any timeout is
/// converted to a `TestCaseError::fail(...)`.
fn compile_and_run_capped(source: &str) -> Result<RunOutcome, TestCaseError> {
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    // Pipeline front-half: lex / parse / typecheck / compile.  All errors
    // here are bucketed as generator rejects.
    let tokens = match Lexer::new(source).tokenize() {
        Ok(t) => t,
        Err(e) => return Ok(RunOutcome::GeneratorReject(format!("lex: {e:?}"))),
    };
    let mut program = match Parser::new(tokens).parse_program() {
        Ok(p) => p,
        Err(e) => return Ok(RunOutcome::GeneratorReject(format!("parse: {e:?}"))),
    };

    let diagnostics = typechecker::check(&mut program);
    let hard_errors: Vec<String> = diagnostics
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.message)
        .collect();
    if !hard_errors.is_empty() {
        return Ok(RunOutcome::GeneratorReject(format!(
            "typecheck: {hard_errors:?}\nsource:\n{source}"
        )));
    }

    let mut compiler = Compiler::new();
    let functions = match compiler.compile_program(&program) {
        Ok(f) => f,
        Err(e) => {
            return Ok(RunOutcome::GeneratorReject(format!(
                "compile: {e:?}\nsource:\n{source}"
            )));
        }
    };

    let script = Arc::new(functions.into_iter().next().expect("empty script"));

    // Run on a worker thread so we can enforce a wall-clock deadline.
    // The worker catches its own panic and reports it via the channel.
    let (tx, rx) = mpsc::sync_channel::<Result<Result<Value, String>, String>>(1);
    let handle = thread::spawn(move || {
        let result = catch_unwind(AssertUnwindSafe(move || {
            let mut vm = Vm::new();
            vm.run(script)
        }));
        let payload = match result {
            Ok(Ok(v)) => Ok(Ok(v)),
            Ok(Err(e)) => Ok(Err(e.message)),
            Err(panic) => {
                let msg = if let Some(s) = panic.downcast_ref::<&'static str>() {
                    (*s).to_string()
                } else if let Some(s) = panic.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "<non-string panic payload>".into()
                };
                Err(msg)
            }
        };
        let _ = tx.send(payload);
    });

    let outcome = match rx.recv_timeout(Duration::from_secs(VM_WALL_CLOCK_SECS)) {
        Ok(Ok(Ok(v))) => RunOutcome::Ok(v),
        Ok(Ok(Err(msg))) => {
            if msg.starts_with("internal:") {
                return Err(TestCaseError::fail(format!(
                    "VM surfaced internal invariant violation: {msg}\nsource:\n{source}"
                )));
            }
            RunOutcome::UserError(msg)
        }
        Ok(Err(panic_msg)) => {
            return Err(TestCaseError::fail(format!(
                "VM PANIC: {panic_msg}\nsource:\n{source}"
            )));
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            // Don't join — the worker is wedged.  Let it leak; the
            // process will end at test-harness termination.
            std::mem::drop(handle);
            return Err(TestCaseError::fail(format!(
                "VM wall-clock TIMEOUT after {VM_WALL_CLOCK_SECS}s\nsource:\n{source}"
            )));
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            return Err(TestCaseError::fail(format!(
                "VM worker thread dropped without sending\nsource:\n{source}"
            )));
        }
    };

    // Best-effort: reap the worker.
    let _ = handle.join();

    Ok(outcome)
}

// ── Generators ───────────────────────────────────────────────────────

/// Identifier pool.  Short names keep shrunk failures readable.  We stay
/// inside `[a-z]{1..4}[0-9]?` and filter out keywords and stdlib module
/// names to avoid accidentally generating a name the compiler will
/// reject or diagnose as shadowing.
fn arb_ident() -> impl Strategy<Value = String> {
    prop::string::string_regex("[a-z]{1,4}[0-9]?")
        .unwrap()
        .prop_filter("not a keyword or stdlib module", |s| {
            !matches!(
                s.as_str(),
                "fn" | "let"
                    | "type"
                    | "trait"
                    | "match"
                    | "when"
                    | "return"
                    | "pub"
                    | "import"
                    | "as"
                    | "else"
                    | "where"
                    | "loop"
                    | "true"
                    | "false"
                    | "mod"
                    | "main"
                    | "list"
                    | "string"
                    | "map"
                    | "set"
                    | "int"
                    | "float"
                    | "io"
                    | "fs"
                    | "env"
                    | "test"
                    | "regex"
                    | "json"
                    | "math"
                    | "channel"
                    | "task"
                    | "time"
                    | "http"
                    | "bytes"
                    | "tcp"
                    | "stream"
                    | "result"
                    | "option"
            )
        })
}

/// Generator-side scope tracker.  Every value expression we generate is
/// Int-typed, so a plain name list suffices — any in-scope variable is
/// a valid Int reference.
#[derive(Clone, Default)]
struct Scope {
    names: Vec<String>,
}

impl Scope {
    fn with(&self, name: &str) -> Self {
        let mut s = self.clone();
        s.names.push(name.to_string());
        s
    }

    fn any_name(&self) -> Option<&str> {
        self.names.last().map(String::as_str)
    }
}

/// Generate an Int-typed expression in `scope`, bounded by a depth
/// budget.  At `depth == 0` we fall back to a leaf, guaranteeing the
/// generator terminates.
fn arb_int_expr(scope: Scope, depth: u32) -> BoxedStrategy<String> {
    if depth == 0 {
        return leaf_int_expr(scope).boxed();
    }

    let s1 = scope.clone();
    let s2 = scope.clone();
    let s3 = scope.clone();
    let s4 = scope.clone();
    let s5 = scope.clone();
    let s6 = scope.clone();
    let s7 = scope.clone();

    prop_oneof![
        // Leaf fallback keeps some short trees in the corpus.
        2 => leaf_int_expr(scope),
        // Arithmetic: (a OP b).  Parens around everything side-steps
        // every precedence subtlety.
        3 => (
            arb_int_expr(s1.clone(), depth - 1),
            arb_int_op(),
            arb_int_expr(s1, depth - 1),
        ).prop_map(|(a, op, b)| format!("({a} {op} {b})")),
        // Match over a bool-shaped condition.  Uses the silt idiom
        // `match expr { true -> ... false -> ... }` instead of a
        // dedicated if/else.
        2 => (
            arb_int_expr(s2.clone(), depth - 1),
            arb_int_expr(s2.clone(), depth - 1),
            arb_int_expr(s2, depth - 1),
        ).prop_map(|(cond, a, b)| {
            format!("(match (({cond}) > 0) {{ true -> ({a}) false -> ({b}) }})")
        }),
        // Let binding into a fresh scope.
        2 => (arb_ident(), arb_int_expr(s3.clone(), depth - 1))
            .prop_flat_map(move |(name, bound)| {
                let inner = s3.with(&name);
                arb_int_expr(inner, depth - 1).prop_map(move |body| {
                    format!("{{ let {name} = ({bound}); ({body}) }}")
                })
            }),
        // Or-pattern over a tuple scrutinee — the round-21 fix region.
        2 => (
            arb_int_expr(s4.clone(), depth - 1),
            arb_int_expr(s4.clone(), depth - 1),
            arb_int_expr(s4.clone(), depth - 1),
            arb_int_expr(s4, depth - 1),
        ).prop_map(|(a, b, hit, miss)| {
            format!(
                "(match (({a}), ({b})) {{ (0, _) | (_, 0) -> ({hit}) _ -> ({miss}) }})"
            )
        }),
        // Immediate closure invocation: `{ x -> body }(arg)`.  Exercises
        // closure creation, upvalue resolution, and the call path.
        2 => (arb_ident(), arb_int_expr(s5.clone(), depth - 1))
            .prop_flat_map(move |(param, arg)| {
                let inner = s5.with(&param);
                arb_int_expr(inner, depth - 1).prop_map(move |body| {
                    format!("({{ {param} -> ({body}) }}({arg}))")
                })
            }),
        // Pipe into an adder closure: `(x |> { v -> v + bias })`.
        1 => (
            arb_int_expr(s6.clone(), depth - 1),
            arb_int_expr(s6, depth - 1),
        ).prop_map(|(seed, bias)| {
            format!("(({seed}) |> {{ v -> v + ({bias}) }})")
        }),
        // Tuple destructure via let: `{ let (u, v) = (a, b); u + v }`.
        1 => (
            arb_int_expr(s7.clone(), depth - 1),
            arb_int_expr(s7, depth - 1),
        ).prop_map(|(a, b)| {
            format!("{{ let (u, v) = (({a}), ({b})); (u + v) }}")
        }),
    ]
    .boxed()
}

/// Leaf productions for an Int-typed expression.
fn leaf_int_expr(scope: Scope) -> BoxedStrategy<String> {
    let maybe_name = scope.any_name().map(str::to_string);
    let var_strat: BoxedStrategy<String> = match maybe_name {
        Some(n) => Just(format!("({n})")).boxed(),
        None => Just(String::from("(0)")).boxed(),
    };
    prop_oneof![
        4 => (-100i64..100).prop_map(|n| format!("({n})")),
        3 => var_strat,
        // `list.length` of a tiny inline list — exercises the stdlib
        // builtin dispatch path without risking a divergent type.
        1 => (0u64..5).prop_map(|n| {
            let elems: Vec<String> = (0..n).map(|i| format!("({i})")).collect();
            format!("list.length([{}])", elems.join(", "))
        }),
    ]
    .boxed()
}

fn arb_int_op() -> impl Strategy<Value = &'static str> {
    prop_oneof![Just("+"), Just("-"), Just("*")]
}

/// Wrap a generated int expression in a full program: `import list`
/// (needed by the `list.length` leaf) plus `fn main() { <expr> }`.
fn arb_int_program() -> impl Strategy<Value = String> {
    (0u32..5).prop_flat_map(|depth| {
        arb_int_expr(Scope::default(), depth)
            .prop_map(|expr| format!("import list\n\nfn main() {{\n  {expr}\n}}\n"))
    })
}

// ── Proptest: generated programs all run cleanly ─────────────────────

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 200,
        max_shrink_iters: 512,
        .. ProptestConfig::default()
    })]

    /// Every generated program that typechecks must compile and execute
    /// without panicking, without hitting the wall-clock cap, and
    /// without surfacing an `"internal:"`-prefixed VM error.  User-level
    /// errors (integer overflow, division by zero, …) are acceptable.
    #[test]
    fn generated_int_programs_run_without_panic(source in arb_int_program()) {
        let outcome = compile_and_run_capped(&source)?;
        match outcome {
            RunOutcome::Ok(_) | RunOutcome::UserError(_) | RunOutcome::GeneratorReject(_) => {
                // All acceptable.
            }
        }
    }

    /// Determinism: running the same generated program twice must yield
    /// the same outcome.  Int programs have no nondeterministic input,
    /// so the result must be byte-identical across runs.
    #[test]
    fn generated_int_programs_are_deterministic(source in arb_int_program()) {
        let first = compile_and_run_capped(&source)?;
        let second = compile_and_run_capped(&source)?;
        match (&first, &second) {
            (RunOutcome::GeneratorReject(_), _) | (_, RunOutcome::GeneratorReject(_)) => {
                // At least one run was rejected upstream — can't compare.
            }
            (RunOutcome::Ok(a), RunOutcome::Ok(b)) => {
                let la = format!("{a:?}");
                let lb = format!("{b:?}");
                prop_assert_eq!(la, lb, "determinism failed\nsource:\n{}", source);
            }
            (RunOutcome::UserError(a), RunOutcome::UserError(b)) => {
                prop_assert_eq!(a, b, "determinism failed (err)\nsource:\n{}", source);
            }
            (a, b) => {
                return Err(TestCaseError::fail(format!(
                    "determinism failed: outcome shape changed\nfirst: {a:?}\nsecond: {b:?}\nsource:\n{source}"
                )));
            }
        }
    }
}
