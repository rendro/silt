#![no_main]
//! The readers of a package's files on any text: `silt.toml` (read and
//! validated as a manifest) and `silt.lock`. A git dependency's files are
//! fetched, so their text is anybody's. Each reading must end without a
//! panic and put an error inside the text; a lockfile that reads must be
//! written to a text that reads back as the same lockfile, and is written
//! alike again (`silt::fuzz_invariants::check_package_files`).
use libfuzzer_sys::fuzz_target;
use silt::fuzz_invariants::check_package_files;

fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data)
        && let Err(e) = check_package_files(text)
    {
        panic!("{e}");
    }
});
