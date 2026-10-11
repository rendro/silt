//! Kind-name locks.
//!
//! An error that names the kind of a value names it with
//! `Value::kind`: a TitleCase name for every `Value` variant, never a
//! generic word ("value"), an article ("a function") or a lowercase
//! form ("tuple"). `Vm::user_facing_type_name` differs from it only
//! by four deliberate
//! aliases (`Record` → record name, `Variant` → parent enum / tag,
//! `VariantConstructor` / `TypeDescriptor` / `PrimitiveDescriptor` →
//! the bare TitleCase head followed by the carried name).
//!
//!   1. `kind_is_titlecase_for_all_value_variants` enumerates every
//!      `Value` variant and asserts its kind.
//!   2. (The kind in the error of a builtin that is given a value of
//!      another kind than its parameter's is locked where that error is
//!      made: `builtins::registry`'s
//!      `arguments_that_are_not_the_row_s_are_one_error`. No checked
//!      program reaches it.)
//!   3. `user_facing_type_name_titlecase_aligned_with_kind` enumerates
//!      every variant and asserts `user_facing_type_name == kind`
//!      modulo the four documented deliberate aliases.

use silt::typeinfo::bv;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use silt::bytecode::{Function, VmClosure};
use silt::runtime::handle::TaskHandle;
use silt::runtime::sync::Channel;
use silt::value::Value;
use silt::vm::Vm;

/// The builtin function `name` (`println`, `list.map`) as a value.
fn builtin(name: &str) -> Value {
    silt::builtins::registry::registry()
        .named(name)
        .unwrap_or_else(|| panic!("the builtin {name}"))
        .value()
}

// ── Test helpers ─────────────────────────────────────────────────────

