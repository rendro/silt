//! The two settings of a VM the oracle runs its programs with: the time
//! slice (`Vm::set_time_slice`) and the step budget
//! (`Vm::set_step_budget`).

use silt::session::testing::compile_str;
use silt::{Buffer, HostIo, Value, Vm, VmError};

/// The value or the error of `main`, and what the program and the
/// runtime wrote, on a VM that `prepare` has set up.
fn run_whole(source: &str, prepare: impl FnOnce(&mut Vm)) -> (Result<Value, VmError>, String) {
    let program = compile_str(source).unwrap_or_else(|errors| panic!("{errors:?}"));
    let out = Buffer::new();
    let mut vm = Vm::new(HostIo::buffer(&out));
    prepare(&mut vm);
    let result = vm.run_program(&program);
    if result.is_ok() {
        vm.settle();
    }
    drop(vm);
    (result, out.contents())
}

/// [`run_whole`], with the message of an error for the error.
fn run_with(source: &str, prepare: impl FnOnce(&mut Vm)) -> (Result<Value, String>, String) {
    let (result, output) = run_whole(source, prepare);
    (result.map_err(|e| e.message), output)
}

/// A program whose own thread is inside a builtin that calls back into
/// it most of the time (`list.map`, `list.fold`, `list.sort_by`, a
/// `Display` impl shown by an interpolation), calls a method, loops,
/// recurses and takes a pattern apart, and whose task and channel do
/// the same on a worker.
const BUSY: &str = r#"
import channel
import list
import string
import task

type Point { x: Int, y: Int }

trait Display for Point {
  fn display(self) -> String { "<{self.x},{self.y}>" }
}

fn fib(n) {
  match n < 2 {
    true -> n
    false -> fib(n - 1) + fib(n - 2)
  }
}

fn sum_to(n) {
  loop i = 0, acc = 0 {
    match i > n {
      true -> acc
      false -> loop(i + 1, acc + i)
    }
  }
}

fn main() {
  let points = [3, 1, 2] |> list.map({ n -> Point { x: n, y: fib(n + 5) } })
  let sorted = list.sort_by(points, { p -> p.x })
  println("{sorted}")
  println(list.fold(sorted, 0, { acc, p -> acc + p.x * p.y }))
  let ch = channel.new(0)
  let worker = task.spawn({ ->
    list.each([1, 2, 3], { n -> channel.send(ch, "{Point { x: n, y: sum_to(n * 10) }}") })
    channel.close(ch)
    sum_to(100)
  })
  channel.each(ch, { text -> println(string.to_upper(text)) })
  println(task.join(worker))
  match sorted {
    [first, ..rest] -> first.x + list.length(rest)
    [] -> 0
  }
}
"#;

/// What a program computes and writes is the same whatever the slice,
/// down to a stop after every single step.
#[test]
fn a_program_runs_the_same_at_every_time_slice() {
    let (value, output) = run_with(BUSY, |_| {});
    assert_eq!(value, Ok(Value::Int(3)), "{output}");
    assert_eq!(
        output,
        "[<1,8>, <2,13>, <3,21>]\n97\n<1,55>\n<2,210>\n<3,465>\n5050\n"
    );
    for slice in [1, 2, 3, 7, 2000] {
        let sliced = run_with(BUSY, |vm| vm.set_time_slice(slice));
        assert_eq!(sliced, (value.clone(), output.clone()), "slice {slice}");
    }
    // A slice of no steps is one of one step.
    let none = run_with(BUSY, |vm| vm.set_time_slice(0));
    assert_eq!(none, (value, output));
}

/// A program that never ends by itself ends when its steps are used
/// up, with an error that says so apart from its message.
#[test]
fn a_step_budget_ends_a_program_that_does_not_end() {
    let endless = "fn main() {\n  loop {\n    loop()\n  }\n}\n";
    let (result, _) = run_whole(endless, |vm| vm.set_step_budget(100_000));
    let error = result.unwrap_err();
    assert!(error.out_of_steps, "{error:?}");
    assert!(!error.type_confusion);
    assert_eq!(error.message, "the step budget is used up");
    // A runtime error of the program is not that.
    let (result, _) = run_whole("fn main() {\n  1 / 0\n}\n", |vm| {
        vm.set_step_budget(100_000)
    });
    assert!(!result.unwrap_err().out_of_steps);
}

