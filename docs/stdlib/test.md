---
title: "test"
---

# test

Assertion functions for test scripts. Each accepts an optional trailing `String`
message argument.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `assert` | `(Bool, String?) -> ()` | Assert value is truthy |
| `assert_eq` | `(a, a, String?) -> ()` | Assert two values are equal |
| `assert_ne` | `(a, a, String?) -> ()` | Assert two values are not equal |


## `test.assert`

```
test.assert(condition: Bool) -> ()
test.assert(condition: Bool, message: String) -> ()
```

Panics if `condition` is `false`. The optional message is included in the error.

```silt
fn main() {
    test.assert(1 + 1 == 2)
    test.assert(1 + 1 == 2, "math should work")
}
```


## `test.assert_eq`

```
test.assert_eq(left: a, right: a) -> ()
test.assert_eq(left: a, right: a, message: String) -> ()
```

Panics if `left != right`, displaying both values.

```silt
fn main() {
    test.assert_eq(list.length([1, 2, 3]), 3)
    test.assert_eq(1 + 1, 2, "addition")
}
```


## `test.assert_ne`

```
test.assert_ne(left: a, right: a) -> ()
test.assert_ne(left: a, right: a, message: String) -> ()
```

Panics if `left == right`, displaying both values.

```silt
fn main() {
    test.assert_ne("hello", "world")
}
```
