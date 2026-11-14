#![no_main]
//! What the client of `http.get` and `http.request` makes of any
//! response a server sends: the bytes as a response's head and body
//! become the program's `Response` without a panic, with the status,
//! headers and body that were sent (a gzip body unpacked), and a
//! redirect is resolved to a URL on the right server
//! (`silt::fuzz_invariants::check_http_response`).
use libfuzzer_sys::fuzz_target;
use silt::fuzz_invariants::check_http_response;

fuzz_target!(|data: &[u8]| {
    if let Err(e) = check_http_response(data) {
        panic!("{e}");
    }
});
