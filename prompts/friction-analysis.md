# Silt Friction Analysis Prompt

Use this prompt to implement 20 programs from scratch in silt, measure friction, and produce a report. The programs are implemented by parallel agents who have never seen silt before — they learn the language from its docs and stdlib reference, then write real programs. This tests whether silt is learnable and ergonomic for someone encountering it for the first time.

---

You are conducting a friction analysis of a programming language called "silt" at /home/klaus/dev/silt. Your goal is to implement 20 programs from scratch, run each one, and produce a structured friction report.

## Phase 1: Spawn agents

Spawn 20 agents in parallel (use `run_in_background: true`), one per program. Do NOT pre-read the docs or build a language summary — each agent learns the language autonomously by exploring the docs as they implement. This is intentional: it tests whether a newcomer can learn silt from its documentation alone.

Each agent gets:
1. A specific program assignment (see below)
2. The self-directed learning instructions (see agent prompt template)
3. Instructions to run with `~/.cargo/bin/cargo run --manifest-path /home/klaus/dev/silt/Cargo.toml -- run <file>`

Each agent should write its program to `programs/<name>.silt`, creating any needed data files.

### Program assignments

Give each agent ONE of these assignments. Include the full language summary with each.

**Agent 1: `programs/todo.silt`** — Interactive todo list manager
- REPL loop: add, list, done, remove, save, load commands
- Todos are records with id, text, done fields
- Persist to a file using `io.write_file` / `io.read_file`
- Use `loop` expression for the REPL loop
- Create sample interaction showing all commands work

**Agent 2: `programs/pipeline.silt`** — Unix-pipe-style text processing
- Read a log file (`programs/log.txt` — create it with ~20 lines of sample log data)
- Implement: grep, uppercase, numbered, sort_by_length, word_count, uniq, reject
- Chain at least 10 different pipeline compositions using `|>`
- Demonstrate higher-order function factories (e.g., `make_grep(pattern)`)

**Agent 3: `programs/expr_eval.silt`** — Expression evaluator
- Define recursive ADT: `type Expr { Num(Int), Add(Expr, Expr), Mul(Expr, Expr), Neg(Expr) }`
- Implement: eval, simplify (algebraic simplification), to_rpn, from_rpn, classify complexity
- Implement `Display` trait for pretty-printing
- Demonstrate deep pattern matching and or-patterns

**Agent 4: `programs/config_parser.silt`** — INI-style config parser
- Create a sample `programs/app.conf` with sections, key-value pairs, comments, blank lines
- Parse into nested map structure (section -> key -> value)
- Validate required keys, report errors with line numbers
- Use ADTs for line classification (Blank, Comment, Section, KeyValue, ParseError)

**Agent 5: `programs/csv_analyzer.silt`** — CSV statistics
- Create `programs/data.csv` with headers and ~15 rows of student data (name, age, score, grade)
- Parse CSV, compute per-column statistics (sum, min, max, avg for numeric columns)
- Filter and sort rows
- Print formatted tables with alignment

**Agent 6: `programs/kvstore.silt`** — Key-value store with JSON persistence
- REPL: GET, SET, DEL, KEYS, COUNT, SAVE, LOAD, QUIT commands
- Store data in a map, persist via `json.stringify` / `json.parse`
- Use `loop` expression for the REPL
- Handle edge cases (missing keys, empty store, file not found)

**Agent 7: `programs/concurrent_processor.silt`** — Parallel file processor
- Spawn 2+ worker tasks that receive file paths via a channel
- Workers process files and send results back via a results channel
- Main task collects results and prints summary
- Demonstrate `channel.select` with the pin operator
- Demonstrate `channel.close` for graceful shutdown

**Agent 8: `programs/text_stats.silt`** — Text file statistics
- Accept a filename from `io.args()` or use a default
- Compute: line count, word count, char count, unique words, avg word length
- Find longest/shortest words, top-N most frequent words
- Format output with aligned columns

**Agent 9: `programs/test_suite.silt`** — Self-testing test framework
- Implement a test runner using `try()` to catch assertion failures
- Write tests covering: arithmetic, strings, lists, maps, Option/Result, records, patterns, pipes, closures
- Track pass/fail counts, print results with visual indicators
- Each test function should be named `test_*`

**Agent 10: `programs/link_checker.silt`** — Markdown link validator
- Create `programs/test.md` with ~10 markdown links (mix of valid/invalid URLs)
- Extract `[text](url)` links using `regex.find_all`
- Validate URL schemes (http, https, ftp, mailto)
- Report valid/broken/unknown links with formatted output

