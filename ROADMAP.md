# Silt Roadmap: Closing Gaps and Elevating the Language

## Context

The EVALUATION.md report scored silt 5/10 for production readiness. The language's design principles are coherent, but the implementation has soundness gaps (type system vs runtime disagreements), architectural bottlenecks (monolithic builtins), stdlib holes (no math module), and tooling gaps (no `silt check`, no error recovery). This roadmap organizes fixes into phases.

## Decisions (Resolved)

1. **Numeric tower** → **A: Remove implicit coercion from interpreter.** Delete the 10 mixed-arithmetic match arms in `eval_binary`. Users write `int.to_float(3) + 2.5`. Aligns with "explicit over implicit."
2. **Cross-type comparison** → **A: Restrict all comparisons to same-type at runtime.** Replace wildcard match arms with typed arms. Cross-type comparison → runtime error. Type safety is non-negotiable for a statically typed language.
3. **Builtin architecture** → **C: Split the match into per-module functions.** Mechanical refactoring, low risk, immediate readability improvement.
4. **Concurrency** → **A: Stay single-threaded, add preemptive yielding.** Step counter in interpreter, yield every N evaluations.
5. **Exhaustiveness** → **A: Maranget usefulness algorithm.** Standard PL algorithm, handles arbitrary pattern nesting.

---

## Phase 1: Correctness (5/10 → 6/10)

### 1.1 Remove implicit int/float coercion — S effort

**Files:**
- `src/interpreter.rs:2550-2560` — delete the 10 mixed-arithmetic match arms (`Int + Float`, `Float + Int`, etc.)
- `tests/integration.rs` — update `test_mixed_int_float_add`, `test_mixed_float_int_sub`, `test_mixed_int_float_div` to assert these produce runtime errors instead of float results
- Add a clear error message: `"cannot mix Int and Float in arithmetic; use int.to_float() or float.to_int() to convert explicitly"`

### 1.2 Restrict comparisons to same-type — S effort

**Files:**
- `src/interpreter.rs:2568-2573` — replace the 6 wildcard comparison arms (`BinOp::Eq`, `Neq`, `Lt`, `Gt`, `Leq`, `Geq`) with explicitly typed arms:
  - `(Int, Eq, Int)`, `(Float, Eq, Float)`, `(Bool, Eq, Bool)`, `(String, Eq, String)`, `(List, Eq, List)`, `(Tuple, Eq, Tuple)`, `(Variant, Eq, Variant)`, `(Record, Eq, Record)`, `(Map, Eq, Map)`, `(Unit, Eq, Unit)`, `(Channel, Eq, Channel)` → use existing `PartialEq`
  - Same for `Neq`, `Lt`, `Gt`, `Leq`, `Geq`
  - Default arm → `Err(err("cannot compare {type1} with {type2}"))`
- `src/value.rs:276-327` — consider removing the blanket `Ord` impl or restricting it. Note: `BTreeMap<Value, Value>` requires `Ord`, so keep it but document it's for internal use only (map key ordering), not for user-facing semantics.
- `tests/integration.rs` — add tests: `5 == "hello"` → runtime error, `3 < true` → runtime error

### 1.3 Add `silt check` command — S effort

**File:** `src/main.rs`
- Add a `"check"` arm to the subcommand match
- Run `Lexer::new → tokenize → Parser::new → parse_program → TypeChecker::new → check_program`
- Report type errors/warnings using existing `SourceError` formatting
- Exit with code 0 if no errors, 1 if errors
- Accept file arguments same as `run`

### 1.4 Runtime errors with source locations — M effort

**Files:**
- `src/interpreter.rs` — the `eval_inner` method has access to `expr.span` but `RuntimeError::Error(String)` discards it. Change to:
  - Add `RuntimeError::Error(String, Option<Span>)` or add a `span` field
  - In `eval_inner`, when constructing errors, include `expr.span`
  - In `call_closure`, maintain a lightweight call stack: `Vec<(String, Span)>` pushed on function entry, popped on exit
- `src/errors.rs` — extend `SourceError::from_runtime` to accept the span and produce a formatted error with source line context
- Goal: runtime errors show `file:line:col` and the offending source line, plus a call stack for function calls

---

## Phase 2: Usability & Stdlib (6/10 → 7/10)

### 2.1 Split dispatch_builtin into per-module functions — M effort

