#![no_main]
use libfuzzer_sys::fuzz_target;
use silt::formatter;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        // The formatter must never panic.
        if let Ok(first) = formatter::format(s) {
            // If it formats once, a second pass must produce the same output
            // (idempotency).
            if let Ok(second) = formatter::format(&first) {
                assert_eq!(first, second, "Formatter is not idempotent");
            }
        }
    }
});
