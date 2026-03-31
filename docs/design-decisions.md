# Design Decisions & Architecture

> Architect's notes on why Silt is designed the way it is.
> This is the honest version -- trade-offs included.

---

## 1. Language Philosophy

### "14 keywords"

Silt has exactly 14 keywords:

```
as  else  fn  import  let  loop  match  mod
pub  return  trait  type  when  where
```

This is not an arbitrary constraint -- it is a forcing function. Every time we
considered adding a keyword (`if`, `for`, `while`, `mut`, `async`, `await`,
`try`, `catch`, `throw`...), we asked: "Can an existing construct handle this?"
The answer was almost always yes.

`if`/`else` is subsumed by `match`. General-purpose iteration uses `loop`
(an expression that binds state and re-enters via `loop(new_values)`), while
collection traversal uses higher-order functions (`list.map`, `list.filter`,
`list.fold`, `list.each`). `mut` doesn't exist because nothing is mutable.
`async`/`await` doesn't exist because concurrency is CSP-based. `try`/`catch`
doesn't exist because errors are values (`try` is a global builtin function,
not a keyword).

We originally had `chan`, `send`, `receive`, `spawn`, and `select` as keywords
(17 total). These were all demoted to module-qualified functions (`channel.new`,
`channel.send`, `channel.receive`, `task.spawn`, `channel.select`) to keep the
global namespace clean and avoid the PHP problem of too many bare globals. The
CSP interface is the same; it just lives in modules now. `select` was the last
to go -- it was replaced by `channel.select([ch1, ch2])` which returns a
`(channel, value)` tuple, used with `match` and the `^` pin operator.

The constraint is practical, not aesthetic. Fewer keywords means fewer
concepts to learn, fewer ways to express the same thing, and fewer ambiguities
in the grammar. A language with 14 keywords fits in working memory.

Compare: Rust has ~40 keywords (plus reserved ones). Go has 25. Python has 35.
The smallest useful languages cluster around 15-25 keywords. We aimed for the
low end and got there.

What is _not_ a keyword matters too. `true`, `false` are builtin literals.
`Ok`, `Err`, `Some`, `None` are builtin variant constructors, not keywords --
they are ordinary values that happen to be defined in the prelude. `_` is a
wildcard pattern token, not a keyword. `try` is a builtin function, not a
keyword. This keeps the keyword count honest and means these names live in
the value namespace, not the syntax.

The global namespace is deliberately minimal: only 10 names (`print`, `println`,
`panic`, `try`, `Ok`, `Err`, `Some`, `None`, `Stop`, `Continue`). Everything
else requires module qualification. This avoids the "PHP problem" where hundreds of functions are
dumped into the global scope, making it unclear where anything comes from and
creating name collision risks.

### Expression-based: everything returns a value

Every construct in Silt is an expression. A `match` returns a value. A block
returns the value of its last expression. A function body is an expression.
There are no "statements" in the grammar -- `let` bindings and `when` guards
are statement-level forms inside blocks, but even blocks are expressions.

```
-- match is an expression
let description = match shape {
  Circle(r) -> "circle with radius {r}"
  Rect(w, h) -> "rect {w}x{h}"
}

-- blocks are expressions (last expression is the value)
let result = {
  let x = compute()
  let y = transform(x)
  x + y
}
```

This eliminates the statement/expression split that plagues languages like
JavaScript (ternary operator exists solely because `if` is a statement) and
C++ (the comma operator, the ternary, statement-expressions as a GCC
extension). In Silt, you never need a special syntax to "get a value out of"
a branching construct.

The trade-off: there is no `void` return. Functions that exist only for side
effects return `()` (Unit), which is a value. This is a non-issue in practice
but can surprise newcomers from statement-oriented languages.

### Immutability as default (and only option)

All bindings in Silt are immutable. There is no `mut` keyword, no mutable
references, no assignment operator that writes to an existing binding.
Rebinding (shadowing) is allowed:

```
let x = 42
let x = x + 1    -- shadowing, not mutation
```

This is the same model as Haskell and Erlang. The `let x = x + 1` creates a
new binding that shadows the old one in the current scope. The old value is
unchanged.

Why no mutation at all? Three reasons:

1. **Concurrency safety.** Silt's CSP concurrency model spawns tasks that
   share the same environment (via `Rc`). If values were mutable, we'd need
   either locking or isolation. Immutability gives us safety for free.

2. **Simpler reasoning.** If a value never changes after creation, you can
   always substitute it at its definition site. Functions are referentially
   transparent unless they do I/O. This makes code easier to follow.

3. **Implementation simplicity.** The interpreter uses `Rc<T>` for shared
   values. No `RefCell` on user-facing values, no borrow checker needed at
   the language level, no aliasing concerns.

The trade-off is real: algorithms that are naturally expressed with mutation
(in-place sorting, graph traversal with visited sets, accumulators) require
either recursion or functional combinators. This is more verbose for some
problems. We accept this cost.

