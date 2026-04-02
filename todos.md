# Friction Report Follow-ups

Findings from the 20-program friction analysis (2026-04-02). Items ordered by impact.

---

## ~~1. Correct the friction report: `!` and `-x` already exist~~ DONE

The report's #1 recommendation ("add `!` boolean negation") is wrong — both `!` and unary `-` are fully implemented and documented in `docs/getting-started.md:345-355`. The 10 instances of `0 - x` and the `match x { true -> false; _ -> true }` patterns across the 20 programs were caused by the briefing summary omitting these operators, not by a language gap.

**Action:** Update `docs/friction-report.md` to note that this was a methodology error. The actual takeaway is that the language summary given to newcomers matters enormously — if it omits features, people will invent verbose workarounds.

**Files:** `docs/friction-report.md`

---

## ~~2. Document pipe + factory interaction~~ DONE

`value |> factory(arg)` calls `factory(value, arg)`, not `factory(arg)(value)`. This is correct and consistent behavior (pipe always inserts as first arg), but it's undocumented and surprised 3 agents. The workaround is pre-binding:

```silt
let grep_error = make_grep("ERROR")
lines |> grep_error
```

**Action:** Add an example to the pipe operator section in `docs/getting-started.md` showing this behavior explicitly, with the pre-binding pattern.

**Files:** `docs/getting-started.md`

---

## 3. `list.filter_map` — or type narrowing?

The concrete problem (from `todo.silt`):

```silt
lines
|> list.map { l -> parse_todo_line(l) }       -- List(Option(Todo))
|> list.filter { opt ->
    match opt { Some(_) -> true; _ -> false }
  }
|> list.map { opt ->
    match opt { Some(t) -> t; _ -> panic("unreachable") }
  }
```

Three passes, an unreachable panic. Two approaches:

### Option A: `list.filter_map` (simple stdlib addition)

```silt
lines |> list.filter_map { l -> parse_todo_line(l) }
```

Function returns `Option(T)`, keeps `Some` values and unwraps them. Exists in Rust, Elixir, OCaml. Zero type system changes, solves 90% of the pain.

### Option B: Type narrowing predicates (TypeScript-style)

```silt
fn is_some(item: Option(a)) -> Bool narrows Some(a) {
  option.is_some(item)
}

lines |> list.map { l -> parse_todo_line(l) } |> list.filter(is_some)
-- compiler knows result is List(Todo), not List(Option(Todo))
```

More powerful — works with any predicate, any ADT:

```silt
type Line { Blank, Comment(String), Section(String), KeyValue(String, String) }
fn is_key_value(line) -> Bool narrows KeyValue(String, String) { ... }
lines |> list.filter(is_key_value)  -- List(KeyValue), not List(Line)
```

**Design considerations for type narrowing:**
- **Syntax:** Where does the narrowing annotation go? `-> Bool narrows T`? `-> Bool where item is T`?
- **HM interaction:** `list.filter` is currently `(List(a), (a -> Bool)) -> List(a)`. With narrowing, the output type differs from input — needs a different signature or special-case in the typechecker.
- **Scope:** Does it work only with builtins (`list.filter`, `list.find`), or can user-defined higher-order functions benefit?
- **Implementation cost:** Major type system feature. New syntax, inference propagation, special-casing.

**Recommendation:** Add `filter_map` now (immediate value, trivial to implement). Design type narrowing as a v2 feature if the pattern proves common enough.

**Files:** `src/interpreter.rs` (add `list.filter_map` builtin)

---

## 4. `_` as closure parameter — parser ambiguity

`list.map(xs) { _ -> 42 }` fails because the parser's `is_trailing_closure()` (parser.rs:1131) sees `{ _ ->` and interprets it as a match arm, not a closure parameter.

```rust
// parser.rs:1126-1131
Token::Ident(name) if name == "_" => return false,  // treats { _ -> as match
```