/// With slice 1 the steps are counted one by one: the program is ended
/// after exactly as many as it was given, so what it wrote until then
/// is the same in every run, and more steps write more.
#[test]
fn at_slice_one_the_budget_is_exact() {
    let counting = "fn main() {\n  loop i = 0 {\n    println(i)\n    loop(i + 1)\n  }\n}\n";
    let lines = |steps: u64| {
        let (result, output) = run_whole(counting, |vm| {
            vm.set_time_slice(1);
            vm.set_step_budget(steps);
        });
        assert!(result.unwrap_err().out_of_steps);
        output.lines().count()
    };
    let (few, more) = (lines(1_000), lines(2_000));
    assert!(few > 10, "{few} lines in 1,000 steps");
    assert_eq!(few, lines(1_000));
    // The loop costs the same number of steps each time round.
    assert!((few * 2).abs_diff(more) <= 2, "{few} lines, then {more}");
    // No step, no line.
    assert_eq!(lines(0), 0);
}

/// A program that ends within its budget is not touched by it, and is
/// counted at the scheduler's own slice when none is set.
#[test]
fn a_program_within_its_budget_runs_as_without_one() {
    let free = run_with(BUSY, |_| {});
    assert_eq!(run_with(BUSY, |vm| vm.set_step_budget(10_000_000)), free);
    let sliced = run_with(BUSY, |vm| {
        vm.set_time_slice(1);
        vm.set_step_budget(10_000_000);
    });
    assert_eq!(sliced, free);
}

/// The budget is the whole program's: a task that does not end is ended
/// by it, a join of the task gives the error on, and a task nobody
/// joins does not keep the program from ending.
#[test]
fn the_budget_ends_the_tasks_of_a_program_too() {
    let spin = "fn spin() {\n  loop {\n    loop()\n  }\n}\n";
    let joined = format!(
        "import task\n{spin}fn main() {{\n  let h = task.spawn(spin)\n  task.join(h)\n}}\n"
    );
    let (result, _) = run_whole(&joined, |vm| vm.set_step_budget(100_000));
    let error = result.unwrap_err();
    assert!(error.out_of_steps, "{error:?}");
    // (The budget's error: the join's, or `main`'s own when its next
    // slice does not start.)
    let message = &error.message;
    assert!(message.ends_with("the step budget is used up"), "{error:?}");

    // `main` returns at once; `settle` waits for the task, which the
    // budget ends, and the runtime reports its failure.
    let unjoined =
        format!("import task\n{spin}fn main() {{\n  let h = task.spawn(spin)\n  7\n}}\n");
    let (result, report) = run_whole(&unjoined, |vm| vm.set_step_budget(100_000));
    assert_eq!(result.unwrap(), Value::Int(7));
    assert!(report.contains("the step budget is used up"), "{report}");
}

/// Two tasks that hand a value to and fro for ever never run a slice to
/// its end: each wait counts as a step, so the budget ends them too.
#[test]
fn the_budget_ends_tasks_that_only_wait_for_each_other() {
    let source = r#"
import channel
import task

fn relay(from, to) {
  loop {
    match channel.receive(from) {
      channel.Message(n) -> channel.send(to, n + 1)
      _ -> ()
    }
    loop()
  }
}

fn main() {
  let a = channel.new(0)
  let b = channel.new(0)
  let left = task.spawn({ -> relay(a, b) })
  let right = task.spawn({ -> relay(b, a) })
  channel.send(a, 0)
  task.join(left)
}
"#;
    // Whichever task the budget ends first, `main` ends for it: with
    // the error of the task it joins, or, when that one waits for the
    // other for ever, with a deadlock that is the budget's too.
    for _ in 0..20 {
        let (result, _) = run_whole(source, |vm| vm.set_step_budget(20_000));
        let error = result.unwrap_err();
        assert!(error.out_of_steps, "{error:?}");
    }
}

/// The three settings a budget must work under: no slice set, the
/// scheduler's own, and one step.
const SLICES: [Option<usize>; 3] = [None, Some(2000), Some(1)];

