//! A panic of the runtime itself, on one of its own threads, ends the
//! program: the thread that runs the program gets the panic out of the
//! call of the VM it is in, or makes next. Before, the thread that
//! panicked was gone and whatever waited for what it was doing waited
//! for ever: a task that joined the lost task, a task that received on
//! the channel only that task would send on, the program's own thread.
//!
//! No silt program can make the runtime panic, so the panic is made by
//! the test-only `silt::vm::panic_for_test`, in the runtime's own code
//! and outside every catch of a builtin's panic. No test here asserts a
//! time: that nothing hangs is shown by the test ending.

#![cfg(feature = "test-hooks")]

use std::panic::{AssertUnwindSafe, catch_unwind};

use silt::session::Program;
use silt::session::testing::compile_str;
use silt::vm::{PanicAt, panic_for_test};
use silt::{Buffer, HostIo, Value, Vm};

fn compiled(source: &str) -> Program {
    compile_str(source).expect("the program compiles")
}

/// The message of the panic that `call` ends with.
fn panic_of(call: impl FnOnce()) -> String {
    let payload = catch_unwind(AssertUnwindSafe(call)).expect_err("the call panics");
    let text = payload.downcast_ref::<String>().cloned();
    text.or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
        .expect("the panic has a message")
}

/// `<handle:0>` would send on a channel that nobody else sends on; a
/// task joins it, a task receives on that channel, and `main` joins
/// both.
///
/// The hook loses a task before a slice of it that is not its first,
/// so the task must certainly have one. A receive alone does not make
/// it so: it may find its value there and go on in the same slice. A
/// sleep does: it always ends the slice. So the task is let through
/// the gate by `main`, when `main` has started every task and printed
/// its line (nothing is spawned or printed while the runtime may be
/// broken), and then sleeps; it is lost as that sleep ends. That the
/// joiner, the receiver and `main` wait by then is likely and not
/// certain: the program ends either way.
const LOST_WHILE_ALL_WAIT: &str = r#"
import channel
import task
import time

fn main() {
  let gate = channel.new(0)
  let only = channel.new(0)
  let lost = task.spawn { ->
    let _ = channel.receive(gate)
    time.sleep(time.ms(5))
    channel.send(only, 1)
  }
  let joiner = task.spawn { -> task.join(lost) }
  let receiver = task.spawn { -> channel.receive(only) }
  println("main waits")
  channel.send(gate, 1)
  task.join(joiner)
  let _ = task.join(receiver)
  println("main went on")
}
"#;

/// A worker panics as it is about to run a task that a joiner, a
/// receiver and the program's thread all wait on. The program's
/// thread, in its `task.join`, gets the panic; the VM gives it again
/// at every later call, and is dropped like any other. The process
/// (the embedder's) lives: another VM runs a program in it.
#[test]
fn a_panic_of_a_worker_reaches_the_thread_that_waits_for_the_lost_task() {
    let program = compiled(LOST_WHILE_ALL_WAIT);
    let out = Buffer::new();
    let mut vm = Vm::new(HostIo::buffer(&out));
    panic_for_test(&vm, PanicAt::Task(0));
    let first = panic_of(|| {
        let _ = vm.run_program(&program);
    });
    assert_eq!(first, "a panic made for a test: Task(0)");
    assert_eq!(out.contents(), "main waits\n");

    let again = "a thread of this VM's runtime has panicked: a panic made for a test: Task(0)";
    assert_eq!(panic_of(|| vm.settle()), again);
    assert_eq!(
        panic_of(|| {
            let _ = vm.run_program(&program);
        }),
        again
    );
    drop(vm);

    let other = Buffer::new();
    let answer = Vm::new(HostIo::buffer(&other)).run_program(&compiled("fn main() { 6 * 7 }"));
    assert_eq!(answer.map_err(|e| e.message), Ok(Value::Int(42)));
}

