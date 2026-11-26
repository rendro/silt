---
title: "Testing"
section: "Language"
order: 12
---

# Testing

Silt has a built-in test runner invoked with `silt test`. It discovers and
runs test functions without any configuration.

## File Conventions

A test file's name ends with `_test.silt` (e.g. `math_test.silt`):
that is how `silt test` finds it.

Without a path argument, `silt test` searches the current directory
recursively, but not a directory that holds a `silt.toml` of its own:
that is another package, whose tests are its own. You can also pass
specific files and directories, any number of them:

```
silt test                      -- search current directory recursively
silt test tests/               -- search tests/ directory recursively
silt test math_test.silt       -- run a single file
silt test a_test.silt tests/   -- run a file and search a directory
```

With several paths the tests of each run, with one summary at the end;
a file named twice (or found in a directory also named) runs once.

A test file is a module of its own, which imports the module it tests.
So it cannot write an impl for a type of that module (an impl goes in
the module of its trait or of its type, see
[Where an Impl Is Written](traits.md#where-an-impl-is-written)): put
the impl beside the type, or declare the trait in the test file and
implement it there, or wrap the value in a type the test file declares.

### Tests of a package

In a package, a test file goes in `tests/`, or beside the code it tests
in `src/`. Either way it imports the package's modules by their names,
as `src/main.silt` does, and sees only their `pub` items:

```
logq/
  silt.toml
  src/
    main.silt
    entry.silt          -- pub type Entry, pub fn parse
    entry_test.silt     -- a test beside the module
  tests/
    entry_test.silt     -- a test of the package from outside
```

```
-- tests/entry_test.silt
import entry
import entry.{ Entry }
import test

fn test_parse() {
  test.assert_eq(entry.parse("info hi"), Some(Entry { level: "info", text: "hi" }))
}
```

`silt test` in the package's directory runs both files. In a package,
`silt check` checks every module of the package, the other `.silt` files
of `src/` (each in a sub-directory, as a program of its own), the other
`.silt` files under `tests/` (each as a program of its own too) and its
test files, without running them. `silt test` checks the package first, as
`silt check` does, with no path and also when the path it is given is
in the package: when a module has an error, it shows the
errors and runs no test, even where no test file imports that module.
`silt init` writes a first test, `src/main_test.silt` (`src/lib_test.silt`
in a library), that imports the entry module and calls a `pub` function
of it.

## Function Conventions

Within a test file, functions are recognized by their name prefix:

- `test_*` -- recognized as a test and executed
- `skip_test_*` -- shown as skipped, not executed
- Any other function name -- ignored by the test runner (available as helpers)

In any other file a function with such a name is an ordinary one, for
`silt check` and the editor as for `silt run`; only a file given to
`silt test` by its path has its `test_*` functions run whatever its name.

A test function takes no arguments and returns `()` or `Result((), e)`, as
`main` does (see [What `main` Returns](design-decisions.md#what-main-returns)).
One that returns `Err(e)`, typically from a `?`, fails; a value it computes
is checked with an assertion, not returned.

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
  test.assert(false)
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

(When a path is passed — `silt test math_test.silt` — the files are named
as found from it, and the `./` prefix is omitted and you see `PASS math_test.silt::test_addition`
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
or has a syntax error, is still reported, and its error fails the run. A
filter that matches no test at all is a mistake, a typo say, and fails the run:
`silt test` writes `no test matches --filter 'pattern'` and exits with status 1.

## Assertions

The `test` module provides assertion functions. The
[test reference](../stdlib/test.md) has each one with its signature and an
example; an editor shows the same text when you hover a `test.*` name.

| Function | Description |
|----------|-------------|
| `test.assert(condition)` | Fails if `condition` is `false` |
| `test.assert_eq(left, right)` | Fails if `left != right` |
| `test.assert_ne(left, right)` | Fails if `left == right` |
| `test.assert_approx(a, b, tolerance)` | Fails if `a` and `b` differ by more than `tolerance` |

## Tasks in Tests

A test runs concurrent work as a program does. A call such as
`task.join` returns when its tasks have ended, and a failure in one of
them is a failure of that call: it fails the test, and is reported under
the test's result line like any runtime error.

```text
  FAIL worker_test.silt::test_workers
    error[runtime]: division by zero
     --> worker_test.silt:5:3
    ...
```

When a test function returns, nothing it started runs on, so no task of
one test can fail a later one. A test whose function fails (a failed
assertion, a `panic`, a returned `Err`) stops the tasks of the calls it
was in, as a program's are when `main` fails. A test whose thread is
deadlocked fails with the deadlock report.

## Exit Code

`silt test` exits with code 0 if all tests pass and code 1 if any test fails.

A test does not end the run. A call of `process.exit` in a test, or in a task
of a call it made, fails that test, whatever the code, and the run goes on with the
next test:

```text
  FAIL math_test.silt::test_bad_input
    error[runtime]: process.exit(2) was called in a test: a test ends by returning, or by failing an assertion
```

To test a tool's exit code, test the function that decides it (one that
returns the code), and let `main` pass it to `process.exit`.