Record update syntax (`user.{ age: 31 }`) is the mitigation. It looks like
mutation but always returns a new value. See Section 7.

### Explicit over implicit

Silt has no:

- **Exceptions.** Error handling uses `Result`/`Option` values. Errors are
  visible in types and must be handled at the call site.
- **Null.** The absence of a value is `None`, a variant of `Option(a)`.
  You cannot forget to check for it because the type system requires it.
- **Implicit conversions.** `1 + 1.0` is a type error. You must convert
  explicitly. (There are a few exceptions for `Int * Float` mixed
  multiplication, which was a pragmatic concession.)
- **Implicit returns from error paths.** The `?` operator is explicit syntax.
  `when-else` requires an explicit diverging `else` block.

The philosophy is borrowed from Rust and Go: make the cost of operations
visible. If a function can fail, its type says so. If a value might be absent,
its type says so. If control flow can exit early, the syntax says so.

---

## 2. Pattern Matching as the Only Branching Construct

### Why no if/else

Silt has no `if` keyword. All branching is done through `match`:

```
fn classify(n) {
  match n {
    0 -> "zero"
    x when x > 0 -> "positive"
    _ -> "negative"
  }
}
```

This is a deliberate choice. `if`/`else` is a special case of `match` over
booleans. By having only one branching construct, we get:

1. **Exhaustiveness checking everywhere.** Every branch point is checked by
   the type checker for completeness. Missing a variant of an enum? The type
   checker warns you. Missing `true` or `false`? Same.

2. **Uniform destructuring.** Pattern matching lets you branch and bind in a
   single operation. `if let` (Rust), `if case` (Swift), `instanceof`
   narrowing (TypeScript) are all ad-hoc solutions to the same problem that
   `match` handles uniformly.

3. **No boolean blindness.** In languages with `if`, you often write
   `if result.is_ok()` and then must unsafely `unwrap()`. With match, the
   successful branch has the value already bound:

```
match parse_int(input) {
  Ok(n) -> use(n)       -- n is bound, type-narrowed
  Err(e) -> handle(e)   -- e is bound
}
```

The trade-off: simple boolean checks are more verbose.

```
-- In a language with if/else:
if debug { print("debug info") }

-- In Silt:
match debug {
  true -> print("debug info")
  false -> ()
}
```

Three extra lines for a trivial case. We accept this because the uniform
approach catches more bugs. In practice, pure boolean checks are less common
than you'd expect -- most "if" in real code is actually a type test or a
null check, which match handles more naturally.

### Guards

Guards (`when` in a match arm) handle the cases that pure pattern matching
cannot:

```
match n {
  0 -> "zero"
  x when x > 0 -> "positive"
  _ -> "negative"
}
```

Guards are checked after the pattern matches, and the pattern's bindings are
available in the guard expression. This is the same model as Erlang and
Haskell.

### `when-else` for inline assertions

The `when` statement is Silt's answer to Swift's `guard let` and Rust's
`let ... else`. It asserts a pattern match and binds on success, or
diverges on failure:

```
fn process(input) {
  when Ok(value) = parse(input) else {
    return Err("parse failed")
  }
  -- value is bound here, type-narrowed
  use(value)
}
```

The `else` block **must** diverge (return or panic). This is enforced at
runtime. The type checker uses `when` for type narrowing -- after a
successful `when Ok(value) = expr`, the type of `value` is the unwrapped
inner type.

This addresses the early-return pattern that is awkward with `match`:

```
-- Without when-else, you'd need nested matches:
fn process(input) {
  match parse(input) {
    Ok(value) -> {
      match find_user(value) {
        Some(user) -> {
          match user.role {
            Admin(perms) -> do_admin_thing(user, perms)
            _ -> Err("unauthorized")
          }
        }
        None -> Err("not found")
      }
    }
    Err(_) -> Err("parse failed")
  }
}

-- With when-else, the same logic is flat:
fn process(input) {
  when Ok(value) = parse(input) else { return Err("parse failed") }
  when Some(user) = find_user(value) else { return Err("not found") }
  when Admin(perms) = user.role else { return Err("unauthorized") }
  do_admin_thing(user, perms)
}
```

The flat version reads top-to-bottom, which is how humans think about
sequential validation. The nested version reads inside-out, which is the
"staircase of doom" pattern.

### List patterns

Pattern matching extends to lists with three forms: `[]` (empty), `[a, b, c]`
(exact length), and `[head, ..tail]` (head/tail destructuring). The `..`
syntax binds remaining elements as a new list, enabling recursive list
processing in the same style as Haskell or Erlang:

```
fn sum(xs) {
  match xs {
    [] -> 0
    [head, ..tail] -> head + sum(tail)
  }
}
```

