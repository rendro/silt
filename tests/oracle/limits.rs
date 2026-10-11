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
    let source = "import channel\nimport time\n\nfn main() {\n  let ch = channel.new(0)\n  loop i = 0 {\n    let _ = channel.recv_timeout(ch, time.ms(1))\n    loop(i + 1)\n  }\n}\n";
    assert_ended_by_budget(source, 100);
}

/// The same with a receive on a channel that a timer closes.
#[test]
fn the_budget_ends_a_main_that_only_waits_for_timer_channels() {
    let source = "import channel\nimport time\n\nfn main() {\n  loop i = 0 {\n    match channel.receive(channel.timeout(1)) {\n      _ -> loop(i + 1)\n    }\n  }\n}\n";
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
        "import channel\nimport task\n\n{feed}\nfn main() {{\n  let ch = channel.new(8)\n  let feeder = task.spawn({{ -> feed(ch) }})\n  channel.each(ch) {{ n -> n + 1 }}\n  task.join(feeder)\n  println(\"done\")\n}}\n"
    );
    let in_a_task = format!(
        "import channel\nimport task\n\n{feed}\nfn main() {{\n  let ch = channel.new(8)\n  let feeder = task.spawn({{ -> feed(ch) }})\n  task.join(task.spawn({{ -> channel.each(ch) {{ n -> n + 1 }} }}))\n  task.join(feeder)\n  println(\"done\")\n}}\n"
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
