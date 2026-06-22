//! Structural no-op lock for the `finish_http_response` extraction.
//!
//! BLOAT (pre-fix): `do_http_get` and `do_http_request` each ended with a
//! byte-identical 5-line block:
//!
//!     match <result> {
//!         Ok(response) => match ureq_response_to_value(response) {
//!             Ok(resp) => Value::Variant("Ok".into(), vec![resp]),
//!             Err(e) => http_response_decode_err(e.message),
//!         },
//!         Err(e) => http_err(&format!("{e}"), url),
//!     }
//!
//! The duplication was extracted into a single private helper,
//! `finish_http_response`, called from both sites. This lock is a pure
//! SOURCE-LEVEL, behavior-preserving assertion: it does NOT exercise live
//! HTTP (untestable in CI). It pins that (a) the helper now exists, and
//! (b) the inner duplicated match-pair no longer appears twice inline.

const DATA_RS: &str = include_str!("../src/builtins/data.rs");

#[test]
fn finish_http_response_helper_exists() {
    assert!(
        DATA_RS.contains("fn finish_http_response("),
        "expected extracted helper `fn finish_http_response(` in src/builtins/data.rs"
    );
}

#[test]
fn duplicated_http_match_block_appears_at_most_once() {
    // The distinctive inner line of the duplicated block. Before the fix it
    // appeared once inside `do_http_get` and once inside `do_http_request`
    // (the two inline copies). After extraction it must appear in exactly
    // one place: the body of `finish_http_response`.
    let needle = "Err(e) => http_response_decode_err(e.message),";
    let count = DATA_RS.matches(needle).count();
    assert_eq!(
        count, 1,
        "expected the duplicated http-response decode arm to appear exactly \
         once (inside finish_http_response), found {count} occurrences \
         (the inline copies in do_http_get/do_http_request were not removed)"
    );
}

#[test]
fn both_call_sites_use_the_helper() {
    let count = DATA_RS.matches("finish_http_response(").count();
    // One definition + two call sites (do_http_get, do_http_request) = 3.
    assert_eq!(
        count, 3,
        "expected `finish_http_response(` to appear 3 times (1 definition + \
         2 call sites), found {count}"
    );
}