The `..` was chosen over alternatives like `...` (JavaScript) or `|`
(Haskell/Erlang) to stay consistent with the existing record rest syntax
(`User { name, .. }`). Without `..`, a list pattern requires an exact length
match -- `[a, b]` matches only two-element lists. Nested patterns work inside
list elements (`[Some(x), ..rest]`), giving full compositional power.

---

## 3. The Type System

### Hindley-Milner: why full inference matters

Silt uses Hindley-Milner (HM) type inference. This means:

- **No type annotations required.** The type checker can infer the type of
  every expression, every function parameter, every return value. Annotations
  are optional documentation, never mandatory.
- **Principal types.** Every expression has a _most general_ type. The
  inference algorithm finds it. There is no ambiguity.
- **Decidable.** Type checking always terminates. No Turing-complete type
  computation.

```
-- No annotations needed; the type checker infers:
-- add: (Int, Int) -> Int
fn add(a, b) {
  a + b
}

-- identity: (a) -> a  (polymorphic)
fn identity(x) = x
```

Compare to TypeScript, where type inference is powerful but not complete --
function parameters often need annotations. Or to Go, where types are inferred
for locals (`x := 42`) but not for function signatures. HM gives you both.

The implementation follows Algorithm W, the classic HM inference algorithm:

1. **Fresh type variables.** Each unknown type gets a fresh variable (`?0`,
   `?1`, ...). The `TypeChecker` struct maintains a `next_var` counter.
2. **Unification.** When two types must be equal (e.g., both sides of `+`),
   we unify them. Unification either succeeds (binding type variables) or
   fails (type error). The `unify` method handles all type constructors.
3. **Substitution.** The `apply` method walks the substitution chain to find
   the most resolved type for any variable.
4. **Occurs check.** Before binding a variable, we check that it doesn't
   occur in the type being bound (no infinite types).

The substitution is stored as a flat `Vec<Option<Type>>` indexed by type
variable id. This is simpler than a union-find but sufficient for our scale.

### Let-polymorphism

HM's key feature is let-polymorphism: bindings introduced with `let` (or
top-level `fn`) can be used at multiple types:

```
fn identity(x) = x

fn main() {
  let a = identity(42)        -- identity used at Int -> Int
  let b = identity("hello")   -- identity used at String -> String
}
```

This works through **generalization** and **instantiation**:

- At a `let` binding, the type checker _generalizes_ free type variables that
  don't appear in the enclosing environment. `identity` gets the scheme
  `forall a. (a) -> a`.
- At each _use_ of the binding, the scheme is _instantiated_ with fresh
  variables. So the first call gets `(?5) -> ?5`, the second gets
  `(?6) -> ?6`, and they unify independently.

This is the `Scheme` type in the code: `{ vars: Vec<TyVar>, ty: Type }`.
The `generalize` and `instantiate` methods implement the two directions.

### Type errors block execution

The type checker runs before the interpreter and blocks execution on errors:

```rust
// main.rs
let type_errors = typechecker::check(&program);
for err in &type_errors {
    eprintln!("{path}:{err}");
}
if type_errors.iter().any(|e| e.is_error()) {
    std::process::exit(1);
}
```

Type **errors** (mismatched types, non-exhaustive matches, trait contract
violations) are fatal -- the program does not run. Type **warnings**
(unused variables, unreachable patterns) are displayed but allow execution
to continue. This gives the type checker real teeth: a program that
type-checks is guaranteed free of the errors the checker covers.

Trait contract enforcement is part of this pass. When a type implements a
trait, the type checker validates that every method declared in the trait is
present in the implementation and that arities match. Missing methods or
arity mismatches produce type errors that block execution.

### What we deliberately left out

- **Higher-kinded types.** No `Functor`, no `Monad`. This keeps the type
  system at HM-level complexity. You can't abstract over type constructors
  (`List`, `Option`, `Result`) -- you write separate `map` functions for
  each. This is the same limitation as Go and Elm.
- **Associated types.** Traits have methods but no associated types. This
  means you can't write a `Collection` trait with an `Item` type. It also
  means traits are simpler to implement and understand.
- **GADTs, type families, dependent types.** Not even on the roadmap. The
  type system is intentionally kept at a level where inference is complete
  and decidable.

---

## 4. Error Handling Without Exceptions

### Result/Option instead of try/catch

Silt has no exception mechanism. Functions that can fail return
`Result(value, error)`. Values that might be absent are `Option(value)`.

```
fn parse_int(s: String) -> Result(Int, String) {
  -- ...
}

fn find_user(id) -> Option(User) {
  -- ...
}
```

This is the Rust/Haskell/OCaml model, not the Java/Python/JavaScript model.
The key difference: errors are **values**. They live in the type signature.
Callers must handle them.

```
-- You can't ignore the error:
let n = parse_int(input)    -- n is Result(Int, String), not Int

-- You must unwrap explicitly:
match parse_int(input) {
  Ok(n) -> use(n)
  Err(e) -> handle(e)
}
```

