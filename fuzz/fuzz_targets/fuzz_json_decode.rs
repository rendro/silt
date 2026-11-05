#![no_main]
use libfuzzer_sys::fuzz_target;

// `json.parse` read two ways on any text: the reading of the text, which
// makes the value as it reads (`decode::seed`), and the walk of the tree
// of the document, which it replaces, must give the same value or the
// same error, for each of a set of types. The input is decoded as it is,
// and as the JSON text its bytes choose (`json_text_of`), which reaches
// the shapes of the types far more often than random bytes do.
use silt::fuzz_invariants::{check_json_decoders_agree, json_text_of};

fuzz_target!(|data: &[u8]| {
    if let Err(e) = check_json_decoders_agree(data) {
        panic!("{e}");
    }
    if let Err(e) = check_json_decoders_agree(&json_text_of(data)) {
        panic!("{e}");
    }
});