**File:** `src/interpreter.rs:747-2670`
- Extract match arms into functions: `dispatch_io(name, args)`, `dispatch_list(name, args)`, `dispatch_string(name, args)`, `dispatch_int(name, args)`, `dispatch_float(name, args)`, `dispatch_map(name, args)`, `dispatch_result(name, args)`, `dispatch_option(name, args)`, `dispatch_regex(name, args)`, `dispatch_json(name, args)`, `dispatch_test(name, args)`
- The top-level `dispatch_builtin` becomes a ~30-line router: `name.split_once('.') → Some(("list", rest)) => self.dispatch_list(rest, args)`
- Globals (`print`, `println`, `panic`, `try`) stay in the top-level match
- Similarly split the typechecker's `register_builtins` into per-module registration functions
- No behavior changes — pure mechanical refactoring

### 2.2 Add math module — S-M effort

**Files:** `src/interpreter.rs` (new dispatch_math), `src/typechecker.rs` (register_math_builtins), `src/module.rs` (add "math" to BUILTIN_MODULES)

Functions to add (all thin wrappers over Rust's `f64` methods):
- `math.sqrt(Float) -> Float`
- `math.pow(Float, Float) -> Float`
- `math.log(Float) -> Float` (natural log)
- `math.log10(Float) -> Float`
- `math.sin(Float) -> Float`, `math.cos(Float) -> Float`, `math.tan(Float) -> Float`
- `math.asin`, `math.acos`, `math.atan`, `math.atan2`
- `math.pi -> Float`, `math.e -> Float` (constants, registered as values not functions)
- `math.abs(Float) -> Float` (already exists as `float.abs`, consider aliasing)

### 2.3 Add map.filter, map.map, map.entries — S effort

**Files:** `src/interpreter.rs`, `src/typechecker.rs`
- `map.filter(m, fn(k, v) -> Bool) -> Map` — iterate BTreeMap, keep entries where predicate returns true
- `map.map(m, fn(k, v) -> (k2, v2)) -> Map` — transform entries
- `map.entries(m) -> List((k, v))` — convert to list of tuples
- `map.from_entries(List((k, v))) -> Map` — inverse of entries

### 2.4 Add list.group_by — S effort

- `list.group_by(xs, fn(x) -> key) -> Map(key, List(x))` — group list elements by key function
- Programs in `programs/` currently do this manually with fold

### 2.5 Improve REPL — M effort

**File:** `src/repl.rs`, `Cargo.toml`
- Add `rustyline` dependency for line editing, history, Ctrl-R search
- Tab completion using `Env::bindings_with_prefix` (already exists at `env.rs:53`)
- Add `:help`, `:env` (show bindings), `:type <expr>` (show inferred type) commands

---

## Phase 3: Type System (7/10 → 8/10)

### 3.1 Maranget exhaustiveness checking — L effort

**File:** `src/typechecker.rs` — replace `check_exhaustiveness` (lines 3143-3279)

Implement the usefulness-based algorithm from "Warnings for pattern matching" (Maranget, JFP 2007):
- Represent patterns as a pattern matrix
- Recursively check if the wildcard pattern is "useful" given the existing arms
- Handles: nested constructors, tuples, or-patterns, wildcards, guards (conservatively)
- The `Pattern` enum in `ast.rs` maps cleanly to the algorithm's constructors
- ~200-400 lines of new code replacing ~140 lines of ad-hoc checking
- Add tests for: uncovered nested enum variants, uncovered tuple combinations, missing bool in nested position

### 3.2 Fix trait dispatch — proper receiver type validation — L effort

**File:** `src/typechecker.rs:2424-2486`
- Currently: `value.method()` is looked up as `"TypeName.method"` string with no verification that `value`'s type matches `TypeName`
- Fix: When typechecking a field access that resolves to a trait method:
  1. Infer the type of the receiver
  2. Resolve it to a concrete type name
  3. Look up `trait_impls` to verify this type has an impl containing this method
  4. If not found, emit a type error instead of returning `fresh_var()`
- Also fix the `fresh_var` fallback at line 2484: unknown field/method access should be `Type::Error` + a diagnostic, not a silently accepted type variable

### 3.3 Fix fresh_var fallback — S effort (quick win)

**File:** `src/typechecker.rs:2484`
- Replace `self.fresh_var()` with `self.error("unknown field or method '{field}'", span); Type::Error`
- Can be done immediately as a standalone fix before the full trait dispatch rework (3.2)

---

## Phase 4: Performance (8/10 → 9/10)

### 4.1 List COW semantics with Rc::make_mut — S-M effort

**File:** `src/value.rs`, `src/interpreter.rs` (all list builtin implementations)
- Change list mutation pattern from `let mut new = (**xs).clone(); new.push(v); Rc::new(new)` to `Rc::make_mut(&mut xs).push(v)` where the list has a single owner
- This requires changing `Value::List(Rc<Vec<Value>>)` to store `Rc` mutably in the operations — the key insight is that `Rc::make_mut` only clones if refcount > 1
- Eliminates O(n²) for the common pattern of building a list via repeated append in a loop

### 4.2 Environment flattening — M effort

**File:** `src/env.rs`
- When creating a child env, copy all parent bindings into the child's HashMap
- Makes `get()` O(1) instead of O(depth)
- Makes `child()` O(n) where n = total bindings in scope — acceptable since lookups vastly outnumber scope creations

### 4.3 Preemptive yielding — M effort

**Files:** `src/interpreter.rs`, `src/scheduler.rs`
- Add a step counter to the interpreter (increment on each `eval_inner` call)
- After every N evaluations (e.g., 1000), check if there are pending tasks in the scheduler
- If so, yield: save current task state, pick next ready task, resume it
- This gives fair scheduling for CPU-bound tasks without any Rc→Arc migration

### 4.4 True rendezvous channels — S-M effort (after 4.3)

**File:** `src/value.rs:66-70`, `src/scheduler.rs`
- With preemptive yielding (4.3), a sender on a capacity-0 channel can be suspended mid-execution
- Remove the `if capacity == 0 { 1 }` promotion at `value.rs:70`
- Update scheduler to handle rendezvous: sender blocks until receiver is ready, and vice versa

---

## Phase 5: Tooling

### 5.1 Comment-preserving formatter — M-L effort

**Files:** `src/lexer.rs`, `src/formatter.rs`
- Add `Comment(String)` token variant to lexer (currently comments are stripped)
- Formatter reads from both AST and parallel token stream to place comments
- "Trivia" approach (like rustfmt): comments attach to adjacent tokens

### 5.2 Parser error recovery — M effort

**File:** `src/parser.rs`
- Change from `Result<T, ParseError>` to collecting errors in `Vec<ParseError>`
- Synchronize at statement boundaries: on error, skip tokens until next `let`, `fn`, `type`, etc.
- Return partial AST with `ExprKind::Error` nodes (similar to `Type::Error` in typechecker)
- Multiple errors reported in one pass

### 5.3 Machine-readable diagnostics — S effort (after 1.3)

- Add `--format json` flag to `silt check`
- Output errors as JSON objects: `{"file": "...", "line": N, "col": N, "message": "...", "severity": "error|warning"}`
- First step toward LSP support

---

## Verification

After each phase, run `cargo test` (323 tests must still pass, with updates for changed semantics).

**Phase 1:**
- 1.1: Update integration tests — `test_mixed_int_float_*` should assert runtime errors
- 1.2: Add tests for cross-type comparison errors (`5 == "hello"` → error)
- 1.3: Run `silt check` on all `programs/*.silt` — verify output
- 1.4: Trigger a division by zero and verify file:line:col appears in error

**Phase 2:**
- 2.1: All 323 existing tests still pass (no behavior change)
- 2.2: Add tests for `math.sqrt(4.0) == 2.0`, `math.pow(2.0, 10.0) == 1024.0`, etc.
- 2.3: Add tests for `map.filter`, `map.map`, `map.entries`
- 2.5: Manual REPL testing — history persistence, tab completion

**Phase 3:**
- 3.1: Add tests for uncovered nested patterns, tuple exhaustiveness, or-pattern coverage
- 3.2: Add tests for calling nonexistent trait methods → type error
- 3.3: Add test for `value.nonexistent_field` → type error (not silently accepted)

**Phase 4:**
- 4.1: Benchmark list.append in a loop (1000 iterations) — should be sub-linear
- 4.3: Spawn two CPU-bound tasks, verify both make progress (not starved)
- 4.4: Test capacity-0 channel blocks sender until receiver is ready
