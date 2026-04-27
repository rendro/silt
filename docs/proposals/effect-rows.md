---
title: "Proposal: effect-row tracking"
section: "Proposals"
status: draft
---

# Effect-row tracking for silt

**tl;dr:** Recommend a coarse, inferred, tracking-only effect system.
Skip handlers and user-defined effects in v1.

**Status:** proposal, not yet implemented.
**Scope (aspirational):** a fixed set of ~5 named effects (`!io`,
`!mut`, `!async`, `!panic`, `!net`), inferred from function bodies,
surfaced on LSP hover, optionally annotated at module boundaries. No
algebraic handlers, no user-defined effects, no row polymorphism in
the user-facing surface.

---

## Part 1 — Background and problem

### What effect tracking is

An *effect* is anything a function does beyond computing its return
value: reading the filesystem, sending packets, mutating shared
state, suspending on `await`, panicking on a bounds check. An
*effect-tracking type system* lifts those effects into the function's
signature so the type checker — not just the runtime — can reason
about them.

In Haskell the lifting goes through monads (`IO a`, `State s a`). In
Koka effects appear as a row attached to the return type
(`int <io,div>`). In Unison they are "abilities" requested by a
computation. In Rust the system is partial and ad-hoc — `async`,
`unsafe`, `const`, `Send`, `Sync` are each one bit of an effect
system fused into the language for one specific reason.

The core promise across all of them is the same: *if the type says
the function is pure, the compiler has checked that it is*.

### Why silt doesn't have it today

Silt already has two mechanisms that are sometimes confused with
effect tracking but are not:

- **Result/Option** are *value-level*. They make failure paths
  explicit at the call site. They say nothing about whether a
  function reads a file, spawns a task, or panics — only about
  whether it returns a wrapped error in the success path.
- **Structured concurrency** is *runtime-level*. It bounds task
  lifetimes and protects channel ownership. It says nothing in the
  type system about whether a function suspends.

Effect tracking is *type-level*. It is complementary to both: a
function returning `Result(Config, ConfigError)` can still secretly
read the filesystem, and a function declared in a "pure-looking"
module can still spawn a task whose cancellation racing the parent
is invisible to the caller.

### Where the absence is felt

Concrete cases in current silt code:

```silt
-- Looks pure. Quietly reads $HOME, opens a file, parses TOML.
fn load_defaults() -> Config {
  let raw = fs.read_to_string(env.home() ++ "/.config/app.toml")
  toml.parse(raw).unwrap()
}
```

A caller hovering over `load_defaults()` sees `() -> Config`. There
is nothing in the type that warns "this touches the filesystem and
will panic if the file is malformed".

```silt
-- Async-coloring question:
fn fetch_and_log(url: String) -> Result(Body, NetError) {
  let resp = http.get(url).await?
  log.info("fetched {url}")
  Ok(resp.body)
}
```

There is no signal in the signature that this function suspends. A
synchronous caller cannot tell from the type alone that they need to
be inside a task scope.

```silt
-- Module boundary: capability question.
module config_loader exports load_defaults, save_user_pref
```

Today the module boundary is purely about visibility. There is no
way to say "this module exports functions that touch the
filesystem" — and therefore no way to deny it of `!io` in a sandbox
target. Effect tracking is the type-level prerequisite for
capability-based deployment.

```silt
-- Mock-friendly testing:
fn business_logic(repo: Repo) -> Result(Report, AppError) {
  -- pure if repo is a fake; impure if repo is a real Postgres handle.
}
```

Without effects in the signature, it is the *parameter* that decides
purity, and the test author has to know that out-of-band. With
effects, `repo: Repo` carrying `!io` would advertise the fact.

---

## Part 2 — Comparable-language survey

### Haskell — monads + transformers

Effects show up in the type as monad constructors: `IO a`,
`State s a`, `ReaderT Config IO a`. The "mtl" pattern stacks them.

**What works:** Total, strong guarantees. `pure` really is pure. The
ecosystem (servant, persistent, conduit) ships production code
where the monad signature is the contract.

