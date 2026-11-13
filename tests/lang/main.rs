//! Test suite: the language at run time: compiler, VM and standard library.
//!
//! One test binary; each module was a separate test crate before.

mod anon_record_resolve_recursion_tests;
mod builtin_module_constant_value_access_runtime_tests;
mod builtin_module_dispatch_tests;
mod builtin_structural_traits_tests;
mod bytes_module_tests;
mod canonical_resolver_isolation_tests;
mod check_json_no_ansi_under_force_color_tests;
mod compiler_literal_bounds_tests;
mod deep_value_stack_tests;
mod dependency_error_attribution_tests;
mod diagnostic_offset_past_end_tests;
mod disabled_feature_help_tests;
mod error_tests;
mod examples_check;
mod fs_walk_stat_tests;
mod fuzz_corpus_presence_tests;
mod fuzz_target_invariants_tests;
mod hardening;
mod host_io_tests;
mod host_module_tests;
mod http_bind_address_tests;
mod http_hardening_tests;
mod http_server_tests;
mod into_value_f64_finite_tests;
mod io_args_after_double_dash_tests;
mod lib_package_check_tests;
mod list_flatten_unfold_bounds_tests;
mod list_zip_bounds_tests;
mod missing_import_recommends_tests;
mod module_constants_completion_tests;
mod numeric_defaulting_tests;
#[cfg(any(feature = "http", feature = "tcp-tls"))]
#[path = "../support/port_file.rs"]
mod port_file;
mod postgres_hardening_tests;
mod postgres_poisoned_mutex_tests;
mod property_tests;
mod qualified_type_paths_tests;
#[path = "../support/quiet.rs"]
mod quiet;
mod round76_row_tail_merge_tests;
mod row_polymorphism_tests;
mod session_tests;
mod stage7_io_pool_tests;
mod stale_method_row_test;
mod tcp_module_tests;
mod tcp_mtls_tests;
mod tcp_shutdown_tests;
mod tcp_tls_tests;
mod time_builtin_bounds_tests;
mod time_sleep_cooperative_tests;
mod url_credentials_redacted_tests;
mod utf8_bom_tests;
mod value_ord_handle_closure_tests;
mod value_partial_eq_reflexive_tests;
mod wave1_frontend_tests;
mod wave1_tooling_tests;
mod wave2_sweep_tests;
