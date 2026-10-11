//! Round 95: string interpolation of a non-Display value must be rejected,
//! at runtime for the polymorphic case as it already is at compile time
//! for the concrete case.
//!
//! ## Background
//!
//! silt's string interpolation requires the interpolated value's type to
//! implement `Display` (docs/language/loops-and-pipes.md: "interpolating
//! [a non-Display type] is a compile error"). For a *concrete* operand the
//! typechecker rejects the non-Display types (`Fn`, `Channel`) at compile
//! time.
//!
//! But silt infers trait bounds from body usage and does NOT statically
//! enforce them on polymorphic functions — exactly as
//! `fn pick(x: a, y: a) -> a { match x > y { ... } }` compiles with no
//! `where a: Compare` and errors at *runtime* on two Channels
//! ("cannot compare Channel and Channel"). Pre-fix, interpolation was the
//! lone inconsistency: `fn show(x: a) -> String { "{x}" }` compiled and,
//! when called with a function or channel, SILENTLY rendered `<fn:..>` /
//! `<channel:0>` instead of erroring — a silent-wrong-behavior hole.
//!
//! The fix makes `Op::DisplayValue` (emitted per interpolated segment)
//! error at the execution site for the same set the Display gate rejects,
//! mirroring how Compare is enforced at runtime for polymorphic code.
//!
//! ## Parity lock
//!
//! The runtime-rejected set is exactly the *concrete* set the
//! typechecker's interpolation Display gate rejects — every first-class
//! value whose canonical type name is absent from the Display
//! `trait_impl_set`: function-shaped values (`Fn`), `Channel`, `Handle`
//! (task handles), `TcpListener` / `TcpStream` (opaque network resources
//! left explicitly unprintable, src/typechecker/mod.rs ~7878), and the
//! reflective descriptor values (`TypeDescriptor` / `PrimitiveDescriptor`).
//! The Display-able set (Int/String/List/record) is exactly what it
//! accepts. Both halves are asserted here so the runtime and compile-time
//! layers cannot drift apart.
//!
//! Round-95 follow-up: the original gate covered only `Fn` + `Channel`
//! and the prose falsely claimed those were "the only first-class values"
//! the Display gate rejects yet can flow through an unbounded type
//! variable. `Handle` (e.g. `task.spawn`) and the Tcp resources reproduce
//! the same silent-`<handle:0>` / `<tcp-stream:0>` rendering; the gate
//! now rejects the full set, sourced from the single VM-side oracle
//! `Vm::value_implements_display` so the runtime layer cannot drift.
//!
//! The Fn / Channel / Handle runtime cases, the Display-able controls and
//! the concrete compile-time parity cases live as golden cases under
//! `tests/golden/typecheck/display/round95_interp_display_runtime__*`.
//! What stays here needs a live socket (TcpListener) or calls the
//! VM-side predicate directly.

use silt::runtime::handle::TaskHandle;
use silt::scheduler::test_support::InProcessRunner;
use silt::value::Value;
use silt::vm::Vm;
use std::sync::Arc;
use std::time::Duration;

#[test]
fn polymorphic_interp_of_tcp_listener_is_rejected() {
    // `tcp.listen` yields a `TcpListener` (an opaque resource left
    // explicitly unprintable). Interpolating it through the bounded
    // `show` is an error, not `val=<tcp-listener:0>`.
    let src = r#"
import tcp
fn show(x: a) -> String where a: Display { "val={x}" }
fn main() {
  match tcp.listen("127.0.0.1:0") {
    Ok(l) -> println(show(l))
    Err(e) -> println("listen failed")
  }
}
"#;
    let outcome = InProcessRunner::new(src)
        .with_budget(Duration::from_secs(10))
        .run_trial();
    let err = outcome
        .error_message
        .expect("expected a Display error, got clean exit");
    assert!(
        err.contains("does not implement trait 'Display'") && err.contains("'TcpListener'"),
        "expected a Display error naming TcpListener, got: {err}"
    );
}

// ── Oracle lock: the VM-side `value_implements_display` predicate is the
//    single source of truth for the runtime gate. Assert it rejects EVERY
//    no-Display first-class value (so the gate and the predicate cannot
//    drift) and accepts the printable ones. This catches a future variant
//    being added to the rejected set in the typechecker but not here. ────

#[test]
fn value_implements_display_predicate_covers_every_no_display_value() {
    // Rejected: function-shaped, Channel, Handle, Tcp resources,
    // descriptors. (Tcp stream/listener constructed directly is awkward —
    // they hold live sockets — so the runtime tests above cover the live
    // path; here we cover the cheaply-constructible no-Display values.)
    let handle = Value::Handle(Arc::new(TaskHandle::new(0)));
    let type_desc = Value::TypeDescriptor(
        silt::typeinfo::builtin_type_named("List")
            .expect("List")
            .clone(),
    );
    let prim_desc = Value::PrimitiveDescriptor("Int");
    for v in [&handle, &type_desc, &prim_desc] {
        assert!(
            !Vm::value_implements_display(v),
            "no-Display value must be rejected by the predicate: {v:?}"
        );
    }
    // Accepted: the printable built-ins still pass.
    for v in [
        Value::Int(1),
        Value::String("x".into()),
        Value::Bool(true),
        Value::list(vec![Value::Int(1)]),
        Value::Unit,
    ] {
        assert!(
            Vm::value_implements_display(&v),
            "Display-able value must be accepted by the predicate: {v:?}"
        );
    }
}
