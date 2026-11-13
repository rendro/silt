#![no_main]
//! What `silt lsp` makes of any bytes from the editor: its transport
//! (`src/lsp/transport.rs`) reads the same messages, and answers the same
//! ones that cannot be read with a JSON-RPC error, however the bytes
//! arrive; and the server handles each message without a panic and
//! answers each request once (`silt::fuzz_invariants::check_lsp_messages`).
use libfuzzer_sys::fuzz_target;
use silt::fuzz_invariants::check_lsp_messages;

fuzz_target!(|data: &[u8]| {
    if let Err(e) = check_lsp_messages(data) {
        panic!("{e}");
    }
});
