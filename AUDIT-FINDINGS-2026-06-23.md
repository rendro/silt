# silt nightly audit — round could NOT complete (build blocked)

Date: 2026-06-23. Branch base: main @ f79834c.

## BLOCKER (why no fixes were pushed)
`cargo build` cannot run: 272 deps, none cached, no vendor dir, and the crate
tarball host `static.crates.io` returns **403 (egress policy denial)** through the
agent proxy. `index.crates.io` is reachable (200) but tarballs are not, and an
offline build fails (no cache). The skill's non-negotiable integrate step
(single `cargo build && cargo test`, every fix locked by a test that fails-before/
passes-after) is therefore impossible. Per proxy policy, 403 denials are reported,
not routed around. No branch pushed — shipping unverified fixes to audit/auto-nightly
would violate the load-bearing verified-lock invariant.

## Audit result: 7 in-scope findings, all BROKEN, zero REGRESSION, zero deferred.
(type-system, vm-runtime, test-coverage agents: zero findings — tree is well-hardened.)

### 1. [docs] phantom `string.concat`
BROKEN. docs/language/design-decisions.md:14 and docs/language/loops-and-pipes.md:120
advertise `string.concat`, which does not exist (string.* registry in
src/typechecker/builtins/string.rs has no `concat`). Same phantom in the diagnostic
at src/typechecker/inference.rs:3530 ("use `string.concat` or `+`").
Fix: docs → use `string.join` or `+`; fix diagnostic text. Lock: include_str! grep
that neither doc contains "string.concat" + assert registry lacks `concat`.

### 2. [formatter] FloatRange pattern loses decimal
BROKEN. src/formatter.rs:5817 renders FloatRange via f64 Display, so `1.0..10.0`
formats to `1..10`, which re-parses as an integer Range — changes match semantics,
breaks idempotency. Fix: mirror the PatternKind::Float arm (append ".0" when no dot).
Lock: round-trip + format(format(x))==format(x) + assert parsed arm is FloatRange.

### 3. [formatter] Map-pattern string key not escaped
BROKEN. src/formatter.rs:5818-5823 re-wraps an already-decoded map-pattern key in
quotes with no re-escaping (sibling StringLit arm at 5746 uses escape_string). Keys
with `"`, `\`, `\n`, `\t` produce unparseable/corrupted output. Fix: use
escape_string(key). Lock: round-trip a key containing `"` and `\`.

### 4. [diagnostics] opcode names leak into VmError messages
BROKEN. src/vm/execute.rs:1634,1652,1681,1726,1737,2286 emit raw opcode names
(MakeRecord/MakeVariant/RecordUpdate/ListConcat/NarrowFloat), violating the
round-58 "internal VM error:" convention. The existing lock
tests/vm_error_identifier_leak_tests.rs passes only because its LEAKING_OPCODE_NAMES
list omits these 5 siblings. Fix: reword to canonical form + extend the lock list.

### 5. [diagnostics] unquoted record name
BROKEN. src/typechecker/inference.rs:3106 prints `record User has no field or method
'email'` (unquoted) while siblings (2347,2372,5366) and the documented format use
`record 'User' ...`. Fix: add quotes. Lock: run-based stderr assertion.

### 6. [diagnostics] off-by-one caret on unknown escape
BROKEN. src/lexer.rs:400 captures span after consuming both `\` and the bad char, so
the caret lands one column past it (siblings capture `start` before advancing). Fix:
snapshot span before advancing. Lock: run/lex test asserting caret column == `\` col.

### 7. [probe/tests] unlocked BUILTIN_MODULES ↔ VM dispatch parity
Missing lock (in-scope). src/module.rs:4 BUILTIN_MODULES (27 modules) and the
`match module` table at src/vm/dispatch.rs:485 must stay in lockstep but no test
drives every module through the VM dispatch. Adding a module everywhere except the
dispatch arm typechecks then crashes at call time ("unknown builtin namespace").
Fix: parity test running one representative fn per BUILTIN_MODULES entry via the
compile+VM path (feature-gate postgres/tcp), asserting != unknown-namespace error.

## Recommended next step
Re-run the audit in a network environment that permits crates.io tarball downloads
(or pre-vendor/cargo-cache the 272 deps). All 7 fixes are mechanical, high-confidence,
and specified to file:line above — a networked integrator can apply, lock, build+test,
and push in one pass.