/// `task.deadline` ends its function at every time slice, whatever the
/// function waits for when its time is up: a sleep, a channel, a join,
/// a wait deep inside a callback of a builtin or a stage of a stream;
/// and a function that never waits at all, where tasks have a time
/// slice. And a function that returns in time is `Ok` of its value at
/// every slice.
#[test]
fn a_deadline_ends_its_function_at_every_slice() {
    let doors = [
        ("a sleep", "time.sleep(time.hours(1))"),
        ("a receive", "channel.receive(never)"),
        ("a send", "channel.send(never, 1)"),
        (
            "a join",
            "task.join(task.spawn({ -> channel.receive(never) }))",
        ),
        (
            "inside a callback",
            "list.fold([1, 2], 0) { acc, i -> acc + recv(never) }",
        ),
        (
            "inside a stream stage",
            "stream.from_list([1, 2]) |> stream.map { x -> x + recv(never) } |> stream.collect",
        ),
        (
            "an inner deadline that is longer",
            "task.deadline(time.hours(1), { -> time.sleep(time.hours(1)) })",
        ),
    ];
    let program = |door: &str, dur: &str| {
        format!(
            "import channel\nimport list\nimport stream\nimport task\nimport time\n\n\
             fn recv(ch) {{\n  match channel.receive(ch) {{\n    channel.Message(v) -> v\n    _ -> 0\n  }}\n}}\n\n\
             fn count(n, acc) {{\n  match n {{\n    0 -> acc\n    _ -> count(n - 1, acc + 1)\n  }}\n}}\n\n\
             fn main() {{\n  let never: Channel(Int) = channel.new(0)\n  \
             match task.deadline({dur}, {{ -> {door} }}) {{\n    \
             Ok(_) -> \"returned\"\n    Err(task.Timeout) -> \"timeout\"\n    Err(_) -> \"other\"\n  }}\n}}\n"
        )
    };
    let at_each = |slices: &[Option<usize>], what: &str, source: &str, expected: &str| {
        for slice in slices.iter().copied() {
            let (result, _) = run_with(source, |vm| {
                if let Some(slice) = slice {
                    vm.set_time_slice(slice);
                }
            });
            assert_eq!(
                result,
                Ok(Value::String(expected.into())),
                "{what} at slice {slice:?}"
            );
        }
    };
    for (what, door) in doors {
        at_each(&SLICES, what, &program(door, "time.ms(20)"), "timeout");
    }
    // A function that never waits is ended 2,000 steps after the first
    // slice that begins when its time is over, whatever the slice of
    // the VM.
    at_each(
        &SLICES,
        "a function that only computes",
        &program("count(2000000000, 0)", "time.ms(20)"),
        "timeout",
    );
    at_each(
        &SLICES,
        "a function that returns in time",
        &program("count(1000, 0)", "time.hours(1)"),
        "returned",
    );
}

