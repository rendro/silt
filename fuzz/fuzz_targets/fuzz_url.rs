#![no_main]
//! The reader of URLs (`src/url.rs`) on any text: it must end without a
//! panic and put an error inside the text, and a URL that reads must be
//! written as a text that reads back as the same URL, hide its user and
//! password when shown, name its host plainly, and give back through
//! each `with_` function what its readers give; the text is also
//! resolved as a reference, escaped and decoded
//! (`silt::testing::fuzz_invariants::check_url`).
use libfuzzer_sys::fuzz_target;
use silt::testing::fuzz_invariants::check_url;

fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data)
        && let Err(e) = check_url(text)
    {
        panic!("{e}");
    }
});
