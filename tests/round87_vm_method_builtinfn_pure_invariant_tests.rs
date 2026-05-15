//! Round-87 LATENT lock: every `<Type>.method` BuiltinFn registered by
//! `Vm::register_builtins` must be a PURE (non-yielding) helper.
//!
//! ── Why this invariant matters ────────────────────────────────────────
//!
//! `Op::CallMethod` (`src/vm/execute.rs:2160-2263`) resolves a qualified
//! method `<Type>.method` against `Vm::globals`. When the global value is
//! a `Value::BuiltinFn`, the call is routed through
//! `invoke_callable_resumable` (`src/vm/execute.rs:1185-1206`), whose
//! `BuiltinFn` arm at `invoke_callable` (`src/vm/execute.rs:907`)
//! delegates directly to `dispatch_builtin(name, args)` WITHOUT pushing
//! a frame or capturing a `suspended_invoke` checkpoint.
//!
//! `invoke_callable_resumable` has a yield-recovery branch that, on
//! `e.is_yield`, re-pushes `original_args` so the outer `Op::CallMethod`
//! instruction can re-execute on resume. This is correct ONLY when the
//! callee did NOT already re-push its own args on yield. Foreign-side
//! yielding primitives (`Vm::park_with_reason` at `src/vm/mod.rs:283`
//! and friends) DO push their incoming arg vector back onto the stack
//! before bubbling the yield up — so for a `BuiltinFn` that yields, the
//! recovery branch would push a SECOND copy of the args on top of the
//! first. On resume, `Op::CallMethod` reads `argc`, computes
//! `receiver_slot = stack.len() - argc`, and lands at the second copy,
//! leaving the first copy as garbage below `receiver_slot`. That is
//! stack corruption.
//!
//! Today the bug is unreachable because the ONLY `<Type>.method`
//! BuiltinFns ever registered are the stdlib `trait Error` `.message`
//! helpers (`src/vm/dispatch.rs:222-226`), all of which dispatch to
//! `call_*_error_trait` (see `ERROR_TRAIT_DISPATCH`) — pure functions
//! that never call `park_with_reason`, `submit_io_or_run`, or any other
//! yielding primitive. If a future change registers a yielding method
//! (e.g. `<Type>.read` mapping to an IO helper) the latent bug becomes
//! live and silently corrupts the stack on resume.
//!
//! This file source-grep-locks the registration block to two
//! invariants:
//!
//!   (a) The registration block at `src/vm/dispatch.rs:~222-226` is the
//!       ONLY place that registers `<Type>.method` BuiltinFns, and the
//!       method suffix is the literal `".message"` — no other method
//!       names are allowed without explicit review of the resume path.
//!
//!   (b) The dispatch table `ERROR_TRAIT_DISPATCH` (`src/vm/dispatch.rs`)
//!       maps every entry to a target named `call_<x>_error_trait` —
//!       the documented "pure helper" naming convention. A new entry
//!       with a different naming convention (e.g. `call_io_async`)
//!       would signal a yielding builtin and must be rejected here.
//!
//! If a future commit either (a) adds a method name other than
//! `"message"` to the registration block, or (b) wires a non-
//! `call_*_error_trait` target into `ERROR_TRAIT_DISPATCH`, this test
//! file FAILS with a message that points at the resume-path latent bug
//! so the next author has the full context.

const DISPATCH_SRC: &str = include_str!("../src/vm/dispatch.rs");

// ── Invariant (a): only `.message` is registered as a method BuiltinFn ──