/// Under `task.deadline` nothing is lost at any time slice: a wait of
/// the function ended by its arm, and the caller has `Ok` of what the
/// function made of it, or the wait ended by the deadline and nothing
/// was taken or sent. Each program counts what arrived against what
/// was sent, with deadlines about as long as the waits they are
/// around, so that many of them pass at the moment a value comes. No
/// program says how many deadlines passed: that is the clock's.
///
/// At a slice of one step a function that has taken a value needs many
/// slices to return it; it has the steps of one standard slice for
/// that, whatever the slice of the VM.
#[test]
fn a_deadline_loses_nothing_at_every_slice() {
    const HEAD: &str = "import channel\nimport task\nimport time\n\n";
    // A receive: every number a rendezvous send handed over is
    // received.
    let receive = r#"
fn main() {
  let ch: Channel(Int) = channel.new(0)
  let producer = task.spawn { ->
    loop i = 0 {
      match i >= 60 {
        true -> channel.close(ch)
        false -> {
          time.sleep(time.ms(2))
          channel.send(ch, i)
          loop(i + 1)
        }
      }
    }
  }
  let r = loop got = 0, sum = 0 {
    match task.deadline(time.ms(2), { -> channel.receive(ch) }) {
      Ok(channel.Message(v)) -> loop(got + 1, sum + v)
      Ok(_) -> (got, sum)
      Err(_) -> loop(got, sum)
    }
  }
  task.join(producer)
  match r {
    (got, sum) -> "received {got}, sum {sum}"
  }
}
"#;
    // A send: a number arrives once for each send that was `Ok`.
    let send = r#"
fn main() {
  let ch: Channel(Int) = channel.new(0)
  let consumer = task.spawn { ->
    loop n = 0, sum = 0 {
      time.sleep(time.ms(2))
      match channel.receive(ch) {
        channel.Message(v) -> loop(n + 1, sum + v)
        _ -> (n, sum)
      }
    }
  }
  loop i = 0 {
    match i >= 60 {
      true -> ()
      false -> match task.deadline(time.ms(2), { -> channel.send(ch, i) }) {
        Ok(_) -> loop(i + 1)
        Err(_) -> loop(i)
      }
    }
  }
  channel.close(ch)
  match task.join(consumer) {
    (got, sum) -> "received {got}, sum {sum}"
  }
}
"#;
    // A value that is there: received with `Ok`, or left with `Err`.
    let held = r#"
fn round(dur, before) {
  let ch: Channel(Int) = channel.new(1)
  channel.send(ch, 7)
  let out = task.deadline(dur, { ->
    time.sleep(before)
    channel.receive(ch)
  })
  match (out, channel.try_receive(ch)) {
    (Ok(channel.Message(7)), channel.Empty) -> 0
    (Err(_), channel.Message(7)) -> 0
    _ -> 1
  }
}

fn wrong(dur, before) {
  loop i = 0, wrong = 0 {
    match i >= 150 {
      true -> wrong
      false -> loop(i + 1, wrong + round(dur, before))
    }
  }
}

fn main() {
  "wrong {wrong(time.micros(300), time.micros(300))} and {wrong(time.ms(0), time.ms(0))}"
}
"#;
    // A function that sleeps to just before its time, sends and
    // returns: `Ok` and sent, or `Err` and not sent.
    let late = r#"
fn main() {
  let sent: Channel(Int) = channel.new(4)
  let wrong = loop i = 0, wrong = 0 {
    match i >= 100 {
      true -> wrong
      false -> {
        let out = task.deadline(time.micros(2000), { ->
          time.sleep(time.micros(1200 + i % 5 * 200))
          channel.send(sent, i)
          i
        })
        let was_sent = match channel.try_receive(sent) {
          channel.Message(_) -> true
          _ -> false
        }
        match (out, was_sent) {
          (Ok(_), true) -> loop(i + 1, wrong)
          (Err(_), false) -> loop(i + 1, wrong)
          _ -> loop(i + 1, wrong + 1)
        }
      }
    }
  }
  "wrong {wrong}"
}
"#;
    // A select with a `Recv` arm and one with a `Send` arm.
    let select = r#"
fn pass(out, never, v) {
  let sent = task.deadline(time.ms(2), { ->
    match channel.select([channel.Send(out, v), channel.Recv(never)]) {
      (_, channel.Sent) -> true
      _ -> false
    }
  })
  match sent {
    Ok(true) -> ()
    _ -> pass(out, never, v)
  }
}

fn main() {
  let a: Channel(Int) = channel.new(0)
  let out: Channel(Int) = channel.new(0)
  let never: Channel(Int) = channel.new(0)
  let producer = task.spawn { ->
    loop i = 0 {
      match i >= 40 {
        true -> channel.close(a)
        false -> {
          time.sleep(time.ms(2))
          channel.send(a, i)
          loop(i + 1)
        }
      }
    }
  }
  let consumer = task.spawn { ->
    loop n = 0, sum = 0 {
      time.sleep(time.ms(2))
      match channel.receive(out) {
        channel.Message(v) -> loop(n + 1, sum + v)
        _ -> (n, sum)
      }
    }
  }
  loop {
    let got = task.deadline(time.ms(2), { ->
      match channel.select([channel.Recv(a), channel.Recv(never)]) {
        (_, r) -> r
      }
    })
    match got {
      Ok(channel.Message(v)) -> {
        pass(out, never, v)
        loop()
      }
      Ok(_) -> channel.close(out)
      Err(_) -> loop()
    }
  }
  task.join(producer)
  match task.join(consumer) {
    (got, sum) -> "arrived {got}, sum {sum}"
  }
}
"#;
    // A deadline inside a deadline whose outer time comes first.
    let nested = r#"
fn main() {
  let ch: Channel(Int) = channel.new(0)
  let producer = task.spawn { ->
    loop i = 0 {
      match i >= 60 {
        true -> channel.close(ch)
        false -> {
          time.sleep(time.ms(2))
          channel.send(ch, i)
          loop(i + 1)
        }
      }
    }
  }
  let r = loop got = 0, sum = 0, inner = 0 {
    let out = task.deadline(time.ms(2), { ->
      task.deadline(time.hours(1), { -> channel.receive(ch) })
    })
    match out {
      Ok(Ok(channel.Message(v))) -> loop(got + 1, sum + v, inner)
      Ok(Ok(_)) -> (got, sum, inner)
      Ok(Err(_)) -> loop(got, sum, inner + 1)
      Err(_) -> loop(got, sum, inner)
    }
  }
  task.join(producer)
  match r {
    (got, sum, inner) -> "received {got}, sum {sum}, inner timeouts {inner}"
  }
}
"#;
    let cases = [
        ("a receive", receive, "received 60, sum 1770"),
        ("a send", send, "received 60, sum 1770"),
        ("a value that is there", held, "wrong 0 and 0"),
        ("a function that sends late", late, "wrong 0"),
        ("a select", select, "arrived 40, sum 780"),
        (
            "a deadline in a deadline",
            nested,
            "received 60, sum 1770, inner timeouts 0",
        ),
    ];
    for (what, body, expected) in cases {
        let source = format!("{HEAD}{body}");
        for slice in SLICES {
            let (result, _) = run_with(&source, |vm| {
                if let Some(slice) = slice {
                    vm.set_time_slice(slice);
                }
            });
            assert_eq!(
                result,
                Ok(Value::String(expected.into())),
                "{what} at slice {slice:?}"
            );
        }
    }
}

