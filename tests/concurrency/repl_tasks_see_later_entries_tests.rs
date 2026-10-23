//! A task of a REPL session sees what later entries define.
//!
//! A task that an entry starts goes on while the next entries are
//! typed. When a later entry hands it a closure, or a value, of its own
//! (through a channel), the task runs it as the session's own code
//! would: it finds the functions, `let`s, impls and traits that were
//! defined after it was started. It used to keep the view of the entry
//! it was started in: a later function was `the global '?' is read
//! before it is set`, a later impl `no method 'n' for type 'Int'`, a
//! later trait's method "a method the program does not have", and a
//! later `Display` impl was silently not used.
//!
//! Each case is run with a task that waits for its job when the job is
//! sent, with a task that never waits (it polls a buffered channel, so
//! nothing stands between getting the job and running it, and it runs
//! while the session takes the later entries in), and with three tasks
//! at once.

#![cfg(feature = "repl")]

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// A session that has not ended after this long is stopped, and its
/// test fails: none of them waits for anything that does not come.
const RUN_TIMEOUT: Duration = Duration::from_secs(60);

/// What `silt repl` wrote for the session `input`: its output after the
/// greeting, and its errors. The session must end by itself, with
/// success.
fn repl(input: &str) -> (String, String) {
    repl_beside(&[], input, Slice::Whole)
}

/// How long a task of a session runs before it gives way to the others.
#[derive(Clone, Copy, Debug)]
enum Slice {
    /// As a session runs: 2,000 steps.
    Whole,
    /// One step (`SILT_TIME_SLICE=1`): every instruction of a task is
    /// a place where the session and the other tasks come in between.
    OneStep,
}