/// The program's own code has returned and its thread waits for the
/// program to end (`Vm::settle`) when the worker panics: `settle`
/// gives the panic.
#[test]
fn a_panic_of_a_worker_reaches_the_thread_that_waits_for_the_program_to_end() {
    let program = compiled(
        r#"
import channel
import task
import time

fn main() {
  let gate = channel.new(0)
  let lost = task.spawn { ->
    let _ = channel.receive(gate)
    time.sleep(time.ms(20))
  }
  println("main returns")
  channel.send(gate, 1)
}
"#,
    );
    let out = Buffer::new();
    let mut vm = Vm::new(HostIo::buffer(&out));
    panic_for_test(&vm, PanicAt::Task(0));
    // The task is let through the gate as `main` returns, and is lost
    // when its sleep ends: 20 ms later, in `settle`, unless this
    // thread was held up for that long before it got there (then the
    // panic comes out of `run_program`, which is as right).
    let panic = panic_of(|| {
        let _ = vm.run_program(&program);
        vm.settle();
    });
    assert_eq!(panic, "a panic made for a test: Task(0)");
    assert_eq!(out.contents(), "main returns\n");
}

/// The program's thread computes and never waits: it gets the panic
/// where it looks for one, every million steps or so of its loop,
/// which would never end otherwise. (The task sleeps, which always
/// ends its first slice: it is certainly lost when the sleep ends.)
#[test]
fn a_panic_of_a_worker_reaches_the_thread_that_computes() {
    let program = compiled(
        r#"
import task
import time

fn forever(n) {
  forever(n + 1)
}

fn main() {
  let lost = task.spawn { -> time.sleep(time.ms(1)) }
  forever(0)
}
"#,
    );
    let mut vm = Vm::new(HostIo::buffer(&Buffer::new()));
    panic_for_test(&vm, PanicAt::Task(0));
    let panic = panic_of(|| {
        let _ = vm.run_program(&program);
    });
    assert_eq!(panic, "a panic made for a test: Task(0)");
}

/// The timer's thread panics when the first deadline is due: nothing
/// would fire a deadline again, and every sleep would last for ever.
/// The program's thread, in its sleep, gets the panic.
#[test]
fn a_panic_of_the_timer_thread_reaches_the_thread_that_sleeps() {
    let program = compiled(
        r#"
import task
import time

fn main() {
  let sleeper = task.spawn { -> time.sleep(time.ms(30)) }
  println("main sleeps")
  time.sleep(time.ms(5))
  task.join(sleeper)
  println("main went on")
}
"#,
    );
    let out = Buffer::new();
    let mut vm = Vm::new(HostIo::buffer(&out));
    panic_for_test(&vm, PanicAt::Timer);
    let panic = panic_of(|| {
        let _ = vm.run_program(&program);
    });
    assert_eq!(panic, "a panic made for a test: Timer");
    assert_eq!(out.contents(), "main sleeps\n");
}

/// The same when the task's sleep is over before its wait begins: the
/// deadline still goes through the timer, which panics.
#[test]
fn a_panic_of_the_timer_thread_comes_for_a_sleep_over_at_once() {
    let program = compiled(
        r#"
import task
import time

fn main() {
  let sleeper = task.spawn { -> time.sleep(time.nanos(1)) }
  println("main waits")
  task.join(sleeper)
  println("main went on")
}
"#,
    );
    let out = Buffer::new();
    let mut vm = Vm::new(HostIo::buffer(&out));
    panic_for_test(&vm, PanicAt::Timer);
    let panic = panic_of(|| {
        let _ = vm.run_program(&program);
    });
    assert_eq!(panic, "a panic made for a test: Timer");
    assert_eq!(out.contents(), "main waits\n");
}