/// The work a function does past its deadline is bounded in all,
/// whatever the function does and whatever ends its slices: about
/// 2,000 steps, counted one by one, in one count for the function and
/// the functions of the deadlines inside it.
///
/// Each case is a loop of 300,000 rounds under a deadline whose time is
/// over from the start (so nothing here depends on a clock), with a
/// mark for every round that was done. The loop must not reach its last
/// round, and the rounds it did reach are few: operations that are
/// ready at once (a `select` whose arm can be completed, a `receive`
/// of what is there, `try_send`, `try_receive`), such operations mixed
/// with computing, a `task.deadline` in every round (one of an hour,
/// one of no time, three in each other, one around ready selects). The
/// count of the rounds is the same at every time slice: a slice that
/// ends in a wait which is over at once counts like one that ran out.
///
/// And what a late function was ended in front of is still there: a
/// `select` that was not to complete its arm any more took nothing
/// and sent nothing.
#[test]
fn the_work_past_a_deadline_is_bounded_whatever_the_function_does() {
    let source = r#"
import channel
import list
import string
import task
import time

-- How many values `ch` holds, taken out of it.
fn drained(ch) {
  loop n = 0 {
    match channel.try_receive(ch) {
      channel.Message(_) -> loop(n + 1)
      _ -> n
    }
  }
}

fn filled(n) {
  let ch: Channel(Int) = channel.new(n)
  loop i = 0 {
    match i >= n {
      true -> ch
      false -> {
        channel.send(ch, i)
        loop(i + 1)
      }
    }
  }
}

fn count(n, acc) {
  match n {
    0 -> acc
    _ -> count(n - 1, acc + 1)
  }
}

-- `round(i)` 300,000 times, with a mark for each round that was done.
fn rounds(marks, round) {
  loop i = 0 {
    match i >= 300000 {
      true -> i
      false -> {
        round(i)
        let _ = channel.try_send(marks, i)
        loop(i + 1)
      }
    }
  }
}

-- The rounds a function that is `rounds` of `round` did under a
-- deadline that is over from the start; -1 if it was not ended.
fn late(round) {
  let marks: Channel(Int) = channel.new(20000)
  match task.deadline(time.ms(0), { -> rounds(marks, round) }) {
    Err(task.Timeout) -> drained(marks)
    _ -> -1
  }
}

fn main() {
  let closed: Channel(Int) = channel.new(1)
  channel.close(closed)
  let sink: Channel(Int) = channel.new(20000)
  let full = filled(12000)
  let timed_out = channel.timeout(time.ms(0))
  let _ = channel.receive(timed_out)
  let recv_closed = late({ _ ->
    let _ = channel.select([channel.Recv(closed)])
  })
  let send_room = late({ i ->
    let _ = channel.select([channel.Send(sink, i)])
  })
  let sent = drained(sink)
  let recv_full = late({ _ ->
    let _ = channel.select([channel.Recv(full)])
  })
  let taken = 12000 - drained(full)
  let recv_timed_out = late({ _ ->
    let _ = channel.select([channel.Recv(timed_out)])
  })
  let receive = late({ _ ->
    let _ = channel.receive(closed)
  })
  let try_receive = late({ _ ->
    let _ = channel.try_receive(closed)
  })
  let try_send = late({ i ->
    let _ = channel.try_send(sink, i)
  })
  let mixed = late({ _ ->
    let _ = channel.select([channel.Recv(closed)])
    let _ = count(40, 0)
  })
  let inner_hour = late({ i ->
    let _ = task.deadline(time.hours(1), { -> i })
  })
  let inner_none = late({ i ->
    let _ = task.deadline(time.ms(0), { -> i })
  })
  let three = late({ i ->
    let _ = task.deadline(time.hours(1), { ->
      task.deadline(time.hours(2), { -> task.deadline(time.hours(3), { -> i }) })
    })
  })
  let inner_selects = late({ _ ->
    let _ = task.deadline(time.hours(1), { ->
      loop j = 0 {
        match j >= 50 {
          true -> j
          false -> {
            let _ = channel.select([channel.Recv(closed)])
            loop(j + 1)
          }
        }
      }
    })
  })
  let counts = [
    recv_closed, send_room, recv_full, recv_timed_out, receive, try_receive, try_send, mixed,
    inner_hour, inner_none, three, inner_selects, sent - send_room, taken - recv_full,
  ]
  counts |> list.map { n -> string.from(n) } |> string.join(" ")
}
"#;
    let mut first: Option<String> = None;
    for slice in [None, Some(2000), Some(7), Some(1)] {
        let (result, _) = run_with(source, |vm| {
            if let Some(slice) = slice {
                vm.set_time_slice(slice);
            }
        });
        let Ok(Value::String(text)) = result else {
            panic!("at slice {slice:?}: {result:?}");
        };
        let counts: Vec<i64> = text
            .split(' ')
            .map(|n| n.parse().expect("a count"))
            .collect();
        let (rounds, kept) = counts.split_at(12);
        // Ended, after some rounds and not many. (Three deadlines in
        // each other and a deadline around fifty selects are large
        // rounds: one or two of them are within the bound.)
        for (case, rounds) in rounds.iter().enumerate() {
            assert!(
                (1..10_000).contains(rounds),
                "case {case} at slice {slice:?}: {rounds} rounds ({text})"
            );
        }
        // What the last `select` of the function completed, it marked,
        // or it was ended between the two: one value at most is sent,
        // or taken, without its mark. Nothing was sent or taken by a
        // `select` the function was ended in front of.
        assert!(
            kept.iter().all(|more| (0..=1).contains(more)),
            "at slice {slice:?}: {text}"
        );
        // The count does not depend on the slice.
        match &first {
            None => first = Some(text.to_string()),
            Some(first) => assert_eq!(&text.to_string(), first, "at slice {slice:?}"),
        }
    }
}

