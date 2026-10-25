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
      Some(n) -> channel.send(to, n + 1)
      None -> ()
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
             fn recv(ch) {{\n  match channel.receive(ch) {{\n    Some(v) -> v\n    _ -> 0\n  }}\n}}\n\n\
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
      Ok(Some(v)) -> loop(got + 1, sum + v)
      Ok(None) -> (got, sum)
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
        Some(v) -> loop(n + 1, sum + v)
        None -> (n, sum)
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
    (Ok(Some(7)), channel.Empty) -> 0
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
      channel.Sent(_) -> true
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
        Some(v) -> loop(n + 1, sum + v)
        None -> (n, sum)
      }
    }
  }
  loop {
    let got = task.deadline(time.ms(2), { ->
      match channel.select([channel.Recv(a), channel.Recv(never)]) {
        channel.Received(_, value) -> value
        channel.Sent(_) -> None
      }
    })
    match got {
      Ok(Some(v)) -> {
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
      Ok(Ok(Some(v))) -> loop(got + 1, sum + v, inner)
      Ok(Ok(None)) -> (got, sum, inner)
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

/// A function that is past its deadline takes nothing new, and the
/// work it still does is bounded in all, whatever ends its slices:
/// about 2,000 steps, counted one by one.
///
/// Each case is a loop of 300,000 rounds under a deadline whose time is
/// over from the start (so nothing here depends on a clock), with a
/// mark for every round that was done.
///
/// - A round that TAKES ends the function before its first mark,
///   whether or not the thing to take is there: a `select` with `Recv`
///   arms (on a closed channel, on a channel that is full, on a timeout
///   channel that has closed), a `receive`, a `try_receive`, a read of a
///   file, a `task.deadline` (its function would be a task that takes
///   in its place). Nothing is taken: the full channel is still full.
/// - A round that only GIVES or computes goes on, and is ended by the
///   count: a `select` with a `Send` arm into room, a `send`, a
///   `try_send`, computing. The loop does not reach its last round, the
///   rounds it did reach are few, and their number is the same at every
///   time slice: a slice that ends in a wait which is over at once
///   counts like one that ran out. One value at most is sent without
///   its mark: the one of the round in which the function was ended.
#[test]
fn the_work_past_a_deadline_is_bounded_whatever_the_function_does() {
    let source = r#"
import channel
import io
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
  -- Rounds that take.
  let recv_closed = late({ _ ->
    let _ = channel.select([channel.Recv(closed)])
  })
  let recv_full = late({ _ ->
    let _ = channel.select([channel.Recv(full)])
  })
  let recv_timed_out = late({ _ ->
    let _ = channel.select([channel.Recv(timed_out)])
  })
  let receive = late({ _ ->
    let _ = channel.receive(full)
  })
  let try_receive = late({ _ ->
    let _ = channel.try_receive(full)
  })
  let receive_timeout = late({ _ ->
    let _ = channel.receive_timeout(full, time.ms(1))
  })
  let mixed = late({ _ ->
    let _ = count(40, 0)
    let _ = channel.select([channel.Recv(closed)])
  })
  let inner_hour = late({ i ->
    let _ = task.deadline(time.hours(1), { -> i })
  })
  let inner_none = late({ i ->
    let _ = task.deadline(time.ms(0), { -> i })
  })
  let reads = late({ _ ->
    let _ = io.read_file("/no/such/file")
  })
  let still_full = drained(full)
  -- Rounds that give, or only compute.
  let send_arm = late({ i ->
    let _ = channel.select([channel.Recv(full), channel.Send(sink, i)])
  })
  let sent_by_arm = drained(sink)
  let send = late({ i -> channel.send(sink, i) })
  let sent = drained(sink)
  let try_send = late({ i ->
    let _ = channel.try_send(sink, i)
  })
  let computing = late({ _ ->
    let _ = count(40, 0)
  })
  let counts = [
    recv_closed, recv_full, recv_timed_out, receive, try_receive, receive_timeout, mixed,
    inner_hour, inner_none, reads, still_full, send_arm, send, try_send, computing,
    sent_by_arm - send_arm, sent - send,
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
        let (takes, rest) = counts.split_at(10);
        let (still_full, rest) = rest.split_first().expect("the full channel's count");
        let (gives, kept) = rest.split_at(4);
        // Ended at the first take, before a mark, with nothing taken.
        assert!(
            takes.iter().all(|rounds| *rounds == 0),
            "at slice {slice:?}: a round that takes was done ({text})"
        );
        assert_eq!(*still_full, 12000, "at slice {slice:?}: {text}");
        // Ended by the count, after some rounds and not many.
        for (case, rounds) in gives.iter().enumerate() {
            assert!(
                (1..10_000).contains(rounds),
                "giving case {case} at slice {slice:?}: {rounds} rounds ({text})"
            );
        }
        // What the function's last send did, it marked, or it was ended
        // between the two: one value at most is sent without its mark.
        assert!(
            kept.iter().all(|more| (0..=1).contains(more)),
            "at slice {slice:?}: {text}"
        );
        // The count does not depend on the slice.
        let text = text.to_string();
        match &first {
            None => first = Some(text),
            Some(first) => assert_eq!(&text, first, "at slice {slice:?}"),
        }
    }
}

/// A function that is past its deadline with a backlog loses no item,
/// at every time slice: a worker that takes an item and puts it into
/// a channel with room, under a deadline that passes while its input
/// still holds thousands, passes on the item it has and takes no
/// further one. Then every item is in the input or in the output:
/// `main` counts both when `task.deadline` has answered, which is when
/// the function has ended. (When a late function still took what was
/// there, it went on taking until its steps were used up, and was
/// ended with an item in hand in most rounds.)
#[test]
fn a_late_worker_loses_no_item_at_every_slice() {
    let source = r#"
import channel
import task
import time

fn count(ch) {
  loop n = 0 {
    match channel.try_receive(ch) {
      channel.Message(_) -> loop(n + 1)
      _ -> n
    }
  }
}

fn fill(ch, n) {
  loop i = 0 {
    match i >= n {
      true -> ()
      false -> {
        channel.send(ch, i)
        loop(i + 1)
      }
    }
  }
}

fn main() {
  loop k = 0, lost = 0 {
    match k >= 10 {
      true -> lost
      false -> {
        let input: Channel(Int) = channel.new(20000)
        let out: Channel(Int) = channel.new(20000)
        fill(input, 20000)
        let _ = task.deadline(time.ms(1), { ->
          channel.each(input) { v -> channel.send(out, v) }
        })
        loop(k + 1, lost + 20000 - count(input) - count(out))
      }
    }
  }
}
"#;
    for slice in FOUR_SLICES {
        let (result, _) = run_with(source, |vm| {
            if let Some(slice) = slice {
                vm.set_time_slice(slice);
            }
        });
        assert_eq!(result, Ok(Value::Int(0)), "at slice {slice:?}");
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

/// A cancel loses nothing that was handed over, at any time slice: a
/// task whose wait had ended when `task.cancel` came (a sender handed
/// it a value, a receiver took the value it waited to send) goes on
/// with that for a bounded number of steps, enough to pass it on by an
/// operation that does not wait.
///
/// Each program brings a cancel close to a hand-over, many times, and
/// then waits WITHOUT a time limit for what the cancelled task was
/// given: a value that was dropped with its task leaves `main` waiting
/// for ever, which is a deadlock and the program's error. The sleeps
/// only shape the race; no program says how often the cancel won.
///
/// At a slice of one step the task gives way after every instruction:
/// the cancel then finds it between its receive and its record more
/// often than woken and not yet run, and the rule is the same.
#[test]
fn a_cancel_loses_nothing_that_was_handed_over_at_every_slice() {
    const HEAD: &str = "import channel\nimport task\nimport time\n\n";
    // A receive: whenever the sender was told "sent", the number was
    // seen by the receiver's code or still lies in the channel.
    let receive = r#"
fn round(i, capacity) {
  let ch: Channel(Int) = channel.new(capacity)
  let seen: Channel(Int) = channel.new(1)
  let sender = task.spawn { ->
    time.sleep(time.ms(1))
    match channel.select([channel.Send(ch, i), channel.Recv(channel.timeout(time.ms(3)))]) {
      channel.Sent(_) -> true
      _ -> false
    }
  }
  let receiver = task.spawn { ->
    match channel.receive(ch) {
      Some(v) -> channel.send(seen, v)
      None -> ()
    }
  }
  time.sleep(time.ms(1))
  task.cancel(receiver)
  match task.join(sender) {
    false -> true
    true -> match channel.select([channel.Recv(seen), channel.Recv(ch)]) {
      channel.Received(_, Some(^i)) -> true
      _ -> false
    }
  }
}

fn main() {
  loop i = 0, all = true {
    match i >= 120 {
      true -> all
      false -> loop(i + 1, all && round(i, 0) && round(i, 1))
    }
  }
}
"#;
    // A send that waited, as the `Send` arm of a select: whenever a
    // receiver has the number, the sender's own record of the send
    // comes; and a sender whose number nobody took recorded nothing.
    let send = r#"
fn round(i) {
  let ch: Channel(Int) = channel.new(0)
  let sent: Channel(Int) = channel.new(1)
  let sender = task.spawn { ->
    match channel.select([channel.Send(ch, i)]) {
      channel.Sent(_) -> channel.send(sent, i)
      _ -> ()
    }
  }
  let receiver = task.spawn { ->
    time.sleep(time.ms(1))
    match channel.select([channel.Recv(ch), channel.Recv(channel.timeout(time.ms(3)))]) {
      channel.Received(_, Some(v)) -> v
      _ -> -1
    }
  }
  time.sleep(time.ms(1))
  task.cancel(sender)
  match task.join(receiver) {
    -1 -> match channel.try_receive(sent) {
      channel.Empty -> true
      _ -> false
    }
    v -> match channel.receive(sent) {
      Some(^v) -> true
      _ -> false
    }
  }
}

fn main() {
  loop i = 0, all = true {
    match i >= 200 {
      true -> all
      false -> loop(i + 1, all && round(i))
    }
  }
}
"#;
    // The same receiver with a `select` of one arm for its receive: a
    // cancelled task that comes to the select with the number there
    // takes it, as its `receive` does, and passes it on.
    let select = receive.replace(
        "match channel.receive(ch) {\n      Some(v) -> channel.send(seen, v)\n      None -> ()",
        "match channel.select([channel.Recv(ch)]) {\n      channel.Received(_, Some(v)) -> channel.send(seen, v)\n      _ -> ()",
    );
    assert_ne!(select, receive);
    let programs = [
        ("a receive", receive),
        ("a receive by a select", &select),
        ("a send", send),
    ];
    for (what, program) in programs {
        let source = format!("{HEAD}{program}");
        for slice in SLICES {
            let (result, _) = run_with(&source, |vm| {
                if let Some(slice) = slice {
                    vm.set_time_slice(slice);
                }
            });
            assert_eq!(result, Ok(Value::Bool(true)), "{what} at slice {slice:?}");
        }
    }
}

/// After a cancel a task takes nothing new and may still give what it
/// holds, at every time slice: the golden case of that name, whose
/// task cancels itself, prints the same lines at each. (Its sends, a
/// `try_send`, a `Send` arm and the join of a task that has ended are
/// done; the select that would take ends it, with the value still in
/// its channel.)
#[test]
fn a_cancelled_task_gives_what_it_holds_and_takes_nothing_new_at_every_slice() {
    let source = include_str!(
        "../golden/concurrency/cancel/cancel_handoff__a_cancelled_task_gives_what_it_holds_and_takes_nothing_new.silt"
    );
    let expected = include_str!(
        "../golden/concurrency/cancel/cancel_handoff__a_cancelled_task_gives_what_it_holds_and_takes_nothing_new.stdout"
    );
    for slice in FOUR_SLICES {
        let (result, output) = run_with(source, |vm| {
            if let Some(slice) = slice {
                vm.set_time_slice(slice);
            }
        });
        assert_eq!(result, Ok(Value::Unit), "at slice {slice:?}");
        assert_eq!(output, expected, "at slice {slice:?}");
    }
}

/// The slices of the tests of a cancel: none set, the scheduler's
/// own, a few steps, one step.
const FOUR_SLICES: [Option<usize>; 4] = [None, Some(2000), Some(7), Some(1)];

/// Every operation that takes ends a cancelled task, whether or not
/// the thing to take is there, at every time slice. A task cancels
/// itself, says so by a send, and comes to the operation with a value
/// ready for it; the line after the operation is never printed, and
/// the value is still in its channel when `main` asks for it, which it
/// does without a time limit. (The short sleep only gives a task that
/// would take the value the time to do so.)
#[test]
fn every_take_ends_a_cancelled_task_at_every_slice() {
    let takes = [
        ("receive", "channel.receive(src)"),
        ("try_receive", "channel.try_receive(src)"),
        (
            "receive_timeout",
            "channel.receive_timeout(src, time.ms(50))",
        ),
        ("a select's Recv arm", "channel.select([channel.Recv(src)])"),
        (
            "a select's Recv arms, one of them a timeout that has closed",
            "channel.select([channel.Recv(timed_out), channel.Recv(src)])",
        ),
        (
            "each",
            "channel.each(src) { v -> println(\"the task took {v}\") }",
        ),
        ("a sink of a stream", "stream.first(src)"),
        (
            "a sink that reads to the end",
            "stream.fold(src, 0) { acc, v -> acc + v }",
        ),
    ];
    for (what, take) in takes {
        let source = format!(
            r#"
import channel
import stream
import task
import time

fn main() {{
  let own: Channel(task.Handle(())) = channel.new(1)
  let src: Channel(Int) = channel.new(1)
  channel.send(src, 5)
  let timed_out: Channel(Int) = channel.timeout(time.ms(0))
  let said = channel.new(1)
  let t = task.spawn {{ ->
    let _ = channel.receive(timed_out)
    match channel.receive(own) {{
      Some(me) -> task.cancel(me)
      None -> ()
    }}
    channel.send(said, 0)
    let _ = {take}
    println("the task went on")
  }}
  channel.send(own, t)
  let _ = channel.receive(said)
  time.sleep(time.ms(2))
  channel.receive(src)
}}
"#
        );
        for slice in FOUR_SLICES {
            let (result, output) = run_with(&source, |vm| {
                if let Some(slice) = slice {
                    vm.set_time_slice(slice);
                }
            });
            let shown = result.map(|value| value.to_string());
            assert_eq!(
                (shown, output.as_str()),
                (Ok("Some(5)".to_string()), ""),
                "{what} at slice {slice:?}"
            );
        }
    }
}

/// A cancelled task starts nothing that would take in its place, at
/// every time slice: a `task.deadline` (its function is a task of its
/// own) and a stream stage (a source, a transform) end the task where
/// it calls them, before a task is started. Each round a task cancels
/// itself, says so, and comes to such a call around a channel that
/// holds one value: the value is still there when `main` asks for it,
/// which it does without a time limit, and the line after the call is
/// never printed. (When the call started its task, that task took the
/// value in a part of the rounds and gave it to nobody, since the
/// cancelled task ends at its own next take or wait.)
#[test]
fn a_cancelled_task_starts_nothing_that_takes_for_it_at_every_slice() {
    let starts = [
        (
            "task.deadline around a receive",
            "task.deadline(time.seconds(5), { -> channel.receive(ch) })",
        ),
        (
            "stream.take, then a sink",
            "ch |> stream.take(1) |> stream.collect",
        ),
        (
            "stream.map, then a sink",
            "ch |> stream.map { x -> x } |> stream.first",
        ),
        (
            "a source, then a sink",
            "stream.from_list([1, 2]) |> stream.collect",
        ),
    ];
    for (what, start) in starts {
        let source = format!(
            r#"
import channel
import stream
import task
import time

fn main() {{
  loop i = 0 {{
    match i >= 60 {{
      true -> i
      false -> {{
        let own: Channel(task.Handle(())) = channel.new(1)
        let ch: Channel(Int) = channel.new(1)
        channel.send(ch, 5)
        let said = channel.new(1)
        let t = task.spawn {{ ->
          match channel.receive(own) {{
            Some(me) -> task.cancel(me)
            None -> ()
          }}
          channel.send(said, 0)
          let _ = {start}
          println("the task went on")
        }}
        channel.send(own, t)
        let _ = channel.receive(said)
        time.sleep(time.ms(1))
        match channel.receive(ch) {{
          Some(5) -> loop(i + 1)
          _ -> -1
        }}
      }}
    }}
  }}
}}
"#
        );
        for slice in FOUR_SLICES {
            let (result, output) = run_with(&source, |vm| {
                if let Some(slice) = slice {
                    vm.set_time_slice(slice);
                }
            });
            assert_eq!(
                (result, output.as_str()),
                (Ok(Value::Int(60)), ""),
                "{what} at slice {slice:?}"
            );
        }
    }
}

/// A worker that is cancelled loses no item, at every time slice,
/// whatever its input holds: it passes on the item it has and takes no
/// further one. (When a cancelled task still took what was ready, a
/// worker whose input was ready again went on taking items until its
/// steps were used up, and was ended with one of them in hand.)
///
/// Each program runs rounds of a worker that takes an item and puts it
/// into a channel with room, and is cancelled: from outside at some
/// moment, or by itself at a place that moves from round to round.
/// Then every item is in the input or in the output. `main` counts
/// both and waits WITHOUT a time limit for what is missing, so an item
/// that was lost is a deadlock, and a slow machine fails nothing. (The
/// sleeps let a worker that would go on taking do so before the
/// count.)
#[test]
fn a_cancelled_worker_loses_no_item_at_every_slice() {
    const HEAD: &str = r#"
import channel
import task
import time

fn fill(ch, n) {
  loop i = 0 {
    match i >= n {
      true -> ()
      false -> {
        channel.send(ch, i)
        loop(i + 1)
      }
    }
  }
}

-- How many values `ch` holds, taken out of it.
fn count(ch) {
  loop n = 0 {
    match channel.try_receive(ch) {
      channel.Message(_) -> loop(n + 1)
      _ -> n
    }
  }
}

-- How many of `n` items were taken out of `input` and not put into
-- `out`: 0, or `main` waits for ever for the rest.
fn missing(input, out, n) {
  time.sleep(time.ms(3))
  loop found = count(input) + count(out) {
    match found >= n {
      true -> found - n
      false -> match channel.select([channel.Recv(input), channel.Recv(out)]) {
        channel.Received(_, Some(_)) -> loop(found + 1)
        _ -> n - found
      }
    }
  }
}

fn work(v, n) {
  match n <= 0 {
    true -> v
    false -> work(v, n - 1)
  }
}

fn each_worker(input, out, steps) {
  channel.each(input) { v -> channel.send(out, work(v, steps)) }
}

fn receive_worker(input, out, steps) {
  loop {
    match channel.receive(input) {
      Some(v) -> {
        channel.send(out, work(v, steps))
        loop()
      }
      None -> ()
    }
  }
}

-- A worker that is cancelled from outside, a moment after it began,
-- with `items` in its input.
fn from_outside(worker, items, rounds) {
  loop k = 0, lost = 0 {
    match k >= rounds {
      true -> lost
      false -> {
        let input: Channel(Int) = channel.new(items)
        let out: Channel(Int) = channel.new(items)
        fill(input, items)
        let t = task.spawn { -> worker(input, out, 0) }
        time.sleep(time.ms(1))
        task.cancel(t)
        loop(k + 1, lost + missing(input, out, items))
      }
    }
  }
}

-- A worker that cancels itself and goes on: 40 rounds, the cancel
-- `k` sends before the work begins, so at another step of a slice
-- each time.
fn by_itself(worker, items, steps) {
  loop k = 0, lost = 0 {
    match k >= 40 {
      true -> lost
      false -> {
        let own: Channel(task.Handle(())) = channel.new(1)
        let input: Channel(Int) = channel.new(700)
        let out: Channel(Int) = channel.new(700)
        let sink: Channel(Int) = channel.new(100)
        fill(input, items)
        let t = task.spawn { ->
          match channel.receive(own) {
            Some(me) -> task.cancel(me)
            None -> ()
          }
          fill(sink, k)
          worker(input, out, steps)
        }
        channel.send(own, t)
        loop(k + 1, lost + missing(input, out, items))
      }
    }
  }
}
"#;
    let programs = [
        (
            "each, cancelled from outside",
            "fn main() {\n  from_outside(each_worker, 20000, 8)\n}\n",
        ),
        (
            "receive in a loop, cancelled from outside",
            "fn main() {\n  from_outside(receive_worker, 20000, 8)\n}\n",
        ),
        (
            "each, cancelled by itself",
            "fn main() {\n  by_itself(each_worker, 700, 0)\n}\n",
        ),
        (
            "receive in a loop, cancelled by itself",
            "fn main() {\n  by_itself(receive_worker, 700, 0)\n}\n",
        ),
        (
            "each, five items of 500 steps, cancelled by itself",
            "fn main() {\n  by_itself(each_worker, 5, 60)\n}\n",
        ),
    ];
    for (what, main) in programs {
        let source = format!("{HEAD}\n{main}");
        for slice in FOUR_SLICES {
            let (result, _) = run_with(&source, |vm| {
                if let Some(slice) = slice {
                    vm.set_time_slice(slice);
                }
            });
            assert_eq!(result, Ok(Value::Int(0)), "{what} at slice {slice:?}");
        }
    }
}

/// A worker that is cancelled while it holds an item passes that item
/// on and takes no further one, at every time slice: it cancels itself
/// in its callback with item 3 of 700, after which the output holds
/// the items 0 to 3 and the input the other 696. And a pipeline over a
/// rendezvous channel loses nothing when its worker is cancelled:
/// every number whose send returned (the producer notes each) is put
/// out by the worker, which `main` waits for without a time limit.
#[test]
fn a_cancelled_worker_passes_on_the_item_it_holds_at_every_slice() {
    let holds = r#"
import channel
import task

fn count(ch) {
  loop n = 0 {
    match channel.try_receive(ch) {
      channel.Message(_) -> loop(n + 1)
      _ -> n
    }
  }
}

fn main() {
  let own: Channel(task.Handle(())) = channel.new(1)
  let input: Channel(Int) = channel.new(700)
  let out: Channel(Int) = channel.new(700)
  loop i = 0 {
    match i >= 700 {
      true -> ()
      false -> {
        channel.send(input, i)
        loop(i + 1)
      }
    }
  }
  let t = task.spawn { ->
    when let Some(me) = channel.receive(own) else { panic("no handle") }
    channel.each(input) { v ->
      match v == 3 {
        true -> task.cancel(me)
        false -> ()
      }
      channel.send(out, v)
    }
  }
  channel.send(own, t)
  -- The four items come, however long that takes.
  let put_out = loop got = [] {
    match channel.receive(out) {
      Some(3) -> [..got, 3]
      Some(v) -> loop([..got, v])
      None -> got
    }
  }
  -- The worker took no fifth: the next item is still the input's.
  when let Some(next) = channel.receive(input) else { panic("no item") }
  "put out {put_out}, the input's next {next}, {count(input)} more"
}
"#;
    let rendezvous = r#"
import channel
import task
import time

fn main() {
  loop k = 0, all = true {
    match k >= 30 {
      true -> all
      false -> {
        let jobs: Channel(Int) = channel.new(0)
        let sent: Channel(Int) = channel.new(100000)
        let out: Channel(Int) = channel.new(100000)
        let producer = task.spawn { ->
          loop i = 0 {
            channel.send(jobs, i)
            channel.send(sent, i)
            loop(i + 1)
          }
        }
        let worker = task.spawn { -> channel.each(jobs) { v -> channel.send(out, v) } }
        time.sleep(time.ms(2))
        task.cancel(worker)
        time.sleep(time.ms(2))
        task.cancel(producer)
        -- For every send that was noted, the worker's output: in the
        -- order of the sends.
        let ok = loop ok = true {
          match channel.try_receive(sent) {
            channel.Message(i) -> match channel.receive(out) {
              Some(^i) -> loop(ok)
              _ -> loop(false)
            }
            _ -> ok
          }
        }
        loop(k + 1, all && ok)
      }
    }
  }
}
"#;
    for slice in FOUR_SLICES {
        let run = |source: &str| {
            run_with(source, |vm| {
                if let Some(slice) = slice {
                    vm.set_time_slice(slice);
                }
            })
            .0
        };
        assert_eq!(
            run(holds).map(|value| value.to_string()),
            Ok("put out [0, 1, 2, 3], the input's next 4, 695 more".to_string()),
            "at slice {slice:?}"
        );
        assert_eq!(run(rendezvous), Ok(Value::Bool(true)), "at slice {slice:?}");
    }
}

/// A cancelled task starts no I/O, at every time slice: an operation
/// it comes to after the cancel is not handed to the I/O threads, and
/// ends the task. A task cancels itself and writes a file: the file is
/// not there when the program has ended, and the line after the write
/// is not printed. And a cancelled task that comes to `tcp.accept`
/// with a connection waiting takes none: `main` accepts it.
#[test]
fn a_cancelled_task_starts_no_io_at_every_slice() {
    let dir = std::env::temp_dir().join(format!("silt-oracle-cancel-io-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("written.txt");
    let path = file.to_string_lossy().replace('\\', "/");
    let write = format!(
        r#"
import channel
import io
import task

fn main() {{
  let own: Channel(task.Handle(())) = channel.new(1)
  let t = task.spawn {{ ->
    match channel.receive(own) {{
      Some(me) -> task.cancel(me)
      None -> ()
    }}
    println("cancelled")
    let _ = io.write_file("{path}", "written by a cancelled task")
    println("the task went on")
  }}
  channel.send(own, t)
}}
"#
    );
    for slice in SLICES {
        let (result, output) = run_with(&write, |vm| {
            if let Some(slice) = slice {
                vm.set_time_slice(slice);
            }
        });
        assert_eq!(result, Ok(Value::Unit), "at slice {slice:?}");
        assert_eq!(output, "cancelled\n", "at slice {slice:?}");
        assert!(!file.exists(), "at slice {slice:?}: the file was written");
    }
    let _ = std::fs::remove_dir_all(&dir);

    #[cfg(feature = "tcp")]
    {
        // The client's connection is made before the task runs; `main`
        // makes a second one, so that its accept returns in any case.
        let accept = r#"
import channel
import task
import tcp

fn main() {
  when let Ok(listener) = tcp.listen("127.0.0.1:0") else { panic("cannot listen") }
  let port = tcp.local_port(listener)
  when let Ok(_first) = tcp.connect("127.0.0.1:{port}") else { panic("cannot connect") }
  let own: Channel(task.Handle(())) = channel.new(1)
  let said = channel.new(1)
  let t = task.spawn { ->
    match channel.receive(own) {
      Some(me) -> task.cancel(me)
      None -> ()
    }
    channel.send(said, 0)
    let _ = tcp.accept(listener)
    println("the task went on")
  }
  channel.send(own, t)
  let _ = channel.receive(said)
  when let Ok(_second) = tcp.connect("127.0.0.1:{port}") else { panic("cannot connect") }
  match tcp.accept(listener) {
    Ok(c) -> {
      tcp.close(c)
      "accepted"
    }
    Err(e) -> panic("accept failed: {e}")
  }
}
"#;
        for slice in SLICES {
            let (result, output) = run_with(accept, |vm| {
                if let Some(slice) = slice {
                    vm.set_time_slice(slice);
                }
            });
            assert_eq!(
                (result, output.as_str()),
                (Ok(Value::String("accepted".into())), ""),
                "at slice {slice:?}"
            );
        }
    }
}

/// An accept loses no connection to a cancel at any time slice. Each
/// round a client connects and the task that accepts is cancelled at
/// about that moment. A connection that the accept had given the task
/// is the task's: it goes on with it and passes it on, after 1,440
/// steps of work, which is within what a cancelled task has. One that
/// came as the accept was given up stays with the listener, for the
/// accept of a later round or for the ones at the end. So in the end
/// every connection that was made was passed on or is accepted then.
///
/// (A connection that was lost would leave an accept waiting for
/// ever, and nothing tells a program that no connection will come: so
/// the accepts at the end give up, each after a few milliseconds, and
/// the count is taken when twenty in a row have found nothing.)
#[cfg(feature = "tcp")]
#[test]
fn a_cancelled_accept_loses_no_connection_at_every_slice() {
    let source = r#"
import channel
import task
import tcp
import time

fn count(n, acc) {
  match n {
    0 -> acc
    _ -> count(n - 1, acc + 1)
  }
}

-- How many connections `passed_on` holds, each closed.
fn drained(passed_on) {
  loop n = 0 {
    match channel.try_receive(passed_on) {
      channel.Message(c) -> {
        tcp.close(c)
        loop(n + 1)
      }
      _ -> n
    }
  }
}

fn main() {
  when let Ok(listener) = tcp.listen("127.0.0.1:0") else { panic("cannot listen") }
  let port = tcp.local_port(listener)
  let passed_on = channel.new(64)
  let made = loop i = 0, made = 0 {
    match i >= 40 {
      true -> made
      false -> {
        let acceptor = task.spawn { ->
          match tcp.accept(listener) {
            Ok(c) -> {
              let _ = count(120, 0)
              channel.send(passed_on, c)
            }
            Err(_) -> ()
          }
        }
        let client = task.spawn { ->
          time.sleep(time.ms(2))
          match tcp.connect("127.0.0.1:{port}") {
            Ok(_) -> 1
            Err(_) -> 0
          }
        }
        time.sleep(time.ms(2))
        task.cancel(acceptor)
        loop(i + 1, made + task.join(client))
      }
    }
  }
  let left = loop left = 0, idle = 0 {
    match task.deadline(time.ms(3), { -> tcp.accept(listener) }) {
      Ok(Ok(c)) -> {
        tcp.close(c)
        loop(left + 1, 0)
      }
      Ok(Err(e)) -> panic("accept failed: {e}")
      Err(_) -> match idle >= 20 {
        true -> left
        false -> loop(left, idle + 1)
      }
    }
  }
  made - left - drained(passed_on)
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

/// A cancelled task that never waits is ended, at every time slice,
/// after the slice the cancel found it in and at most 2,000 steps
/// more. The task prints a line each time round a loop that has no
/// end; `main` cancels it and prints a line of its own when the cancel
/// has returned. The program ends (the loop was ended), and the rounds
/// printed after `main`'s line are fewer than those steps, since a
/// round is more than one step.
///
/// (How many rounds the task did before the cancel is the machine's
/// to say. And the count after it is not the same at every slice, as
/// it is for a deadline that is over from the start: the cancel comes
/// at no fixed step of the task, and the rest of the slice it comes in
/// is not counted.)
#[test]
fn a_cancelled_task_that_never_waits_is_ended_at_every_slice() {
    let source = r#"
import channel
import task

fn main() {
  let started: Channel(Int) = channel.new(1)
  let busy = task.spawn { ->
    loop i = 0 {
      let _ = channel.try_send(started, i)
      println("round")
      loop(i + 1)
    }
  }
  let _ = channel.receive(started)
  task.cancel(busy)
  println("cancelled")
}
"#;
    for slice in [None, Some(2000), Some(7), Some(1)] {
        let (result, output) = run_with(source, |vm| {
            if let Some(slice) = slice {
                vm.set_time_slice(slice);
            }
        });
        assert_eq!(result, Ok(Value::Unit), "at slice {slice:?}");
        let mut lines = output.lines();
        let before = lines.by_ref().take_while(|line| *line == "round").count();
        let after = lines.filter(|line| *line == "round").count();
        assert_eq!(
            output.lines().count(),
            before + 1 + after,
            "at slice {slice:?}: the lines are the rounds and the cancel's"
        );
        let steps = 2000 + slice.unwrap_or(2000);
        assert!(
            after < steps,
            "at slice {slice:?}: {after} rounds after the cancel, {before} before"
        );
    }
}

/// A task that is cancelled while it runs past its deadline has one
/// count of steps, not a second one for the cancel. The function of a
/// deadline that is over from the start prints a line each time round
/// a long loop; in one program the task around it is joined, in the
/// other it is cancelled when the function has begun. The function
/// prints the same number of lines in both, at every time slice: the
/// cancel adds no step to what the deadline allows.
#[test]
fn a_task_cancelled_while_late_has_one_count_at_every_slice() {
    let program = |end: &str| {
        format!(
            r#"
import channel
import task
import time

fn rounds() {{
  loop i = 0 {{
    match i >= 300000 {{
      true -> i
      false -> {{
        println("round")
        loop(i + 1)
      }}
    }}
  }}
}}

fn main() {{
  let started: Channel(Int) = channel.new(1)
  let around = task.spawn {{ ->
    task.deadline(time.ms(0), {{ ->
      let _ = channel.try_send(started, 0)
      rounds()
    }})
  }}
  let _ = channel.receive(started)
  {end}
}}
"#
        )
    };
    let joined = program("let _ = task.join(around)");
    let cancelled = program("task.cancel(around)");
    let mut first: Option<usize> = None;
    for slice in [None, Some(2000), Some(7), Some(1)] {
        let rounds = |source: &str| {
            let (result, output) = run_with(source, |vm| {
                if let Some(slice) = slice {
                    vm.set_time_slice(slice);
                }
            });
            assert_eq!(result, Ok(Value::Unit), "at slice {slice:?}");
            assert!(output.lines().all(|line| line == "round"), "{output}");
            output.lines().count()
        };
        let (joined, cancelled) = (rounds(&joined), rounds(&cancelled));
        assert!((1..2000).contains(&joined), "at slice {slice:?}: {joined}");
        assert_eq!(cancelled, joined, "at slice {slice:?}");
        assert_eq!(*first.get_or_insert(joined), joined, "at slice {slice:?}");
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

/// Wherever a program's budget ends it, the error names a place of the
/// program: in the code that installs Error's default `message`, the impl
/// that leaves it out; in that default, which is silt's code, the call
/// of `Error.message`.
#[test]
fn a_budget_that_ends_in_errors_default_message_names_a_place_of_the_program() {
    let source = "type E { E(Int) }\ntrait Error for E {}\n\
                  fn m(e: a) -> String where a: Error { Error.message(e) }\n\
                  fn main() {\n  println(Error.message(E(1)))\n  println(m(E(2)))\n}\n";
    let mut ended = 0;
    for steps in 0..200 {
        let (result, _) = run_whole(source, |vm| {
            vm.set_time_slice(1);
            vm.set_step_budget(steps);
        });
        let Err(error) = result else {
            break;
        };
        assert!(error.out_of_steps, "{error:?}");
        assert!(
            error.span.is_some(),
            "the budget of {steps} steps: {error:?}"
        );
        ended += 1;
    }
    assert!(
        (10..200).contains(&ended),
        "the program ends after {ended} steps"
    );
}
