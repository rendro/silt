//! The playground's functions, run on the machine that builds them. What
//! differs in a browser (one thread, the page's clock) is tested there:
//! `tests/wasm.mjs`.

use std::time::Duration;

use silt::SystemClock;
use silt_playground::{PageClock, STEP_BUDGET, TOKEN_KINDS, check, complete, format, run, tokens};

fn run_it(source: &str) -> silt_playground::Run {
    run(source, SystemClock, STEP_BUDGET)
}

#[test]
fn a_program_runs_and_its_output_is_collected() {
    let ran = run_it("import list\nfn main() {\n  list.each(1..3) { n -> println(n) }\n}\n");
    assert_eq!(ran.output, "1\n2\n3\n");
    assert_eq!(
        (ran.diagnostics.as_str(), ran.error.as_str(), ran.ok),
        ("", "", true)
    );
}

#[test]
fn a_static_error_is_reported_as_silt_run_reports_it_and_nothing_runs() {
    let ran = run_it("fn main() {\n  println(\"before\")\n  list.length([1])\n}\n");
    assert!(!ran.ok);
    assert_eq!(ran.output, "");
    assert!(
        ran.diagnostics
            .starts_with("error[type]: module 'list' is not imported\n --> main.silt:3:3"),
        "{}",
        ran.diagnostics
    );
    assert!(
        ran.diagnostics.contains("help: add `import list`"),
        "{}",
        ran.diagnostics
    );
}

#[test]
fn a_runtime_error_keeps_what_was_printed_before_it() {
    let ran =
        run_it("fn main() {\n  println(\"before\")\n  let zero = 0\n  println(1 / zero)\n}\n");
    assert!(!ran.ok);
    assert_eq!(ran.output, "before\n");
    assert!(ran.error.starts_with("error[runtime]: "), "{}", ran.error);
    assert!(ran.error.contains(" --> main.silt:4:"), "{}", ran.error);
}

#[test]
fn a_main_that_returns_err_has_failed() {
    let ran = run_it("fn main() {\n  Err(\"no\")\n}\n");
    assert!(!ran.ok);
    assert!(ran.error.contains("main returned Err: no"), "{}", ran.error);
    assert!(ran.error.contains(" --> main.silt:1:4"), "{}", ran.error);
}

#[test]
fn a_program_without_main_is_an_error_of_run_and_not_of_check() {
    let source = "fn helper() {\n  1\n}\n";
    let ran = run_it(source);
    assert!(!ran.ok);
    assert!(ran.diagnostics.contains("main"), "{}", ran.diagnostics);
    assert_eq!(check(source), vec![]);
}

#[test]
fn a_program_that_loops_for_ever_is_ended_by_the_step_budget() {
    let ran = run(
        "fn spin(n) {\n  spin(n + 1)\n}\nfn main() {\n  println(\"start\")\n  spin(0)\n}\n",
        SystemClock,
        50_000,
    );
    assert!(!ran.ok);
    assert_eq!(ran.output, "start\n");
    assert!(
        ran.error.contains("the step budget is used up"),
        "{}",
        ran.error
    );
    assert!(
        ran.error
            .contains("the playground ends a program after 50000 steps"),
        "{}",
        ran.error
    );
}

#[test]
fn a_task_that_failed_and_that_nobody_joined_is_reported_in_the_output() {
    let ran = run_it(
        "import task\nfn main() {\n  let unjoined = task.spawn { -> panic(\"in the task\") }\n  println(\"main is done\")\n}\n",
    );
    assert!(ran.output.starts_with("main is done\n"), "{ran:?}");
    assert!(ran.output.contains("in the task"), "{ran:?}");
}

/// A fixed time of day: 2026-10-05T12:00:00Z.
fn noon() -> f64 {
    1_791_201_600_000.0
}

#[test]
fn a_sleep_moves_the_page_clock_on_instead_of_waiting() {
    let source = r#"
import time
import result
fn main() {
  time.sleep(time.minutes(90))
  println(time.now() |> time.to_datetime(0) |> time.format("%H:%M") |> result.unwrap_or(""))
}
"#;
    let started = std::time::Instant::now();
    let ran = run(source, PageClock::new(noon), STEP_BUDGET);
    assert_eq!(ran.output, "13:30\n", "{ran:?}");
    assert!(started.elapsed() < Duration::from_secs(60));
}

