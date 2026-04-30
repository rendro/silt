---
title: "Proposal: scope canonical alias / assoc-binding registries to a compile session"
section: "Proposals"
status: draft
---

# Scope canonical alias / assoc-binding registries to a compile session

**Status:** draft. Round-62 audit deferred this. Not actively biting in
production silt today, but the LSP path is structurally exposed (see
"Reproduction" below — multi-file LSP pulls share alias state).

**Scope:** move `register_alias` / `register_assoc_binding` storage
out of the process-global `RwLock<HashMap>` and into a compile-scoped
`Resolver` shared by every `TypeChecker` instance produced for one
compile invocation. No public API change for embedders. No language
change. Internal refactor that closes the cross-pull contamination
hazard for LSP and any future embedder.

## Background

`src/types/canonical.rs` exposes two read/write registries:

- `register_alias(name, AliasInfo)` / `lookup_alias(name) -> Option<AliasInfo>`
  — records a `type Alias = ...` decl so later type lookups resolve
  through it.
- `register_assoc_binding(trait_name, target_head, assoc_name, ty)` /
  `lookup_assoc_binding(...)` — records an associated-type binding
  from a `type Foo = ...` clause inside an impl.

Both are stored as module-level
`static REG: OnceLock<RwLock<HashMap<String, _>>>` keyed on resolved
strings (the interner is `thread_local!`, so `Symbol` keys are not
cross-thread comparable — see `canonical.rs:101-107`). Every
`TypeChecker` constructed in the process reads and writes the same
maps.

The dominant consumer is `canonical::canonicalize(&Type) -> Type`
(`canonical.rs:215`), called from many sites (`typechecker/mod.rs`
:1173, :1178, :1739, :1781, :1898, :1899, :3892, :5372, :5917;
`typechecker/inference.rs` :2396, :3894). `canonicalize` reads aliases
and assoc bindings during reduction. Recursive entry is bounded:
`AssocProj` reduction calls `canonicalize(&binding.ty)` exactly once,
and `binding.ty` was canonicalised at registration time so re-entry
on the same projection cannot fire (canonical.rs:289-296).

## Reproduction

**LSP pull diagnostics across two files in one workspace.** The LSP
server constructs a fresh `TypeChecker` per pull
(`src/lsp/diagnostics.rs:94`, `src/lsp/definitions.rs:339,383,397,411,
424,442`, `src/lsp/fields.rs:332`). All instances share the global
registry. Sequence:

1. User opens `a.silt` containing `type Mass = Float`. LSP pull
   triggers `check_with_package_and_imports_options(...)`. Registry
   gains `("Mass", AliasInfo { target: Float, ... })`.
2. User opens `b.silt`. `b.silt` does NOT declare `Mass`. LSP pull
   constructs a new `TypeChecker`, runs `check`. The TypeChecker's
   own env, schemes, traits etc are empty wrt `Mass`, but
   `canonicalize` lookups still resolve `Mass` to `Float` via the
   global registry.
3. Visible symptom: `b.silt` referencing `Mass` typechecks and
   produces no "unknown type" diagnostic, even though no import
   brought `Mass` in. Hover/completion may surface `Mass` as a
   known type.

This is structurally observable today but rarely tripped because
alias names tend to be either (a) pkg-prefixed conventions or
(b) common enough to be unique within a workspace. No incident is
attributed to it.

## Constraints discovered during analysis

The original "move both registries into `TypeChecker` as private
fields" plan does not work as a one-line refactor. Three constraints
forced the redesign:

1. **`canonicalize` is called across `TypeChecker` boundaries.**
   `pub fn check_with_package_and_imports_options` constructs a
   fresh `TypeChecker` per imported module
   (`typechecker/mod.rs:6571`). The CLI compile pipeline runs this
   once per module in import order. Today's design relies on the
   global registry being a cross-module sharing point: module A's
   `pub type Mass = Float` registers into the global, then module B
   importing A finds `Mass` via lookup. Moving the registry into
   `TypeChecker` breaks cross-module alias resolution.

2. **`ModuleExports` lists alias names but not bodies.** The
   exports struct (`typechecker/mod.rs:414-449`) carries `aliases:
   Vec<Symbol>` and `alias_arity: Vec<usize>` but NOT the alias
   target type. The current code reaches the body via the global
   `lookup_alias`. Per-`TypeChecker` scoping requires extending
   `ModuleExports` to carry full `AliasInfo` and `AssocBinding`
   payloads, plus an importer-side merge step.

3. **The variant-ordinal registry in `src/value.rs:156-171` cannot
   move.** It is consumed by `Value::cmp`, called from the VM and
   from `Value::Hash`. The VM does not own a `TypeChecker`. So the
   variant-ordinal pattern stays global and cross-process by
   necessity. The alias / assoc-binding registries are different —
   ONLY the typechecker calls `canonicalize`. The VM never does.
   Confirmed by `rg 'canonical::canonicalize\b' src/`: zero matches
   outside `src/typechecker/` and `src/types/canonical.rs` itself.

## Fix: introduce a compile-session `Resolver`

Add a `Resolver` struct that owns the alias and assoc-binding maps:

