---
title: "test"
section: "Standard Library"
order: 15
---

# test

Assertion functions for test scripts. A failed assertion is a runtime error
that says what failed, with both values and the place of the call.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `assert` | `(Bool) -> ()` | Assert a condition is true |
| `assert_eq` | `(a, a) -> () where a: Equal + Display` | Assert two values are equal |
| `assert_ne` | `(a, a) -> () where a: Equal + Display` | Assert two values are not equal |


## `test.assert`

```
test.assert(condition: Bool) -> ()
```

Panics if `condition` is `false`.

```silt
import test

fn main() {
  test.assert(1 + 1 == 2)
}
```


## `test.assert_eq`

```
test.assert_eq(left: a, right: a) -> () where a: Equal + Display
```

Panics if `left != right`, displaying both values.

```silt
import list
import test

fn main() {
  test.assert_eq(list.length([1, 2, 3]), 3)
  test.assert_eq(1 + 1, 2)
}
```


## `test.assert_ne`

```
test.assert_ne(left: a, right: a) -> () where a: Equal + Display
```

Panics if `left == right`, displaying both values.

```silt
import test

fn main() {
  test.assert_ne("hello", "world")
}
```