/// An accept under a deadline loses no connection at any time slice:
/// one that reaches an accept as it is given up stays with the
/// listener, and the next accept is given it.
#[cfg(feature = "tcp")]
#[test]
fn an_accept_under_a_deadline_loses_no_connection_at_every_slice() {
    let source = r#"
import channel
import task
import tcp
import time

fn main() {
  when let Ok(listener) = tcp.listen("127.0.0.1:0") else { panic("cannot listen") }
  let port = tcp.local_port(listener)
  let done: Channel(Int) = channel.new(1)
  let client = task.spawn { ->
    let made = loop i = 0, made = 0 {
      match i >= 40 {
        true -> made
        false -> {
          time.sleep(time.ms(3))
          match tcp.connect("127.0.0.1:{port}") {
            Ok(_) -> loop(i + 1, made + 1)
            Err(_) -> loop(i + 1, made)
          }
        }
      }
    }
    channel.close(done)
    made
  }
  let given = loop given = 0, idle = 0 {
    match task.deadline(time.ms(3), { -> tcp.accept(listener) }) {
      Ok(Ok(c)) -> {
        tcp.close(c)
        loop(given + 1, 0)
      }
      Ok(Err(e)) -> panic("accept failed: {e}")
      Err(_) -> match channel.try_receive(done) {
        channel.Closed -> match idle >= 20 {
          true -> given
          false -> loop(given, idle + 1)
        }
        _ -> loop(given, idle)
      }
    }
  }
  task.join(client) - given
}
"#;
    for slice in SLICES {
        let (result, _) = run_with(source, |vm| {
            if let Some(slice) = slice {
                vm.set_time_slice(slice);
            }
        });
        assert_eq!(result, Ok(Value::Int(0)), "at slice {slice:?}");
    }
}