Compare to exceptions, where any function can throw and the type signature
doesn't tell you. Java tried checked exceptions and they were widely reviled
(but the concept was right -- the implementation was wrong). Silt's approach
is checked exceptions done correctly: the "checkedness" lives in the return
type, not in a separate annotation system.

### The `?` operator

For the common case of "propagate the error to the caller," the `?` operator
provides ergonomic sugar:

```
fn process(input) {
  let n = parse_int(input)?      -- returns Err early if parse fails
  let result = validate(n)?       -- returns Err early if validation fails
  Ok(result * 2)
}
```

The `?` operator desugars to: if the value is `Ok(v)` or `Some(v)`, unwrap
to `v`. If it is `Err(e)` or `None`, immediately return it from the current
function. In the interpreter, this is implemented via `RuntimeError::Return`:

```rust
ExprKind::QuestionMark(expr) => {
    let val = self.eval(expr, env)?;
    match &val {
        Value::Variant(name, fields) if name == "Ok" && fields.len() == 1 => {
            Ok(fields[0].clone())
        }
        Value::Variant(name, fields) if name == "Some" && fields.len() == 1 => {
            Ok(fields[0].clone())
        }
        Value::Variant(name, _) if name == "Err" || name == "None" => {
            Err(RuntimeError::Return(val))
        }
        _ => Err(err("? operator requires Result or Option")),
    }
}
```

This is the same design as Rust's `?` operator, and it works for both
`Result` and `Option`. The trick is that `RuntimeError::Return` is caught at
function call boundaries (in `call_closure`), so the early return propagates
to the caller, not to the top level.

### `when-else` for inline guards

See Section 2. `when-else` is the complement to `?`: it handles the cases
where you want custom error handling, destructuring, or type narrowing that
goes beyond simple `Result`/`Option` propagation.

### Why this matters

Errors as values give you:

- **Composability.** `map_ok`, `unwrap_or`, and pipes work on error values
  the same as any other value.
- **Visibility.** You can see in a function's signature whether it can fail.
- **No hidden control flow.** There is no invisible stack unwinding. A `?`
  is visible in the source. A `when-else` is visible. Errors don't teleport
  through the call stack.

The trade-off: more boilerplate for simple cases. In Python, you write
`open(file)` and let the exception propagate. In Silt, you write
`io.read_file(file)?` and the `?` is mandatory. Two extra characters, but
those two characters tell every reader that this call can fail and the error
goes to the caller. We think that trade-off is worth it.

---

## 5. Cooperative CSP Concurrency

### Why CSP over async/await

CSP (Communicating Sequential Processes) uses channels and message passing
for concurrency. Tasks communicate by sending and receiving values on
typed channels:

```
let ch = channel.new()
let handle = task.spawn(fn() {
  let result = do_work()
  channel.send(ch, result)
})
let msg = channel.receive(ch)
```

Compare to async/await:

```javascript
// JavaScript
async function process() {
  const result = await doWork();
  return result;
}
```

CSP avoids the "colored function" problem. In async/await languages, async
functions can only be called from other async functions. This creates a viral
annotation (`async` must propagate up the call stack) and a split world
(sync code and async code are different). In Silt, any function can `spawn`,
`send`, or `receive`. There is no function coloring.

Go proved that CSP scales to real applications. Erlang proved it works for
high-reliability systems. We chose the same model because it is simpler to
reason about: tasks are independent, they communicate through explicit
channels, and the communication points are visible in the code.

### Why cooperative over preemptive

Silt's concurrency is cooperative, not preemptive. Tasks yield control at
explicit points: `channel.send`, `channel.receive`, `channel.select`, and
`task.join`. Between those
points, a task runs to completion without interruption.

This was chosen for implementation simplicity and determinism:

1. **Single-threaded.** The entire runtime is single-threaded. The
   `Scheduler` struct is not `Send` or `Sync`. Values use `Rc` (not `Arc`)
   and `RefCell` (not `Mutex`). This eliminates all thread-safety overhead.

2. **Deterministic scheduling.** Tasks are run in FIFO order from the ready
   queue. The same program with the same inputs produces the same interleaving.
   This makes debugging and testing much easier.

3. **No thread-safety concerns for values.** Because only one task runs at a
   time, there are no data races. The `Channel` uses `RefCell` for its buffer,
   which would panic with actual parallelism, but is safe in a cooperative
   model.

The scheduler implements a simple round-robin loop:

```rust
pub fn take_ready_tasks(&mut self) -> Vec<Task> {
    // Drain all Ready tasks for execution
    let mut ready = Vec::new();
    let mut remaining = Vec::new();
    for task in self.tasks.drain(..) {
        if task.state == TaskState::Ready {
            ready.push(task);
        } else {
            remaining.push(task);
        }
    }
    self.tasks = remaining;
    ready
}
```

