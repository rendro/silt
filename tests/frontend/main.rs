//! Test suite: lexer, parser and formatter.
//!
//! One test binary; each module was a separate test crate before.

mod effect_annotation_parse_tests;
mod examples_fmt_check_tests;
mod formatter_examples_roundtrip_tests;
mod formatter_idempotency_tests;
mod formatter_line_comment_tests;
mod formatter_map_set_multiline_tests;
mod formatter_null_comment_tests;
mod formatter_round35_tests;
mod formatter_trailing_comma_extended_tests;
mod formatter_trailing_comma_tests;
mod parser_unclosed_delim_recovery_tests;
mod round75_formatter_trait_params_where_tests;
mod round84_formatter_bracket_interior_comment_preserved_tests;
mod round85_formatter_inline_record_interior_comment_preserved_tests;
mod round92_parser_hint_tests;
mod round93_parser_hint_tests;
mod round97_formatter_close_line_comment_tests;
mod type_param_parser_tests;
