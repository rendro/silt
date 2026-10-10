//! The oracle on programs whose verdict is known: what it runs, what it
//! leaves alone, and that it sees what it is there to see.

use crate::oracle::{Compared, Expect, Input, Kind, NotRun, Source, Verdict, examine};

fn verdict_with(text: &str, expect: Expect) -> Verdict {
    examine(&Input {
        name: "selfcheck".to_string(),
        source: Source::Memory(vec![("main.silt".to_string(), text.to_string())]),
        expect,
    })
}

fn verdict(text: &str) -> Verdict {
    verdict_with(text, Expect::default())
}

#[track_caller]
fn assert_finding(verdict: Verdict, kind: Kind, detail: &str) {
    match verdict {
        Verdict::Finding(finding) if finding.kind == kind && finding.detail.contains(detail) => {}
        other => panic!(
            "expected a {} finding with {detail:?}, got {other:?}",
            kind.name()
        ),
    }
}

#[test]
fn a_program_that_checks_clean_is_run_and_compared() {
    let passed = verdict("import list\nfn main() {\n  println(list.length([1, 2]))\n  7\n}\n");
    assert!(
        matches!(passed, Verdict::Passed(Compared::Everything)),
        "{passed:?}"
    );
}

/// A runtime error of the program is no finding: both runs end in it.
#[test]
fn a_runtime_error_that_both_runs_share_passes() {
    let passed = verdict("fn main() {\n  println(\"before\")\n  1 / 0\n}\n");
    assert!(
        matches!(passed, Verdict::Passed(Compared::Everything)),
        "{passed:?}"
    );
}

#[test]
fn a_program_with_a_check_error_is_not_run() {
    let not_run = verdict("fn main() {\n  1 + \"one\"\n}\n");
    assert!(
        matches!(not_run, Verdict::NotRun(NotRun::CheckError)),
        "{not_run:?}"
    );
}

#[test]
fn a_program_without_main_is_not_run() {
    let not_run = verdict("fn helper() {\n  1\n}\n");
    assert!(
        matches!(not_run, Verdict::NotRun(NotRun::NotCompiled)),
        "{not_run:?}"
    );
}

/// A builtin that reaches outside the VM keeps a program from running,
/// called or held as a value, in code that would never run too.
#[test]
fn a_program_that_names_a_builtin_that_reaches_outside_is_not_run() {
    for (text, module) in [
        (
            "import io\nfn main() {\n  io.read_file(\"/nonexistent\")\n}\n",
            "io",
        ),
        (
            "import fs\nfn main() {\n  let exists = fs.exists\n  exists(\"/nonexistent\")\n}\n",
            "fs",
        ),
        (
            "import stream\nfn unused() {\n  stream.file_lines(\"/nonexistent\")\n}\nfn main() {\n  1\n}\n",
            "stream",
        ),
    ] {
        match verdict(text) {
            Verdict::NotRun(NotRun::Outside(named)) => assert_eq!(named, module),
            other => panic!("{text}: {other:?}"),
        }
    }
}

/// `io.inspect` and the streams over lists stay inside the VM.
#[test]
fn the_builtins_of_those_modules_that_stay_inside_are_run() {
    let inspect = verdict("import io\nfn main() {\n  println(io.inspect([1, 2]))\n}\n");
    assert!(
        matches!(inspect, Verdict::Passed(Compared::Everything)),
        "{inspect:?}"
    );
    let stream = verdict(
        "import stream\nfn main() {\n  println(stream.count(stream.from_list([1, 2, 3])))\n}\n",
    );
    assert!(
        matches!(stream, Verdict::Passed(Compared::Invariants)),
        "{stream:?}"
    );
}

/// The output of a program with tasks is compared where it is known.
#[test]
fn a_program_with_tasks_is_compared_when_its_output_is_given() {
    let text = "import task\nfn main() {\n  let h = task.spawn({ -> 41 + 1 })\n  println(task.join(h))\n}\n";
    let free = verdict(text);
    assert!(
        matches!(free, Verdict::Passed(Compared::Invariants)),
        "{free:?}"
    );
    let expect = |stdout: &str| Expect {
        succeeds: true,
        stdout: Some(stdout.to_string()),
        end: None,
    };
    let held = verdict_with(text, expect("42\n"));
    assert!(
        matches!(held, Verdict::Passed(Compared::Everything)),
        "{held:?}"
    );
    assert_finding(
        verdict_with(text, expect("43\n")),
        Kind::Expectation,
        "stdout is not the expected one",
    );
}

#[test]
fn a_run_that_fails_where_success_is_expected_is_a_finding() {
    let succeeds = || Expect {
        succeeds: true,
        ..Expect::default()
    };
    assert_finding(
        verdict_with("fn main() {\n  1 / 0\n}\n", succeeds()),
        Kind::Expectation,
        "runtime error: division by zero",
    );
    assert_finding(
        verdict_with("fn main() {\n  Err(\"no\")\n}\n", succeeds()),
        Kind::Expectation,
        "main returned",
    );
    // A task that fails and that nobody joins makes `silt run` fail.
    let unjoined = "import task\nimport time\nfn main() {\n  let h = task.spawn({ -> 1 / 0 })\n  time.sleep(time.ms(50))\n}\n";
    assert_finding(
        verdict_with(unjoined, succeeds()),
        Kind::Expectation,
        "a task failed and nobody joined it: division by zero",
    );
}

/// The system's random source is the one thing two runs of a program
/// without tasks do not share: the oracle sees the difference when it
/// is told that the output is the program's own.
#[test]
fn two_runs_that_write_different_output_are_a_finding() {
    let text = "import uuid\nfn main() {\n  println(uuid.v4())\n}\n";
    let free = verdict(text);
    assert!(
        matches!(free, Verdict::Passed(Compared::Invariants)),
        "{free:?}"
    );
    let own = Expect {
        succeeds: true,
        stdout: Some("not a UUID\n".to_string()),
        end: None,
    };
    assert_finding(verdict_with(text, own), Kind::Expectation, "stdout");
}
