# Silt Language Evaluation Prompt

Use this prompt to produce an independent, source-verified assessment of where silt stands.

---

You are evaluating a programming language called "silt" at /home/klaus/dev/silt from scratch. You have zero prior context.

Silt's stated design principles (from its own documentation) are:
1. Minimal keyword count as a forcing function
2. Expression-based: everything returns a value
3. Fully immutable: no mutation, shadowing allowed
4. Pattern matching as the sole branching construct
5. Explicit over implicit: errors as values, no exceptions, no null
6. Module-qualified stdlib: minimal global namespace
7. CSP concurrency: channels and tasks, no async/await
8. Hindley-Milner type inference

Your task: Critically evaluate silt against these principles. For each principle, assess whether the implementation actually delivers on the promise. Find contradictions, gaps where the implementation undermines the principle, and places where the principle produces genuine elegance.

Also evaluate:
- Implementation quality (read the Rust source — architecture, code clarity, test coverage)
- Practical usability (read the 10 programs in programs/ — do they feel natural or forced?)
- The type system (does it catch real bugs? where is it lenient?)
- The stdlib (is it minimal yet expressive? anything missing that should exist? anything present that shouldn't?)
- Documentation honesty (do the docs match reality?)

Read actual source code, not just docs. Check claims by examining the implementation. Run cargo test if you can.

Produce a structured report: strengths, weaknesses, contradictions, and an overall assessment. Be specific with file paths and line numbers. Rate it on a scale of 1-10 for production readiness and explain why.

## Methodology guidance

### What makes this evaluation fair

- **Verify claims against source code.** Don't trust docs alone. The lexer keyword list, the interpreter's `eval_binary`, the typechecker's exhaustiveness checker, the `is_truthy` function, the `Env::define` method — these are where principles are either upheld or violated.
- **Run the tests.** `~/.cargo/bin/cargo test` (or `cargo test`). Note the count, pass/fail, and what categories they cover.
- **Read the programs, not just the examples.** The 10 programs in `programs/` are the real stress test. The 9 files in `examples/` are curated showcases. Friction shows up in the larger programs.
- **Cross-reference documents.** The repo contains multiple markdown files that were written at different times. Check whether they agree on basic facts (keyword count, global count, module count, LoC, test count).
- **Distinguish language-level from implementation-level.** "Fully immutable" may be true at the language level while the Rust implementation uses `RefCell`. That's normal for interpreters — call it out but don't overweight it.

### What to look for specifically

**In the lexer (`src/lexer.rs`):**
- Count the actual keywords. Do they match the claimed 14?
- How are `true`/`false` handled — keywords or literals?
- Number parsing edge cases (overflow?)

**In the parser (`src/parser.rs`):**
- How does `expect()` work? Does it check values or just discriminants?
- How is trailing closure disambiguation handled?

**In the interpreter (`src/interpreter.rs`):**
- How does `eval_binary` handle mixed int/float arithmetic?
- What does `is_truthy` do? Which values are implicitly falsy?
- How are builtins dispatched? How large is the match tree?
- Does TCO actually work (look for the trampoline loop)?
- How does the cooperative scheduler yield?

**In the typechecker (`src/typechecker.rs`):**
- Is exhaustiveness checking shallow or deep (nested patterns, tuples)?
- Are type errors advisory or do they halt execution?
- How are builtin types registered?

**In `src/value.rs`:**
- How is `Eq` implemented for `Value` containing `f64`?
- What happens with closures in `Hash`?
- How do channels work (capacity-0 promotion)?

**In `src/env.rs`:**
- Is `define()` mutation? What does that mean for "fully immutable"?

**In `src/scheduler.rs`:**
- Are `BlockedSend`/`BlockedReceive` states actually used?

### Known areas of interest from prior evaluations

Previous evaluations have identified these as areas worth verifying (they may have been fixed since):
- Implicit int/float coercion in arithmetic (check `eval_binary`)
- Cross-type comparison silently returning false (check the catch-all in `eval_binary`)
- `is_truthy` implementing JavaScript-style truthiness (0, "", Unit, None are falsy)
- Parser accepting any identifier where `for` is expected in trait impls
- `list.get` with negative indices wrapping via `i64 as usize`
- Double "panic:" prefix in panic error messages
- Stale documentation with inconsistent metrics across files

### Parallel exploration strategy

This evaluation benefits from parallel investigation:
1. **Source analysis** — Read all 14 .rs files in src/ plus tests/
2. **Programs analysis** — Read all 10 programs in programs/ and 9 in examples/
3. **Documentation analysis** — Read all .md files and cross-reference claims
4. **Test execution** — Run `cargo test` and note results

These four workstreams are independent and can run concurrently.

### Report structure

```
# Silt Language Evaluation Report

## Test Results
(pass/fail counts, categories)

## Principle-by-Principle Assessment
(for each of the 8 principles: score /10, evidence, contradictions)

## Implementation Quality
- Architecture
- Code clarity
- Test coverage
- Specific bugs found (with file:line)

## Practical Usability
(per-program ratings, recurring friction patterns)

## Type System Assessment
(what it catches, what it misses, soundness gaps)

## Standard Library Assessment
(completeness, missing pieces, questionable inclusions)

## Documentation Honesty
(claims vs reality, cross-document consistency)

## Contradictions Summary
(table: contradiction, severity, evidence)

## Production Readiness: X/10
(what would raise/lower the score)

## Verdict
(2-3 paragraph summary)
```