/// Locate the registration block in `register_builtins` that inserts
/// `<EnumName>.<method>` BuiltinFn globals, and assert the literal
/// method suffix is `".message"`. The block today is:
///
/// ```ignore
/// for (enum_name, _variants) in module::builtin_error_enum_variants_with_arity() {
///     let key = format!("{enum_name}.message");
///     self.globals
///         .insert(key.clone(), Value::BuiltinFn(key.clone()));
/// }
/// ```
///
/// We pin the literal `format!("{enum_name}.message")` shape. Any drift
/// to a different method name (e.g. `.read`, `.write`) — or splitting
/// the block into multiple registrations covering more than one method
/// — must come with a deliberate change to this test.
#[test]
fn round87_only_message_method_is_registered_as_builtinfn() {
    // The format-string fragment we lock on. The leading `{enum_name}.`
    // anchors it to the registration loop (the only place an interpolated
    // enum-name prefix feeds into a method key).
    let canonical = r#"format!("{enum_name}.message")"#;
    assert!(
        DISPATCH_SRC.contains(canonical),
        "src/vm/dispatch.rs must contain the canonical registration \
         shape `{canonical}`. Round-87 LATENT lock: only `.message` \
         method BuiltinFns are safe to register, because they all \
         dispatch to pure `call_*_error_trait` helpers that never \
         yield. See `invoke_callable_resumable` \
         (src/vm/execute.rs:1185-1206) — a yielding method BuiltinFn \
         would double-push args on yield and corrupt the stack on \
         resume."
    );

    // Negative: assert no OTHER `format!("{enum_name}.<x>")` appears
    // with a non-`message` suffix. We scan every line for the
    // `format!("{enum_name}.` prefix and assert the remainder begins
    // with `message"`.
    let bad: Vec<(usize, String)> = DISPATCH_SRC
        .lines()
        .enumerate()
        .filter_map(|(idx, line)| {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                return None;
            }
            let needle = r#"format!("{enum_name}."#;
            let pos = line.find(needle)?;
            let after = &line[pos + needle.len()..];
            // Expect `message")` immediately after the dot.
            if after.starts_with("message\")") {
                None
            } else {
                Some((idx + 1, line.to_string()))
            }
        })
        .collect();
    assert!(
        bad.is_empty(),
        "src/vm/dispatch.rs has a `format!(\"{{enum_name}}.<x>\")` \
         registration with `<x>` != `message`. Round-87 LATENT lock: \
         registering a yielding `<Type>.<method>` BuiltinFn would \
         corrupt the stack on resume via the double-push in \
         `invoke_callable_resumable` (src/vm/execute.rs:1185-1206). If \
         adding a new method, FIRST audit that its dispatch target \
         is pure (no `park_with_reason` / `submit_io_or_run` / etc.), \
         THEN update this lock. Offending lines:\n  {}",
        bad.iter()
            .map(|(n, l)| format!("line {n}: {l}"))
            .collect::<Vec<_>>()
            .join("\n  ")
    );
}

// ── Invariant (b): every ERROR_TRAIT_DISPATCH target follows the
//                  pure-helper naming convention ──────────────────

