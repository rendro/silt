#![no_main]
//! The csv reader (`csv.parse`, `csv.split_line`) on any text: it must
//! end without a panic and put an error inside the text, and the records
//! of a text that reads, written again with every field in quotes, must
//! read back as the same records (`silt::fuzz_invariants::check_csv`).
use libfuzzer_sys::fuzz_target;
use silt::fuzz_invariants::check_csv;

fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data)
        && let Err(e) = check_csv(text)
    {
        panic!("{e}");
    }
});
