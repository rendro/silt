#![no_main]
use libfuzzer_sys::fuzz_target;
use silt::fuzz_invariants::check_formatter_invariants;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        // The formatter must never panic, never refuse a text that
        // parses (a refusal is how its own check of the result fails),
        // and its result must be a fixed point.
        check_formatter_invariants(s).unwrap_or_else(|err| {
            panic!("Formatter invariant violated: {err}");
        });
    }
});
