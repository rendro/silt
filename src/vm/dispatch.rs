//! Builtin registration and dispatch.

use std::panic::AssertUnwindSafe;

use super::{Vm, VmError};
use crate::builtins;
use crate::module;
use crate::value::Value;

// ── Round-62 follow-up: dispatch arms for (Variant, Variant) /
// (Record, Record) — and the corresponding entries in the hash
// allowlist — were deleted in this round.
//
// The auto-derive synthesis pass in
// `src/typechecker/mod.rs::synthesize_auto_derive_impls` now
// processes BOTH user-declared types AND built-in enums / records
// (Option, Result, Weekday, Method, ChannelResult, Step, IoError /
// JsonError / ..., Date, Time, DateTime, Duration, Instant,
// FileStat, Response, Request). Every (trait, type) pair stamped
// in `trait_impl_set` receives a synthesized `<Type>.<method>`
// global at compile time, and `Op::CallMethod`'s qualified-global
// lookup resolves the call before it ever reaches
// `dispatch_trait_method`.
//
// The deadness instrumentation (six atomic counters + their
// reset/snapshot helpers) was removed alongside the arms. The
// barrage in `tests/auto_derive_dead_arm_proof_tests.rs` now
// stands as a behavioural lock — every shape (user and built-in)
// must produce the expected output, which it cannot do via the
// catch-all error arm that remains in `dispatch_trait_method`.

/// Invoke a registered foreign function while catching panics that escape it.
///
/// A panicking foreign function would otherwise tear down the scheduler worker
/// thread (or the main thread), leaving other tasks unable to progress. We
/// instead convert a caught panic into a [`VmError`] whose message preserves
/// the panic payload when it is a `&str` or `String`.
fn invoke_foreign_fn(
    name: &str,
    f: &super::runtime::ForeignFn,
    args: &[Value],
) -> Result<Value, VmError> {
    match std::panic::catch_unwind(AssertUnwindSafe(|| f(args))) {
        Ok(result) => result,
        Err(payload) => {
            let msg = decode_panic_payload(&payload);
            Err(VmError::new(format!(
                "foreign function '{name}' panicked: {msg}"
            )))
        }
    }
}

