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

A test file is a module of its own, which imports the module it tests.
So it cannot write an impl for a type of that module (an impl goes in
the module of its trait or of its type, see
[Where an Impl Is Written](traits.md#where-an-impl-is-written)): put
the impl beside the type, or declare the trait in the test file and
implement it there, or wrap the value in a type the test file declares.

## Function Conventions

Within a test file, functions are recognized by their name prefix:

- `test_*` -- recognized as a test and executed
- `skip_test_*` -- shown as skipped, not executed
- Any other function name -- ignored by the test runner (available as helpers)

```silt
import string
import test

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

A test has ended when its test function has returned **and the tasks it
spawned can do no more**: each has ended or waits, with no timer and no I/O
of theirs pending (the rule of
[When a program ends](../concurrency.md#when-a-program-ends), applied to
the test's own tasks). The runner waits for that before it gives the
test's result, so:

- a task that fails after the test function has returned fails that test,
  not a later one;
- a task that never stops keeps its test, and the run, from ending: cancel
  it before the test returns;
- a task left waiting for something nobody will give is dropped with its
  test, and is neither waited for nor reported by a later test.

A test whose function fails (a failed assertion, a `panic`, a returned
`Err`) is not waited for: its tasks are stopped where they are, as a program's are when `main`
fails. A ticker that the test would have cancelled on its last line does
not keep the run going.

The tasks that a file's top-level code spawns are the file's. The runner
waits for them in the same way before the first test, and those that wait
then stay for the tests: a top-level task that answers requests on a channel
serves every test of the file, and a test that waits for its answer is not
deadlocked. A test has ended when neither its own tasks nor the file's can
do more. If a task of the top-level code fails and nobody joins it, the file
has failed: `FAIL <file> (a task spawned by the file's top-level code
failed)`, once, after the file's last test, with the reports under it.

## Exit Code

`silt test` exits with code 0 if all tests pass and code 1 if any test fails,
including a test that failed because a task it spawned failed.
