---
title: "Strict-effects mode: migration guide"
section: "Guide"
status: stable
---

# `--strict-effects` migration guide

`silt` shipped Phase D of the effect-row tracking proposal in v0.13: an
opt-in mode that promotes the gradual-rollout `EffectSet::TOP`
default for unannotated user functions to `EffectSet::EMPTY` (pure)
and asks the typechecker to enforce it. The result is a
capability-boundary substrate you can wave at any module to get an
honest accounting of which functions touch the filesystem, the
network, the wall clock, the OS entropy pool, or anything else
labelled with one of silt's five tracked effects.

This guide shows you what the mode does, how to enable it, and how
to migrate an existing package.

## What strict-effects mode does

By default (and through v0.14 without the flag), every
unannotated user function carries the permissive `!*` ("any effect")
declared bound. The typechecker still INFERS the body's real effect
set — you can see it on LSP hover — but it does not enforce that the
body stays inside any particular bound, because no bound was
declared.

Strict mode flips that single bit: the absence of an annotation
means `!{}` (pure). A pure function is the strictest possible bound,
so any effectful call from the body — `io.read_file`, `time.now`,
`tcp.connect` — exceeds it and the typechecker emits an error. The
error names the inferred effect set AND ships a copy-paste-ready
annotation in its `help:` line:

```
error[type]: function 'load_settings' uses effect !{fs, io} but is declared pure (no annotation under --strict-effects)
 --> main.silt:3:61
   |
 3 | fn load_settings(path: String) -> Result(String, IoError) = io.read_file(path)
   |                                                             ^ function 'load_settings' uses effect !{fs, io} but is declared pure (no annotation under --strict-effects)
  = note: fn body uses !{fs, io}; under --strict-effects, missing annotation means !{}
  = help: annotate as `fn load_settings(path: String) -> Result(String, IoError) !{fs, io}` to make the effect explicit, or wrap the IO behind a callable passed in by the caller
```

The literal annotation in the `help:` line is the load-bearing piece:
copy it, paste it back over your existing fn header, and the error
goes away. Multiplied across a codebase, that's the "every effectful
function announces itself in its signature" property strict mode is
designed to deliver.

## How to enable it

Two surfaces, both opt-in, both off by default.

**Per CLI invocation:**

```
silt check --strict-effects main.silt
silt run --strict-effects main.silt
silt test --strict-effects
```

**Package-wide via `silt.toml`:**

```toml
[package]
name = "my_app"
version = "0.1.0"

[lints]
strict-effects = true
```

The CLI flag wins over the manifest. Manifest absent (or `[lints]`
absent) keeps the legacy permissive default.

When the LSP picks up a workspace whose `silt.toml` has the field
set, every diagnostic the server publishes runs under strict mode —
so editors will surface the strict-mode `help:` line in their
hover/diagnostic UIs without any per-invocation flag.

## Migration recipe

The whole story is a tight loop:

1. Run `silt check --strict-effects main.silt` (or for a package,
   `silt check --strict-effects` from the project root).
2. For each error the typechecker emits, copy the literal annotation
   from the `help:` line into the offending fn's signature.
3. Re-run. Repeat until clean.

For a small package the loop converges in one or two passes. For
larger codebases you can scope the rollout: enable the flag, capture
the diagnostic stream, fix one effect family at a time (`!{fs}`
first, then `!{net}`, then `!{time}`, etc.).

### Example: before/after

**Before** (`main.silt`, default mode — typechecks):

```silt
import io

fn load_settings(path: String) -> Result(String, IoError) =
  io.read_file(path)

fn main() {
  let _settings = load_settings("config.toml")
  ()
}
```

Run `silt check --strict-effects main.silt` and you'll get:

```
error[type]: function 'load_settings' uses effect !{fs, io} but is declared pure (no annotation under --strict-effects)
 --> main.silt:4:3
   |
 4 |   io.read_file(path)
   |   ^ function 'load_settings' uses effect !{fs, io} but is declared pure (no annotation under --strict-effects)
  = note: fn body uses !{fs, io}; under --strict-effects, missing annotation means !{}
  = help: annotate as `fn load_settings(path: String) -> Result(String, IoError) !{fs, io}` to make the effect explicit, or wrap the IO behind a callable passed in by the caller
```

**After** (paste the annotation, re-run, fix the next caller, repeat):

```silt
import io

fn load_settings(path: String) -> Result(String, IoError) !{fs, io} =
  io.read_file(path)

fn main() !{fs, io} {
  let _settings = load_settings("config.toml")
  ()
}
```

That's the whole migration. The first re-run after annotating
`load_settings` will surface the same diagnostic for `main`, because
`main` indirectly inherits `!{fs, io}` through its call to
`load_settings`; paste the suggested annotation onto `main` and the
file goes clean. The annotations are now the functions' contracts;
future commits that add a `tcp.connect` call to either body will fail
typecheck until the bound is widened.

## When you'd want it

- **Capability boundaries.** Library authors who want every effect
  surface to be visible at the API edge — so callers can look at a
  signature and tell a pure function from an effectful one without
  reading the body.
- **Sandbox / replay tooling.** Code that needs to deny `!net` or
  `!fs` at deployment time gets the type-level guarantee for free.
- **Reproducibility audits.** A function whose declared bound
  excludes `!time` and `!random` is reproducible-given-input. The
  type checker enforces it.
- **Large refactors.** When you're moving IO out of business logic,
  strict mode tells you exactly which functions still touch IO.

## When you wouldn't

- **Scripts and prototypes.** The friction of annotating every
  effectful function outweighs the benefit when the code is going to
  be deleted next week. Default mode is the right answer.
- **Throwaway one-shot programs.** Same reasoning.
- **Mid-refactor packages where the effect surface is in flux.**
  Annotate when the surface stabilises, not before.

A future major silt version may flip the default — that's the
explicit eventual destination called out in the proposal — but the
choice still stays with you in v0.14, per package, per invocation.

## Related material

- `docs/proposals/effect-rows.md` — the full design memo, including
  why the v1 vocabulary is exactly five effects and why silt skips
  algebraic handlers.
- The `--strict-effects` CLI surface is wired through `silt check`,
  `silt run`, and `silt test`. Watch mode (`-w`) inherits the flag.
- The LSP picks up the workspace's `silt.toml` automatically; no
  client-side config is required.