**What works:**
- `{ x, _ -> x }` — first token is `x`, not `_`, so closure detected
- `fn(_, y) { y }` — `fn(` unambiguously starts a lambda
- `{ (_) -> 42 }` — `(` isn't caught by the heuristic

**The fundamental tension:** `{ _ -> expr }` is syntactically ambiguous between a match arm (wildcard pattern) and a closure (discarded parameter). The current heuristic prefers match arm.

**Options:**
1. **Context-aware parsing:** If `{` is in trailing closure position (after function call), prefer closure interpretation. Complex — requires parser to track context.
2. **Accept and document:** `{ _ -> ... }` is always a match arm. Use `fn(_) { ... }` when discarding a closure parameter. This already works.
3. **Different discard syntax:** e.g., `_unused` convention.

**Recommendation:** Option 2 — document the behavior. The `fn(_) { ... }` syntax is explicit and works. Add a note in the trailing closure documentation.

**Files:** `docs/getting-started.md` (document the behavior), possibly `src/parser.rs` (if fixing)

---

## 5. Better JSON support

### What already works (but agents didn't discover)

Records serialize to JSON correctly:

```silt
type Employee { name: String, salary: Int, skills: List(String) }
let emp = Employee { name: "Alice", salary: 50000, skills: ["python", "go"] }
json.stringify(emp)  -- {"name":"Alice","salary":50000,"skills":["python","go"]}
```

The `json_transform.silt` agent built JSON strings manually because they didn't know this. Documentation fix needed.

### The actual gap: round-trip asymmetry

| Direction | Mechanism | Works? |
|-----------|-----------|--------|
| Record -> JSON | `json.stringify(record)` | Yes |
| JSON -> Record | Not possible | `json.parse` returns a map, not a record |
| Homogeneous Map -> JSON | `json.stringify(map)` | Yes |
| JSON -> Map | `json.parse(str)` | Yes, but runtime-mixed value types |
| Heterogeneous Map literal | `#{ "name": "Alice", "age": 30 }` | No — typechecker rejects mixed value types |

### Improvement opportunities

**a) Document record serialization** (immediate, zero cost):
Add an example to `docs/stdlib-reference.md` showing `json.stringify` with records.

**b) JSON-to-record deserialization** (medium effort, high value):
```silt
type Employee { name: String, salary: Int }
let emp = json.parse_as(Employee, json_string)?
```
Would close the round-trip gap. Requires the interpreter to match JSON object keys to record field names and coerce types.

**c) Improve error message for string interpolation in JSON context:**
The `json_transform` agent reported that `"unexpected character: '\'"` was misleading when the actual problem was unescaped `{` being treated as interpolation start. The error should mention unterminated string interpolation.

**Files:** `docs/stdlib-reference.md` (document record serialization), `src/interpreter.rs` (json.parse_as), `src/lexer.rs` or `src/errors.rs` (better error message)

---

## 6. Raw string literals

Embedding JSON or regex in strings currently requires heavy escaping:

```silt
"\{\"name\":\"" + name + "\",\"salary\":" + int.to_string(salary) + "}"
```

### Proposed syntax: `r"..."` and `r#"..."#` (Rust-style)

```silt
let pattern = r"\d+\.\d+"           -- no escape processing
let json = r#"{"name": "Alice"}"#   -- can contain quotes
```

**Semantics:**
- No escape processing (`\n`, `\t`, `\"` are literal)
- No string interpolation (`{expr}` is literal)
- `r#` variant allows embedded `"` characters
- `#` count can increase for strings containing `"#`: `r##"she said "hi"#"##`

**Implementation:**
- Lexer: recognize `r"` / `r#"` as raw string start, scan to matching delimiter
- Parser: no change — raw strings produce the same string literal AST node
- No interpolation segments inside raw strings

**Design decision:** No raw-but-interpolating variant. Raw means raw. If you want interpolation, use a regular string.

**Files:** `src/lexer.rs` (raw string tokenization), `docs/getting-started.md` (document syntax)
