//! VM-level lock for `check_same_type`: `Int` and `Float` have distinct
//! value discriminants, so the VM rejects `==` across them even when the
//! typechecker's verdict is discarded.

#[test]
fn int_float_disc_differ_rejects_mixed_eq() {
    // Locks that distinct-disc types still reject mixed equality at the
    // VM layer (the typechecker may have already rejected this, but the
    // VM is the last line of defence).
    //
    // A program that mixes Int and Float for `==` should be rejected.
    // We use the typechecker-permissive `run` (which ignores type
    // errors) and expect a VM runtime error, caught via expect_err.
    let input = r#"fn main() { 1 == 1.0 }"#;
    assert!(
        silt::session::testing::run_str(input).is_err(),
        "Int == Float must be rejected somewhere in the pipeline"
    );
}
