//! Test suite: documentation, parity and source-text checks.
//!
//! One test binary; each module was a separate test crate before.

mod builtin_docs_frontmatter_tests;
mod cargo_fuzz_pin_tests;
mod docs_false_claims_tests;
mod docs_frontmatter_parity_tests;
mod docs_markdown_println_parity_tests;
mod docs_module_placeholder_not_stdlib_tests;
mod docs_no_builtin_trait_redeclared_tests;
mod docs_primitive_descriptor_list_tests;
mod docs_readme_tooling_and_postgres_tests;
mod docs_site_tests;
mod docs_supervisor_snippet_runs_tests;
mod docs_top_level_order_tests;
mod editor_grammar_builtins_tests;
mod editor_grammar_constructors_tests;
mod editor_grammar_keywords_tests;
mod editor_grammar_modules_tests;
mod editor_grammar_number_literals_tests;
mod editor_grammar_primitives_tests;
mod editor_grammar_string_escapes_tests;
mod error_codes_page_tests;
mod features_footer_cargo_parity_tests;
mod fmt_check_message_tests;
mod fuzz_target_wiring_tests;
mod keyword_list_parity_tests;
mod lexer_keyword_parity_tests;
mod normalize_path_one_definition_tests;
mod readme_keyword_parity_tests;
mod public_api_tests;
mod release_asset_name_parity_tests;
mod release_version_drift_tests;
mod round101_pattern_binder_walker_parity_lock_tests;
mod round62_cleanup_lock_tests;
mod round86_editor_grammar_operators_lexer_parity_tests;
mod rustdoc_warnings_lock_tests;
mod scheduled_run_failure_tests;
mod side_crate_lock_tests;
mod stdlib_reference_tests;
mod tcp_tls_stream_wrapper_tests;
mod vscode_metadata_and_primitive_partition_tests;