```rust
pub struct Resolver {
    aliases: HashMap<String, AliasInfo>,
    assoc_bindings: HashMap<(String, String, String), AssocBinding>,
}
```

Construct one `Resolver` per `silt run` / `silt check` /
`silt test` / LSP pull. Pass `&mut Resolver` to every `TypeChecker`
constructed during that compile invocation, and `&Resolver` to every
read site (`canonicalize`, `apply_alias`).

The cross-module sharing that today rides on the global registry
moves onto the `Resolver`: importing module B reuses module A's
`Resolver`, so A's aliases stay visible to B without a per-module
`ModuleExports` payload extension.

LSP closes the contamination hazard by constructing a fresh
`Resolver` per pull (parallel to today's fresh `TypeChecker`).

## Migration plan

1. Add `pub struct Resolver` in `src/types/canonical.rs` with the
   fields above and methods `register_alias`, `lookup_alias`,
   `register_assoc_binding`, `lookup_assoc_binding`. Body is the
   same as today's free fns minus the `RwLock`.

2. Change `pub fn canonicalize(ty: &Type) -> Type` to
   `pub fn canonicalize(resolver: &Resolver, ty: &Type) -> Type`.
   Same for `apply_alias`. The change is pure threading.

3. Add `resolver: Resolver` field to `TypeChecker`. Initialise in
   `TypeChecker::new()`. Replace every
   `crate::types::canonical::register_alias(...)` /
   `lookup_alias(...)` call inside the typechecker with
   `self.resolver.register_alias(...)`. Replace every
   `canonical::canonicalize(t)` with
   `canonical::canonicalize(&self.resolver, t)`.

4. Extend the cross-module entry points
   (`check_with_package_and_imports_options`,
   `check_with_package_and_imports`) with an optional
   `resolver: Option<&mut Resolver>` parameter. When `Some`,
   reuse the caller's resolver; when `None`, allocate a
   per-`TypeChecker` resolver (fresh-pull behaviour).

5. Update CLI compile pipeline (`src/cli/pipeline.rs`) to construct
   one `Resolver` per `compile_pipeline` call and thread it through
   each module's typecheck. Update LSP entry points
   (`src/lsp/diagnostics.rs`, `src/lsp/definitions.rs`,
   `src/lsp/fields.rs`) to allocate a fresh `Resolver` per pull.

6. Delete the global statics (`alias_registry`, `assoc_registry`)
   and the free `pub fn` accessors. Tests that today call them
   directly (`tests/canonical_type_equality_phase_b_tests.rs:4`)
   must construct a `Resolver` and pass it through.

## Locking tests

In `tests/canonical_resolver_isolation_tests.rs` (new):

- `two_resolvers_do_not_share_aliases` — two `Resolver` instances,
  register `type Mass = Float` on the first, look up `Mass` on the
  second, assert `None`. Deletes today's process-global behaviour.
- `cross_module_compile_shares_one_resolver` — exercise the CLI
  compile pipeline with two modules; module A declares an alias,
  module B imports A and uses the alias name; assert the use
  resolves. Locks the "session-shared resolver" contract.
- `lsp_pull_does_not_inherit_other_files_aliases` — drive
  `src/lsp/diagnostics.rs` (or the helper test infra) twice with
  different files, where file A declares an alias and file B does
  not import A; assert file B does not see the alias. Locks the
  fix on the LSP-pull contamination repro above.

## Effort estimate

~6-10 hours. Up from the original ~4-6h, because:

- Threading `&Resolver` through `canonicalize` touches ~12
  call sites across `typechecker/mod.rs`, `typechecker/inference.rs`.
- Cross-module plumbing (`check_with_package_and_imports_options`)
  needs the new optional parameter.
- LSP and CLI entry points need a tiny construction shim.

The mechanical risk is missing one `canonicalize` site — the
compiler will catch this (signature change is breaking). No
silent-regression risk.

## Out of scope

- The variant-ordinal registry (`src/value.rs:156`) stays global.
  It is consumed by the VM at runtime and cannot be scoped to a
  compile session. (Justification: VM does not own a `TypeChecker`,
  and `Value::cmp` must work on every `Value` regardless of which
  compile produced it.)
- A public `Resolver` API for embedders. Today there are no
  embedders. The `Resolver` is `pub(crate)` on first cut; promote
  to `pub` only when an embedder materialises.
- `OnceLock` simplification. Registration happens during decl-walk
  pass and is read during inference of the same compile, so there
  is no clean "build once then freeze" boundary unless you
  introduce an explicit phase change. Per-`Resolver` `RwLock` (or
  `Mutex`) is fine; the lock is uncontended within a single
  compile.

## Why now

The audit framework's stated principle is "REGRESSION rate trends
toward zero across rounds." Process-global mutable state that
survives between unrelated compile invocations is a structural
REGRESSION-waiting-to-happen — and the LSP contamination repro is
already structurally observable.

But there is no current incident, no current embedder, and no
current test failure attributable to this design. So the proposal
stays a draft, not a P0. A reasonable trigger: the next time an
LSP user reports "this file typechecks even though it shouldn't",
fix it under this proposal rather than patching the symptom.