**Agent 11: `programs/calculator.silt`** — Scientific calculator with history
- Implement a stack-based calculator with operations: +, -, *, /, sqrt, pow, sin, cos, log
- Use the `math` module extensively (sqrt, pow, sin, cos, log, pi, e)
- Maintain a history list of all computations performed
- Implement undo (pop last result), clear, and show-history commands
- Use `float.parse`, `float.to_string` for number handling
- REPL loop using `loop` expression with `io.read_line`
- Format output to 4 decimal places using `float.to_string(f, 4)`

**Agent 12: `programs/state_machine.silt`** — Vending machine simulator
- Define ADT for states: `type State { Idle, Accepting(Int), Dispensing(String, Int), OutOfStock }`
- Define ADT for events: `type Event { InsertCoin(Int), SelectItem(String), Cancel, Restock }`
- Implement `transition(state, event) -> (State, String)` using nested match expressions
- Track inventory as a map of item names to quantities and prices
- Run a sequence of 15+ events and print state transitions with formatted output
- Demonstrate deep pattern matching on ADTs with tuple returns
- Use `where` clauses on at least one function

**Agent 13: `programs/maze_solver.silt`** — Grid-based pathfinding
- Represent a maze as a list of strings (each char is wall `#` or open `.` or start `S` or end `E`)
- Define a hard-coded 10x10 maze with a solvable path
- Implement BFS using `list.fold_until` for the search loop with `Stop`/`Continue`
- Track visited cells and reconstruct the path
- Print the maze with the solution path marked (e.g., `*`)
- Use `string.chars` for grid access, records for coordinates
- Exercise `list.filter`, `list.map`, `list.contains`, `list.append` heavily

**Agent 14: `programs/json_transform.silt`** — JSON data pipeline
- Define a sample JSON dataset (array of 10+ employee objects with name, department, salary, skills array)
- Write it as a string, parse with `json.parse`
- Implement transformations: group by department, compute avg salary per dept, find top earners, filter by skill, flatten skills into unique list
- Chain all transformations using `|>` with `map.entries`, `map.from_entries`, `list.group_by`, `list.sort_by`
- Output results as pretty-printed JSON using `json.pretty`
- This tests the JSON module + map module + list module integration

**Agent 15: `programs/trait_zoo.silt`** — Trait system showcase
- Define 3+ ADTs: `type Shape { Circle(Float), Rect(Float, Float), Triangle(Float, Float, Float) }`
- Define `type Color { Red, Green, Blue, Custom(Int, Int, Int) }`
- Define `type Styled { Styled(Shape, Color) }`
- Implement `Display` trait for all three types with pretty formatting
- Define a custom trait `trait Area { fn area(self) -> Float }` and implement for `Shape`
- Define `trait Perimeter { fn perimeter(self) -> Float }` and implement for `Shape`
- Use `where` clauses: `fn describe<T>(item: T) -> String where T: Display`
- Create a list of shapes, compute areas and perimeters, sort by area, display all
- Exercise `math.pi`, `math.sqrt` for calculations

**Agent 16: `programs/encoder.silt`** — Text encoding/decoding toolkit
- Implement Caesar cipher: `encode_caesar(text, shift)` and `decode_caesar(text, shift)`
- Implement ROT13 as a special case
- Implement run-length encoding: `rle_encode("aaabbc")` → `"3a2b1c"`, and `rle_decode`
- Implement a simple substitution cipher using a map-based key
- Use `string.chars` extensively for character-level manipulation
- Use `list.fold` for accumulation, `list.map` for transformation
- Use `regex.replace_all` for one of the encoders
- Test each encoder by round-tripping (encode then decode, verify match)
- Print formatted comparison tables showing original → encoded → decoded

**Agent 17: `programs/data_gen.silt`** — Synthetic data generator
- Implement a linear congruential PRNG: `next_random(seed) -> (Int, Int)` returning (value, new_seed)
- Use `list.unfold` to generate sequences of random numbers from a seed
- Generate: random names (by picking from lists), random ages, random scores
- Build a list of 20 "student" records with generated data
- Compute statistics: mean, median (sort + middle), std deviation (using `math.sqrt`)
- Implement histogram: bucket values into ranges, display with `string.repeat` for bars
- Use `list.group_by`, `list.sort_by`, `list.zip`, `list.enumerate`

**Agent 18: `programs/diff_tool.silt`** — Line-by-line file differ
- Create two sample text files (`programs/files/original.txt` and `programs/files/modified.txt`) with ~15 lines each, some shared, some different
- Read both files with `io.read_file`, split into lines
- Implement longest common subsequence (LCS) algorithm using `list.fold` or recursion
- Produce a unified diff output showing added (+), removed (-), and context lines
- Track statistics: lines added, lines removed, lines unchanged
- Format output with colored markers (just use `+`/`-`/` ` prefixes)
- Use `list.zip`, `list.enumerate`, `list.filter`