/// A thread of the I/O pool panics when its operation has returned,
/// before it hands the value over: the task that waits for the value
/// would wait for ever, and the program would count as having I/O in
/// flight. The program's thread, which joins that task, gets the
/// panic.
#[test]
fn a_panic_of_an_io_thread_reaches_the_thread_that_waits_for_the_reader() {
    let program = compiled(
        r#"
import io
import task

fn main() {
  let reader = task.spawn { -> io.read_file("no such file of the runtime panic tests") }
  println("main waits")
  let _ = task.join(reader)
  println("main went on")
}
"#,
    );
    let out = Buffer::new();
    let mut vm = Vm::new(HostIo::buffer(&out));
    panic_for_test(&vm, PanicAt::Io);
    let panic = panic_of(|| {
        let _ = vm.run_program(&program);
    });
    assert_eq!(panic, "a panic made for a test: Io");
    assert_eq!(out.contents(), "main waits\n");
}

/// A panic on the program's own thread, in the VM's code, comes out of
/// the call as it always did, while a task waits on a channel.
#[test]
fn a_panic_on_the_program_thread_comes_out_of_the_call() {
    let program = compiled(
        r#"
import channel
import task
import time

fn main() {
  let never = channel.new(0)
  let waits = task.spawn { -> channel.receive(never) }
  println("main waits")
  let _ = channel.receive_timeout(never, time.ms(1))
  println("main went on")
}
"#,
    );
    let out = Buffer::new();
    let mut vm = Vm::new(HostIo::buffer(&out));
    panic_for_test(&vm, PanicAt::Program);
    let panic = panic_of(|| {
        let _ = vm.run_program(&program);
    });
    assert_eq!(panic, "a panic made for a test: Program");
    assert_eq!(out.contents(), "main waits\n");
}

/// Dropping a VM whose runtime has panicked runs its shutdown over
/// state that may be half updated. A second panic there is dropped:
/// it would abort the embedder's process if its thread were unwinding
/// with the first. A VM whose runtime has not panicked hides nothing:
/// a panic of its shutdown comes out.
#[test]
fn a_second_panic_in_the_shutdown_of_a_broken_runtime_is_dropped() {
    let program = compiled(LOST_WHILE_ALL_WAIT);
    let mut vm = Vm::new(HostIo::buffer(&Buffer::new()));
    panic_for_test(&vm, PanicAt::Task(0));
    let first = panic_of(|| {
        let _ = vm.run_program(&program);
    });
    assert_eq!(first, "a panic made for a test: Task(0)");
    panic_for_test(&vm, PanicAt::Shutdown);
    drop(vm);

    let sound = Vm::new(HostIo::buffer(&Buffer::new()));
    panic_for_test(&sound, PanicAt::Shutdown);
    assert_eq!(
        panic_of(|| drop(sound)),
        "a panic made for a test: Shutdown"
    );
}

/// The library never ends the process, whatever the VM writes to: a VM
/// on the process's own stdout and stderr, as the `silt` command makes
/// it, gives the panic of its worker out of the call, and this process
/// lives to say so. (Only `silt::scheduler::exit_on_panic`, which the
/// command calls and no test of this binary does, makes a runtime end
/// the process.)
#[test]
fn an_embedder_on_the_process_output_gets_the_panic_and_lives() {
    let program = compiled(LOST_WHILE_ALL_WAIT);
    let mut vm = Vm::new(HostIo::process());
    panic_for_test(&vm, PanicAt::Task(0));
    let panic = panic_of(|| {
        let _ = vm.run_program(&program);
    });
    assert_eq!(panic, "a panic made for a test: Task(0)");
}

/// A second panic while the first is still unwinding: the worker that
/// panicked drops the VM of the task it was about to run, and that
/// drop panics too. It is dropped, the first panic goes its way to the
/// program's thread, and the process is not aborted (which is what
/// Rust does with a panic that leaves a destructor during unwinding).
#[test]
fn a_second_panic_while_the_first_unwinds_is_dropped() {
    let program = compiled(LOST_WHILE_ALL_WAIT);
    let mut vm = Vm::new(HostIo::buffer(&Buffer::new()));
    panic_for_test(&vm, PanicAt::Task(0));
    panic_for_test(&vm, PanicAt::Unwinding);
    let panic = panic_of(|| {
        let _ = vm.run_program(&program);
    });
    assert_eq!(panic, "a panic made for a test: Task(0)");
}
