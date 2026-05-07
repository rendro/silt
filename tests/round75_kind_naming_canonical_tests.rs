//! Round 75 — kind-name canonical-form locks.
//!
//! Two drift bugs collapsed to a single canonical form:
//!
//! - **ERR-1 GAP** — `src/builtins/common.rs::value_kind` returned the
//!   generic word `"value"` for ~half of the `Value` variants
//!   (`Map`, `Set`, `Range`, `Variant`, `Record`, `Unit`, `Channel`,
//!   `Handle`, `VmClosure`, `BuiltinFn`, `VariantConstructor`,
//!   `TypeDescriptor`, `PrimitiveDescriptor`, `TcpListener`,
//!   `TcpStream`). Round 73f's principle was *"name the offending
//!   kind"* — collapsing 14+ distinct shapes onto one generic word
//!   defeats that principle. Post-fix every variant is enumerated
//!   explicitly with a TitleCase name mirrored from
//!   `vm::Vm::type_name`.
//!
//! - **ERR-4 LATENT** — `vm::Vm::user_facing_type_name` had drifted
//!   from its sibling `type_name` along two axes:
//!     1. Lowercase surface words (`"range"`, `"tuple"`) where
//!        `type_name` returned TitleCase (`"Range"`, `"Tuple"`).
//!     2. Indefinite-article forms (`"a function"`, `"a channel"`,
//!        `"a TCP listener"`, `"a TCP stream"`, `"a task handle"`,
//!        `` "a constructor `name`" ``) where `type_name` returned a
//!        bare TitleCase noun (`"Fn"`, `"Channel"`, `"TcpListener"`,
//!        `"TcpStream"`, `"Handle"`, `"VariantConstructor"`).
//!   Two error-rendering paths emitted different strings for the same
//!   value — exactly the dual-shape drift that "one way to do things"
//!   forbids. Post-fix `user_facing_type_name` delegates to
//!   `type_name` for every variant except the four deliberate aliases
//!   (`Record` → record name, `Variant` → parent enum / tag,
//!   `VariantConstructor` / `TypeDescriptor` / `PrimitiveDescriptor` →
//!   the bare TitleCase head followed by the carried name).
//!
//! These locks pin both layers:
//!
//!   1. `value_kind_titlecase_for_all_value_variants` enumerates every
//!      `Value` variant and asserts `value_kind == type_name` (i.e.
//!      no variant falls back to the old `"value"` word).
//!   2. `runtime_canonical_form_includes_real_kind_for_map` exercises
//!      the runtime path: `int.abs(some_map)` must surface `got Map`,
//!      not `got value`.
//!   3. `user_facing_type_name_titlecase_aligned_with_type_name`
//!      enumerates every variant and asserts
//!      `user_facing_type_name == type_name` modulo the four
//!      documented deliberate aliases.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use silt::builtins::value_kind;
use silt::bytecode::{Function, VmClosure};
use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::value::{Channel, TaskHandle, Value};
use silt::vm::Vm;

// ── Test helpers ─────────────────────────────────────────────────────

fn run_err(input: &str) -> String {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = match compiler.compile_program(&program) {
        Ok(f) => f,
        Err(e) => return e.message,
    };
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    let err = vm.run(script).expect_err("expected runtime error");
    format!("{err}")
}

/// Build one representative `Value` for each enum variant. TCP shapes
/// require a real socket — bound to `127.0.0.1:0` so the OS picks an
/// ephemeral port. The listener and stream are carried in a struct so
/// the caller controls their lifetime (the VM holds `Arc` refs).
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
    // Spawn a server-side accept on a thread so the client connect succeeds.
    let server = std::thread::spawn(move || listener.accept().expect("accept").0);
    let client_stream = std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect");
    let _server_stream = server.join().expect("server join");
    let client_stream_clone = client_stream.try_clone().expect("clone client stream");
    // Fresh listener (the original was consumed by .accept()) for the
    // TcpListener Value.
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
        // Use a custom tag with no `__type_of__` binding in a fresh VM
        // so the deliberate-alias arm falls back to the bare tag.
        // Stdlib enums (Option/Result) would resolve to their parent
        // enum name and confound the equality check.
        variant: Value::Variant("MyVariantR75".to_string(), vec![Value::Int(1)]),
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