**Agent 19: `programs/router.silt`** — URL router/dispatcher
- Define routes as a list of records: `{ method: String, pattern: String, handler: String }`
- Define 10+ routes with path parameters (e.g., `/users/:id`, `/posts/:id/comments`)
- Implement `match_route(method, path, routes)` that finds the matching route
- Use `regex.captures` to extract path parameters from URL patterns
- Implement path parameter extraction into a map (e.g., `#{ "id": "42" }`)
- Test with 10+ sample requests, print match results with extracted params
- Demonstrate closures stored in records (handler factories)
- Use `list.find`, `map.set`, `map.entries`, `string.split`

**Agent 20: `programs/budget.silt`** — Personal budget tracker
- Define transaction records: `{ date: String, category: String, amount: Float, description: String }`
- Create 20+ sample transactions across 5+ categories (income, food, transport, entertainment, utilities)
- Implement: total by category, monthly summary, running balance, budget vs actual comparison
- Define budgets as a map of category → limit, flag over-budget categories
- Use `list.group_by` for categorization, `list.fold` for totals
- Use `float.to_string(f, 2)` for currency formatting
- Print formatted reports with `string.pad_left`/`string.pad_right` for alignment
- Implement a "forecast" that projects spending based on current rate
- Use `Result` for validating transaction data (negative amounts, missing fields)

### Agent prompt template

Use this template for each agent's prompt (fill in `{PROGRAM_NAME}` and `{ASSIGNMENT}`):

```
You are implementing a program in a language called "silt". You have never used this language before. Your task is to write `{PROGRAM_NAME}` based on the assignment below. You must learn the language yourself from its documentation.

## Assignment
{ASSIGNMENT}

## Learning the Language

You are responsible for learning silt as you go. The documentation is at:
- `docs/getting-started.md` — language tour, syntax basics, all core concepts
- `docs/stdlib-reference.md` — complete stdlib API (every function, every module)
- `docs/language-guide.md` — deep dive into all language features
- `docs/concurrency.md` — channels, tasks, and concurrency patterns (read if your program uses concurrency)
- `examples/` — working example programs showing idiomatic patterns

Start by reading `docs/getting-started.md` to understand the basics. Then skim `docs/stdlib-reference.md` to see what's available. Refer back to the docs whenever you're unsure about syntax, available functions, or language features.

## Instructions

1. Read the docs above to learn the language. You will need to refer back to them throughout implementation.
2. Read 1-2 files in `examples/` to see idiomatic silt patterns.
3. Write the program to the specified path.
4. Create any required data files (log.txt, data.csv, app.conf, test.md).
5. Run the program: `~/.cargo/bin/cargo run --manifest-path /home/klaus/dev/silt/Cargo.toml -- run <file>`
6. If it fails, read the error, fix the code, and retry. Track how many attempts it took.
7. **Before reporting missing features or friction**, search the docs (`docs/stdlib-reference.md`, `docs/language-guide.md`) to verify the feature actually doesn't exist. Many "missing" features are just undiscovered — check before reporting.
8. Once working, record your friction report.

## Friction Report Format

At the end, output a structured friction report as a comment block at the top of your .silt file:

{- Friction Report
   Attempts: N (how many edit-run cycles before it worked)
   Rating: X/10 (how natural did the code feel?)
   Highlights: (what worked well)
   Friction: (what was awkward, confusing, or required workarounds)
   Missing: (stdlib functions or language features you wished existed — ONLY after verifying they don't exist in docs)
   Bugs: (any interpreter/typechecker issues encountered)
-}

Do NOT read existing programs in programs/ — write yours from scratch based on the assignment and language docs only.
```

## Phase 2: Review friction reports

After all 20 implementation agents complete, spawn 20 review agents in parallel (use `run_in_background: true`), one per program. Each review agent gets:

1. The program source file (`programs/<name>.silt`) — including the friction report comment block
2. Access to all docs (`docs/getting-started.md`, `docs/stdlib-reference.md`, `docs/language-guide.md`, `docs/concurrency.md`)
3. Instructions to fact-check every friction claim

### Review agent prompt template

Use this template for each review agent (fill in `{PROGRAM_PATH}`):