/// Build one representative `Value` for each enum variant. TCP shapes
/// require a real socket — bound to `127.0.0.1:0` so the OS picks an
/// ephemeral port. The listener and stream are carried in a struct so
/// the caller controls their lifetime (the VM holds `Arc` refs).
struct AllVariants {
    int: Value,
    float: Value,
    bool_: Value,
    string: Value,
    list: Value,
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

/// A program's record type `Point`.
fn point_type() -> Arc<silt::typeinfo::TypeInfo> {
    silt::typeinfo::TypeInfo::new_record(
        silt::defs::TypeId(silt::defs::DefId(9000)),
        "Point",
        vec![("x".to_string(), silt::typeinfo::FieldType::Int)],
    )
}

fn build_all_variants() -> AllVariants {
    use silt::runtime::handle::{TcpListenerHandle, TcpStreamHandle};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let port = listener.local_addr().unwrap().port();
    // Spawn a server-side accept on a thread so the client connect succeeds.
    let server = std::thread::spawn(move || listener.accept().expect("accept").0);
    let client_stream = std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect");
    let _server_stream = server.join().expect("server join");
    // Fresh listener (the original was consumed by .accept()) for the
    // TcpListener Value.
    let fresh_listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind 2");
    let tcp_listener_handle = TcpListenerHandle::new(1, fresh_listener);
    let tcp_stream_handle = TcpStreamHandle::plain(2, client_stream);

    AllVariants {
        int: Value::Int(7),
        float: Value::Float(silt::value::Float::new(1.5).expect("a finite number")),
        bool_: Value::Bool(true),
        string: Value::String("hi".into()),
        list: Value::list(vec![Value::Int(1)]),
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
        tuple: Value::tuple(vec![Value::Int(1), Value::Int(2)]),
        record: Value::record(point_type(), vec![Value::Int(1)]),
        // A variant names its enum type.
        variant: Value::variant(bv::SOME, vec![Value::Int(1)]),
        vm_closure: Value::VmClosure(Arc::new(VmClosure {
            function: Arc::new(Function::returning_unit("f".to_string(), 0)),
            upvalues: Vec::new(),
        })),
        builtin_fn: builtin("println"),
        variant_constructor: Value::VariantConstructor(bv::SOME.tag()),
        type_descriptor: Value::TypeDescriptor(point_type()),
        primitive_descriptor: Value::PrimitiveDescriptor("Int"),
        channel: Value::Channel(Channel::new(0, 0)),
        handle: Value::Handle(Arc::new(TaskHandle::new(0))),
        bytes: Value::Bytes(Arc::new(vec![1, 2, 3])),
        tcp_listener: Value::TcpListener(Arc::new(tcp_listener_handle)),
        tcp_stream: Value::TcpStream(tcp_stream_handle),
        unit: Value::Unit,
    }
}

/// Iterate every variant in `AllVariants` paired with its expected
/// kind. Used by both tests.
fn for_each_variant<F: FnMut(&Value, &'static str)>(av: &AllVariants, mut f: F) {
    f(&av.int, "Int");
    f(&av.float, "Float");
    f(&av.bool_, "Bool");
    f(&av.string, "String");
    f(&av.list, "List");
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

// ── Test 1: the kind of every variant ───────────────────────────────

#[test]
fn kind_is_titlecase_for_all_value_variants() {
    let av = build_all_variants();
    for_each_variant(&av, |v, expected| {
        assert_eq!(v.kind(), expected, "the kind of {v:?}");
    });
}

// ── Test 3: user_facing_type_name aligned with the kind ────────────

#[test]
fn user_facing_type_name_titlecase_aligned_with_kind() {
    // Pre-fix `user_facing_type_name` returned lowercase + indefinite-
    // article forms ("tuple", "a function", "a channel", ...)
    // for values whose kind is TitleCase ("Tuple",
    // "Fn", "Channel", ...). Post-fix the two paths agree byte-for-byte
    // except for four deliberate aliases that carry semantic content:
    //
    //   - Record(name) → name
    //   - Variant(tag) → parent-enum-or-tag
    //   - VariantConstructor(name) → "VariantConstructor `name`"
    //   - TypeDescriptor(name) / PrimitiveDescriptor(name) →
    //     "TypeDescriptor `name`" / "PrimitiveDescriptor `name`"
    //
    // For each variant, assert either equality or the documented alias
    // shape. No "a " article anywhere. No lowercase form anywhere.
    let av = build_all_variants();
    let vm = Vm::new(silt::HostIo::process());
    for_each_variant(&av, |v, expected_type_name| {
        let ufn = vm.user_facing_type_name(v);
        let tn = v.kind();
        // No "a " article allowed (catches "a function", "a channel",
        // "a constructor", "a TCP listener", "a TCP stream",
        // "a task handle"). Use word-boundary check: the literal
        // prefix "a " or middle " a " in the rendered string is the
        // pre-fix indefinite-article form.
        assert!(
            !ufn.starts_with("a "),
            "user_facing_type_name regressed to indefinite-article \
             form for {v:?}: {ufn:?}"
        );
        // No lowercase head (catches "tuple"). The very first
        // character must be uppercase or a backtick (descriptor form
        // begins with TitleCase head).
        let first = ufn.chars().next().unwrap_or(' ');
        assert!(
            first.is_uppercase() || first == '`',
            "user_facing_type_name regressed to lowercase head for \
             {v:?}: {ufn:?}"
        );
        // Match-by-shape: equality with type_name OR a documented
        // deliberate alias: a variant is named by its enum type.
        let ok = match v {
            Value::Record(record) => ufn == record.ty().name,
            Value::Variant(variant) => ufn == variant.ty().name,
            Value::VariantConstructor(tag) => ufn == format!("VariantConstructor `{tag}`"),
            Value::TypeDescriptor(ty) => ufn == format!("TypeDescriptor `{}`", ty.name),
            Value::PrimitiveDescriptor(name) => ufn == format!("PrimitiveDescriptor `{name}`"),
            _ => ufn == tn,
        };
        assert!(
            ok,
            "user_facing_type_name vs type_name divergence for {v:?}: \
             ufn={ufn:?}, type_name={tn:?}, expected_type_name={expected_type_name:?}"
        );
    });
}
