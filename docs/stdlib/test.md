---
title: "test"
section: "Standard Library"
order: 17
---

# test

Assertion functions for test scripts. A failed assertion is a runtime error
that says what failed, with both values and the place of the call.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `assert` | `(Bool) -> ()` | Assert a condition is true |
| `assert_approx` | `(Float, Float, Float) -> ()` | Assert two floats differ by at most the tolerance |
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


## `test.assert_approx`

```
test.assert_approx(a: Float, b: Float, tolerance: Float) -> ()
```

Panics if `a` and `b` differ by more than `tolerance`: it passes when
`|a - b| <= tolerance`. The tolerance is absolute, and a negative one is an
error. For floats that come out of arithmetic, where `==` asks too much.

```silt
import test

fn main() {
  test.assert_approx(0.1 + 0.2, 0.3, 0.000001)
  test.assert_approx(2.0, 2.5, 0.5)
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
