//! Round 76 ERR-1 GAP — FFI `value_type_name` parity lock.
//!
//! `src/value.rs::value_type_name` (private fn used by every
//! `FromValue::from_value` impl — `i64`, `f64`, `bool`, `String`,
//! `()`, `Vec<Value>`) hand-rolled its own match arms and drifted
//! from the two canonical kind oracles
//! (`builtins::common::value_kind` and `vm::Vm::type_name`) on four
//! variants:
//!
//! | Variant                    | value_type_name (pre-fix) | canonical          |
//! |----------------------------|---------------------------|--------------------|
//! | Value::BuiltinFn           | "Fn"                      | "BuiltinFn"        |
//! | Value::VariantConstructor  | "Constructor"             | "VariantConstructor"|
//! | Value::TypeDescriptor      | "Type"                    | "TypeDescriptor"   |
//! | Value::PrimitiveDescriptor | "Type"                    | "PrimitiveDescriptor"|
//!
//! Round 75 ERR-1 GAP (commit dd86958) aligned `common::value_kind`
//! with `Vm::type_name` for all 22 variants and locked the matrix in
//! `tests/round75_kind_naming_canonical_tests.rs`. The third sibling
//! `value_type_name` was missed. This round 76 fix collapses the third
//! helper onto the canonical oracle: `value_type_name` now delegates
//! to `crate::builtins::value_kind`. Per the project's "one way to do
//! things" convention.
//!
//! This lock is parametric — it enumerates every `Value` variant and
//! asserts the FFI error message produced by every `FromValue` impl
//! contains the canonical TitleCase kind name (matching
//! `Vm::type_name(v)`). It also asserts the four specific pre-fix
//! strings ("got Fn" for BuiltinFn, "got Constructor", "got Type" for
//! both descriptor variants) NEVER appear for those variants.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use silt::builtins::value_kind;
use silt::bytecode::{Function, VmClosure};
use silt::value::{Channel, FromValue, TaskHandle, Value};
use silt::vm::Vm;

// ── Builders mirroring tests/round75_kind_naming_canonical_tests.rs ──

struct AllVariants {
    int: Value,
    float: Value,
    ext_float: Value,
    bool_: Value,
    string: Value,
    list: Value,
    range: Value,
    map: Value,
    set: Value,
    tuple: Value,
    record: Value,
    variant: Value,
    vm_closure: Value,
    builtin_fn: Value,
    variant_constructor: Value,
    type_descriptor: Value,
    primitive_descriptor: Value,
    channel: Value,
    handle: Value,
    bytes: Value,
    tcp_listener: Value,
    tcp_stream: Value,
    unit: Value,
}

fn build_all_variants() -> AllVariants {
    use silt::value::{TcpListenerHandle, TcpStreamHandle};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || listener.accept().expect("accept").0);
    let client_stream = std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect");
    let _server_stream = server.join().expect("server join");
    let client_stream_clone = client_stream.try_clone().expect("clone client stream");
    let fresh_listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind 2");
    let tcp_listener_handle = TcpListenerHandle {
        id: 1,
        listener: fresh_listener,
    };
    let tcp_stream_handle = TcpStreamHandle {
        id: 2,
        inner: parking_lot::Mutex::new(Box::new(client_stream_clone)),
        closed: std::sync::atomic::AtomicBool::new(false),
        shutdown_sock: parking_lot::Mutex::new(Some(client_stream)),
        reader_socket: None,
    };

    let mut record_fields = BTreeMap::new();
    record_fields.insert("x".to_string(), Value::Int(1));

    AllVariants {
        int: Value::Int(7),
        float: Value::Float(1.5),
        ext_float: Value::ExtFloat(2.5),
        bool_: Value::Bool(true),
        string: Value::String("hi".to_string()),
        list: Value::List(Arc::new(vec![Value::Int(1)])),
        range: Value::Range(0, 5),
        map: Value::Map(Arc::new({
            let mut m = BTreeMap::new();
            m.insert(Value::String("k".into()), Value::Int(1));
            m
        })),
        set: Value::Set(Arc::new({
            let mut s = BTreeSet::new();
            s.insert(Value::Int(1));
            s
        })),
        tuple: Value::Tuple(vec![Value::Int(1), Value::Int(2)]),
        record: Value::Record("Point".to_string(), Arc::new(record_fields)),
        variant: Value::Variant("MyVariantR76".to_string(), vec![Value::Int(1)]),
        vm_closure: Value::VmClosure(Arc::new(VmClosure {
            function: Arc::new(Function::new("f".to_string(), 0)),
            upvalues: Vec::new(),
        })),
        builtin_fn: Value::BuiltinFn("println".to_string()),
        variant_constructor: Value::VariantConstructor("Some".to_string(), 1),
        type_descriptor: Value::TypeDescriptor("Point".to_string()),
        primitive_descriptor: Value::PrimitiveDescriptor("Int".to_string()),
        channel: Value::Channel(Arc::new(Channel::new(0, 0))),
        handle: Value::Handle(Arc::new(TaskHandle::new(0))),
        bytes: Value::Bytes(Arc::new(vec![1, 2, 3])),
        tcp_listener: Value::TcpListener(Arc::new(tcp_listener_handle)),
        tcp_stream: Value::TcpStream(Arc::new(tcp_stream_handle)),
        unit: Value::Unit,
    }
}