```
You are reviewing a friction report for a silt program at `{PROGRAM_PATH}`. Your job is to fact-check every claim in the friction report against the actual language documentation.

## Instructions

1. Read the program file and extract the friction report comment block.
2. Run the program to verify it works: `~/.cargo/bin/cargo run --manifest-path /home/klaus/dev/silt/Cargo.toml -- run {PROGRAM_PATH}`
3. For EACH item in the "Friction", "Missing", and "Bugs" sections:
   a. Search `docs/stdlib-reference.md`, `docs/getting-started.md`, and `docs/language-guide.md` for the feature or function mentioned.
   b. If the feature EXISTS in the docs, mark the item as a **false positive** and note where it's documented.
   c. If the feature genuinely does NOT exist, mark it as **confirmed**.
   d. If it's a real limitation but the reporter mischaracterized it, mark as **partially confirmed** and clarify.
4. Check if the program uses any workarounds that are unnecessary given actual language features.

## Output Format

Return a structured review:

Program: {PROGRAM_PATH}
Rating (from implementer): X/10
Attempts: N

CONFIRMED friction:
- (item) — confirmed, not in docs

FALSE POSITIVES:
- (item) — actually exists: documented in (file), section (name)

PARTIALLY CONFIRMED:
- (item) — (clarification of what's actually true)

UNNECESSARY WORKAROUNDS:
- (description) — could have used (feature) instead

ADJUSTED RATING: X/10 (your assessment after removing false positives)
```

## Phase 3: Aggregate into final report

After all 20 review agents complete:

1. Read each review agent's output
2. Compile the final friction analysis report, incorporating only **confirmed** and **partially confirmed** findings
3. Score each friction point by relevance

### Aggregation process

For each friction point that appears across programs:
1. Count how many programs reported it (frequency)
2. Count how many times it survived review (confirmed rate)
3. Assign a **relevance score** (1-5):
   - **5 — Critical**: Blocks progress or forces unsafe patterns (e.g., unreachable panics). Appeared in 5+ programs.
   - **4 — High**: Significant workaround required, felt unnatural. Appeared in 3+ programs.
   - **3 — Medium**: Minor inconvenience, reasonable workaround exists. Appeared in 2+ programs.
   - **2 — Low**: Cosmetic or stylistic preference. Appeared in 1-2 programs.
   - **1 — Noise**: Reported but debatable whether it's actually friction.

### Final report structure

Write the report to `docs/friction-report.md`:

```markdown
# Silt Language Friction Report

Generated: {date}
Method: 20 programs implemented from scratch by agents with no prior silt experience. Each program's friction report was independently reviewed against the language documentation to eliminate false positives.

## Executive Summary
(overall rating, key findings, top friction points — only confirmed findings)

## Per-Program Results

| # | Program | Impl Rating | Reviewed Rating | Attempts | Lines | Highlight | Primary Friction |
|---|---------|:-----------:|:---------------:|:--------:|:-----:|-----------|-----------------|
| 1 | todo.silt | X/10 | X/10 | N | L | ... | ... |
| ... | ... | ... | ... | ... | ... | ... | ... |

**Average (impl): X.X / 10**
**Average (reviewed): X.X / 10**

## Confirmed Friction Points

| Relevance | Friction Point | Programs | Confirmed | Description |
|:---------:|---------------|:--------:|:---------:|-------------|
| 5 | (name) | N/20 | N/N | ... |
| 4 | (name) | N/20 | N/N | ... |
| ... | ... | ... | ... | ... |

## False Positive Summary
(friction points that were reported but don't hold up — shows what the docs already cover but agents didn't find. This is itself a signal about documentation discoverability.)

## What Felt Natural
(features that consistently worked well across programs)

## Missing Standard Library Functions
(consolidated list — only confirmed missing after doc review)

## Bugs Encountered
(confirmed interpreter, typechecker, or parser bugs)

## Language Snapshot

### Keywords (N)
...

### Globals (N)
...

### Module builtins (N across M modules)
(table of modules with function counts)

### Codebase metrics
(LoC, test count — verify by running `wc -l src/*.rs` and `cargo test`)

## Code Showcases
(3-4 examples of silt at its best, taken from the implemented programs)

## Verdict
(2-3 paragraph summary of the friction analysis findings)
```

## Notes

- The whole point is that agents implement programs from scratch without seeing existing code. This tests learnability, not just expressiveness.
- Each agent learns the language autonomously from the docs. No pre-built summary is provided — this tests whether the documentation itself is sufficient. If agents can't find a feature, that's a docs friction signal.
- If an agent gets stuck, that IS the friction data — don't help it beyond pointing at docs.
- Count edit-run cycles honestly. A program that works on the first try is a strong signal.
- Agents must verify "missing features" against the docs before reporting them. The previous run had many false positives (e.g., reporting `-x` and `!` as missing when they're documented in getting-started.md). Requiring doc verification reduces noise.
- Programs should be non-trivial (50-200 lines each). Toy programs don't reveal friction.
- Programs 1-10 focus on core features (ADTs, REPL, pipes, concurrency, regex, JSON). Programs 11-20 push deeper into the math module, trait system, `where` clauses, `list.unfold`, character-level string processing, and complex data transformations. Together they cover the full stdlib surface area.