/// A send on a closed channel is one error at every time slice,
/// whichever function was to send and wherever the call stands: on the
/// program's own thread, in a task, inside a callback of a builtin,
/// inside a stage of a stream.
#[test]
fn a_send_on_a_closed_channel_is_the_same_error_at_every_slice() {
    let doors = [
        ("send", "channel.send(closed, 1)"),
        ("try_send", "channel.try_send(closed, 1)"),
        ("a Send arm", "channel.select([channel.Send(closed, 1)])"),
        (
            "a Send arm beside a Recv arm that is ready",
            "channel.select([channel.Recv(closed), channel.Send(closed, 1)])",
        ),
        (
            "in a task",
            "task.join(task.spawn({ -> channel.try_send(closed, 1) }))",
        ),
        (
            "in a callback",
            "list.map([1, 2]) { x -> channel.try_send(closed, x) }",
        ),
        (
            "in a stream stage",
            "stream.from_list([1, 2]) |> stream.map { x -> channel.try_send(closed, x) } |> stream.collect",
        ),
    ];
    for (what, door) in doors {
        let source = format!(
            "import channel\nimport list\nimport stream\nimport task\n\nfn main() {{\n  let closed = channel.new(4)\n  channel.close(closed)\n  let _ = {door}\n  println(\"not reached\")\n}}\n"
        );
        for slice in SLICES {
            let (result, output) = run_with(&source, |vm| {
                if let Some(slice) = slice {
                    vm.set_time_slice(slice);
                }
            });
            let error = result.expect_err(what);
            assert!(
                error.ends_with("send on closed channel 0"),
                "{what} at slice {slice:?}: {error}"
            );
            assert_eq!(output, "", "{what} at slice {slice:?}");
        }
    }
}

/// `source` under a budget of `steps` at each of [`SLICES`]: it ends,
/// and with `out_of_steps`. (A program that the budget does not end
/// does not end at all: there is no timeout here.)
#[track_caller]
fn assert_ended_by_budget(source: &str, steps: u64) {
    for slice in SLICES {
        let (result, _) = run_whole(source, |vm| {
            if let Some(slice) = slice {
                vm.set_time_slice(slice);
            }
            vm.set_step_budget(steps);
        });
        let error = result.expect_err("the program does not end by itself");
        assert!(error.out_of_steps, "slice {slice:?}: {error:?}");
    }
}

/// The program's own thread, when each of its slices ends in a wait,
/// never runs a slice to its end. Its waits are steps, and no slice
/// starts once they have used the budget up: `main` alone, looping on a
/// receive with a timeout.
#[test]
fn the_budget_ends_a_main_that_only_waits_for_a_timeout() {
    let source = "import channel\nimport time\n\nfn main() {\n  let ch = channel.new(0)\n  loop i = 0 {\n    let _ = channel.receive_timeout(ch, time.ms(1))\n    loop(i + 1)\n  }\n}\n";
    assert_ended_by_budget(source, 100);
}

/// The same with a receive on a channel that a timer closes.
#[test]
fn the_budget_ends_a_main_that_only_waits_for_timer_channels() {
    let source = "import channel\nimport time\n\nfn main() {\n  loop i = 0 {\n    match channel.receive(channel.timeout(time.ms(1))) {\n      _ -> loop(i + 1)\n    }\n  }\n}\n";
    assert_ended_by_budget(source, 100);
}