fn for_each_variant<F: FnMut(&Value, &'static str)>(av: &AllVariants, mut f: F) {
    f(&av.int, "Int");
    f(&av.float, "Float");
    f(&av.ext_float, "ExtFloat");
    f(&av.bool_, "Bool");
    f(&av.string, "String");
    f(&av.list, "List");
    f(&av.range, "Range");
    f(&av.map, "Map");
    f(&av.set, "Set");
    f(&av.tuple, "Tuple");
    f(&av.record, "Record");
    f(&av.variant, "Variant");
    f(&av.vm_closure, "Fn");
    f(&av.builtin_fn, "BuiltinFn");
    f(&av.variant_constructor, "VariantConstructor");
    f(&av.type_descriptor, "TypeDescriptor");
    f(&av.primitive_descriptor, "PrimitiveDescriptor");
    f(&av.channel, "Channel");
    f(&av.handle, "Handle");
    f(&av.bytes, "Bytes");
    f(&av.tcp_listener, "TcpListener");
    f(&av.tcp_stream, "TcpStream");
    f(&av.unit, "Unit");
}

// ── Test 1: parametric — every FromValue impl uses canonical kind ────

/// For every `Value` variant that is NOT the target of a given
/// `FromValue` impl, calling `T::from_value(&v)` produces an Err whose
/// message contains the canonical kind name (matching
/// `Vm::type_name(v)` and `value_kind(v)`).
///
/// Pre-fix: BuiltinFn → "got Fn", VariantConstructor → "got Constructor",
/// TypeDescriptor → "got Type", PrimitiveDescriptor → "got Type".
/// Post-fix: all four route through `value_kind` and surface the
/// canonical `BuiltinFn` / `VariantConstructor` / `TypeDescriptor` /
/// `PrimitiveDescriptor` strings.
#[test]
fn from_value_error_messages_use_canonical_kind_for_all_variants() {
    let av = build_all_variants();
    let vm = Vm::new();

    // Each FromValue impl's expected target name. We skip the variant
    // whose name matches (it would succeed), and for the variants the
    // impl coerces (e.g. f64 accepts Int/Float/ExtFloat) we also skip.
    fn check_i64_err(v: &Value, expected_kind: &str, vm: &Vm) {
        if matches!(v, Value::Int(_)) {
            return;
        }
        let err = i64::from_value(v).expect_err("non-Int should fail");
        assert!(
            err.contains(&format!("got {expected_kind}")),
            "i64::from_value({v:?}) message should contain `got {expected_kind}` \
             (canonical kind via value_kind), got: {err}"
        );
        // Cross-check: same kind that vm::type_name reports.
        assert!(
            err.contains(vm.type_name(v)),
            "i64::from_value({v:?}) message should contain Vm::type_name() \
             output, got: {err}"
        );
    }

    fn check_f64_err(v: &Value, expected_kind: &str, vm: &Vm) {
        // f64 accepts Int, Float, ExtFloat — skip those.
        if matches!(v, Value::Int(_) | Value::Float(_) | Value::ExtFloat(_)) {
            return;
        }
        let err = f64::from_value(v).expect_err("non-numeric should fail");
        assert!(
            err.contains(&format!("got {expected_kind}")),
            "f64::from_value({v:?}) message should contain `got {expected_kind}`, \
             got: {err}"
        );
        assert!(err.contains(vm.type_name(v)));
    }

    fn check_bool_err(v: &Value, expected_kind: &str, vm: &Vm) {
        if matches!(v, Value::Bool(_)) {
            return;
        }
        let err = bool::from_value(v).expect_err("non-Bool should fail");
        assert!(
            err.contains(&format!("got {expected_kind}")),
            "bool::from_value({v:?}) message should contain `got {expected_kind}`, \
             got: {err}"
        );
        assert!(err.contains(vm.type_name(v)));
    }

    fn check_string_err(v: &Value, expected_kind: &str, vm: &Vm) {
        if matches!(v, Value::String(_)) {
            return;
        }
        let err = String::from_value(v).expect_err("non-String should fail");
        assert!(
            err.contains(&format!("got {expected_kind}")),
            "String::from_value({v:?}) message should contain `got {expected_kind}`, \
             got: {err}"
        );
        assert!(err.contains(vm.type_name(v)));
    }

    fn check_unit_err(v: &Value, expected_kind: &str, vm: &Vm) {
        if matches!(v, Value::Unit) {
            return;
        }
        let err = <()>::from_value(v).expect_err("non-Unit should fail");
        assert!(
            err.contains(&format!("got {expected_kind}")),
            "<()>::from_value({v:?}) message should contain `got {expected_kind}`, \
             got: {err}"
        );
        assert!(err.contains(vm.type_name(v)));
    }

    fn check_vec_err(v: &Value, expected_kind: &str, vm: &Vm) {
        // Vec<Value> accepts List and Range — skip those.
        if matches!(v, Value::List(_) | Value::Range(..)) {
            return;
        }
        let err = <Vec<Value>>::from_value(v).expect_err("non-List should fail");
        assert!(
            err.contains(&format!("got {expected_kind}")),
            "<Vec<Value>>::from_value({v:?}) message should contain \
             `got {expected_kind}`, got: {err}"
        );
        assert!(err.contains(vm.type_name(v)));
    }

    for_each_variant(&av, |v, expected_kind| {
        // Cross-lock invariant: the kind name is the canonical one.
        assert_eq!(value_kind(v), expected_kind);
        assert_eq!(vm.type_name(v), expected_kind);

        check_i64_err(v, expected_kind, &vm);
        check_f64_err(v, expected_kind, &vm);
        check_bool_err(v, expected_kind, &vm);
        check_string_err(v, expected_kind, &vm);
        check_unit_err(v, expected_kind, &vm);
        check_vec_err(v, expected_kind, &vm);
    });
}

