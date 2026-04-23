#![no_main]
use libfuzzer_sys::fuzz_target;
use silt::formatter;
use silt::fuzz_invariants::check_formatter_invariants;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        // The formatter must never panic.
        if let Ok(first) = formatter::format(s) {
            // Structural invariants: token count (minus whitespace),
            // delimiter balance, comment-marker count, and parse-
            // preservation must all be upheld between input and output.
            check_formatter_invariants(s, &first).unwrap_or_else(|err| {
                panic!("Formatter invariant violated: {err}");
            });

            // Idempotency: a second pass must produce the same output.
            if let Ok(second) = formatter::format(&first) {
                assert_eq!(first, second, "Formatter is not idempotent");
            }
        }
    }
});