**What doesn't:** The transformer pyramid (`ReaderT Env (StateT S
(ExceptT E IO))`). Lifting through stacks. Type errors that mention
seven type variables. mtl-vs-effect-libraries flame wars (`fused-effects`,
`polysemy`, `effectful`) that have churned for a decade.

**Adoption:** Real, but Haskell's reach is bounded by exactly this
ergonomic tax.

### Koka — effect rows + handlers

Effects appear after the return type as a row: `int <div,console>`.
Handlers (`with handler { ... }`) discharge them. Rows are
polymorphic and inferred.

**What works:** Effects compose without nesting. Handlers let you
implement state, exceptions, async, generators in user code. Daan
Leijen's design notes (https://koka-lang.github.io/koka/doc/book.html)
are the canonical reference.

**What doesn't:** Mostly research-grade adoption. Type errors
involving rows are still hard. The killer-app demo — async
implemented as a library handler — is impressive but doesn't compose
with the rest of an ecosystem written assuming a runtime.

**Adoption:** Microsoft Research, academic users, no significant
production deployments.

### OCaml 5 — handlers without typing

OCaml 5 shipped algebraic effect handlers in 2022. Crucially, **the
type system does not track them.** A function that performs an
effect has the same signature as one that doesn't.

**What works:** Eio, the production-grade async runtime, is built on
this primitive. Performance is excellent. Migration cost from OCaml
4 was near-zero.

**What doesn't:** Unhandled effects become runtime errors. There is
no compile-time check that a handler exists.

**Rationale (paraphrased from KC Sivaramakrishnan's papers and the
ocaml-multicore retrospective at
https://kcsrk.info/papers/drafts/retro-concurrency.pdf):** typing
effects properly required either row polymorphism (which clashes
with OCaml's existing structural-row machinery for objects/variants)
or a redesign too invasive for a language with two decades of
deployed code. The team explicitly chose to ship the runtime
primitive and defer typing — a signal that even researchers who
understand effect typing concluded the cost-to-value ratio for a
mature ML-family language did not warrant a coupled rollout.

**Adoption:** Heavy. Eio is the new mainline async story. Untyped
effects are the load-bearing primitive in production OCaml today.

This is the strongest evidence-based data point silt has. A
well-resourced team that knows the theory chose to defer the typing.

### Unison — abilities

Effects are "abilities" attached to types: `Text ->{IO} ()`.
Handlers are first-class. The compiler tracks ability usage.

**What works:** Distributed-execution use case is legitimately
novel — abilities feed Unison's serialization model.

**What doesn't:** Niche language, very small community. Pain points
not yet stress-tested at scale.

**Adoption:** A few production users at Unison Computing and
adjacent shops.

### Rust — partial fixed effects

Rust has `async`, `unsafe`, `const`, plus `Send`/`Sync` auto-traits.
Each is a single bit of an effect system bolted on for one purpose.

**What works:** `unsafe` as a soundness boundary is universally
accepted. `Send`/`Sync` make data-race freedom checkable.

**What doesn't:** *Function colour* — the entire async-vs-sync split
that the async-keyword-generics RFC
(https://rust-lang.github.io/rfcs/3719-async-keyword-generics.html
or its working-group equivalent) is trying to fix. Library authors
write each function twice. `async fn` in traits was a multi-year
saga. Rust is the cautionary tale for *partial* effect systems.

**Adoption:** Universal in Rust, but the friction is the dominant
ergonomic complaint about the language.

### Scala 3 Caprese — capabilities

Capture checking tracks which capabilities (file handles, async
contexts) escape from a scope. Effects-as-capabilities means the
effect *is* a value you have access to.

**What works:** Avoids the "two universes" problem. Capabilities are
ordinary values.

**What doesn't:** Experimental. Adds yet another sigil to Scala's
already-dense surface syntax. Adoption pending.

**Adoption:** Experimental in Scala 3.x.

### F* / Idris — refinement-typed effects

Effects can carry refinement predicates: "this function reads only
files matching `*.toml`". Verified microkernels, cryptographic
proofs.

**What works:** When you need a proof, you get a proof.

**What doesn't:** SMT-solver dependence. Engineering tax measured in
person-years per kLoc. Wildly out of scope for a general-purpose
language.

**Adoption:** Verification-shop niche. Project Everest (HACL*,
EverParse) ships in Firefox and Linux kernel TLS. Not a general
ergonomic story.

---

## Part 3 — Value vs friction

### Value to silt

- **LSP hover safety.** Hover over `load_defaults` and see
  `() -> Config !{io,panic}`. Caller knows immediately.
- **Capability boundaries.** Modules can refuse to import
  `!net`-capable functions. Sandbox targets become possible.
- **Async clarity.** `!async` in the signature is the type-level
  signal silt currently lacks. Resolves the "is this thing
  suspending" question without requiring a function-colour split.
- **Mock-friendly testing.** Effect-free signatures advertise
  testability. Effectful ones advertise the dependency surface.
- **Future-proof for sandboxing, distributed exec, deterministic
  replay** — all features that need an effect substrate to land
  without compiler-wide rework.

### Friction

- **Implementation cost.** ~4-6 weeks tracking-only. ~3-6 months
  with handlers.
- **Stdlib sweep.** ~400 builtins to annotate. Mechanical but
  load-bearing — a wrong annotation poisons every downstream
  inference.
- **Error-message engineering tax.** "Effect `!io` from call to
  `fs.read` not in declared row `!{}`" needs as much polish as the
  type checker's existing diagnostics.
- **Two universes problem.** Pure code and effectful code start to
  feel like two languages. Mitigated by inference + `!*` default
  but not eliminated.
- **Viral propagation.** A `!panic` deep in a call graph propagates
  out to the leaves. Either users opt out via `!*` (defeats the
  purpose) or they propagate honestly (cognitive load).
- **LSP regressions.** Hover, completion, and goto-definition all
  need to know about effects. Each is a place for jank.
- **Cognitive load.** Users who never wanted to think about effects
  now have to.

---

## Part 4 — Decision matrix

| Option | Impl cost | Friction | Value | Diag risk | Compounding |
|---|---|---|---|---|---|
| 1. Skip | 0 | 0 | 0 | 0 | 0 |
| 2. Coarse fixed-set, inferred | 5-7 wk | low | high | medium | high |
| 3. Coarse + user-extensible | 10-14 wk | medium | high | high | high |
| 4. Full algebraic + handlers | 6 mo+ | high | medium | high | medium |

**Option 1** ships silt without the feature. Defensible — silt is
9.3/10 today. But the score evaluation flagged effect rows as the
single critical missing piece.

**Option 2** delivers most of the value (LSP safety, capability
substrate, async clarity) with the lowest implementation cost. Fits
silt's "one way to do things" philosophy: a fixed vocabulary the
whole community shares.

**Option 3** opens the door to ecosystem fragmentation — every
library invents its own `!Audit`, `!Telemetry`, `!Retry`. Pay the
ergonomic cost without the consistency benefit. **Reject.**

**Option 4** is the Koka path. Buys handlers as a feature, but
silt's structured concurrency already handles most of what handlers
would buy (cancellation, scope-bounded resources). Defer.

---

## Part 5 — Recommendation

**Ship a coarse, inferred, tracking-only effect system.**

- Fixed set of ~5 named effects: `!io`, `!mut`, `!async`, `!panic`,
  `!net`.
- Effects inferred from the body. No handler machinery.
- Surfaced on LSP hover. Optional annotation at module / function
  boundaries.
- Existing user code defaults to `!*` (top: "any effect"). Strict
  checking is opt-in via `--strict-effects`. Future major version
  flips the default.

```silt
-- Inferred. Hover shows: () -> Config !{io,panic}
fn load_defaults() -> Config {
  let raw = fs.read_to_string(env.home() ++ "/.config/app.toml")
  toml.parse(raw).unwrap()
}

