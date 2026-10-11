//! The oracle on programs whose verdict is known: what it runs, what it
//! leaves alone, and that it sees what it is there to see.

use crate::oracle::{Compared, Cut, Expect, Input, Kind, NotRun, Source, Steps, Verdict, examine};

/// The step budgets of these programs' runs.
const STEPS: Steps = Steps {
    each: 100_000,
    again: 100_000,
};

fn verdict_of(source: Source, expect: Expect, steps: Steps) -> Verdict {
    let input = Input {
        name: "selfcheck".to_string(),
        source,
        real_time: true,
        expect,
    };
    examine(&input, steps)
}

fn verdict_with(text: &str, expect: Expect) -> Verdict {
    let source = Source::Memory(vec![("main.silt".to_string(), text.to_string())]);
    verdict_of(source, expect, STEPS)
}

/// What a golden case with `cmd: run` and `exit: 0` says of its
/// program.
fn succeeds() -> Expect {
    Expect {
        succeeds: true,
        ..Expect::default()
    }
}

/// A directory of its own with `files` (path from the directory, text)
/// in it; removed when the value is dropped.
struct Files(std::path::PathBuf);

impl Files {
    fn new(name: &str, files: &[(&str, &str)]) -> Files {
        let dir = std::env::temp_dir().join(format!("silt-oracle-{}-{name}", std::process::id()));
        for (path, text) in files {
            let file = dir.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, text).unwrap();
        }
        Files(dir)
    }

    fn file(&self, path: &str) -> std::path::PathBuf {
        self.0.join(path)
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
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
    assert_finding(
        verdict_with(text, own),
        Kind::Differs,
        "slice 2000 against slice 1: stdout differs",
    );
}

/// A program that does not end is cut short by the step budget: no
/// finding, and nothing compared.
#[test]
fn a_program_that_does_not_end_is_cut_short() {
    let endless = verdict("fn main() {\n  loop {\n    loop()\n  }\n}\n");
    assert!(
        matches!(endless, Verdict::Cut(Cut::OutOfSteps)),
        "{endless:?}"
    );
    // So is one whose task does not end, joined or not.
    let spin = "import task\nfn spin() {\n  loop {\n    loop()\n  }\n}\n";
    for main in [
        "fn main() {\n  task.join(task.spawn(spin))\n}\n",
        "fn main() {\n  let h = task.spawn(spin)\n  1\n}\n",
    ] {
        let cut = verdict(&format!("{spin}{main}"));
        assert!(
            matches!(cut, Verdict::Cut(Cut::OutOfSteps)),
            "{main}: {cut:?}"
        );
    }
}

/// A program that prints while it loops writes more at slice 2000 than
/// at slice 1 before its steps are used up: that is no difference of
/// the two runs.
#[test]
fn what_a_program_wrote_before_it_was_cut_short_is_not_compared() {
    let counting = "fn main() {\n  loop i = 0 {\n    println(i)\n    loop(i + 1)\n  }\n}\n";
    let cut = verdict(counting);
    assert!(matches!(cut, Verdict::Cut(Cut::OutOfSteps)), "{cut:?}");
}

/// A program that ends at one slice and is cut short at the other is a
/// finding: a fault that loses a program at a slice boundary shows so.
/// Here the program is whole and the budget is what differs: with one
/// step, slice 2000 runs the program to its end (the budget is looked
/// at where a slice ends) and slice 1 does not.
#[test]
fn a_program_that_ends_at_one_slice_only_is_a_finding() {
    let text = "fn main() {\n  println(\"ran\")\n  41 + 1\n}\n";
    let source = || Source::Memory(vec![("main.silt".to_string(), text.to_string())]);
    let one_step = Steps { each: 1, again: 1 };
    assert_finding(
        verdict_of(source(), Expect::default(), one_step),
        Kind::OneSlice,
        "ends at slice 2000 (main returned 42) and not at slice 1: out of steps (1 steps)",
    );
    // The run that was cut short is repeated with the larger budget
    // first: a program that then ends is compared as usual.
    let repeated = Steps {
        each: 1,
        again: 100_000,
    };
    let passed = verdict_of(source(), Expect::default(), repeated);
    assert!(
        matches!(passed, Verdict::Passed(Compared::Everything)),
        "{passed:?}"
    );
    // And one that is still cut short then is the finding.
    let longer = "fn main() {\n  loop i = 0 {\n    match i >= 100 {\n      true -> i\n      false -> loop(i + 1)\n    }\n  }\n}\n";
    let source = Source::Memory(vec![("main.silt".to_string(), longer.to_string())]);
    let short = Steps { each: 1, again: 50 };
    assert_finding(
        verdict_of(source, Expect::default(), short),
        Kind::OneSlice,
        "ends at slice 2000 (main returned 100) and not at slice 1: out of steps (50 steps)",
    );
}