/// [`repl`] in a directory that has the module files `modules`, each a
/// name and its text, with tasks that give way after `slice`.
fn repl_beside(modules: &[(&str, &str)], input: &str, slice: Slice) -> (String, String) {
    static RUNS: AtomicUsize = AtomicUsize::new(0);
    let dir: PathBuf = std::env::temp_dir().join(format!(
        "silt_repl_tasks_{}_{}",
        std::process::id(),
        RUNS.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("a directory for the session");
    // (Files, not pipes: a session that is stopped leaves nobody
    // waiting to read.)
    let file = |name: &str| dir.join(name);
    std::fs::write(file("stdin"), input).expect("the session is written");
    for (name, text) in modules {
        std::fs::write(file(name), text).expect("a module is written");
    }
    let open = |name: &str| std::fs::File::create(file(name)).expect("a file of the session");
    let mut silt = Command::new(env!("CARGO_BIN_EXE_silt"));
    if let Slice::OneStep = slice {
        silt.env("SILT_TIME_SLICE", "1");
    }
    let mut silt = silt
        .arg("repl")
        .current_dir(&dir)
        .env("NO_COLOR", "1")
        .env("SILT_HISTORY_FILE", file("history"))
        .stdin(Stdio::from(
            std::fs::File::open(file("stdin")).expect("the session is read"),
        ))
        .stdout(Stdio::from(open("stdout")))
        .stderr(Stdio::from(open("stderr")))
        .spawn()
        .expect("silt repl starts");
    let started = Instant::now();
    let status = loop {
        match silt.try_wait().expect("silt repl is waited for") {
            Some(status) => break Some(status),
            None if started.elapsed() >= RUN_TIMEOUT => {
                let _ = silt.kill();
                let _ = silt.wait();
                break None;
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    };
    let read = |name: &str| std::fs::read_to_string(file(name)).unwrap_or_default();
    let (stdout, stderr) = (read("stdout").replace("\r\n", "\n"), read("stderr"));
    let _ = std::fs::remove_dir_all(&dir);
    let status = status.unwrap_or_else(|| {
        panic!("the session did not end:\n{input}\nstdout:\n{stdout}\nstderr:\n{stderr}")
    });
    assert!(
        status.success(),
        "{status}\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let output = stdout
        .strip_prefix("Silt REPL (type :quit to exit, :help for commands)\n")
        .unwrap_or_else(|| panic!("the session's greeting: {stdout}"));
    (output.to_string(), stderr)
}

/// How the tasks that run a session's jobs get them.
#[derive(Clone, Copy, Debug)]
enum Workers {
    /// One task, which waits for its job.
    Waiting,
    /// One task, which asks for its job again and again and never
    /// waits: it runs while the later entries are taken in, and runs
    /// the job in the step after it got it.
    Polling,
    /// Three tasks, each of which waits for a job.
    Three,
}

impl Workers {
    fn count(self) -> usize {
        match self {
            Workers::Three => 3,
            _ => 1,
        }
    }
}

/// A session that declares `earlier`, starts its tasks, declares
/// `later`, and then has each task run the closure `{ -> job }` and
/// shows what each gave.
fn session(workers: Workers, earlier: &str, later: &str, job: &str) -> String {
    let work = match workers {
        Workers::Polling => {
            "fn work() {
  match channel.try_receive(jobs) {
    channel.Message(job) -> channel.send(outs, job())
    _ -> work()
  }
}"
        }
        _ => {
            "fn work() {
  match channel.receive(jobs) {
    Some(job) -> channel.send(outs, job())
    None -> ()
  }
}"
        }
    };
    let each = |line: &dyn Fn(usize) -> String| -> String {
        (0..workers.count())
            .map(line)
            .collect::<Vec<_>>()
            .join("\n")
    };
    [
        "import task\nimport channel".to_string(),
        "let jobs = channel.new(8)\nlet outs = channel.new(8)".to_string(),
        earlier.to_string(),
        work.to_string(),
        each(&|n| format!("let w{n} = task.spawn {{ -> work() }}")),
        later.to_string(),
        each(&|_| format!("channel.send(jobs, {{ -> {job} }})")),
        each(&|_| "channel.receive(outs)".to_string()),
        each(&|n| format!("task.join(w{n})")),
    ]
    .join("\n")
        + "\n"
}

/// The tasks, started after `earlier` and before `later`, each run
/// `job` and give `expected`; the session reports no error.
fn assert_tasks_see(earlier: &str, later: &str, job: &str, expected: &str) {
    // (The task that never waits also with a slice of one step.)
    for (workers, slice) in [
        (Workers::Waiting, Slice::Whole),
        (Workers::Polling, Slice::Whole),
        (Workers::Polling, Slice::OneStep),
        (Workers::Three, Slice::Whole),
    ] {
        let input = session(workers, earlier, later, job);
        let (output, errors) = repl_beside(&[], &input, slice);
        assert_eq!(
            output,
            format!("Some({expected})\n").repeat(workers.count()),
            "{workers:?} {slice:?}\n{input}\nstderr:\n{errors}"
        );
        assert_eq!(errors, "", "{workers:?} {slice:?}\n{input}");
    }
}

/// It was `the global '?' is read before it is set`.
#[test]
fn a_task_calls_a_function_and_reads_a_let_of_a_later_entry() {
    assert_tasks_see(
        "",
        "fn later() -> String { \"fn\" }\nlet g = 5",
        "\"{later()} {g}\"",
        "fn 5",
    );
}

/// An impl for a builtin type, written after the task was started and
/// called on a value: the call names the impl's method.
#[test]
fn a_task_calls_a_method_of_an_impl_of_a_later_entry() {
    assert_tasks_see(
        "trait N { fn n(self) -> String }",
        "trait N for Int { fn n(self) -> String { \"int\" } }",
        "N.n(5)",
        "int",
    );
}

/// The same through a generic function of an entry before the task,
/// which finds the method by the type of the value it is given: of a
/// builtin type, and of a type of a later entry. It was `no method 'n'
/// for type 'Int'`.
#[test]
fn a_task_finds_an_impl_of_a_later_entry_through_an_earlier_generic_function() {
    assert_tasks_see(
        "trait N { fn n(self) -> String }\nfn call(v: a) -> String where a: N { N.n(v) }",
        "type Q { q: Int }\n\
         trait N for Int { fn n(self) -> String { \"int\" } }\n\
         trait N for Q { fn n(self) -> String { \"q{self.q}\" } }",
        "\"{call(5)} {call(Q { q: 1 })}\"",
        "int q1",
    );
}

/// A `Display` impl of a later entry is used wherever the value is
/// shown: in an interpolation and by `string.from`, inside a list. It
/// was not used, and nothing said so: `[Q {q: 2}]|[Q {q: 2}]`.
#[test]
fn a_task_shows_a_value_with_the_display_impl_of_a_later_entry() {
    assert_tasks_see(
        "import string\nfn sh(v: a) -> String where a: Display { \"{v}|{string.from(v)}\" }",
        "type Q { q: Int }\ntrait Display for Q { fn display(self) -> String { \"Q!\" } }",
        "sh([Q { q: 2 }])",
        "[Q!]|[Q!]",
    );
}

/// A `json.Encode` impl of a later entry is used wherever the value is
/// written, as a `Display` impl is shown: here in a list.
#[test]
fn a_task_writes_a_value_with_the_json_encode_impl_of_a_later_entry() {
    assert_tasks_see(
        "import json\nfn js(v: a) -> String where a: json.Encode { json.stringify([v]) }",
        "type Q { q: Int }\n\
         trait json.Encode for Q { fn encode(self) -> json.Value { json.String(\"Q!\") } }",
        "js(Q { q: 2 })",
        "[\"Q!\"]",
    );
}

/// A `json.Decode` impl of a later entry decodes its type wherever it is
/// in the document, as a `json.Encode` impl writes it: here as the
/// element of a list.
#[test]
fn a_task_decodes_a_document_with_the_json_decode_impl_of_a_later_entry() {
    assert_tasks_see(
        "import json\nfn ld(text: String, type a) -> Result(List(a), json.JsonError) where a: json.Decode { json.parse_list(text, a) }",
        "type Q { q: Int }\n\
         trait json.Decode for Q { fn decode(value: json.Value) -> Result(Self, json.JsonError) { Ok(Q { q: 9 }) } }",
        "ld(\"[1]\", Q)",
        "Ok([Q {q: 9}])",
    );
}

/// A trait of a later entry, its method called on a value whose type
/// the call does not know. It was `the call of a method names a method
/// the program does not have`.
#[test]
fn a_task_calls_a_method_of_a_trait_of_a_later_entry() {
    assert_tasks_see(
        "",
        "trait M { fn m(self) -> String }\ntrait M for Int { fn m(self) -> String { \"m{self}\" } }",
        "{\n  let call = { y -> M.m(y) }\n  call(7)\n}",
        "m7",
    );
}

/// The decoder finds a record type of a later entry by its id, for a
/// field of a record type of a later entry. It was
/// `json.parse: unknown record type`.
#[test]
fn a_task_decodes_into_record_types_of_a_later_entry() {
    assert_tasks_see(
        "import json",
        "type Inner { n: Int }\ntype Outer { name: String, inner: Inner, all: List(Inner) }",
        r#"json.parse("\{\"name\": \"a\", \"inner\": \{\"n\": 2\}, \"all\": [\{\"n\": 3\}]\}", Outer)"#,
        "Ok(Outer {name: a, inner: Inner {n: 2}, all: [Inner {n: 3}]})",
    );
}

/// The function of a `task.deadline` is a task of its own, started by
/// whoever calls `task.deadline`: here by a task of an earlier entry,
/// in a closure of a later one. It starts with what that task has of
/// the program, which may be the earlier entry's, and finds the later
/// function, the later impl through an earlier generic function, and
/// the later `Display` impl as any task does.
#[test]
fn the_function_of_a_deadline_sees_later_entries_in_a_task_of_an_earlier_one() {
    assert_tasks_see(
        "import time\ntrait N { fn n(self) -> String }\n\
         fn call(v: a) -> String where a: N { N.n(v) }",
        "fn later() -> String { \"fn\" }\n\
         trait N for Int { fn n(self) -> String { \"int\" } }\n\
         type Q { q: Int }\n\
         trait Display for Q { fn display(self) -> String { \"Q!\" } }",
        "task.deadline(time.ms(60000), { -> \"{later()} {call(5)} {[Q { q: 1 }]}\" })",
        "Ok(fn int [Q!])",
    );
}

/// A deadline whose time is over ends its function after a fixed
/// amount of further work, wherever that is. Here it is over from the
/// start, and the function works for longer and longer before it
/// reads a later entry's function and calls a later entry's impl, the
/// two places where its VM takes the program's newest definitions: the
/// end falls before them, on them and behind them. Each call answers
/// one of two things, the value or `Timeout`; none fails; and what
/// the task and the session have of the program is whole afterwards.
#[test]
fn a_deadline_ends_its_function_around_its_look_for_later_entries() {
    let rounds = 190..=235;
    let mut input = format!(
        "import task
import channel
import time
let jobs = channel.new(64)
let outs = channel.new(64)
fn burn(n) {{
  match n {{
    0 -> 0
    _ -> burn(n - 1)
  }}
}}
trait N {{ fn n(self) -> String }}
fn call(v: a) -> String where a: N {{ N.n(v) }}
fn work(left) {{
  match left {{
    0 -> ()
    _ -> match channel.receive(jobs) {{
      Some(job) -> {{
        channel.send(outs, job())
        work(left - 1)
      }}
      None -> ()
    }}
  }}
}}
let worker = task.spawn {{ -> work({}) }}
fn later() -> String {{ \"fn\" }}
trait N for Int {{ fn n(self) -> String {{ \"int\" }} }}
",
        rounds.clone().count() + 1
    );
    for n in rounds.clone() {
        input += &format!(
            "channel.send(jobs, {{ -> task.deadline(time.ms(0), {{ -> {{
  let burned = burn({n})
  \"{{later()}} {{call(burned)}}\"
}} }}) }})
channel.receive(outs)
"
        );
    }
    input +=
        "channel.send(jobs, { -> task.deadline(time.ms(60000), { -> \"{later()} {call(5)}\" }) })
channel.receive(outs)
task.join(worker)
task.deadline(time.ms(60000), { -> \"{later()} {call(5)}\" })
";
    for slice in [Slice::Whole, Slice::OneStep] {
        let (output, errors) = repl_beside(&[], &input, slice);
        assert_eq!(errors, "", "{slice:?}\n{output}");
        let answers: Vec<&str> = output.lines().collect();
        let (swept, rest) = answers.split_at(rounds.clone().count());
        let in_time = "Some(Ok(fn int))";
        let ended = "Some(Err(the function was ended by its deadline))";
        // In time first, ended from some amount of work on, and
        // nothing else.
        let first_ended = (swept.iter().position(|answer| *answer == ended))
            .unwrap_or_else(|| panic!("{slice:?}: no function was ended: {swept:?}"));
        assert!(first_ended > 0, "{slice:?}: every function was ended");
        assert!(
            swept[..first_ended].iter().all(|answer| *answer == in_time)
                && swept[first_ended..].iter().all(|answer| *answer == ended),
            "{slice:?}: {swept:?}"
        );
        assert_eq!(rest, [in_time, "Ok(fn int)"], "{slice:?}");
    }
}

/// Not a closure but a value of a later entry's type, shown by the
/// task's own code, which is of an entry before the type and its
/// `Display` impl.
#[test]
fn a_task_shows_a_value_of_a_later_entry_in_its_own_code() {
    let (output, errors) = repl(
        "import task
import channel
let values = channel.new(8)
let outs = channel.new(8)
fn show() {
  match channel.receive(values) {
    Some(v) -> channel.send(outs, \"shown: {v}\")
    None -> ()
  }
}
let shower = task.spawn { -> show() }
type Q { q: Int }
trait Display for Q { fn display(self) -> String { \"Q!\" } }
channel.send(values, [Q { q: 3 }])
channel.receive(outs)
task.join(shower)
",
    );
    assert_eq!(
        (output.as_str(), errors.as_str()),
        ("Some(shown: [Q!])\n", "")
    );
}

/// An entry that fails adds nothing a task can see: one that does not
/// check declares nothing (the function it would have replaced is the
/// one the task calls), and what an entry declared before it failed
/// when it ran is gone with it, for the task as for the session.
#[test]
fn an_entry_that_fails_adds_nothing_a_task_sees() {
    let (output, errors) = repl(
        "import task
import channel
let jobs = channel.new(8)
let outs = channel.new(8)
fn f() -> Int { 1 }
fn work(left) {
  match left {
    0 -> ()
    _ -> match channel.receive(jobs) {
      Some(job) -> {
        channel.send(outs, job())
        work(left - 1)
      }
      None -> ()
    }
  }
}
let worker = task.spawn { -> work(3) }
fn f() -> Int { \"not an Int\" }
channel.send(jobs, { -> f() })
channel.receive(outs)
let f = 1 / 0
channel.send(jobs, { -> f() + 10 })
channel.receive(outs)
fn f() -> Int { 2 }
channel.send(jobs, { -> f() + 20 })
channel.receive(outs)
task.join(worker)
",
    );
    assert_eq!(output, "Some(1)\nSome(11)\nSome(22)\n", "{errors}");
    let errors: Vec<&str> = (errors.lines())
        .filter(|line| line.starts_with("error"))
        .collect();
    assert_eq!(
        errors,
        [
            "error[type]: type mismatch: expected Int, got String",
            "error[runtime]: division by zero"
        ]
    );
}

/// A task calls a copy of its own of a top-level function. When a
/// later entry defines the function again, the task that had copied
/// the old one calls the new one wherever the code it runs names it:
/// in a closure of the later entry, and as a value of it.
#[test]
fn a_task_calls_a_function_defined_again_after_it_copied_the_old_one() {
    let (output, errors) = repl(
        "import task
import channel
let jobs = channel.new(8)
let outs = channel.new(8)
fn f() -> Int { 1 }
fn work(left) {
  match left {
    0 -> ()
    _ -> match channel.receive(jobs) {
      Some(job) -> {
        channel.send(outs, job())
        work(left - 1)
      }
      None -> ()
    }
  }
}
let worker = task.spawn { -> work(4) }
channel.send(jobs, { -> f() })
channel.receive(outs)
fn f() -> Int { 2 }
channel.send(jobs, { -> f() })
channel.receive(outs)
channel.send(jobs, f)
channel.receive(outs)
fn f() -> Int { 3 }
channel.send(jobs, { -> f() * 10 })
channel.receive(outs)
task.join(worker)
",
    );
    assert_eq!(output, "Some(1)\nSome(2)\nSome(2)\nSome(30)\n", "{errors}");
    assert_eq!(errors, "");
}

/// A session only ever adds to what its tasks can find: a second impl
/// of a trait for a type is refused in a later entry as it is in one
/// file, so a method that a task has found once is the method for
/// good. (`Vm::look_again` rests on it: a task looks for the session's
/// newest definitions only where it has found none.)
#[test]
fn a_second_impl_in_a_later_entry_is_refused() {
    let (output, errors) = repl(
        "import task
import channel
let jobs = channel.new(8)
let outs = channel.new(8)
trait N { fn n(self) -> String }
trait N for Int { fn n(self) -> String { \"first\" } }
fn work(left) {
  match left {
    0 -> ()
    _ -> match channel.receive(jobs) {
      Some(job) -> {
        channel.send(outs, job())
        work(left - 1)
      }
      None -> ()
    }
  }
}
let worker = task.spawn { -> work(2) }
channel.send(jobs, { -> N.n(5) })
channel.receive(outs)
trait N for Int { fn n(self) -> String { \"second\" } }
N.n(5)
channel.send(jobs, { -> N.n(5) })
channel.receive(outs)
task.join(worker)
",
    );
    assert_eq!(output, "Some(first)\nfirst\nSome(first)\n", "{errors}");
    let errors: Vec<&str> = (errors.lines())
        .filter(|line| line.starts_with("error"))
        .collect();
    assert_eq!(
        errors,
        ["error[type]: duplicate implementation of trait 'N' for type 'Int'"]
    );
}

/// The session ends while a task still runs code of a later entry
/// (it never waits) and another still waits: it ends at once, and
/// neither is reported.
#[test]
fn the_session_ends_while_a_task_runs_code_of_a_later_entry() {
    let (output, errors) = repl(
        "import task
import channel
let jobs = channel.new(8)
fn spin(n) {
  match channel.try_receive(jobs) {
    channel.Message(job) -> spin(job())
    _ -> spin(n)
  }
}
let spinning = task.spawn { -> spin(0) }
let waiting = task.spawn { -> channel.receive(channel.new(0)) }
fn later() -> Int { 7 }
channel.send(jobs, { -> later() })
channel.send(jobs, { -> later() + 1 })
1 + 1
",
    );
    assert_eq!((output.as_str(), errors.as_str()), ("2\n", ""));
}

/// A module whose import fails when it runs, after it has set a `let`
/// and its functions, until the file `X2_READY` is written. What it
/// defines reads the file `X2_STAMP` when the module runs. (Files, not
/// the environment: `env.set` is refused while a task is live.)
const LATE: &str = "import io
pub let stamp = match io.read_file(\"X2_STAMP\") {
  Ok(s) -> s
  Err(_) -> \"none\"
}
pub type Tag { t: Int }
trait Display for Tag { fn display(self) -> String { \"tag-{stamp}\" } }
pub fn tag() -> Tag { Tag { t: 1 } }
pub fn stamp_now() -> String { stamp }
pub let ready = match io.read_file(\"X2_READY\") {
  Ok(s) -> s
  Err(_) -> panic(\"not ready\")
}
";

/// A session that imports [`LATE`] twice: the first import fails when
/// it runs, the second runs to its end and reads another stamp. A task
/// that was started before, with `work` (which takes `left` jobs), is
/// asked what the module defines, as the session's own code is.
fn import_twice(work: &str) -> String {
    format!(
        "import task
import channel
import io
let jobs = channel.new(8)
let outs = channel.new(8)
{work}
let worker = task.spawn {{ -> work(2) }}
let _ = io.write_file(\"X2_STAMP\", \"one\")
import late
fn poke() -> String {{ \"poke\" }}
channel.send(jobs, {{ -> poke() }})
channel.receive(outs)
let _ = io.write_file(\"X2_STAMP\", \"two\")
let _ = io.write_file(\"X2_READY\", \"yes\")
import late
\"{{late.stamp}} {{late.stamp_now()}} {{late.tag()}} {{[late.tag()]}}\"
channel.send(jobs, {{ -> \"{{late.stamp}} {{late.stamp_now()}} {{late.tag()}} {{[late.tag()]}}\" }})
channel.receive(outs)
task.join(worker)
task.join(task.spawn {{ -> \"{{late.stamp}} {{late.tag()}}\" }})
"
    )
}

const IMPORTED_TWICE: &str = "Some(poke)
two two tag-two [tag-two]
Some(two two tag-two [tag-two])
two tag-two
";

/// An entry that fails when it runs has defined things: the `let`s and
/// functions of a module it imported, up to where the module failed.
/// When the module is imported again, what it defines is defined anew,
/// in global slots of its own, and that is what a task reads: the
/// second import's values, as the session's own code does, by a global
/// (`late.stamp`), through a function of the module, and by the
/// module's `Display` impl. It read the FIRST import's `stamp`, with no
/// error: the second import set the slots of the first again, the
/// session's VM went on with slots of its own, and the task kept the
/// old ones. (Before a task saw later entries at all, it failed.)
///
/// The task has taken the session's newest definitions in between (it
/// is sent a later function after the failed import), and a task that
/// is started after both imports reads the same.
#[test]
fn a_task_reads_what_an_import_that_failed_once_defines() {
    let work = "fn work(left) {
  match left {
    0 -> ()
    _ -> match channel.receive(jobs) {
      Some(job) -> {
        channel.send(outs, job())
        work(left - 1)
      }
      None -> ()
    }
  }
}";
    let (output, errors) = repl_beside(&[("late.silt", LATE)], &import_twice(work), Slice::Whole);
    assert_eq!(output, IMPORTED_TWICE, "{errors}");
    let errors: Vec<&str> = (errors.lines())
        .filter(|line| line.starts_with("error"))
        .collect();
    assert_eq!(errors, ["error[runtime]: panic: not ready"]);
}

/// The same with a task that never waits: it asks for its job again
/// and again while the two imports are taken in and run. A hundred
/// sessions, every other one with tasks that give way after each step:
/// what the task reads does not depend on when it looks.
#[test]
fn a_task_that_never_waits_reads_what_an_import_that_failed_once_defines() {
    let work = "fn work(left) {
  match left {
    0 -> ()
    _ -> match channel.try_receive(jobs) {
      channel.Message(job) -> {
        channel.send(outs, job())
        work(left - 1)
      }
      _ -> work(left)
    }
  }
}";
    let session = import_twice(work);
    for round in 0..100 {
        let slice = [Slice::Whole, Slice::OneStep][round % 2];
        let (output, errors) = repl_beside(&[("late.silt", LATE)], &session, slice);
        assert_eq!(output, IMPORTED_TWICE, "round {round} {slice:?}\n{errors}");
    }
}

/// What an entry that failed when it ran has handed out keeps what it
/// was made with, and what is defined again is new, for a task as for
/// the session: a closure the failed entry sent to a task reads the
/// entry's own `let` and calls the entry's own impl (old code, old
/// bindings, as for a definition that is made again); the impl that a
/// later entry writes in its place is the one every later call finds,
/// also a call in a function older than both, made by the task. And no
/// global slot is ever set a second time in a session, whatever its
/// entries do: that is what lets a task keep what it has found.
///
/// (An entry of several declarations, which `Repl::eval` takes and a
/// terminal does not: there an impl is an entry of its own, and cannot
/// fail when it runs.)
#[test]
#[cfg(feature = "test-hooks")]
fn a_session_sets_no_slot_twice() {
    use silt::repl::Repl;
    use silt::session::ProjectSetup;

    let dir = std::env::temp_dir().join(format!("silt_repl_slots_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a directory for the session");
    // The module reads its files by their full names: the session's
    // own directory is the test's.
    let at = format!("{}/", dir.display()).replace('\\', "/");
    let late = LATE.replace("X2_", &format!("{at}X2_"));
    for name in ["X2_STAMP", "X2_READY"] {
        let _ = std::fs::remove_file(dir.join(name));
    }
    std::fs::write(dir.join("late.silt"), late).expect("a module is written");
    let mut repl = Repl::new(ProjectSetup::Discover(dir.clone()));
    let before = silt::vm::slots_set_again();
    let mut run = |entry: &str| -> Result<String, String> {
        let evaluation = repl.eval(entry);
        let errors: Vec<String> = (evaluation.diagnostics.iter())
            .filter(|d| d.is_error())
            .map(|d| d.message.clone())
            .collect();
        match (evaluation.committed, evaluation.value) {
            (true, Some(value)) => Ok(repl.show(&value)),
            _ => Err(errors.join("; ")),
        }
    };
    let ok = |shown: &str| Ok(shown.to_string());
    let failed = |error: &str| Err(error.to_string());
    for entry in [
        "import task\nimport channel\nimport io",
        "let jobs = channel.new(8)\nlet outs = channel.new(8)",
        "trait N { fn n(self) -> String }\nfn call(v: a) -> String where a: N { N.n(v) }",
        "fn work() {\n  match channel.receive(jobs) {\n    Some(job) -> {\n      \
         channel.send(outs, job())\n      work()\n    }\n    None -> ()\n  }\n}",
        "let worker = task.spawn { -> work() }",
    ] {
        assert_eq!(run(entry), ok("()"), "{entry}");
    }
    // An entry that writes an impl, hands a closure to the task and
    // then fails.
    assert_eq!(
        run("let stamp = \"one\"\n\
             trait N for Int { fn n(self) -> String { \"first {stamp}\" } }\n\
             let sent = channel.send(jobs, { -> \"{stamp} {N.n(5)} {call(5)}\" })\n\
             let got = channel.receive(outs)\n\
             let boom = 1 / 0"),
        failed("division by zero")
    );
    // The impl is gone with the entry, for the session and for the task.
    assert!(run("N.n(5)").is_err());
    // It is written again, and differently.
    assert_eq!(
        run("trait N for Int { fn n(self) -> String { \"second\" } }"),
        ok("()")
    );
    assert_eq!(run("\"{N.n(5)} {call(5)}\""), ok("second second"));
    assert_eq!(
        run("channel.send(jobs, { -> \"{N.n(5)} {call(5)}\" })"),
        ok("()")
    );
    assert_eq!(run("channel.receive(outs)"), ok("Some(second second)"));
    // A module whose import fails once, and a redefinition of each kind.
    let write = |name: &str, text: &str| format!("io.write_file(\"{at}{name}\", \"{text}\")");
    assert_eq!(run(&write("X2_STAMP", "one")), ok("Ok(())"));
    assert_eq!(run("import late"), failed("panic: not ready"));
    assert_eq!(run(&write("X2_STAMP", "two")), ok("Ok(())"));
    assert_eq!(run(&write("X2_READY", "y")), ok("Ok(())"));
    assert_eq!(run("import late"), ok("()"));
    assert_eq!(
        run("channel.send(jobs, { -> \"{late.stamp} {late.tag()}\" })"),
        ok("()")
    );
    assert_eq!(run("channel.receive(outs)"), ok("Some(two tag-two)"));
    for entry in [
        "let stamp = \"again\"",
        "fn call(v: a) -> String where a: N { \"again {N.n(v)}\" }",
        "type Q { q: Int }",
        "trait N for Q { fn n(self) -> String { \"q\" } }",
        "type Q { r: Int }",
        "trait N for Q { fn n(self) -> String { \"r\" } }",
        "import late",
    ] {
        assert_eq!(run(entry), ok("()"), "{entry}");
    }
    assert_eq!(run("call(Q { r: 1 })"), ok("again r"));
    assert_eq!(silt::vm::slots_set_again(), before);
    drop(repl);
    let _ = std::fs::remove_dir_all(&dir);
}