-- Annotated. Compiler enforces the row.
fn parse_config(raw: String) -> Result(Config, ConfigError) !{} {
  toml.parse(raw)
}
```

### Out of scope

- **Algebraic effect handlers.** Defer to v2 if and only if real
  user demand emerges. OCaml 5 evidence says we can defer
  indefinitely.
- **User-defined effects.** Same. The fixed vocabulary is the point.
- **Refinement-typed effects** (F*-style). Never. Wildly out of
  scope for silt's target audience.
- **Linear / affine resource tracking.** Separate feature, evaluate
  independently. Adjacent but orthogonal.
- **Row polymorphism in user surface.** Internal-only for inference.
  Users see concrete sets.

### Grounding in the OCaml lesson

The OCaml 5 team — researchers who understood effect typing
deeply — chose handlers without typing because the typing rollout
would have broken their existing surface. Silt's situation is
different (no two decades of code to break) but the inverse lesson
applies: silt has the runtime story already (structured
concurrency), so the typing without handlers is the half that adds
new value. The cheap half is the valuable half.

---

## Part 6 — Open decisions before implementation

### (a) Granularity finalisation

Proposed five effects, mapped onto silt's current operational
reality:

- `!io` — filesystem (`fs.*`), terminal (`io.print`, `io.read`),
  environment-variable read (`env.get`). Anything that touches the
  local machine outside silt's heap.
- `!net` — TCP (`tcp.*`), HTTP (`http.*`), Postgres (`postgres.*`),
  any client that opens a socket. Distinguished from `!io` because
  sandboxes commonly grant filesystem but deny network.
- `!async` — `task.spawn`, `channel.recv`, cooperative yield. Any
  call that requires a task scope.
- `!panic` — explicit `panic`, array-bounds error, division by
  zero, integer overflow in checked mode. Anything that aborts
  control flow without going through `Result`.
- `!mut` — mutable references, scope captures of mutable bindings.
  *Caveat:* silt has no top-level mutable state today. `!mut` only
  meaningfully applies to functions that close over mutable
  locals. Open question: is `!mut` worth a slot, or should we drop
  it and reserve four effects?

**Recommendation:** keep `!mut` reserved but unused at v1. Document
it as "future use". Five-slot vocabulary stays stable.

### (b) Default for legacy code

`!*` (top, forgiving) or `!{}` (bottom, strict)?

**Recommendation:** `!*` by default with `--strict-effects` opt-in.
Flipping to `!{}` is a future-major-version migration. Same shape as
Rust's edition mechanism.

### (c) Stdlib sweep ordering

One-shot all 400 builtins, or roll out by module priority?

**Recommendation:** one round, mechanical, follow the existing
docs-sweep template. Big PR, mechanical review, lock test catches
drift. Module-priority rollout doubles the engineering work and
leaves the inference machinery in a half-correct state for months.

### (d) Compatibility boundary

What about external / unannotated code?

**Recommendation:** treat as `!*` with a warning. Refusing to call
unannotated code would break every existing program on first
upgrade. Most languages chose `!*`-with-warning; we should too.

### (e) LSP UI

Where does the inferred effect set render in the hover popup?

**Recommendation:** between the type signature and the `---` doc
separator. Example:

```
fn load_defaults() -> Config
!{io, panic}
---
Loads the user's default config from $HOME/.config/app.toml.
```

This keeps effects visually adjacent to the signature without
crowding the doc-comment block.

---

## Part 7 — Phased implementation plan (if approved)

**Phase A — plumbing (2 weeks).** Add `EffectSet` to the internal
type representation. Inference machinery. No syntax change.
Existing programs unaffected. Tests verify inference produces
expected sets on synthetic examples.

**Phase B — annotations and hover surfacing (1-2 weeks).** Parser
accepts `!{set}` syntax in fn signatures. LSP renders inferred set
on hover. Annotation enforcement at fn boundaries. Stdlib annotated
as `!*` (compatibility default).

**Phase C — stdlib sweep (1-2 weeks).** Annotate every stdlib
builtin with its real effect set. ~400 builtins. Mechanical. Lock
test catches drift.

**Phase D — `--strict-effects` flag (1 week).** When set,
unannotated functions default to `!{}` and propagate; type checker
rejects effectful calls from pure context. Future major version
flips the default.

**Total: 5-7 weeks tracking-only.**

---

## References

- Koka language guide: https://koka-lang.github.io/koka/doc/book.html
- OCaml 5 multicore retrospective (KC Sivaramakrishnan et al.):
  https://kcsrk.info/papers/drafts/retro-concurrency.pdf
- OCaml 5 effect handlers manual: https://ocaml.org/manual/effects.html
- Unison abilities: https://www.unison-lang.org/docs/fundamentals/abilities/
- Rust async-keyword-generics RFC discussion:
  https://rust-lang.github.io/rfcs/
- Scala 3 Caprese capture checking:
  https://docs.scala-lang.org/scala3/reference/experimental/cc.html
- Daan Leijen, "Algebraic Effects for Functional Programming" (Koka
  design notes).