/// The same with an operation of the I/O pool, which `main` waits for
/// like for a channel: reading a file again and again.
#[test]
fn the_budget_ends_a_main_that_only_waits_for_io() {
    let dir = std::env::temp_dir().join(format!("silt-oracle-limits-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("read.txt");
    std::fs::write(&file, "text\n").unwrap();
    let path = file.to_string_lossy().replace('\\', "/");
    let source = format!(
        "import io\n\nfn main() {{\n  loop i = 0 {{\n    let _ = io.read_file(\"{path}\")\n    loop(i + 1)\n  }}\n}}\n"
    );
    assert_ended_by_budget(&source, 100);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A budget of no steps starts no slice: nothing of the program runs,
/// whatever the slice.
#[test]
fn a_budget_of_no_steps_runs_nothing() {
    let source = "fn main() {\n  println(\"ran\")\n}\n";
    for slice in SLICES {
        let (result, output) = run_whole(source, |vm| {
            if let Some(slice) = slice {
                vm.set_time_slice(slice);
            }
            vm.set_step_budget(0);
        });
        assert!(result.unwrap_err().out_of_steps, "slice {slice:?}");
        assert_eq!(output, "", "slice {slice:?}");
    }
}

/// The budget is looked at where a slice ends: at the scheduler's slice
/// a program of a few hundred steps runs to its end on a budget of one
/// step, and at slice 1 it does not.
#[test]
fn a_program_may_run_one_slice_more_than_its_budget() {
    let source = "fn main() {\n  println(\"ran\")\n  loop i = 0 {\n    match i >= 100 {\n      true -> i\n      false -> loop(i + 1)\n    }\n  }\n}\n";
    let (result, output) = run_whole(source, |vm| vm.set_step_budget(1));
    assert_eq!(result.unwrap(), Value::Int(100));
    assert_eq!(output, "ran\n");
    let (result, _) = run_whole(source, |vm| {
        vm.set_time_slice(1);
        vm.set_step_budget(1);
    });
    assert!(result.unwrap_err().out_of_steps);
}

/// `out_of_steps` marks what the budget did, and nothing else: a fault
/// of the program's own code is that fault, with the flag unset, also
/// when a task has used the budget up meanwhile. (`main` sleeps in a
/// builtin, so its slice goes on to the fault; the task's 20,000 steps
/// take a few milliseconds of the half second. Should they ever take
/// longer, the test fails at its last line: it does not pass without
/// the case it is about.)
#[test]
fn a_fault_of_the_program_is_not_the_budgets() {
    let spin = "import task\nimport time\n\nfn spin() {\n  loop {\n    loop()\n  }\n}\n";
    for (fault, message) in [
        ("10 / (3 - 3)", "division by zero"),
        (
            "panic(\"a real fault of the program\")",
            "a real fault of the program",
        ),
    ] {
        let source = format!(
            "{spin}\nfn main() {{\n  let _ = task.spawn(spin)\n  time.sleep(time.ms(500))\n  println(\"before the fault\")\n  {fault}\n}}\n"
        );
        let (result, output) = run_whole(&source, |vm| vm.set_step_budget(20_000));
        let error = result.unwrap_err();
        assert!(error.message.contains(message), "{error:?}");
        assert!(!error.out_of_steps, "{error:?}");
        assert!(output.starts_with("before the fault\n"), "{output}");
        // The task was ended by the budget, and the runtime says so.
        assert!(output.contains("the step budget is used up"), "{output}");
    }
}

/// A builtin that gives the rest of its slice away after each item
/// (`channel.each`, a stage of a stream) has not run a slice to its
/// end: it costs the budget one step, not the slice's 2,000. So a
/// program that leans on one needs about the same budget at every
/// slice. Each of these handles 3,000 items; at 2,000 steps an item
/// they would need six million.
#[test]
fn a_slice_given_away_costs_one_step_at_any_slice() {
    let feed = "fn feed(ch) {\n  loop i = 0 {\n    match i >= 3000 {\n      true -> channel.close(ch)\n      false -> {\n        channel.send(ch, i)\n        loop(i + 1)\n      }\n    }\n  }\n}\n";
    let on_main = format!(
        "import channel\nimport task\n\n{feed}\nfn main() {{\n  let ch = channel.new(8)\n  let feeder = task.spawn({{ -> feed(ch) }})\n  channel.each(ch) {{ n -> let _ = n + 1 }}\n  task.join(feeder)\n  println(\"done\")\n}}\n"
    );
    let in_a_task = format!(
        "import channel\nimport task\n\n{feed}\nfn main() {{\n  let ch = channel.new(8)\n  let feeder = task.spawn({{ -> feed(ch) }})\n  task.join(task.spawn({{ -> channel.each(ch) {{ n -> let _ = n + 1 }} }}))\n  task.join(feeder)\n  println(\"done\")\n}}\n"
    );
    let stages = "import stream\n\nfn main() {\n  let total = stream.from_range(1, 3000)\n    |> stream.map { n -> n + 1 }\n    |> stream.filter { n -> n % 2 == 0 }\n    |> stream.fold(0) { acc, n -> acc + n }\n  println(total)\n  println(\"done\")\n}\n";
    for (what, source) in [
        ("channel.each on main", on_main.as_str()),
        ("channel.each in a task", in_a_task.as_str()),
        ("stream stages", stages),
    ] {
        for slice in SLICES {
            let (result, output) = run_whole(source, |vm| {
                if let Some(slice) = slice {
                    vm.set_time_slice(slice);
                }
                vm.set_step_budget(600_000);
            });
            assert!(result.is_ok(), "{what}, slice {slice:?}: {result:?}");
            assert!(
                output.ends_with("done\n"),
                "{what}, slice {slice:?}: {output}"
            );
        }
    }
}
