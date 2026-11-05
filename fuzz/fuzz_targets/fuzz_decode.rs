#![no_main]
use libfuzzer_sys::fuzz_target;

// The decoder on any text: `json.parse` and `toml.parse`,
// `toml.parse_map` and `toml.parse_list` into each of a set of types,
// with and without written impls of some of them, as the VM's frame runs
// a decoding (two readings with the impls between them). Each decoding
// must end without a panic, and alike when it runs again; and where the
// text is a JSON document, `json.decode` of the `json.Value` it holds
// must decode alike (the same value or error, the impls called with the
// same parts in the same order). libFuzzer's limits catch a decoding
// that does not end or grows without bound. The input is decoded as it
// is, and as the JSON and the TOML text its bytes choose (`json_text_of`,
// `toml_text_of`), which reach the shapes of the types far more often
// than random bytes do.
use silt::fuzz_invariants::{
    check_json_decoding, check_toml_decoding, json_text_of, toml_text_of,
};

fuzz_target!(|data: &[u8]| {
    let checked = [
        check_json_decoding(data),
        check_json_decoding(&json_text_of(data)),
        check_toml_decoding(&toml_text_of(data)),
    ];
    for result in checked {
        if let Err(e) = result {
            panic!("{e}");
        }
    }
    if let Ok(text) = std::str::from_utf8(data)
        && let Err(e) = check_toml_decoding(text)
    {
        panic!("{e}");
    }
});