#[test]
fn check_gives_offsets_in_utf16_code_units() {
    // `é` is two bytes and one unit; `😀` is four bytes and two units.
    let source = "-- é😀\nfn main() {\n  nowhere\n}\n";
    let problems = check(source);
    assert_eq!(problems.len(), 1, "{problems:?}");
    let utf16: Vec<u16> = source.encode_utf16().collect();
    let name: Vec<u16> = "nowhere".encode_utf16().collect();
    let from = utf16.windows(name.len()).position(|w| w == name).unwrap() as u32;
    assert_eq!((problems[0].from, problems[0].to), (from, from + 7));
    assert_eq!(problems[0].severity, "error");
    assert!(problems[0].message.contains("nowhere"), "{problems:?}");
}

#[test]
fn check_puts_the_help_under_the_message() {
    let problems = check("fn main() {\n  list.length([1])\n}\n");
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert_eq!(
        problems[0].message,
        "module 'list' is not imported\nhelp: add `import list` at the top of the file"
    );
}

#[test]
fn format_formats_and_reports_what_is_no_program() {
    assert_eq!(
        format("fn main(){println( 1 )}\n").as_deref(),
        Ok("fn main() {\n  println(1)\n}\n")
    );
    let refused = format("fn main( {\n").unwrap_err();
    assert!(refused.starts_with("error[parse]: "), "{refused}");
    assert!(refused.contains(" --> main.silt:1:"), "{refused}");
}

#[test]
fn complete_offers_keywords_builtins_and_what_the_text_declares() {
    let source = "type Shape {\n  Circle(Int),\n  Square(Int),\n}\nlet limit = 3\nfn area(s) {\n  s\n}\nfn main() {\n  are\n}\n";
    let items = complete(source);
    let kind_of = |label: &str| {
        items
            .iter()
            .find(|item| item.label == label)
            .map(|item| item.kind)
    };
    assert_eq!(kind_of("match"), Some("keyword"));
    assert_eq!(kind_of("true"), Some("keyword"));
    assert_eq!(kind_of("println"), Some("function"));
    assert_eq!(kind_of("Ok"), Some("constructor"));
    assert_eq!(kind_of("list"), Some("module"));
    assert_eq!(kind_of("list.map"), Some("function"));
    assert_eq!(kind_of("math.pi"), Some("constant"));
    assert_eq!(kind_of("channel.Message"), Some("constructor"));
    assert_eq!(kind_of("Shape"), Some("type"));
    assert_eq!(kind_of("Circle"), Some("constructor"));
    assert_eq!(kind_of("limit"), Some("variable"));
    assert_eq!(kind_of("area"), Some("function"));
    // This build has no network: what it cannot run it does not offer.
    assert_eq!(kind_of("http.get"), None);
    assert_eq!(kind_of("tcp"), None);
}

#[test]
fn tokens_are_the_lexers_with_utf16_offsets() {
    let source = "-- é😀\nlet s = \"a{1}b\" {- c -}\nfn f(x: Int) { x |> g }\n";
    let utf16: Vec<u16> = source.encode_utf16().collect();
    let flat = tokens(source);
    assert_eq!(flat.len() % 3, 0);
    let got: Vec<(String, &str)> = flat
        .chunks(3)
        .map(|t| {
            let text = String::from_utf16(&utf16[t[0] as usize..t[1] as usize]).unwrap();
            (text, TOKEN_KINDS[t[2] as usize])
        })
        .collect();
    let want = [
        ("-- é😀", "comment"),
        ("let", "keyword"),
        ("=", "punctuation"),
        ("\"a{", "string"),
        ("1", "number"),
        ("}b\"", "string"),
        ("{- c -}", "comment"),
        ("fn", "keyword"),
        ("(", "punctuation"),
        (":", "punctuation"),
        ("Int", "type"),
        (")", "punctuation"),
        ("{", "punctuation"),
        ("|>", "operator"),
        ("}", "punctuation"),
    ];
    let got: Vec<(&str, &str)> = got
        .iter()
        .map(|(text, kind)| (text.as_str(), *kind))
        .collect();
    assert_eq!(got, want);
}

#[test]
fn a_text_with_errors_has_tokens_too() {
    let flat = tokens("let x = \"open\nfn § main");
    assert!(
        flat.chunks(3)
            .any(|t| TOKEN_KINDS[t[2] as usize] == "keyword")
    );
}