When a task blocks on a channel, it transitions to `BlockedSend` or
`BlockedReceive`. The `try_unblock` method checks if blocked tasks can be
woken up (e.g., the channel now has data or capacity).

### Trade-offs

The cooperative, single-threaded model has real limitations:

- **No true parallelism.** A CPU-bound task blocks everything. You cannot
  utilize multiple cores.
- **Long computations block other tasks.** If a task does heavy computation
  between yield points, other tasks starve.
- **Deadlock detection is heuristic.** The interpreter detects deadlocks by
  checking if no tasks can make progress (`run_pending_tasks_once` returns
  false). This is sufficient but not sophisticated.

These are acceptable for v1. Real parallelism (OS threads, work-stealing) is
a v2 consideration. The CSP interface (`channel.new`, `channel.send`,
`channel.receive`, `task.spawn`) is designed to be forward-compatible: the
user-facing API works identically whether the runtime is cooperative or
preemptive.

---

## 6. The Pipe Operator & Trailing Closures

### Pipes: eliminate nesting, read left-to-right

The pipe operator `|>` passes the left side as the first argument to the
right side:

```
let result =
  [1, 2, 3, 4, 5]
  |> list.filter { x -> x > 2 }
  |> list.map { x -> x * 10 }
  |> list.fold(0) { acc, x -> acc + x }
```

Without pipes, this would be:

```
let result = list.fold(list.map(list.filter([1, 2, 3, 4, 5], fn(x) { x > 2 }), fn(x) { x * 10 }), 0, fn(acc, x) { acc + x })
```

The pipe version reads left-to-right, top-to-bottom, in the order that
operations happen. The nested version reads inside-out, in the reverse order.
For data processing pipelines, pipes are strictly better.