// ── Test 2: the four specific pre-fix strings are gone ──────────────

/// Lock the four specific drift cases called out in the round 76 ERR-1
/// GAP report. These are the *exact* strings the pre-fix
/// `value_type_name` returned for these four variants. After the fix
/// they MUST be replaced with the canonical kind names, and the old
/// strings MUST NOT appear in the FFI error messages.
#[test]
fn from_value_error_messages_no_longer_use_pre_fix_drift_strings() {
    let av = build_all_variants();

    // BuiltinFn: pre-fix "got Fn" → post-fix "got BuiltinFn"
    let err_builtin = i64::from_value(&av.builtin_fn).expect_err("BuiltinFn ≠ Int");
    assert_eq!(
        err_builtin, "expected Int, got BuiltinFn",
        "BuiltinFn error must now use canonical name `BuiltinFn`, not `Fn`"
    );

    // VariantConstructor: pre-fix "got Constructor" → post-fix "got VariantConstructor"
    let err_ctor = i64::from_value(&av.variant_constructor).expect_err("Ctor ≠ Int");
    assert_eq!(
        err_ctor, "expected Int, got VariantConstructor",
        "VariantConstructor error must now use canonical name \
         `VariantConstructor`, not `Constructor`"
    );

    // TypeDescriptor: pre-fix "got Type" → post-fix "got TypeDescriptor"
    let err_td = i64::from_value(&av.type_descriptor).expect_err("TD ≠ Int");
    assert_eq!(
        err_td, "expected Int, got TypeDescriptor",
        "TypeDescriptor error must now use canonical name \
         `TypeDescriptor`, not `Type`"
    );

    // PrimitiveDescriptor: pre-fix "got Type" → post-fix "got PrimitiveDescriptor"
    let err_pd = i64::from_value(&av.primitive_descriptor).expect_err("PD ≠ Int");
    assert_eq!(
        err_pd, "expected Int, got PrimitiveDescriptor",
        "PrimitiveDescriptor error must now use canonical name \
         `PrimitiveDescriptor`, not `Type`"
    );

    // Negative regression locks: the four pre-fix drift strings must
    // not occur for these specific variants in any FromValue impl.
    for (label, v) in [
        ("BuiltinFn", &av.builtin_fn),
        ("VariantConstructor", &av.variant_constructor),
        ("TypeDescriptor", &av.type_descriptor),
        ("PrimitiveDescriptor", &av.primitive_descriptor),
    ] {
        for err in [
            i64::from_value(v).unwrap_err(),
            f64::from_value(v).unwrap_err(),
            bool::from_value(v).unwrap_err(),
            String::from_value(v).unwrap_err(),
            <()>::from_value(v).unwrap_err(),
            <Vec<Value>>::from_value(v).unwrap_err(),
        ] {
            // For BuiltinFn, "got Fn" must not appear.
            if label == "BuiltinFn" {
                assert!(
                    !err.contains("got Fn") || err.contains("got BuiltinFn"),
                    "BuiltinFn drift regression: error contains `got Fn` \
                     without `got BuiltinFn`: {err}"
                );
            }
            if label == "VariantConstructor" {
                assert!(
                    !err.contains("got Constructor") || err.contains("got VariantConstructor"),
                    "VariantConstructor drift regression: error contains \
                     `got Constructor` without `got VariantConstructor`: {err}"
                );
            }
            if label == "TypeDescriptor" || label == "PrimitiveDescriptor" {
                // The drift was the bare word `"Type"` — i.e. the message
                // ended in "got Type" with no further suffix. Post-fix
                // it always continues into `Descriptor`.
                assert!(
                    !err.ends_with("got Type"),
                    "{label} drift regression: error ends with bare \
                     `got Type` (pre-fix shape): {err}"
                );
            }
        }
    }
}
