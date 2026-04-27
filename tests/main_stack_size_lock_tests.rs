//! Lock test for the explicit main-thread stack-size reserve in
//! `src/main.rs`.
//!
//! Background: round-61 + the deferred-items batch (canonical type
//! equality, auto-derive body synthesis, row polymorphism, associated
//! types) deepened the type-system recursion enough that Windows'
//! default 1 MiB main-thread stack overflows on small but realistic
//! programs (e.g. `import list as l; fn main() { println(l.sum([1,2,3])) }`).
//!
//! The fix wraps the dispatcher in a worker thread with an explicit
//! 8 MiB stack reserve. This source-grep lock catches any future
//! refactor that removes or shrinks the reserve before CI flags it on
//! Windows.

#[test]
fn main_uses_explicit_stack_reserve() {
    let src = include_str!("../src/main.rs");
    assert!(
        src.contains("SILT_STACK_SIZE"),
        "src/main.rs is expected to declare SILT_STACK_SIZE; the explicit \
         main-thread stack reserve guards Windows builds where the default \
         1 MiB stack overflows on type-system recursion. Re-introduce the \
         constant + Builder::new().stack_size(SILT_STACK_SIZE).spawn(...) \
         pattern in main()."
    );
    assert!(
        src.contains("stack_size(SILT_STACK_SIZE)"),
        "src/main.rs must spawn the dispatcher on a thread with \
         stack_size(SILT_STACK_SIZE); the constant is set but no longer \
         applied to a worker thread."
    );
    assert!(
        src.contains("8 * 1024 * 1024") || src.contains("8388608"),
        "SILT_STACK_SIZE must be at least 8 MiB. Smaller values regress \
         Windows CI on aliased-import runtime tests."
    );
}
