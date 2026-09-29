---
title: "Testing"
section: "Language"
order: 11
---

# Testing

Silt has a built-in test runner invoked with `silt test`. It discovers and
runs test functions without any configuration.

## File Conventions

Test files must use one of these naming patterns to be auto-discovered:

- `*_test.silt` (e.g. `math_test.silt`)
- `*.test.silt` (e.g. `math.test.silt`)

Without a path argument, `silt test` searches the current directory
recursively. You can also pass a specific file or directory:

```
silt test                      -- search current directory recursively
silt test tests/               -- search tests/ directory recursively
silt test math_test.silt       -- run a single file
```

## Function Conventions

Within a test file, functions are recognized by their name prefix:

- `test_*` -- recognized as a test and executed
- `skip_test_*` -- shown as skipped, not executed
- Any other function name -- ignored by the test runner (available as helpers)

```silt
import test
import string

fn test_addition() {
    test.assert_eq(1 + 1, 2)
}

fn test_string_length() {
    test.assert_eq(string.length("hello"), 5)
}

fn skip_test_not_ready_yet() {
    test.assert(false, "this would fail")
}

fn helper(x) {
    x * 2
}

fn test_with_helper() {
    test.assert_eq(helper(3), 6)
}
```

Running `silt test` in a directory containing this file produces:

```
  PASS ./math_test.silt::test_addition
  PASS ./math_test.silt::test_string_length
  SKIP ./math_test.silt::skip_test_not_ready_yet
  PASS ./math_test.silt::test_with_helper

4 tests: 3 passed, 0 failed, 1 skipped
```

(When a single file is passed directly — `silt test math_test.silt` — the
`./` prefix is omitted and you see `PASS math_test.silt::test_addition`
instead.)

## Filtering Tests

Use `--filter <pattern>` to run only tests whose names contain the pattern:

```
silt test --filter addition     -- runs only test_addition
silt test --filter string       -- runs only test_string_length
```

The filter matches against the function name (not the file name). A file with
no matching test is skipped entirely: it is not compiled and nothing is
reported for it. A file whose tests cannot be listed, because it cannot be read
or does not lex, is still reported, and its error fails the run.

## Assertions

The `test` module provides assertion functions. Hover over any
`test.*` call in your editor for the full signature and per-function
documentation (the LSP surfaces it from the inlined builtin docs);
the LSP's completion popup on `test.` also enumerates every assertion
entry point.

| Function | Description |
|----------|-------------|
| `test.assert(condition)` | Fails if `condition` is `false` |
| `test.assert_eq(left, right)` | Fails if `left != right` |
| `test.assert_ne(left, right)` | Fails if `left == right` |

All assertions accept an optional trailing `String` message argument.

## Spawned Tasks

A test that spawns tasks is responsible for them. If a task fails and the
test never joins it (`task.join`) or cancels it (`task.cancel`), the failure
fails **the test that spawned the task** -- also when the task was spawned by
another task of that test. The failure is reported under the test's result
line, rendered like any runtime error:

```text
  FAIL spawn_test.silt::test_worker
    error[runtime]: task <handle:0> failed and was never joined: division by zero
     --> spawn_test.silt:5:29
    ...
      = help: join the task with task.join to handle its error, or cancel it with task.cancel
```

The runner looks for failed tasks after each test. A task that fails only
after its test has returned -- while a later test runs -- still fails the
test that spawned it. That test is then reported again, as failed, with the
note `(a task it spawned failed after the test had returned)`, and the
summary counts it as failed instead of passed. The same holds for a failure
noticed after the last test: the runner looks once more before it prints
the summary. A task that is still running when the summary is printed is not
a failure, and a failure after that point is not reported. A task spawned by
a file's top-level code counts as a failure of the file.

Join the tasks a test spawns (or cancel them) before the test returns, and
the result of the test does not depend on timing.

## Exit Code

`silt test` exits with code 0 if all tests pass and code 1 if any test fails,
including a test that failed because a task it spawned failed.
