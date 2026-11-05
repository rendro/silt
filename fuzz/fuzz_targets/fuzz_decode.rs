#![no_main]
use libfuzzer_sys::fuzz_target;

// The decoders read two ways on any text: the reading of the text, which
// makes the value as it reads (`decode::seed`), and the walk of the tree
// of the document, which it replaces, must give the same value or the
// same error, for each of a set of types: `json.parse` (and `json.decode`
// of the document read), and `toml.parse`, `toml.parse_map` and
// `toml.parse_list`. The input is decoded as it is, and as the JSON and
// the TOML text its bytes choose (`json_text_of`, `toml_text_of`), which
// reach the shapes of the types far more often than random bytes do.
use silt::fuzz_invariants::{
    check_json_decoders_agree, check_toml_decoders_agree, json_text_of, toml_text_of,
};

fuzz_target!(|data: &[u8]| {
    let checked = [
        check_json_decoders_agree(data),
        check_json_decoders_agree(&json_text_of(data)),
        check_toml_decoders_agree(&toml_text_of(data)),
    ];
    for result in checked {
        if let Err(e) = result {
            panic!("{e}");
        }
    }
    if let Ok(text) = std::str::from_utf8(data)
        && let Err(e) = check_toml_decoders_agree(text)
    {
        panic!("{e}");
    }
});