/// Iterate every variant in `AllVariants` paired with its expected
/// `type_name` output. Used by both the `value_kind` test and the
/// `user_facing_type_name` test.
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

// ── Test 1: value_kind matches type_name for every variant ──────────

#[test]
fn value_kind_titlecase_for_all_value_variants() {
    // Pre-fix this test would fail for 14+ variants because they all
    // collapsed to the generic word `"value"`. Post-fix every arm is
    // enumerated explicitly and matches the canonical TitleCase from
    // `vm::Vm::type_name`.
    let av = build_all_variants();
    let vm = Vm::new();
    for_each_variant(&av, |v, expected| {
        let kind = value_kind(v);
        let tn = vm.type_name(v);
        assert_eq!(
            kind, expected,
            "common::value_kind drift for {v:?}: expected {expected:?}, got {kind:?}"
        );
        assert_eq!(
            kind, tn,
            "common::value_kind diverged from vm::type_name for {v:?}: \
             value_kind={kind:?}, type_name={tn:?}"
        );
        // Negative lock: never the generic word again.
        assert_ne!(
            kind, "value",
            "common::value_kind regressed to the generic \"value\" \
             fallback for variant {v:?} — round 75 ERR-1 GAP fix \
             enumerated every variant explicitly so this can never \
             surface again"
        );
    });
}

// ── Test 2: runtime path emits the real kind for Map ────────────────

#[test]
fn runtime_canonical_form_includes_real_kind_for_map() {
    // Pre-fix `int.abs(map)` would surface `"int.abs requires Int, got
    // value"` because `value_kind` collapsed Map onto the generic word.
    // Post-fix the diagnostic names the offending kind: `"got Map"`.
    //
    // The typechecker rejects most direct mismatches, so we route the
    // value through a generic `id` function whose return type is unconstrained
    // — the typechecker can't know it's a Map at the call site.
    let err = run_err(
        r#"
import int
fn id(x) { x }
fn main() { int.abs(id(#{ "k": 1 })) }
"#,
    );
    assert!(
        err.contains("got Map"),
        "expected runtime error to name the offending kind as `Map`, \
         got: {err}"
    );
    assert!(
        !err.contains("got value"),
        "round 75 ERR-1 GAP regression: runtime error fell back to \
         the generic `\"value\"` word, got: {err}"
    );
}

// ── Test 3: user_facing_type_name aligned with type_name ────────────

#[test]
fn user_facing_type_name_titlecase_aligned_with_type_name() {
    // Pre-fix `user_facing_type_name` returned lowercase + indefinite-
    // article forms ("range", "tuple", "a function", "a channel", ...)
    // for values whose `type_name` was TitleCase ("Range", "Tuple",
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
    let vm = Vm::new();
    for_each_variant(&av, |v, expected_type_name| {
        let ufn = vm.user_facing_type_name(v);
        let tn = vm.type_name(v);
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
        // No lowercase head (catches "range", "tuple"). The very first
        // character must be uppercase or a backtick (descriptor form
        // begins with TitleCase head).
        let first = ufn.chars().next().unwrap_or(' ');
        assert!(
            first.is_uppercase() || first == '`',
            "user_facing_type_name regressed to lowercase head for \
             {v:?}: {ufn:?}"
        );
        // Match-by-shape: equality with type_name OR a documented
        // deliberate alias. The Variant tag chosen in `build_all_variants`
        // (`MyVariantR75`) has no stdlib `__type_of__` binding, so the
        // alias arm falls back to the bare tag.
        let ok = match v {
            Value::Record(name, _) => ufn == *name,
            Value::Variant(tag, _) => ufn == *tag,
            Value::VariantConstructor(name, _) => {
                ufn == format!("VariantConstructor `{name}`")
            }
            Value::TypeDescriptor(name) => ufn == format!("TypeDescriptor `{name}`"),
            Value::PrimitiveDescriptor(name) => {
                ufn == format!("PrimitiveDescriptor `{name}`")
            }
            _ => ufn == tn,
        };
        assert!(
            ok,
            "user_facing_type_name vs type_name divergence for {v:?}: \
             ufn={ufn:?}, type_name={tn:?}, expected_type_name={expected_type_name:?}"
        );
    });
}
