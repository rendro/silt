//! Test suite: the typechecker: types, traits, patterns and exhaustiveness.
//!
//! One test binary; each module was a separate test crate before.

mod error_enums_are_shown_as_their_text_tests;
mod fuzz_typechecker_target_tests;
mod hash_eq_ord_contract_tests;
mod interp_display_runtime_tests;
mod iopool_panic_typed_err_tests;
mod kind_naming_tests;
mod postgres_typed_timeout_tests;
mod typeof_head_display_tests;
mod typeof_render_tests;
mod unified_trait_registration_tests;
mod vm_error_display_tests;
mod warnings_fix_tests;