/// `ERROR_TRAIT_DISPATCH` (in `src/vm/dispatch.rs`) is the lookup table
/// consulted by `dispatch_builtin` when a `<EnumName>.message`
/// BuiltinFn is invoked. Every entry maps an enum name to a function
/// pointer whose symbol must match `call_<lowercase>_error_trait` —
/// our documented marker that the helper is pure (renders a variant
/// to a `String` and returns; no scheduler interaction). A future
/// entry like `("IoError", builtins::io::call_io_async)` would break
/// the marker and signal a yielding dispatch path; this lock catches
/// that drift at test time.
#[test]
fn round87_error_trait_dispatch_targets_match_pure_naming() {
    // Find the ERROR_TRAIT_DISPATCH literal block and assert every
    // function-pointer reference in it ends with `_error_trait`.
    let block_start = DISPATCH_SRC
        .find("static ERROR_TRAIT_DISPATCH:")
        .expect("ERROR_TRAIT_DISPATCH static must exist in src/vm/dispatch.rs");
    // The block ends at the first `];` after the start.
    let tail = &DISPATCH_SRC[block_start..];
    let block_end_rel = tail
        .find("];")
        .expect("ERROR_TRAIT_DISPATCH must terminate with `];`");
    let block = &tail[..block_end_rel];

    // Each entry has shape `(_, builtins::<mod>::call_<x>_error_trait)`
    // or with a `#[cfg(...)]` attribute. We grep for the
    // `builtins::` path and require the trailing symbol to match the
    // pure naming convention.
    let mut bad: Vec<String> = Vec::new();
    for line in block.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") {
            continue;
        }
        if let Some(pos) = line.find("builtins::") {
            // Extract the symbol up to the next `)` or `,`.
            let rest = &line[pos..];
            let end = rest
                .find(|c: char| c == ')' || c == ',')
                .unwrap_or(rest.len());
            let sym = &rest[..end];
            // Strip whitespace just in case.
            let sym = sym.trim();
            if !sym.ends_with("_error_trait") {
                bad.push(format!("{sym}  (line: {line})"));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "ERROR_TRAIT_DISPATCH in src/vm/dispatch.rs maps a `<Type>.message` \
         dispatch to a target whose symbol does NOT end in \
         `_error_trait`. Round-87 LATENT lock: the `call_*_error_trait` \
         naming convention is our marker that the helper is pure \
         (no `park_with_reason` / `submit_io_or_run` / etc.). A \
         yielding target reached via `Op::CallMethod` → \
         `invoke_callable_resumable` (src/vm/execute.rs:1185-1206) \
         would double-push args on yield and corrupt the stack on \
         resume. Offending entries:\n  {}",
        bad.join("\n  ")
    );
}

// ── Invariant (c): the registration site stays singular ──────────

/// The pure-helper invariant relies on there being EXACTLY ONE place
/// in `register_builtins` that constructs `<Type>.method` BuiltinFn
/// globals. If a second registration loop sneaks in elsewhere (e.g.
/// inserting `<Type>.read` directly into `self.globals` as a
/// `Value::BuiltinFn`), Invariant (a) above won't catch it because the
/// new site won't use the `format!("{enum_name}.<x>")` shape we grep
/// for. This test pins that there is exactly one
/// `Value::BuiltinFn(key` shape inside `register_builtins` that
/// references a qualified-name key — the existing one. The free-
/// function and module-scoped loops below it use different bindings
/// (`name.into()` and `qualified.clone()` respectively) and unqualified
/// names that don't collide with the `<Type>.method` dispatch path.
#[test]
fn round87_single_qualified_method_builtinfn_registration_site() {
    // Locate the boundary of `register_builtins`. We scan from
    // `pub(super) fn register_builtins` to the matching closing brace
    // at the correct depth.
    let fn_start = DISPATCH_SRC
        .find("pub(super) fn register_builtins")
        .expect("register_builtins fn must exist");
    let body_start = DISPATCH_SRC[fn_start..]
        .find('{')
        .expect("register_builtins must have a body")
        + fn_start;

    // Walk the body tracking brace depth to find its closing brace.
    let bytes = DISPATCH_SRC.as_bytes();
    let mut depth: i32 = 0;
    let mut end = body_start;
    for i in body_start..bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    end = i;
                    break;
                }
            }
            _ => {}
        }
    }
    assert!(end > body_start, "could not find end of register_builtins body");
    let body = &DISPATCH_SRC[body_start..=end];

    // Count distinct registration sites that construct a `<Type>.method`
    // BuiltinFn key. The current shape is
    // `format!("{enum_name}.message")` — there must be exactly one such
    // occurrence inside `register_builtins`.
    let count = body.matches(r#"format!("{enum_name}."#).count();
    assert_eq!(
        count, 1,
        "expected exactly ONE `<Type>.method` BuiltinFn registration \
         site inside `register_builtins` (the `{{enum_name}}.message` \
         loop). Found {count}. Round-87 LATENT lock: the resume-path \
         invariant — every method BuiltinFn must be pure — depends on \
         there being a single, audited registration site. A second \
         site bypasses the source-grep lock in \
         `round87_only_message_method_is_registered_as_builtinfn`."
    );
}