/// Decode a panic payload into a human-readable string, preserving the
/// common `&'static str` and `String` cases and falling back to a
/// placeholder for other payload types.
fn decode_panic_payload(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

/// Run a builtin module dispatch arm under `catch_unwind`, converting any
/// panic that escapes the builtin into a clean `VmError`. This mirrors
/// [`invoke_foreign_fn`] for user-registered FFI — a panic in a builtin
/// would otherwise tear down the current scheduler worker thread.
///
/// Intended to wrap each arm of the module-name match in `dispatch_builtin`.
/// Callers that capture `&mut Vm` (or other non-`UnwindSafe` state) should
/// wrap the closure in [`AssertUnwindSafe`] before passing it here.
fn catch_builtin_panic<F>(module: &str, f: F) -> Result<Value, VmError>
where
    F: FnOnce() -> Result<Value, VmError> + std::panic::UnwindSafe,
{
    match std::panic::catch_unwind(f) {
        Ok(result) => result,
        Err(payload) => {
            let msg = decode_panic_payload(&payload);
            Err(VmError::new(format!(
                "builtin module '{module}' panicked: {msg}"
            )))
        }
    }
}

/// Uniform signature shared by every `call_<x>_error_trait` helper.
type ErrorTraitFn = fn(&str, &[Value]) -> Result<Value, VmError>;

/// Round-73 BLOAT-3: dispatch table for built-in `trait Error` impls.
///
/// Each `(enum_name, fn_ptr)` entry corresponds to a previous match
/// arm in `dispatch_builtin` of the form
/// `"<Enum>" => catch_builtin_panic("<Enum>",
///   AssertUnwindSafe(|| <module>::call_<x>_error_trait(func, args)))`.
///
/// PgError / TcpError stay cfg-gated by being conditionally included
/// in the table — the gate must match the gate on the corresponding
/// `call_*_error_trait` symbol.
///
/// Lock test: `tests/round73_error_enum_registry_parity_tests.rs`.
static ERROR_TRAIT_DISPATCH: &[(&str, ErrorTraitFn)] = &[
    ("IoError", builtins::io::call_io_error_trait),
    ("JsonError", builtins::data::call_json_error_trait),
    ("TomlError", builtins::toml::call_toml_error_trait),
    ("ParseError", builtins::numeric::call_parse_error_trait),
    ("HttpError", builtins::data::call_http_error_trait),
    ("RegexError", builtins::data::call_regex_error_trait),
    #[cfg(feature = "postgres")]
    ("PgError", builtins::postgres::call_pg_error_trait),
    #[cfg(feature = "tcp")]
    ("TcpError", builtins::tcp::call_tcp_error_trait),
    ("TimeError", builtins::data::call_time_error_trait),
    ("BytesError", builtins::bytes::call_bytes_error_trait),
    (
        "ChannelError",
        builtins::concurrency::call_channel_error_trait,
    ),
];

/// Look up the `trait Error` dispatch helper for a given builtin enum
/// name. Returns `None` for any name not in the table (including
/// cfg-gated names whose feature is disabled).
pub(crate) fn error_trait_dispatch(enum_name: &str) -> Option<ErrorTraitFn> {
    ERROR_TRAIT_DISPATCH
        .iter()
        .find(|(n, _)| *n == enum_name)
        .map(|(_, f)| *f)
}

/// Render a stdlib error variant via its `Error::message()`
/// implementation, returning `None` when the tag isn't a stdlib-error
/// variant (or rendering fails for any reason — caller falls back to
/// the default constructor-form render).
///
/// Used by `Value::Display` to collapse the dual shape between
/// `format!("{e}")` and `e.message()` for stdlib error enums per the
/// silt "explicit over implicit / one way" principle. User-defined
/// enums are not affected — the registry only covers stdlib error
/// enums.
pub fn render_stdlib_error_message(tag: &str, fields: &[Value]) -> Option<String> {
    let enum_name = crate::module::variant_to_error_enum(tag)?;
    let dispatch_fn = error_trait_dispatch(enum_name)?;
    let variant_value = Value::Variant(tag.into(), fields.to_vec());
    match dispatch_fn("message", &[variant_value]).ok()? {
        Value::String(s) => Some(s),
        _ => None,
    }
}

impl Vm {
    /// Register all builtin functions and variant constructors in globals.
    pub(super) fn register_builtins(&mut self) {
        // ── Prelude + stdlib-error enum variants ──
        // Round-71 PARALLEL-ARRAY-DRIFT fix made the prelude loop
        // data-driven from `module::builtin_prelude_enum_variants_with_arity()`,
        // mirroring the round-64 DUP-1 fix that already drove the
        // stdlib-error variants from
        // `module::builtin_error_enum_variants_with_arity()`. Both loops
        // had byte-identical bodies (`Variant` for nullary, otherwise
        // `VariantConstructor`); round-75 DEAD-7 collapses them via
        // `chain()`. The two registries are still authored separately
        // (the prelude registry mirrors `module::builtin_enum_variants`
        // minus the error enums; the error registry parallels
        // `src/typechecker/builtins/errors.rs::register`), and the
        // parity tests at `tests/error_enum_dispatch_parity_tests.rs`
        // and `tests/round71_dispatch_collapse_and_parity_tests.rs`
        // keep them in lockstep on `(variant, arity)`. Each variant is
        // globally unique (module-prefixed for error enums) so we
        // register every entry as a bare global.
        for (_enum_name, variants) in module::builtin_prelude_enum_variants_with_arity()
            .iter()
            .chain(module::builtin_error_enum_variants_with_arity().iter())
        {
            for (variant, arity) in variants.iter() {
                let value = if *arity == 0 {
                    Value::Variant((*variant).into(), Vec::new())
                } else {
                    Value::VariantConstructor((*variant).into(), *arity)
                };
                self.globals.insert((*variant).into(), value);
            }
        }

        // ── __type_of__<variant> mappings for builtin error enums ──
        // Phase 1 of the stdlib error redesign: `CallMethod` dispatch
        // looks up the parent type name via `__type_of__<tag>` to route
        // `err.message()` to `IoError.message`, etc. User-declared enums
        // register these globals at codegen time; builtin enums have to
        // be seeded here so the same dispatch works.
        //
        // We also register declaration-order ordinals for every builtin
        // variant in the same loop. The typechecker registers ordinals
        // during `check_program`, but a Vm constructed without a
        // preceding type-check pass (some unit tests, FFI use cases)
        // still needs ordinals for `cmp_gen(Monday, Friday)` to honour
        // declaration order. The two registrations are idempotent —
        // re-registering the same (name, ordinal) is a no-op write.
        for (enum_name, variants) in module::builtin_enum_variants() {
            for (idx, variant) in variants.iter().enumerate() {
                let key = format!("__type_of__{variant}");
                self.globals.insert(key, Value::String((*enum_name).into()));
                crate::value::register_variant_ordinal(variant, idx as u32);
            }
        }

        // ── trait Error for builtin error enums — compiled message() ──
        // Registered as built-in functions so `err.message()` dispatches
        // via `CallMethod` → `<EnumName>.message` global. The function
        // body lives in the corresponding `call_*_error_trait` helper
        // and is routed through the matching `dispatch_builtin` arm.
        // `.display()` is handled generically by `dispatch_trait_method`
        // via `display_value`, so it does not need a per-enum BuiltinFn.
        //
        // Round-73 BLOAT-1 fix: derived from
        // `module::builtin_error_enum_variants_with_arity` so adding a
        // new typed-error enum no longer requires editing this list.
        // Parity lock: `tests/round73_error_enum_registry_parity_tests.rs`.
        for (enum_name, _variants) in module::builtin_error_enum_variants_with_arity() {
            let key = format!("{enum_name}.message");
            self.globals
                .insert(key.clone(), Value::BuiltinFn(key.clone()));
        }

        // Primitive type descriptors.
        // Round-73 BLOAT-2 fix: name set hoisted to
        // `module::BUILTIN_PRIMITIVE_NAMES`.
        for name in module::BUILTIN_PRIMITIVE_NAMES {
            self.globals
                .insert((*name).into(), Value::PrimitiveDescriptor((*name).into()));
        }

        // Builtin container type descriptors — uppercase names so users
        // can pass `List`, `Map`, etc. as `type a` arguments or invoke
        // static-style trait methods (`List.empty()`). These don't
        // collide with the lowercase module names (`list`, `map`) used
        // for module calls.
        // Round-73 BLOAT-2 fix: name set hoisted to
        // `module::BUILTIN_GENERIC_CONTAINER_NAMES`.
        for name in module::BUILTIN_GENERIC_CONTAINER_NAMES {
            self.globals
                .insert((*name).into(), Value::TypeDescriptor((*name).into()));
        }

        // Math constants
        self.globals
            .insert("math.pi".into(), Value::Float(std::f64::consts::PI));
        self.globals
            .insert("math.e".into(), Value::Float(std::f64::consts::E));

        // Non-module builtin functions (not scoped to a module).
        // Sourced from `module::builtin_free_function_names()` so that
        // module.rs is the single source of truth — adding a new free
        // function (e.g. `eprintln`, `assert`) only requires touching
        // the registry plus the typechecker registration site.
        // Parity lock: `tests/builtin_free_function_parity_tests.rs`.
        for &name in module::builtin_free_function_names() {
            self.globals
                .insert(name.into(), Value::BuiltinFn(name.into()));
        }

        // Module-scoped builtin functions — derived from the module registry
        // so that module.rs is the single source of truth.
        for &module_name in module::BUILTIN_MODULES {
            for func in module::builtin_module_functions(module_name) {
                let qualified = format!("{module_name}.{func}");
                self.globals
                    .insert(qualified.clone(), Value::BuiltinFn(qualified));
            }
        }

        // Float constants — registered after builtin function names so that
        // `float.max` and `float.min` resolve to the constant values (f64::MAX,
        // f64::MIN) when used as bare expressions, overriding the BuiltinFn
        // entries.  Function calls like `float.max(a, b)` still work because
        // they compile to Op::CallBuiltin, which bypasses the global lookup.
        self.globals
            .insert("float.max_value".into(), Value::Float(f64::MAX));
        self.globals
            .insert("float.min_value".into(), Value::Float(f64::MIN));
        self.globals
            .insert("float.epsilon".into(), Value::Float(f64::EPSILON));
        self.globals
            .insert("float.min_positive".into(), Value::Float(f64::MIN_POSITIVE));
        self.globals
            .insert("float.infinity".into(), Value::ExtFloat(f64::INFINITY));
        self.globals.insert(
            "float.neg_infinity".into(),
            Value::ExtFloat(f64::NEG_INFINITY),
        );
        self.globals
            .insert("float.nan".into(), Value::ExtFloat(f64::NAN));
    }

    // ── Built-in trait methods on primitive types ──────────────────

    /// Handle built-in trait methods like .display(), .equal(), .compare()
    /// on primitive types. Returns Some(result) if handled, None otherwise.
    pub(super) fn dispatch_trait_method(
        &self,
        receiver: &Value,
        method: &str,
        extra_args: &[Value],
    ) -> Option<Result<Value, VmError>> {
        match method {
            "display" => {
                if !extra_args.is_empty() {
                    return Some(Err(VmError::new("display() takes no arguments".into())));
                }
                // Runtime Display gate — the .display() twin of the
                // round-95 `Op::DisplayValue` gate (src/vm/execute.rs
                // ~:1457). For a *concrete* receiver the typechecker
                // already rejects `.display()` on no-Display types
                // ("unknown method 'display' on type Fn"), but silt
                // enforces inferred trait bounds at the EXECUTION site
                // for polymorphic code, so a Var-typed receiver reaches
                // this arm ungated. Pre-fix, `fn show(x: a) -> String
                // { x.display() }` over a lambda / channel / task handle
                // silently rendered `<fn:..>` / `<channel:0>` /
                // `<handle:0>` — while the equivalent interpolation
                // `"{x}"` errored at runtime and the sibling `.equal()` /
                // `.compare()` arms below carry their own runtime gates.
                // Reject the same set here, sourced from the single
                // oracle `Vm::value_implements_display` so the two
                // execution-site gates cannot drift. Records, variants
                // (incl. stdlib error enums) and every printable
                // built-in pass the oracle and fall through unchanged.
                if !Self::value_implements_display(receiver) {
                    // Same canonical-name reporting as Op::DisplayValue:
                    // function-shaped values collapse to "Fn" via
                    // `dispatch_name_for_value`; the descriptor values
                    // (whose canonical name is the *carried* type name)
                    // fall back to `type_name` so the diagnostic names
                    // the descriptor kind, not the reflected type.
                    let name = match receiver {
                        Value::TypeDescriptor(_) | Value::PrimitiveDescriptor(_) => {
                            self.type_name(receiver).to_string()
                        }
                        _ => crate::types::canonical::dispatch_name_for_value(receiver)
                            .unwrap_or_else(|| self.type_name(receiver).to_string()),
                    };
                    return Some(Err(VmError::new(format!(
                        "type '{name}' does not implement Display"
                    ))));
                }
                Some(Ok(Value::String(self.display_value(receiver))))
            }
            "equal" => {
                if extra_args.len() != 1 {
                    return Some(Err(VmError::new("equal() takes 1 argument".into())));
                }
                // Execution-site backstop mirroring the `Op::Eq` gate
                // (`equality_operand_violation`, src/vm/execute.rs): an
                // operand that is, or transitively contains, a
                // function-shaped leaf has no Equal impl. A polymorphic
                // wrapper (`fn eq(a: x, b: x) -> Bool { a.equal(b) }`)
                // can launder such values past the typechecker's
                // concrete-operand gate, and `PartialEq for Value` would
                // silently answer with `Arc::ptr_eq` identity.
                if Self::value_contains_fn(receiver) || Self::value_contains_fn(&extra_args[0]) {
                    return Some(Err(VmError::new(
                        "type 'Fn' does not implement Equal".into(),
                    )));
                }
                // Defensive fallback. For every valid user/builtin type
                // that passes the round-93 field-aware auto-derive gate
                // (`compute_auto_derive_field_negatives`), a synth-emitted
                // `<Type>.equal` global is produced and `Op::CallMethod`
                // (src/vm/execute.rs ~:2250) resolves it FIRST, so a
                // Variant/Record receiver never reaches this arm. Types
                // with non-supportable fields (e.g. Channel/Map/Tuple/
                // Function/Bytes/Handle) are now statically REJECTED by
                // that gate (`type 'X' does not implement trait`), so the
                // old "such fields are laundered through here" path no
                // longer exists. The only theoretical fall-through is a
                // `Value::Variant` with no `__type_of__` registration
                // (src/vm/mod.rs ~:940), which is not constructible from a
                // valid program. `impl PartialEq for Value` (src/value.rs
                // 1662) compares records and variants structurally,
                // so this arm stays sound even on that malformed input.
                Some(Ok(Value::Bool(*receiver == extra_args[0])))
            }
            "compare" => {
                if extra_args.len() != 1 {
                    return Some(Err(VmError::new("compare() takes 1 argument".into())));
                }
                let other = &extra_args[0];
                // Execution-site backstop mirroring `ordering_with_fn_gate`
                // (src/vm/arithmetic.rs): reject operands that are, or
                // transitively contain, a function-shaped leaf before any
                // arm can defer to `Value::cmp`, which orders closures by
                // `Arc::as_ptr` — an ASLR-nondeterministic result for a
                // polymorphic `fn cmp(a: x, b: x) -> Int { a.compare(b) }`
                // laundering a container of functions past the typechecker.
                if Self::value_contains_fn(receiver) || Self::value_contains_fn(other) {
                    return Some(Err(VmError::new(
                        "type 'Fn' does not implement Compare".into(),
                    )));
                }
                let ord = match (receiver, other) {
                    (Value::Int(a), Value::Int(b)) => a.cmp(b),
                    // Round-71: collapsed four byte-identical NaN /
                    // non-finite arms into one or-pattern, mirroring
                    // `src/vm/arithmetic.rs:130-134`. The two pre-round
                    // wordings ("compare() cannot compare non-finite
                    // float values" on Float/Float and "compare() cannot
                    // compare NaN values" on the three Float/ExtFloat
                    // shapes) are unified to the canonical wording used
                    // by `arithmetic.rs::compare` — NaN IS non-finite,
                    // so the broader phrasing covers every partial_cmp
                    // failure across both dispatch surfaces.
                    (
                        Value::Float(a) | Value::ExtFloat(a),
                        Value::Float(b) | Value::ExtFloat(b),
                    ) => match a.partial_cmp(b) {
                        Some(ord) => ord,
                        None => {
                            return Some(Err(VmError::new(
                                "cannot compare non-finite float values".into(),
                            )));
                        }
                    },
                    (Value::String(a), Value::String(b)) => a.cmp(b),
                    (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
                    // List vs List: the typechecker auto-derives Compare for
                    // List (see src/typechecker/mod.rs:8012), so a value of
                    // `List(T)` flowing through a `Compare` bound must
                    // resolve here. Defer to the existing element-wise
                    // ordering on `Value::cmp`, which already handles
                    // List/Range pairings (see src/vm/arithmetic.rs:138).
                    (Value::List(_), Value::List(_))
                    | (Value::List(_), Value::Range(..))
                    | (Value::Range(..), Value::List(_))
                    | (Value::Range(..), Value::Range(..)) => receiver.cmp(other),
                    // Defensive fallback. For every valid user/builtin type
                    // that passes the round-93 field-aware auto-derive gate
                    // (`compute_auto_derive_field_negatives`), a synth-emitted
                    // `<Type>.compare` global is produced and `Op::CallMethod`
                    // (src/vm/execute.rs ~:2250) resolves it FIRST, so a
                    // Variant/Record receiver never reaches this arm. Types
                    // with non-supportable fields (e.g. Channel/Map/Tuple/
                    // Function/Bytes/Handle) are now statically REJECTED by
                    // that gate (`type 'X' does not implement trait`), so the
                    // old "such fields are laundered through here" path no
                    // longer exists. The only theoretical fall-through is a
                    // `Value::Variant` with no `__type_of__` registration
                    // (src/vm/mod.rs ~:940), which is not constructible from a
                    // valid program. `fn cmp` (src/value.rs 1782)
                    // orders records and variants structurally, so this arm
                    // stays sound even on that malformed input.
                    (Value::Variant(..), Value::Variant(..))
                    | (Value::Record(..), Value::Record(..)) => receiver.cmp(other),
                    //
                    // Unit vs Unit: typechecker auto-derives Compare for `()`
                    // (src/typechecker/mod.rs:8009). All units are equal.
                    (Value::Unit, Value::Unit) => std::cmp::Ordering::Equal,
                    _ => {
                        return Some(Err(VmError::new(format!(
                            "compare() not supported between {} and {}",
                            self.type_name(receiver),
                            self.type_name(other)
                        ))));
                    }
                };
                let result = match ord {
                    std::cmp::Ordering::Less => -1,
                    std::cmp::Ordering::Equal => 0,
                    std::cmp::Ordering::Greater => 1,
                };
                Some(Ok(Value::Int(result)))
            }
            "hash" => {
                // The typechecker auto-derives `Hash` for Int / Float /
                // ExtFloat / Bool / String / List (and more). At runtime,
                // user-defined `trait Hash for T` impls are resolved via
                // the qualified-global path in `Op::CallMethod`; only
                // auto-derived primitives fall through to here.
                //
                // `Value` already implements `std::hash::Hash` with a
                // canonical bit-hash for floats (see src/value.rs:2161).
                // We reuse that impl via `DefaultHasher` so the result
                // matches `HashMap<Value, Value>` keying.
                if !extra_args.is_empty() {
                    return Some(Err(VmError::new("hash() takes no arguments".into())));
                }
                // Execution-site backstop mirroring the `"equal"` /
                // `"compare"` arms above: a receiver that is, or
                // transitively contains, a function-shaped leaf has no
                // Hash impl (`gate_field_supports_trait`,
                // src/typechecker/mod.rs: "Functions support none of
                // Equal/Compare/Hash"). The `(Hash, List)` auto-derive
                // stamp is registered unconditionally
                // (`register_auto_derived_impls_for`,
                // src/typechecker/mod.rs) without walking element types,
                // so `[fn(y) { y }].hash()` reaches this arm — and the
                // std `Hash` impl on `Value` hashes every closure as a
                // constant discriminant tag ("not meaningfully
                // hashable", src/value.rs), so two distinct closures
                // would hash identically and collide silently.
                if Self::value_contains_fn(receiver) {
                    return Some(Err(VmError::new(
                        "type 'Fn' does not implement Hash".into(),
                    )));
                }
                // Only honour hash() for types the typechecker actually
                // auto-derives Hash for — emitting a dispatch error for
                // anything else keeps the user-impl path authoritative.
                // The Variant/Record entries below are a defensive
                // fallback. For every valid user/builtin type that passes
                // the round-93 field-aware auto-derive gate
                // (`compute_auto_derive_field_negatives`), a synth-emitted
                // `<Type>.hash` global is produced and `Op::CallMethod`
                // (src/vm/execute.rs ~:2250) resolves it FIRST, so a
                // Variant/Record receiver never reaches this arm. Types
                // with non-supportable fields (e.g. Channel/Map/Tuple/
                // Function/Bytes/Handle) are now statically REJECTED by
                // that gate (`type 'X' does not implement trait`), so the
                // old "such fields are laundered through here" path no
                // longer exists. The only theoretical fall-through is a
                // `Value::Variant` with no `__type_of__` registration
                // (src/vm/mod.rs ~:940), which is not constructible from a
                // valid program. `impl Hash for Value` (src/value.rs
                // 2134) hashes records and variants structurally, so
                // this arm stays sound even on that malformed input.
                match receiver {
                    Value::Int(_)
                    | Value::Float(_)
                    | Value::ExtFloat(_)
                    | Value::Bool(_)
                    | Value::String(_)
                    | Value::List(_)
                    // Range hashes via the same `impl Hash for Value`
                    // (src/value.rs:2134); typechecker registers Hash for
                    // every `List(T)` that flows through a `Hash` bound,
                    // and `1..5` reaches dispatch as `Value::Range`.
                    | Value::Range(..)
                    | Value::Tuple(_)
                    | Value::Map(_)
                    | Value::Set(_)
                    | Value::Variant(..)
                    | Value::Record(..)
                    | Value::Unit => {
                        use std::collections::hash_map::DefaultHasher;
                        use std::hash::{Hash, Hasher};
                        let mut hasher = DefaultHasher::new();
                        receiver.hash(&mut hasher);
                        // Preserve the full hash width via bit-cast — the
                        // typechecker declares the return type as `Int`
                        // (i64), and a wrapping reinterpretation is
                        // cheaper and more collision-resistant than
                        // truncation.
                        Some(Ok(Value::Int(hasher.finish() as i64)))
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    // ── Builtin dispatch ──────────────────────────────────────────

    pub(super) fn dispatch_builtin(
        &mut self,
        name: &str,
        args: &[Value],
    ) -> Result<Value, VmError> {
        // Foreign functions take priority -- lets embedders override builtins.
        if let Some(f) = self.runtime.foreign_fns.get(name).cloned() {
            return invoke_foreign_fn(name, &f, args);
        }
        if let Some((module, func)) = name.split_once('.') {
            // Each arm is wrapped in `catch_builtin_panic` so that a panic
            // inside a builtin module becomes a clean `VmError` instead of
            // tearing down the current scheduler worker thread. Mirrors
            // `invoke_foreign_fn` for user-registered FFI.
            match module {
                "list" => catch_builtin_panic(
                    "list",
                    AssertUnwindSafe(|| builtins::collections::call_list(self, func, args)),
                ),
                "string" => catch_builtin_panic(
                    "string",
                    AssertUnwindSafe(|| builtins::string::call(self, func, args)),
                ),
                "int" => catch_builtin_panic(
                    "int",
                    AssertUnwindSafe(|| builtins::numeric::call_int(func, args)),
                ),
                "float" => catch_builtin_panic(
                    "float",
                    AssertUnwindSafe(|| builtins::numeric::call_float(func, args)),
                ),
                "map" => catch_builtin_panic(
                    "map",
                    AssertUnwindSafe(|| builtins::collections::call_map(self, func, args)),
                ),
                "set" => catch_builtin_panic(
                    "set",
                    AssertUnwindSafe(|| builtins::collections::call_set(self, func, args)),
                ),
                "result" => catch_builtin_panic(
                    "result",
                    AssertUnwindSafe(|| builtins::core::call_result(self, func, args)),
                ),
                "option" => catch_builtin_panic(
                    "option",
                    AssertUnwindSafe(|| builtins::core::call_option(self, func, args)),
                ),
                "io" => catch_builtin_panic(
                    "io",
                    AssertUnwindSafe(|| builtins::io::call(self, func, args)),
                ),
                "bytes" => catch_builtin_panic(
                    "bytes",
                    AssertUnwindSafe(|| builtins::bytes::call(self, func, args)),
                ),
                "crypto" => catch_builtin_panic(
                    "crypto",
                    AssertUnwindSafe(|| builtins::crypto::call(self, func, args)),
                ),
                "encoding" => catch_builtin_panic(
                    "encoding",
                    AssertUnwindSafe(|| builtins::encoding::call(self, func, args)),
                ),
                "uuid" => catch_builtin_panic(
                    "uuid",
                    AssertUnwindSafe(|| builtins::uuid::call(self, func, args)),
                ),
                "stream" => catch_builtin_panic(
                    "stream",
                    AssertUnwindSafe(|| builtins::stream::call(self, func, args)),
                ),
                #[cfg(feature = "tcp")]
                "tcp" => catch_builtin_panic(
                    "tcp",
                    AssertUnwindSafe(|| builtins::tcp::call(self, func, args)),
                ),
                "fs" => catch_builtin_panic(
                    "fs",
                    AssertUnwindSafe(|| builtins::io::call_fs(self, func, args)),
                ),
                "env" => catch_builtin_panic(
                    "env",
                    AssertUnwindSafe(|| builtins::io::call_env(self, func, args)),
                ),
                "test" => catch_builtin_panic(
                    "test",
                    AssertUnwindSafe(|| builtins::core::call_test(self, func, args)),
                ),
                "math" => catch_builtin_panic(
                    "math",
                    AssertUnwindSafe(|| builtins::numeric::call_math(func, args)),
                ),
                "regex" => catch_builtin_panic(
                    "regex",
                    AssertUnwindSafe(|| builtins::data::call_regex(self, func, args)),
                ),
                "json" => catch_builtin_panic(
                    "json",
                    AssertUnwindSafe(|| builtins::data::call_json(self, func, args)),
                ),
                "toml" => catch_builtin_panic(
                    "toml",
                    AssertUnwindSafe(|| builtins::toml::call(self, func, args)),
                ),
                "channel" => catch_builtin_panic(
                    "channel",
                    AssertUnwindSafe(|| builtins::concurrency::call_channel(self, func, args)),
                ),
                "task" => catch_builtin_panic(
                    "task",
                    AssertUnwindSafe(|| builtins::concurrency::call_task(self, func, args)),
                ),
                "time" => catch_builtin_panic(
                    "time",
                    AssertUnwindSafe(|| builtins::data::call_time(self, func, args)),
                ),
                "http" => catch_builtin_panic(
                    "http",
                    AssertUnwindSafe(|| builtins::data::call_http(self, func, args)),
                ),
                #[cfg(feature = "postgres")]
                "postgres" => catch_builtin_panic(
                    "postgres",
                    AssertUnwindSafe(|| builtins::postgres::call(self, func, args)),
                ),
                // ── Built-in trait Error impls ──
                // Phase 1 of the stdlib error redesign: `trait Error`
                // impls for builtin error enums are routed through
                // dedicated dispatch helpers rather than compiled from
                // silt source. IoError, JsonError, TomlError, and
                // ParseError are wired up; HttpError/RegexError trait
                // impls still return `Result(_, String)` today and
                // will migrate in later phases.
                //
                // Round-73 BLOAT-3 fix: 11 byte-near-identical
                // `<Enum>` => catch_builtin_panic("<Enum>",
                // AssertUnwindSafe(|| <module>::call_<x>_error_trait(...)))
                // arms collapsed onto `ERROR_TRAIT_DISPATCH` table
                // lookup. Every `call_*_error_trait` shares the
                // uniform `fn(&str, &[Value]) -> Result<Value,VmError>`
                // signature so a single function pointer suffices.
                // PgError/TcpError stay cfg-gated by being conditionally
                // included in the table.
                #[cfg(test)]
                "__test_panic_builtin" => {
                    catch_builtin_panic(
                        "__test_panic_builtin",
                        AssertUnwindSafe(|| {
                            // Force a panic regardless of the function name; the
                            // test harness uses this arm to verify that
                            // `catch_builtin_panic` converts the panic into a
                            // clean `VmError`.
                            let _ = (&*self, func, args);
                            panic!("synthetic builtin panic for test")
                        }),
                    )
                }
                _ => {
                    if let Some(f) = error_trait_dispatch(module) {
                        catch_builtin_panic(module, AssertUnwindSafe(|| f(func, args)))
                    } else if let Some(f) = self.runtime.foreign_fns.get(name).cloned() {
                        invoke_foreign_fn(name, &f, args)
                    } else {
                        // Round-64 LATENT fix: previously "unknown
                        // module: <X>". The trait-method dispatch path
                        // lowers `enum_value.method()` into the same
                        // module-style name, so when a builtin error
                        // enum is feature-gated off (e.g. `PgError`
                        // without the postgres feature), users would
                        // see "unknown module: PgError" — confusing,
                        // since PgError is presented to them as an
                        // enum, not a module. The "builtin namespace"
                        // wording matches the user mental model for
                        // both module-qualified function calls and
                        // qualified-global error-trait dispatch.
                        Err(VmError::new(format!("unknown builtin namespace: {module}")))
                    }
                }
            }
        } else {
            match name {
                "println" => {
                    if args.len() != 1 {
                        return Err(VmError::new(format!(
                            "println takes 1 argument, got {}",
                            args.len()
                        )));
                    }
                    println!("{}", self.display_value(&args[0]));
                    Ok(Value::Unit)
                }
                "print" => {
                    if args.len() != 1 {
                        return Err(VmError::new(format!(
                            "print takes 1 argument, got {}",
                            args.len()
                        )));
                    }
                    print!("{}", self.display_value(&args[0]));
                    Ok(Value::Unit)
                }
                "panic" => {
                    let msg = args.first().map(|v| v.to_string()).unwrap_or_default();
                    Err(VmError::new(format!("panic: {msg}")))
                }
                _ => {
                    if let Some(f) = self.runtime.foreign_fns.get(name).cloned() {
                        invoke_foreign_fn(name, &f, args)
                    } else {
                        Err(VmError::new(format!("unknown builtin: {name}")))
                    }
                }
            }
        }
    }

    /// Get current epoch milliseconds. Uses `__wasm_epoch_ms` foreign function
    /// if registered (WASM), otherwise falls back to `SystemTime`.
    pub(crate) fn epoch_ms(&self) -> Result<i64, VmError> {
        if let Some(f) = self.runtime.foreign_fns.get("__wasm_epoch_ms") {
            match invoke_foreign_fn("__wasm_epoch_ms", f, &[])? {
                Value::Int(ms) => Ok(ms),
                _ => Err(VmError::new("__wasm_epoch_ms returned non-Int".into())),
            }
        } else {
            use std::time::{SystemTime, UNIX_EPOCH};
            let dur = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|e| VmError::new(format!("clock failed: {e}")))?;
            Ok(dur.as_millis() as i64)
        }
    }
}
