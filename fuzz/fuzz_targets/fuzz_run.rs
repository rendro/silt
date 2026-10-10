#![no_main]
use libfuzzer_sys::fuzz_target;

// The differential oracle on any text (tests/oracle/oracle.rs, which
// this target includes): a text that is a program, checks clean and
// names no builtin that reaches outside the VM is compiled, every
// compiled function must pass the bytecode verifier again, and the
// program is run twice, in slices of 2000 steps and of one step. The
// two runs must agree in output, in the failures of tasks that nobody
// joined and in `main`'s value or error (for a program that uses tasks
// or the clock only the following holds), and no run and no task may
// end in a `type_confusion` error, an internal error or a panic, or
// not end.
//
// This is the one target that runs programs. What makes that possible
// is the step budget (`Vm::set_step_budget`): a program that does not
// end is cut short and is no finding. The oracle gives a program that
// sleeps a clock that leaps, so a sleep takes no time.
//
// The budget is small on purpose. A fuzzer finds the slow programs
// first, and a program that nests a value 100,000 deep needs a million
// steps to build it: showing or comparing such a value overflows the
// native stack (see `abort` in tests/oracle/skip.txt), which is known
// and would be all this target reports.
#[allow(dead_code)]
#[path = "../../tests/oracle/oracle.rs"]
mod oracle;

use oracle::{Expect, Input, Source, Verdict, examine};

/// The step budget of each run.
const STEPS: u64 = 200_000;

fuzz_target!(|data: &[u8]| {
    // The lexer's and the parser's targets have the texts that are not
    // UTF-8 or no program; the oracle does not run them.
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let input = Input {
        name: "the input".to_string(),
        source: Source::Memory(vec![("main.silt".to_string(), text.to_string())]),
        real_time: false,
        expect: Expect::default(),
    };
    if let Verdict::Finding(finding) = examine(&input, STEPS) {
        panic!("{}: {}", finding.kind.name(), finding.detail);
    }
});