/// A program that the command runs and that does not check, or has no
/// `main`, in the oracle's session is a finding of the oracle: counted
/// as "not run" it would pass for ever.
#[test]
fn a_program_that_is_to_run_and_does_not_check_is_a_finding() {
    assert_finding(
        verdict_with("fn main() {\n  1 + \"one\"\n}\n", succeeds()),
        Kind::Expectation,
        "the oracle has: check error",
    );
    assert_finding(
        verdict_with("fn helper() {\n  1\n}\n", succeeds()),
        Kind::Expectation,
        "the oracle has: not compiled for main",
    );
    // A builtin that reaches outside is the oracle's own reason.
    let outside = verdict_with("import io\nfn main() {\n  io.args()\n}\n", succeeds());
    assert!(
        matches!(outside, Verdict::NotRun(NotRun::Outside(_))),
        "{outside:?}"
    );
}

/// The same for a program on disk, as the directory cases of the golden
/// corpus are: a script with a module beside it, and a package with a
/// manifest and a dependency. Both are run; with an error in a module
/// they are findings, and "not run" only when nothing says that they
/// run.
#[test]
fn programs_on_disk_are_run_and_one_that_does_not_check_is_a_finding() {
    let main = "import util\nfn main() {\n  println(util.twice(21))\n}\n";
    let util = "pub fn twice(n) {\n  n * 2\n}\n";
    let broken = "pub fn twice(n) {\n  n * \"two\"\n}\n";
    let expect = || Expect {
        succeeds: true,
        stdout: Some("42\n".to_string()),
        end: None,
    };

    let script = Files::new("script", &[("main.silt", main), ("util.silt", util)]);
    let ran = verdict_of(Source::Script(script.file("main.silt")), expect(), STEPS);
    assert!(
        matches!(ran, Verdict::Passed(Compared::Everything)),
        "{ran:?}"
    );
    let script = Files::new(
        "script-broken",
        &[("main.silt", main), ("util.silt", broken)],
    );
    let source = || Source::Script(script.file("main.silt"));
    assert_finding(
        verdict_of(source(), expect(), STEPS),
        Kind::Expectation,
        "the oracle has: check error",
    );
    let not_run = verdict_of(source(), Expect::default(), STEPS);
    assert!(
        matches!(not_run, Verdict::NotRun(NotRun::CheckError)),
        "{not_run:?}"
    );

    let manifest = "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nutil = { path = \"util\" }\n";
    let dep_manifest = "[package]\nname = \"util\"\nversion = \"0.1.0\"\n";
    let package = |name: &str, lib: &str| {
        Files::new(
            name,
            &[
                ("silt.toml", manifest),
                ("src/main.silt", main),
                ("util/silt.toml", dep_manifest),
                ("util/src/lib.silt", lib),
            ],
        )
    };
    let files = package("package", util);
    let ran = verdict_of(
        Source::Package(files.file("src/main.silt")),
        expect(),
        STEPS,
    );
    assert!(
        matches!(ran, Verdict::Passed(Compared::Everything)),
        "{ran:?}"
    );
    let files = package("package-broken", broken);
    assert_finding(
        verdict_of(
            Source::Package(files.file("src/main.silt")),
            expect(),
            STEPS,
        ),
        Kind::Expectation,
        "the oracle has: check error",
    );
}
