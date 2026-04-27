//! Concurrency-dependent integration tests.
//!
//! Split out from `tests/integration.rs` so that scheduler poisoning from
//! channel/task/select/deadlock tests is contained to this binary's process.
//! Cargo runs each `tests/*.rs` file as its own process; keeping these tests
//! here means a poisoned scheduler in one of them does not hang the rest of
//! the integration suite.

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::types::Severity;
use silt::value::Value;
use silt::vm::Vm;
use std::sync::Arc;

fn run(input: &str) -> Value {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = compiler.compile_program(&program).expect("compile error");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    vm.run(script).expect("runtime error")
}

fn run_ok(input: &str) {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = compiler.compile_program(&program).expect("compile error");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    vm.run(script).expect("runtime error");
}

fn run_err(input: &str) -> String {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = match compiler.compile_program(&program) {
        Ok(f) => f,
        Err(e) => return e.message,
    };
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    let err = vm.run(script).expect_err("expected runtime error");
    format!("{err}")
}

/// Like `run`, but asserts that the typechecker produces no hard errors
/// (warnings are allowed). This catches typechecker regressions that would
/// incorrectly reject valid code.
#[allow(dead_code)]
fn run_typed(input: &str) -> Value {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let type_errors = silt::typechecker::check(&mut program);
    let hard_errors: Vec<_> = type_errors
        .iter()
        .filter(|e| e.severity == Severity::Error)
        .collect();
    assert!(
        hard_errors.is_empty(),
        "expected no type errors, got: {:?}",
        hard_errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
    let mut compiler = Compiler::new();
    let functions = compiler.compile_program(&program).expect("compile error");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    vm.run(script).expect("runtime error")
}

/// Like `run_ok`, but asserts that the typechecker produces no hard errors.
#[allow(dead_code)]
fn run_typed_ok(input: &str) {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let type_errors = silt::typechecker::check(&mut program);
    let hard_errors: Vec<_> = type_errors
        .iter()
        .filter(|e| e.severity == Severity::Error)
        .collect();
    assert!(
        hard_errors.is_empty(),
        "expected no type errors, got: {:?}",
        hard_errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
    let mut compiler = Compiler::new();
    let functions = compiler.compile_program(&program).expect("compile error");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    vm.run(script).expect("runtime error");
}

#[test]
fn test_channel_with_integers() {
    let result = run(r#"
import channel
import task
fn main() {
  let ch = channel.new(5)

  let producer = task.spawn(fn() {
    channel.send(ch, 10)
    channel.send(ch, 20)
    channel.send(ch, 30)
  })

  task.join(producer)

  let Message(a) = channel.receive(ch)
  let Message(b) = channel.receive(ch)
  let Message(c) = channel.receive(ch)
  a + b + c
}
    "#);
    assert_eq!(result, Value::Int(60));
}

#[test]
fn test_task_failure_span_points_at_task_body() {
    // Regression test for commit d9dd98b: when a spawned closure fails, the
    // user-visible error's source span must point at the failing instruction
    // *inside the closure*, not at the `task.join` site in the parent.
    //
    // Line layout of the source string below (line 1 is the leading newline):
    //   line 2: import task
    //   line 3: fn main() {
    //   line 4:   let h = task.spawn(fn() {
    //   line 5:     let y = 10
    //   line 6:     let x = y / 0    <-- division-by-zero; this is THE line
    //   line 7:     x
    //   line 8:   })
    //   line 9:   task.join(h)       <-- join site; must NOT be reported
    //   line 10: }
    let src = r#"
import task
fn main() {
  let h = task.spawn(fn() {
    let y = 10
    let x = y / 0
    x
  })
  task.join(h)
}
    "#;
    const DIVISION_LINE: usize = 6;
    const JOIN_LINE: usize = 9;

    let tokens = Lexer::new(src).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = compiler.compile_program(&program).expect("compile error");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    let err = vm.run(script).expect_err("expected runtime error from 1/0");

    // Message must still mention division so diagnostics are not degraded.
    assert!(
        err.message.contains("division"),
        "error message should mention division, got: {}",
        err.message
    );

    // The whole point of the regression: the span must exist and must point
    // at the division line inside the spawned closure, NOT the join site.
    let span = err
        .span
        .expect("task failure must carry a source span after enrich_error");
    assert_eq!(
        span.line, DIVISION_LINE,
        "expected error span at the division line {} inside the spawned \
         closure, got line {} (message: {}). If this points at line {} \
         (task.join), the scheduler is re-wrapping errors at the join site \
         instead of preserving the child VM's original span.",
        DIVISION_LINE, span.line, err.message, JOIN_LINE
    );
    assert_ne!(
        span.line, JOIN_LINE,
        "error span must not point at the task.join site (line {})",
        JOIN_LINE
    );
}

#[test]
fn test_select_expression() {
    let result = run(r#"
import channel
fn main() {
  let ch1 = channel.new(10)
  let ch2 = channel.new(10)

  channel.send(ch2, "from ch2")

  match channel.select([Recv(ch1), Recv(ch2)]) {
    (^ch1, Message(msg)) -> "got from ch1"
    (^ch2, Message(msg)) -> msg
    _ -> "none"
  }
}
    "#);
    assert_eq!(result, Value::String("from ch2".into()));
}

#[test]
fn test_select_with_spawn() {
    let result = run(r#"
import channel
import task
fn main() {
  let ch1 = channel.new(10)
  let ch2 = channel.new(10)

  let p = task.spawn(fn() {
    channel.send(ch1, "first")
  })
  task.join(p)

  match channel.select([Recv(ch1), Recv(ch2)]) {
    (^ch1, Message(msg)) -> msg
    (^ch2, Message(msg)) -> msg
    _ -> "none"
  }
}
    "#);
    assert_eq!(result, Value::String("first".into()));
}

#[test]
fn test_channel_timeout_fires() {
    // channel.timeout should close after the specified duration
    let result = run(r#"
import channel
fn main() {
  let timer = channel.timeout(50)
  let result = channel.receive(timer)
  match result {
    Closed -> "timed_out"
    _ -> "unexpected"
  }
}
    "#);
    assert_eq!(result, Value::String("timed_out".into()));
}

#[test]
fn test_channel_timeout_with_select() {
    // select with a timeout channel — timeout should fire when no data arrives
    let result = run(r#"
import channel
fn main() {
  let ch = channel.new(1)
  let timer = channel.timeout(50)

  match channel.select([Recv(ch), Recv(timer)]) {
    (_, Closed) -> "timeout"
    (_, Message(_)) -> "data"
    _ -> "other"
  }
}
    "#);
    assert_eq!(result, Value::String("timeout".into()));
}

#[test]
fn test_channel_timeout_data_beats_timeout() {
    // If data arrives before timeout, select should return the data
    let result = run(r#"
import channel
import task
fn main() {
  let ch = channel.new(1)
  let timer = channel.timeout(5000)

  channel.send(ch, "fast")

  match channel.select([Recv(ch), Recv(timer)]) {
    (_, Message(val)) -> val
    (_, Closed) -> "timeout"
    _ -> "other"
  }
}
    "#);
    assert_eq!(result, Value::String("fast".into()));
}

#[test]
fn test_select_send_operation() {
    // Select with a send operation — should succeed when channel has room
    let result = run(r#"
import channel
fn main() {
  let ch = channel.new(1)

  match channel.select([Send(ch, 42)]) {
    (_, Sent) -> "sent"
    _ -> "other"
  }
}
    "#);
    assert_eq!(result, Value::String("sent".into()));
}

#[test]
fn test_select_mixed_send_receive() {
    // Select with both send and receive operations.
    // Both operations are simultaneously ready (inbox has data, outbox has
    // capacity) and select chooses fairly between them, so either outcome
    // is valid — we just assert the select resolved to one of the ready ops.
    let result = run(r#"
import channel
fn main() {
  let inbox = channel.new(1)
  let outbox = channel.new(1)

  channel.send(inbox, "hello")

  match channel.select([Recv(inbox), Send(outbox, "world")]) {
    (_, Message(val)) -> val
    (_, Sent) -> "sent"
    _ -> "other"
  }
}
    "#);
    assert!(
        matches!(&result, Value::String(s) if s == "hello" || s == "sent"),
        "expected one of the ready select arms, got {result:?}"
    );
}

#[test]
fn test_select_send_with_timeout() {
    // Select: send to a full channel with a timeout
    let result = run(r#"
import channel
fn main() {
  let ch = channel.new(1)
  channel.send(ch, "fill")

  let timer = channel.timeout(50)

  match channel.select([Send(ch, "more"), Recv(timer)]) {
    (_, Closed) -> "timeout"
    (_, Sent) -> "sent"
    _ -> "other"
  }
}
    "#);
    assert_eq!(result, Value::String("timeout".into()));
}

#[test]
fn test_channel_passing_complex_values() {
    let result = run(r#"
import channel
fn main() {
  let ch = channel.new(5)
  channel.send(ch, [1, 2, 3])
  let Message(list) = channel.receive(ch)
  list
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![Value::Int(1), Value::Int(2), Value::Int(3),]))
    );
}

#[test]
fn test_channel_close() {
    // After close, receive on empty channel returns Closed
    let result = run(r#"
import channel
fn main() {
  let ch = channel.new(10)
  channel.send(ch, 1)
  channel.close(ch)
  let Message(a) = channel.receive(ch)
  let b = channel.receive(ch)
  match b {
    Closed -> a
    _ -> -1
  }
}
    "#);
    assert_eq!(result, Value::Int(1));
}

#[test]
fn test_channel_module_qualified() {
    let result = run(r#"
import channel
fn main() {
  let ch = channel.new(10)
  channel.send(ch, 42)
  let Message(val) = channel.receive(ch)
  val
}
    "#);
    assert_eq!(result, Value::Int(42));
}

#[test]
fn test_channel_module_qualified_close() {
    let result = run(r#"
import channel
fn main() {
  let ch = channel.new(10)
  channel.send(ch, 1)
  channel.close(ch)
  let Message(a) = channel.receive(ch)
  let b = channel.receive(ch)
  match b {
    Closed -> a
    _ -> -1
  }
}
    "#);
    assert_eq!(result, Value::Int(1));
}

#[test]
fn test_channel_module_try_send_receive() {
    let result = run(r#"
import channel
fn main() {
  let ch = channel.new(1)
  channel.try_send(ch, 99)
  channel.try_receive(ch)
}
    "#);
    assert_eq!(
        result,
        Value::Variant("Message".into(), vec![Value::Int(99)])
    );
}

#[test]
fn test_channel_select_basic() {
    let result = run(r#"
import channel
fn main() {
  let ch1 = channel.new(10)
  let ch2 = channel.new(10)
  channel.send(ch2, "from ch2")

  match channel.select([Recv(ch1), Recv(ch2)]) {
    (^ch2, Message(msg)) -> msg
    _ -> "unexpected"
  }
}
    "#);
    assert_eq!(result, Value::String("from ch2".into()));
}

#[test]
fn test_channel_select_with_spawn() {
    let result = run(r#"
import channel
import task
fn main() {
  let ch1 = channel.new(10)
  let ch2 = channel.new(10)

  let p = task.spawn(fn() {
    channel.send(ch1, "first")
  })
  task.join(p)

  match channel.select([Recv(ch1), Recv(ch2)]) {
    (^ch1, Message(msg)) -> msg
    (^ch2, Message(msg)) -> msg
    _ -> "none"
  }
}
    "#);
    assert_eq!(result, Value::String("first".into()));
}

#[test]
fn test_channel_select_returns_tuple() {
    let result = run(r#"
import channel
fn main() {
  let ch = channel.new(10)
  channel.send(ch, 42)

  let result = channel.select([Recv(ch)])
  match result {
    (_, Message(val)) -> val
    _ -> 0
  }
}
    "#);
    assert_eq!(result, Value::Int(42));
}

#[test]
fn test_select_with_multiple_producers() {
    // Multiple producers on different channels, select should pick up messages.
    let result = run(r#"
import channel
import task
fn main() {
  let ch1 = channel.new(10)
  let ch2 = channel.new(10)
  let ch3 = channel.new(10)

  let p1 = task.spawn(fn() { channel.send(ch1, 1) })
  let p2 = task.spawn(fn() { channel.send(ch2, 2) })
  let p3 = task.spawn(fn() { channel.send(ch3, 3) })

  task.join(p1)
  task.join(p2)
  task.join(p3)

  -- Select three times to collect all messages
  loop i = 0, total = 0 {
    match i >= 3 {
      true -> total
      _ -> {
        let (_, msg) = channel.select([Recv(ch1), Recv(ch2), Recv(ch3)])
        match msg {
          Message(val) -> loop(i + 1, total + val)
          _ -> total
        }
      }
    }
  }
}
    "#);
    assert_eq!(result, Value::Int(6));
}

#[test]
fn test_channel_receive_blocks_scheduled_task() {
    // A scheduled task blocks on receive, gets woken by a send from another task.
    let result = run(r#"
import channel
import task
fn main() {
  let ch = channel.new(1)

  -- Consumer spawns first but has to wait for producer.
  let consumer = task.spawn(fn() {
    let Message(val) = channel.receive(ch)
    val * 2
  })

  -- Producer sends after a moment.
  let producer = task.spawn(fn() {
    channel.send(ch, 21)
  })

  task.join(producer)
  task.join(consumer)
}
    "#);
    assert_eq!(result, Value::Int(42));
}

#[test]
fn test_deadlock_detected_single_task() {
    // A spawned task blocks on receive from a channel that nobody sends to.
    // The main thread joins the task. Deadlock detection should fire and
    // report an error rather than hanging forever.
    let err = run_err(
        r#"
import channel
import task
fn main() {
  let ch = channel.new(1)
  let h = task.spawn(fn() {
    channel.receive(ch)
  })
  task.join(h)
}
    "#,
    );
    assert!(
        err.contains("deadlock"),
        "expected deadlock error, got: {err}"
    );
}

#[test]
fn test_deadlock_detected_two_tasks() {
    // Two tasks each waiting on the other's channel — classic deadlock.
    let err = run_err(
        r#"
import channel
import task
fn main() {
  let ch1 = channel.new(1)
  let ch2 = channel.new(1)
  let t1 = task.spawn(fn() {
    channel.receive(ch1)
  })
  let t2 = task.spawn(fn() {
    channel.receive(ch2)
  })
  task.join(t1)
}
    "#,
    );
    assert!(
        err.contains("deadlock"),
        "expected deadlock error, got: {err}"
    );
}

#[test]
fn test_select_with_only_send_operations() {
    // Select with only send operations on channels with room.
    let result = run(r#"
import channel
fn main() {
  let ch1 = channel.new(1)
  let ch2 = channel.new(1)

  match channel.select([Send(ch1, "a"), Send(ch2, "b")]) {
    (_, Sent) -> "sent_ok"
    _ -> "unexpected"
  }
}
    "#);
    assert_eq!(result, Value::String("sent_ok".into()));
}

#[test]
fn test_select_send_succeeds_before_receive() {
    // Send should succeed immediately when channel has space, even if receive ops also present.
    let result = run(r#"
import channel
fn main() {
  let empty_ch = channel.new(10)
  let send_ch = channel.new(1)

  -- empty_ch has nothing to receive, but send_ch has room
  match channel.select([Recv(empty_ch), Send(send_ch, 99)]) {
    (_, Sent) -> "sent_first"
    (_, Message(_)) -> "received"
    _ -> "other"
  }
}
    "#);
    assert_eq!(result, Value::String("sent_first".into()));
}

#[test]
fn test_select_send_to_closed_channel() {
    // Select with send to a closed channel should return Closed.
    let result = run(r#"
import channel
fn main() {
  let ch = channel.new(1)
  channel.close(ch)

  match channel.select([Send(ch, 42)]) {
    (_, Closed) -> "closed"
    (_, Sent) -> "sent"
    _ -> "other"
  }
}
    "#);
    assert_eq!(result, Value::String("closed".into()));
}

#[test]
fn test_select_receive_and_send_same_channel() {
    // Select with both receive and send on the same buffered channel.
    // With data present and capacity available, BOTH operations are ready.
    // Fair select chooses between them, so either outcome is valid.
    let result = run(r#"
import channel
fn main() {
  let ch = channel.new(5)
  channel.send(ch, 100)

  match channel.select([Recv(ch), Send(ch, 200)]) {
    (_, Message(val)) -> val
    (_, Sent) -> -1
    _ -> -2
  }
}
    "#);
    assert!(
        matches!(&result, Value::Int(100) | Value::Int(-1)),
        "expected receive (100) or send (-1), got {result:?}"
    );
}

#[test]
fn test_channel_each_multiple_consumers_all_items() {
    // Verify channel.each with multiple consumers processes all items.
    let result = run(r#"
import channel
import task
fn main() {
  let jobs = channel.new(10)
  let results = channel.new(10)

  channel.send(jobs, 1)
  channel.send(jobs, 2)
  channel.send(jobs, 3)
  channel.send(jobs, 4)
  channel.close(jobs)

  let w1 = task.spawn(fn() {
    channel.each(jobs) { n ->
      channel.send(results, n + 100)
    }
  })
  let w2 = task.spawn(fn() {
    channel.each(jobs) { n ->
      channel.send(results, n + 200)
    }
  })

  task.join(w1)
  task.join(w2)

  -- Collect 4 results
  let Message(a) = channel.receive(results)
  let Message(b) = channel.receive(results)
  let Message(c) = channel.receive(results)
  let Message(d) = channel.receive(results)

  -- Each original value is processed exactly once.
  -- Strip the worker prefix and sum the base values.
  let base_a = match a > 200 { true -> a - 200  _ -> a - 100 }
  let base_b = match b > 200 { true -> b - 200  _ -> b - 100 }
  let base_c = match c > 200 { true -> c - 200  _ -> c - 100 }
  let base_d = match d > 200 { true -> d - 200  _ -> d - 100 }
  base_a + base_b + base_c + base_d
}
    "#);
    // 1 + 2 + 3 + 4 = 10
    assert_eq!(result, Value::Int(10));
}

#[test]
fn test_double_close_channel() {
    // Closing a channel twice should not panic.
    run_ok(
        r#"
import channel
fn main() {
  let ch = channel.new(5)
  channel.close(ch)
  channel.close(ch)
}
    "#,
    );
}

#[test]
fn test_select_on_empty_list_errors() {
    // Select on an empty list should produce an error.
    let err = run_err(
        r#"
import channel
fn main() {
  channel.select([])
}
    "#,
    );
    assert!(err.contains("at least one operation"), "got: {err}");
}

#[test]
fn test_select_multiple_ready_channels() {
    // When multiple channels have data, select picks one (non-deterministic but valid).
    let result = run(r#"
import channel
fn main() {
  let ch1 = channel.new(1)
  let ch2 = channel.new(1)
  let ch3 = channel.new(1)

  channel.send(ch1, 10)
  channel.send(ch2, 20)
  channel.send(ch3, 30)

  -- Select should pick one of the three
  let (_, msg) = channel.select([Recv(ch1), Recv(ch2), Recv(ch3)])
  match msg {
    Message(val) -> val > 0
    _ -> false
  }
}
    "#);
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn test_select_timeout_with_multiple_empty_channels() {
    // Select across multiple empty channels plus a timeout — timeout should win.
    let result = run(r#"
import channel
fn main() {
  let ch1 = channel.new(1)
  let ch2 = channel.new(1)
  let ch3 = channel.new(1)
  let timer = channel.timeout(50)

  match channel.select([Recv(ch1), Recv(ch2), Recv(ch3), Recv(timer)]) {
    (_, Closed) -> "timeout_won"
    (_, Message(_)) -> "got_data"
    _ -> "other"
  }
}
    "#);
    assert_eq!(result, Value::String("timeout_won".into()));
}

#[test]
fn test_channel_close_wakes_blocked_receiver() {
    // A task blocked on receive should wake up when channel is closed.
    let result = run(r#"
import channel
import task
fn main() {
  let ch = channel.new()

  let receiver = task.spawn(fn() {
    match channel.receive(ch) {
      Closed -> "woken_by_close"
      Message(_) -> "got_message"
    }
  })

  -- Close the channel to wake the blocked receiver
  channel.close(ch)
  task.join(receiver)
}
    "#);
    assert_eq!(result, Value::String("woken_by_close".into()));
}

#[test]
fn test_channel_type_annotation_parameterized() {
    // Locks in 3a4edd6 G1: `Channel(T)` must resolve to `Type::Channel(T)`,
    // not the catch-all `Type::Generic("Channel", [T])`. Without the fix,
    // the annotation was a different nominal type than the value returned
    // by `channel.new(...)` and the `let` binding failed to type-check.
    let result = run(r#"
import channel
fn main() {
  let ch: Channel(Int) = channel.new(10)
  channel.send(ch, 42)
  match channel.receive(ch) {
    Message(v) -> v
    Sent -> 0
    Closed -> 0
    Empty -> 0
  }
}
"#);
    assert_eq!(result, Value::Int(42));
}

#[test]
fn test_channel_type_annotation_unparameterized() {
    // Locks in 3a4edd6 G1: bare `Channel` (no type arg) must also be
    // recognized as `Type::Channel(fresh_var)` so that partial annotations
    // remain valid. Covers the other branch of the G1 fix.
    let result = run(r#"
import channel
fn main() {
  let ch: Channel = channel.new(10)
  channel.send(ch, 7)
  match channel.receive(ch) {
    Message(v) -> v
    Sent -> 0
    Closed -> 0
    Empty -> 0
  }
}
"#);
    assert_eq!(result, Value::Int(7));
}