The design is borrowed from Elixir (which got it from F#, which got it from
OCaml). The choice to pipe into the **first** argument (not the last) matches
Elixir's convention and works well with collection functions like `list.map(xs,
fn)` where the collection is the natural first argument.

### Trailing closures: lightweight lambda syntax

When the last argument to a function is a closure, it can be written outside
the parentheses:

```
-- These are equivalent:
[1, 2, 3] |> list.map(fn(x) { x * 2 })
[1, 2, 3] |> list.map { x -> x * 2 }
```

The trailing closure syntax `{ params -> body }` is lighter than
`fn(params) { body }`. It is inspired by Kotlin's trailing lambda syntax and
Swift's trailing closures.

Multiple arguments still work:

```
[1, 2, 3] |> list.fold(0) { acc, x -> acc + x }
```

Here `list.fold(0)` provides the first explicit argument (initial value), and
the trailing closure provides the second (the reducer function). The piped
value (`[1, 2, 3]`) becomes the first argument.

### The design tension

Trailing closures and match expressions both use `{` ... `}` with `->`.
This creates a parsing ambiguity:

```
match x {
  1 -> "one"
  _ -> "other"
}
```

vs.

```
something { x -> x + 1 }
```

The parser resolves this with the `is_trailing_closure` function, which
looks ahead from a `{` token for an `->` arrow preceded only by identifiers,
commas, and parenthesized patterns. Match arms have patterns (constructors,
literals) that trailing closures don't, so the heuristic works in practice.
But it's the trickiest part of the parser. See Section 9 for the full story.

---

## 7. Record Update Syntax

```
let u = User { name: "Alice", age: 30, email: "a@b.com" }
let u2 = u.{ age: 31 }
let u3 = u.{ age: 31, email: "new@b.com" }
```

The syntax `u.{ age: 31 }` reads as "u, but with age 31." It always returns
a new record. The original `u` is unchanged (immutability).

Why this design:

- **No keyword cost.** No `with` keyword, no `...spread` operator. Just
  the dot and braces that already exist in the language.
- **Reads naturally.** `u.{ age: 31 }` is close to English: "u, with age 31."
- **Consistent with field access.** `u.age` accesses a field. `u.{ age: 31 }`
  updates fields. The dot ties them together.

Compare to other approaches:

- **Haskell:** `u { age = 31 }` -- similar but uses `=` not `:`, and the
  record update syntax is widely considered one of Haskell's warts.
- **Elm:** `{ u | age = 31 }` -- introduces a `|` operator specific to
  records.
- **Rust:** `User { age: 31, ..u }` -- the spread operator is at the end,
  which is backward from how you think about it ("start with u, change age").
- **JavaScript:** `{ ...u, age: 31 }` -- requires spread syntax.

Silt's approach avoids introducing any new syntax. The implementation in the
interpreter is straightforward: clone the base record's `BTreeMap`, insert
the updated fields, return a new `Record` value.

```rust
ExprKind::RecordUpdate { expr, fields } => {
    let base = self.eval(expr, env)?;
    match base {
        Value::Record(name, base_fields) => {
            let mut new_fields = (*base_fields).clone();
            for (fname, fexpr) in fields {
                new_fields.insert(fname.clone(), self.eval(fexpr, env)?);
            }
            Ok(Value::Record(name, Rc::new(new_fields)))
        }
        _ => Err(err("record update on non-record value")),
    }
}
```

---

## 8. Implementation Architecture

### Tree-walk interpreter

Silt v1 is a tree-walk interpreter: the parser produces an AST, and the
interpreter walks the AST directly to evaluate it. No bytecode, no
compilation step.

This is the simplest correct implementation strategy. It is slow (function
calls, pattern matching, and closures all involve tree traversal) but has
key advantages:

1. **Easy to debug.** The interpreter's `eval` method is a direct match on
   `ExprKind`. You can trace execution by printing the AST node being
   evaluated.
2. **Easy to extend.** Adding a new expression kind means adding one arm to
   `eval`. There is no separate compiler to update.
3. **Correct first.** The semantics are unambiguous because the interpreter
   _is_ the semantics. A bytecode VM must preserve the same behavior, which
   is easy to verify against a reference tree-walk interpreter.

A bytecode VM is planned for v2, where performance matters. The tree-walk
interpreter will remain as the reference implementation.

### `Rc<T>` for values

Runtime values use `Rc` (reference-counted pointer) for shared data:

```rust
pub enum Value {
    Int(i64),
    Float(f64),
    Bool(bool),
    String(String),
    List(Rc<Vec<Value>>),
    Map(Rc<BTreeMap<String, Value>>),
    Record(String, Rc<BTreeMap<String, Value>>),
    Closure(Rc<Closure>),
    Channel(Rc<Channel>),
    Handle(Rc<TaskHandle>),
    // ...
}
```

Why `Rc` and not garbage collection?

- **Immutability makes reference counting work.** In a mutable language,
  reference cycles are common (object A points to object B, which points back
  to A). In Silt, values are immutable, so reference cycles cannot form in
  user code. `Rc` never leaks.
- **Deterministic destruction.** Values are freed as soon as their last
  reference is dropped. No GC pauses, no unpredictable latency.
- **Simple implementation.** No write barriers, no mark-and-sweep, no
  generational collection. `Rc::clone()` increments a counter.
  `Drop` decrements it.

The choice of `Rc` over `Arc` is deliberate: the runtime is single-threaded
(cooperative concurrency), so atomic reference counting overhead is
unnecessary. This would need to change for true multi-threaded concurrency
in v2.

Small values (`Int`, `Float`, `Bool`) are stored inline in the `Value` enum,
not behind `Rc`. This avoids heap allocation for the most common values.

### Hand-rolled lexer

The lexer is written by hand, not generated by a lexer generator (like lex
or logos). This gives us:

- **String interpolation.** Silt strings can contain `{expr}` interpolations.
  The lexer emits `StringStart`, `StringMiddle`, and `StringEnd` tokens with
  interleaved expression tokens. This is fiendishly difficult to express in a
  regular grammar but straightforward in a hand-written state machine.
- **Precise error positions.** Every token carries a `Span` with line and
  column. Errors point to the exact character.
- **Newline tokens.** The lexer emits `Newline` tokens (not whitespace).
  These are significant for the parser's newline sensitivity rules (Section 9).

### Pratt parser

The parser uses Pratt parsing (top-down operator precedence) for expressions.
The `parse_expr_bp` method takes a minimum binding power and handles:

- **Prefix operators** (`-`, `!`) in `parse_unary`
- **Postfix operators** (`?`, function call, index, trailing closure) in the
  main loop, gated by `!self.has_newline_before()`
- **Infix operators** (`+`, `-`, `*`, `/`, `|>`, `.`, comparisons, boolean
  ops) in the main loop after `skip_nl()`

Pratt parsing handles complex precedence hierarchies cleanly. The binding
powers range from 10 (`||`) to 130 (`.` field access). For comparison,
a recursive descent parser would need a separate function for each precedence
level -- Silt has 12 levels, so that would be 12 mutually recursive functions
instead of one loop.

### Newline sensitivity

The lexer emits `Newline` tokens. The parser treats them as significant in
some contexts and insignificant in others. Postfix operators are
newline-sensitive; infix operators are not. See Section 9 for details.

---

## 9. The Newline Sensitivity Problem

This is the most subtle design decision in Silt's grammar.

### The problem

Match arms and trailing closures both use `->`:

```
-- Match arm:   pattern -> body
-- Closure:     params -> body
```

Consider this code:

```
let result = match x {
  Ok(v) -> v
  Err(e) -> handle(e)
}
```

And this code:

```
xs |> list.map { x -> x + 1 }
```

Both use `{` ... `}` with `->` inside. The parser must distinguish between
a match body and a trailing closure attached to the preceding expression.

Now consider this:

```
let f = some_function
{ x -> x + 1 }
```

Is `{ x -> x + 1 }` a trailing closure applied to `some_function`, or
a standalone block? It depends on whether there is a newline between
`some_function` and `{`.

### The solution

Silt splits operators into two categories:

**Postfix operators** (newline-sensitive -- do NOT cross newlines):
- Function call `f(args)`
- Trailing closure `f { x -> body }`
- Question mark `expr?`
- Index `expr[i]`

**Infix operators** (newline-insensitive -- DO cross newlines):
- Pipe `|>`
- Arithmetic `+`, `-`, `*`, `/`, `%`
- Comparison `==`, `!=`, `<`, `>`, `<=`, `>=`
- Boolean `&&`, `||`
- Field access `.`
- Range `..`

The rule: if a newline appears before a postfix operator, that operator is
not applied to the preceding expression. If a newline appears before an infix
operator, the parser skips it and continues the expression.

```
let a = foo
  |> bar       -- OK: |> is infix, crosses the newline

let b = foo
  { x -> x }  -- NOT a trailing closure: { is postfix, newline blocks it
```

This is implemented with a save/restore mechanism:

```rust
// First, try postfix operators -- newline-sensitive.
if !self.has_newline_before() {
    match self.peek() {
        Token::Question => { /* handle ? */ }
        Token::LParen => { /* handle call */ }
        Token::LBrace if self.is_trailing_closure() => { /* handle closure */ }
        _ => {}
    }
}

// Save position, skip newlines, try infix operators.
let saved = self.save();
self.skip_nl();
match self.peek() {
    Token::Dot => { /* handle field access */ }
    Token::Pipe => { /* handle |> */ }
    Token::Plus | Token::Minus | ... => { /* handle arithmetic */ }
    _ => { self.restore(saved); break; }
}
```

### Why this works

The intuition: postfix operators "belong to" the expression on their left.
If there is a line break, the next line is probably a new statement, not
a continuation. Infix operators, by contrast, clearly indicate continuation
-- you wouldn't write `1 +` at the end of a line if you didn't intend to
continue.

This is the same insight behind JavaScript's ASI (Automatic Semicolon
Insertion), Go's semicolon rules, and Kotlin's newline handling. But Silt's
rule is simpler because it applies uniformly to all postfix operators and
all infix operators, without special cases.

The `is_trailing_closure` heuristic adds another layer: it looks ahead from
`{` to check if the block contains `->` preceded only by parameter-like
tokens. If yes, it is a trailing closure. If no (the block contains patterns,
literals, or constructors before `->` or has no `->` at all), it is a match
body or a plain block.

### Remaining edge cases

The newline sensitivity rule handles almost all cases, but there are known
edge cases where the programmer must be careful about line breaks:

```
-- This works (trailing closure on same line):
list |> list.map { x -> x + 1 }

-- This also works (trailing closure after function call):
list |> list.map
  { x -> x + 1 }   -- actually doesn't attach because of newline!

-- To cross lines, use explicit parentheses:
list |> list.map(fn(x) { x + 1 })
```

This is a genuine ergonomic rough edge. In practice, pipe chains naturally
put the trailing closure on the same line or use multi-line closures that
start on the same line:

```
list |> list.filter { user ->
  user.age > 18 && user.active
}
```

---

## 10. Module System Design

### File = module

Each `.silt` file is a module. The file name is the module name. No
`module` declaration at the top of the file, no nested modules, no module
paths with slashes.

```
-- File: math.silt
pub fn add(a, b) = a + b
fn internal_helper(x) = x * 2   -- private
```

```
-- File: main.silt
import math
import math.{ add }
import math as m
```

This is the Go/Python model: the file system _is_ the module hierarchy.
It is simpler than Rust's module system (which requires `mod` declarations
and allows multiple modules per file) and simpler than Haskell's (which
requires `module` headers and allows re-exports).

### Private by default

Everything is private unless marked `pub`. This is the Rust convention. The
alternative (public by default, as in Python and Go) leads to accidentally
exposing implementation details.

```
pub fn add(a, b) = a + b        -- exported
fn helper(x) = x * 2            -- private, cannot be imported
pub type Shape { ... }           -- exported, including its constructors
```

When a `pub type` with enum variants is exported, all its constructors are
automatically exported too. You can't export `Result` without exporting `Ok`
and `Err` -- that would be unusable.

### Lazy loading with circular import detection

Modules are loaded lazily on first import. The `ModuleLoader` maintains:

- A `loaded` cache (module name to `ModuleExports`)
- A `loading` set (modules currently being loaded, for cycle detection)

```rust
if self.loading.contains(module_name) {
    return Err(format!(
        "circular import detected: module '{module_name}' is already being loaded"
    ));
}
```

Circular imports are rejected with a clear error message. This is simpler
than topological sorting or lazy initialization schemes. If you need mutual
recursion between modules, factor the shared code into a third module.

### Builtin modules bypass the file system

Standard library modules (`io`, `string`, `int`, `float`, `list`, `map`,
`result`, `option`, `test`, `channel`, `task`) are registered directly in the
global environment as builtin functions. They don't correspond to `.silt` files:

```rust
const BUILTIN_MODULES: &[&str] = &[
    "io", "string", "int", "float", "list", "map",
    "result", "option", "test", "channel", "task",
];
```

When you write `import string`, the module loader recognizes it as a builtin
and skips file lookup. The functions are already registered as
`string.split`, `string.join`, etc. in the global environment.

This means stdlib functions are available without shipping `.silt` source
files. The trade-off is that stdlib functions are implemented in Rust and
can't be inspected or overridden from Silt code.

---

## 11. Known Limitations & Future Directions

Being honest about what is missing (and what has been addressed):

**REPL.** A read-eval-print loop is now available via `silt repl`.

**Code formatter.** `silt fmt <file>` formats source files to a standard style.

**Tail-call optimization.** TCO is now implemented. Self-recursive functions
in tail position are optimized to avoid stack overflow.

**No package manager.** There is no dependency management, no versioning, no
package registry. Modules are files in the project directory. This is
adequate for single-project development but doesn't scale to reusable
libraries.

**No bytecode VM.** The tree-walk interpreter is correct but slow. A bytecode
VM (like Lua's or CPython's) would give 10-50x speedup for computation-heavy
programs. This is a v2 priority.

**No FFI.** Silt cannot call C or Rust functions. The standard library is
the only interface to the host system. FFI is a future consideration but
introduces significant complexity (memory safety, value representation
bridging).

**Single-threaded concurrency.** The cooperative scheduler cannot use
multiple CPU cores. For I/O-bound workloads this is fine; for CPU-bound
workloads it is a hard limitation. Real OS threads are a v2 consideration,
but would require switching from `Rc` to `Arc` and `RefCell` to `Mutex`
throughout the runtime.

**String-keyed maps only.** The runtime `Map` type uses `BTreeMap<String,
Value>`. Map keys must be strings at runtime, even though the type system
allows `Map(k, v)` with any key type. This is a simplification that should
be addressed.

**Type checker coverage is still growing.** The type checker catches type
mismatches, non-exhaustive matches, and trait contract violations, and it
blocks execution on errors. But some edge cases are not yet covered. The
checker improves with each release.

---

## 12. What We'd Do Differently

### The trailing closure disambiguation is the trickiest part

The `is_trailing_closure` heuristic works by looking ahead for `->` preceded
only by parameter-like tokens. This is fragile:

- It cannot handle destructuring patterns in trailing closure parameters
  without expanding the lookahead.
- It relies on match patterns looking syntactically different from closure
  parameters, which is true for constructors and literals but not for plain
  identifiers.
- It is the only place in the parser that requires unbounded lookahead.

If starting over, we might use a different syntax for trailing closures
(e.g., `do { x -> body }` with a keyword) or for match arms (e.g., `|`
prefix as in OCaml). The current design works but is the source of the most
parser complexity.

### Type checker strictness was a journey

The type checker started as a warnings-only pass to avoid blocking execution
on an incomplete checker. It has since matured to the point where type errors
(mismatches, non-exhaustive matches, trait violations) block execution,
while warnings (unused bindings, unreachable patterns) still allow it. This
graduated approach let us ship early and tighten incrementally. The remaining
work is expanding coverage, not changing the enforcement model.

### Cooperative concurrency is a stepping stone

The cooperative, single-threaded model is simple and correct, but it limits
what users can build. A web server handling concurrent connections would
work (I/O naturally yields), but a parallel computation pipeline would not
benefit from concurrency at all.

The good news: the CSP interface (`channel.new`, `channel.send`,
`channel.receive`, `task.spawn`, `channel.select`) is runtime-agnostic. Switching to
a preemptive, multi-threaded scheduler requires changes only in the runtime,
not in user code. This was a deliberate design choice -- the user-facing API
is forward-compatible.

### The environment model

The interpreter uses a linked-list environment (`Env` with an `Rc<RefCell>`
inner and an optional parent). This is standard for tree-walk interpreters
but has O(n) lookup depth for deeply nested scopes. A flat environment with
De Bruijn indices would be faster but harder to implement correctly for
closures. For a v1 interpreter, the linked-list model is fine.

### What we got right

- **Pattern matching as the only branching construct.** Every user who
  initially misses `if`/`else` comes to appreciate the consistency after a
  week.
- **The pipe operator.** Data processing code is dramatically more readable.
- **Errors as values.** No more "which functions can throw?" guessing games.
- **The keyword constraint (now 14).** It forced us to find general solutions
  instead of special-casing each problem with new syntax. Demoting all
  concurrency keywords to module functions was a net positive.
- **Record update syntax.** `u.{ age: 31 }` is the most natural update
  syntax we've seen in any language.
