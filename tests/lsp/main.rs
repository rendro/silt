//! Test suite: the language server, driven over stdio.
//!
//! One test binary; each module was a separate test crate before. Modules
//! that talk to the server use the shared client in `support`.

#![cfg(feature = "lsp")]

mod support;

mod builtin_signature_help_tests;
mod docs_lsp_capabilities_advertisement_tests;
mod lsp;
mod lsp_builtin_doc_extraction_tests;
mod lsp_chained_hover_tests;
mod lsp_clean_shutdown_tests;
mod lsp_closure_params_tests;
mod lsp_code_action_from_diagnostic_tests;
mod lsp_code_action_tests;
mod lsp_destructured_binding_tests;
mod lsp_diagnostics_completion_symbols_tests;
mod lsp_diagnostics_user_import_filter_tests;
mod lsp_doc_comment_extraction_tests;
mod lsp_fetches_nothing_tests;
mod lsp_fixed_order_tests;
mod lsp_fn_decl_rename_and_handlers_tests;
mod lsp_fn_destructured_params_tests;
mod lsp_folding_comment_string_skip_tests;
mod lsp_folding_no_dups_tests;
mod lsp_hover_callee_signature_tests;
mod lsp_hover_fn_decl_tests;
mod lsp_hover_stdlib_method_tests;
mod lsp_inlay_trait_default_tests;
mod lsp_lambda_loop_binder_rename_tests;
mod lsp_pattern_binders_and_type_name_completion_tests;
mod lsp_preload_pull_tests;
mod lsp_rename_binding_site_tests;
mod lsp_rename_enum_variant_tests;
mod lsp_rename_gated_constructor_rejection_tests;
mod lsp_rename_let_and_field_tests;
mod lsp_rename_nested_shorthand_binder_tests;
mod lsp_rename_record_shorthand_binder_tests;
mod lsp_rename_unicode_ident_rejection_tests;
mod lsp_rename_user_shadow_tests;
mod lsp_selection_range_decl_bounds_tests;
mod lsp_selection_range_fn_body_tests;
mod lsp_semantic_tokens_tests;
mod lsp_symbol_selection_and_fn_type_tests;
mod lsp_tier2_tests;
mod lsp_trait_rename_references_tests;
mod lsp_type_def_impl_tests;
mod lsp_type_position_rename_tests;
mod lsp_uri_encoding_tests;
mod lsp_workspace_tests;
mod recursive_let_lsp_tests;
mod rename_sweep;
mod rename_sweep_tests;
mod stage5_session_tests;
mod stage5_span_range_tests;
