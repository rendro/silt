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

Running this file produces:

```
  PASS math_test.silt::test_addition
  PASS math_test.silt::test_string_length
  SKIP math_test.silt::skip_test_not_ready_yet
  PASS math_test.silt::test_with_helper

4 tests: 3 passed, 0 failed, 1 skipped
```

## Filtering Tests

Use `--filter <pattern>` to run only tests whose names contain the pattern:

```
silt test --filter addition     -- runs only test_addition
silt test --filter string       -- runs only test_string_length
```

The filter matches against the function name (not the file name). Files that
cannot contain any matching test are skipped entirely.

## Assertions

The `test` module provides assertion functions. See the
[test module reference](../stdlib/test.md) for details.

| Function | Description |
|----------|-------------|
| `test.assert(condition)` | Fails if `condition` is `false` |
| `test.assert_eq(left, right)` | Fails if `left != right` |
| `test.assert_ne(left, right)` | Fails if `left == right` |

All assertions accept an optional trailing `String` message argument.

## Exit Code

`silt test` exits with code 0 if all tests pass and code 1 if any test fails.
