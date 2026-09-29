//! Test suite: the typechecker: types, traits, patterns and exhaustiveness.
//!
//! One test binary; each module was a separate test crate before.

mod canonical_type_equality_phase_b_tests;
mod ext_float_trait_impls_tests;
mod ffi_generic_returns_tests;
mod range_type_tests;
mod round101_supertrait_container_arg_canon_tests;
mod round73_error_trait_dispatch_table_tests;
mod round73_postgres_typed_timeout_tests;
mod round74_extfloat_user_trait_dispatch_tests;
mod round74_hash_eq_ord_contract_tests;
mod round74_infinite_type_canonical_form_tests;
mod round74_vmerror_display_aligned_tests;
mod round75_fuzz_typechecker_target_tests;
mod round75_kind_naming_canonical_tests;
mod round76_iopool_panic_typed_err_tests;
mod round79_typechecker_fixes_tests;
mod round80_typechecker_fixes_tests;
mod round82_stdlib_types_registry_tests;
mod round84_anonrec_unify_eq_tests;
mod round85_anonrec_hash_ord_contract_tests;
mod round92_range_hash_tests;
mod round95_interp_display_runtime_tests;
mod typeof_render_tests;
mod unified_trait_registration_tests;
mod vm_error_display_tests;
mod vm_trait_dispatch_runtime_tests;
