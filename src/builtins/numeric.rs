//! Numeric builtin functions (`int.*`, `float.*`, `math.*`).

use super::typed::builtins;
use crate::typeinfo::bv;
use crate::value::{Float, Value};
use crate::vm::VmError;

/// Where the number in `s` goes wrong, for `InvalidDigit(offset)`: the
/// byte offset, in `s` as written (before the whitespace around it is
/// trimmed), of the first byte that cannot continue the number, or the
/// length of the trimmed number where it stops short (`"1e"` is 2, a
/// lone sign 1). `decimal` reads the grammar of `float.parse` (a
/// fraction and an exponent), else the one of `int.parse` (a sign and
/// digits); the digits are those of `radix`.
fn invalid_digit_offset(s: &str, decimal: bool, radix: u32) -> i64 {
    let lead = s.len() - s.trim_start().len();
    let bytes = s.trim().as_bytes();
    let digits = |from: usize| {
        bytes[from..]
            .iter()
            .take_while(|b| char::from(**b).is_digit(radix))
            .count()
    };
    let mut at = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
    let whole = digits(at);
    at += whole;
    let mut fraction = 0;
    if decimal && bytes.get(at) == Some(&b'.') {
        fraction = digits(at + 1);
        at += 1 + fraction;
    }
    if whole + fraction > 0 && decimal && matches!(bytes.get(at), Some(b'e' | b'E')) {
        at += 1;
        at += usize::from(matches!(bytes.get(at), Some(b'+' | b'-')));
        at += digits(at);
    }
    (lead + at) as i64
}

/// Classify a `ParseIntError` into the matching `IntError` variant.
/// `s` is the original input, used to recover an offset for
/// `InvalidDigit` since `IntErrorKind::InvalidDigit` doesn't expose one.
fn classify_int_parse_error(err: &std::num::ParseIntError, s: &str, radix: u32) -> Value {
    use std::num::IntErrorKind;
    match err.kind() {
        IntErrorKind::Empty => Value::variant(bv::INT_EMPTY, []),
        IntErrorKind::PosOverflow => Value::variant(bv::INT_OVERFLOW, []),
        IntErrorKind::NegOverflow => Value::variant(bv::INT_UNDERFLOW, []),
        // InvalidDigit, Zero, and any future-added kinds fall through
        // to `InvalidDigit(offset)`: std doesn't promise IntErrorKind
        // stays exhaustive, so the `_` arm is not dead code even today.
        _ => {
            let offset = invalid_digit_offset(s, false, radix);
            Value::variant(bv::INT_INVALID_DIGIT, [Value::Int(offset)])
        }
    }
}

/// Classify a `ParseFloatError` into the matching `FloatError` variant:
/// a text of nothing but whitespace is `Empty`, any other one
/// `InvalidDigit(offset)` (a lone sign too, as for `int.parse`).
/// (An out-of-range literal parses, to an infinity: `float.parse` tells
/// it apart itself.)
fn classify_float_parse_error(s: &str) -> Value {
    if s.trim().is_empty() {
        return Value::variant(bv::FLOAT_EMPTY, []);
    }
    let offset = invalid_digit_offset(s, true, 10);
    Value::variant(bv::FLOAT_INVALID_DIGIT, [Value::Int(offset)])
}

/// What a variant of `IntError` or of `FloatError` reads as: the two
/// enums have the same variants.
fn parse_error_text(tag: &str, fields: &[Value]) -> Option<String> {
    Some(match (tag, fields) {
        ("Empty", []) => "cannot parse empty string".to_string(),
        ("InvalidDigit", [Value::Int(offset)]) => {
            format!("invalid digit at byte {offset}")
        }
        ("Overflow", []) => "number too large".to_string(),
        ("Underflow", []) => "number too small".to_string(),
        _ => return None,
    })
}

/// What `IntError`'s `message` says of the variant `tag` with `fields`:
/// `None` if they are no variant of it.
pub(crate) fn int_error_text(tag: &str, fields: &[Value]) -> Option<String> {
    parse_error_text(tag, fields)
}

/// What `FloatError`'s `message` says of the variant `tag` with
/// `fields`: `None` if they are no variant of it. It has the variants
/// of `IntError` and `NotFinite`; a literal out of range is said by the
/// `Float` bound it passes.
pub(crate) fn float_error_text(tag: &str, fields: &[Value]) -> Option<String> {
    match (tag, fields) {
        ("NotFinite", []) => Some("not a finite number".to_string()),
        ("Overflow", []) => Some("number out of range: above float.max_value".to_string()),
        ("Underflow", []) => Some("number out of range: below float.min_value".to_string()),
        _ => parse_error_text(tag, fields),
    }
}

/// Build a `Float` from the result of a float operation, raising the
/// error `msg` names when the result is NaN or infinite: a `Float` is
/// always finite, so an operation with no finite result fails the way
/// integer overflow does.
pub(crate) fn checked_float(f: f64, msg: impl FnOnce() -> String) -> Result<Value, VmError> {
    Float::new(f)
        .map(Value::Float)
        .ok_or_else(|| VmError::new(msg()))
}

/// The radix `radix` of `int.<name>`: one from 2 to 36 (the digits 0-9
/// and then a-z), else a runtime error.
fn radix_of(name: &str, radix: i64) -> Result<u32, VmError> {
    match u32::try_from(radix) {
        Ok(r @ 2..=36) => Ok(r),
        _ => Err(VmError::new(format!(
            "int.{name}: radix must be from 2 to 36, got {radix}"
        ))),
    }
}

/// `int.*`
/// The text of `f` with exactly `decimals` decimal places, rounded to
/// the nearest, a tie away from zero (`2.5` is `3`, `-0.125` to two
/// places `-0.13`), as `float.round` rounds one.
fn decimal_text(f: f64, decimals: usize) -> String {
    // Rust writes the exact value rounded to the nearest, but a tie to
    // even. A number is a tie at `decimals` places only when its last
    // binary digit is worth 2^-(decimals + 1): its text at one place
    // more is then exact, and ends in the 5 that is half a unit.
    let bits = f.abs().to_bits();
    let (mantissa, exponent) = match bits >> 52 {
        0 => (bits, -1074),
        biased => ((bits & ((1 << 52) - 1)) | (1 << 52), biased as i64 - 1075),
    };
    let tie = mantissa != 0
        && f.is_finite()
        && exponent + i64::from(mantissa.trailing_zeros()) == -(decimals as i64) - 1;
    if !tie {
        return format!("{f:.decimals$}");
    }
    let exact = format!("{f:.prec$}", prec = decimals + 1);
    let mut text = exact[..exact.len() - 1]
        .trim_end_matches('.')
        .as_bytes()
        .to_vec();
    // One unit more in the last place, carried leftwards.
    let mut carry = true;
    for b in text.iter_mut().rev().filter(|b| b.is_ascii_digit()) {
        match *b {
            b'9' => *b = b'0',
            _ => {
                *b += 1;
                carry = false;
                break;
            }
        }
    }
    if carry {
        let first_digit = usize::from(text.first() == Some(&b'-'));
        text.insert(first_digit, b'1');
    }
    String::from_utf8(text).expect("ASCII digits")
}

pub(crate) mod int {
    use super::*;

    builtins! {
        fn parse(s: &str) -> Value {
            match s.trim().parse::<i64>() {
                Ok(n) => Value::variant(bv::OK, [Value::Int(n)]),
                Err(e) => Value::variant(bv::ERR, [classify_int_parse_error(&e, s, 10)]),
            }
        }

        // As `int.parse`, in the radix `radix`: its letters of either
        // case.
        fn parse_radix(s: &str, radix: i64) -> Result<Value, VmError> {
            let radix = radix_of("parse_radix", radix)?;
            Ok(match i64::from_str_radix(s.trim(), radix) {
                Ok(n) => Value::variant(bv::OK, [Value::Int(n)]),
                Err(e) => Value::variant(bv::ERR, [classify_int_parse_error(&e, s, radix)]),
            })
        }

        // Lower-case letters, and a `-` in front of a negative number.
        fn to_radix(n: i64, radix: i64) -> Result<String, VmError> {
            let radix = radix_of("to_radix", radix)?;
            let mut magnitude = n.unsigned_abs();
            let mut digits = Vec::new();
            loop {
                let digit = (magnitude % u64::from(radix)) as u32;
                digits.push(char::from_digit(digit, radix).expect("a digit of the radix"));
                magnitude /= u64::from(radix);
                if magnitude == 0 {
                    break;
                }
            }
            if n < 0 {
                digits.push('-');
            }
            Ok(digits.into_iter().rev().collect())
        }

        // `base` to the power `exponent`. A result outside the Ints is
        // the overflow `*` raises; a negative exponent, which has no
        // Int result, is a runtime error too.
        fn pow(base: i64, exponent: i64) -> Result<i64, VmError> {
            let overflow = || VmError::new(format!("integer overflow: pow({base}, {exponent})"));
            if exponent < 0 {
                return Err(VmError::new(format!(
                    "int.pow: negative exponent {exponent}"
                )));
            }
            match u32::try_from(exponent) {
                Ok(exponent) => base.checked_pow(exponent).ok_or_else(overflow),
                // Beyond `u32`: only 0, 1 and -1 stay in range.
                Err(_) => match base {
                    0 | 1 => Ok(base),
                    -1 => Ok(if exponent % 2 == 0 { 1 } else { -1 }),
                    _ => Err(overflow()),
                },
            }
        }

        fn bit_and(a: i64, b: i64) -> i64 {
            a & b
        }

        fn bit_or(a: i64, b: i64) -> i64 {
            a | b
        }

        fn bit_xor(a: i64, b: i64) -> i64 {
            a ^ b
        }

        // The 64 bits of `n` moved `count` places up: the bits moved
        // past the top are gone, 0s come in at the bottom. A count of
        // 64 or more gives 0; a negative count is 0, as every count in
        // the standard library.
        fn shift_left(n: i64, count: i64) -> i64 {
            match count {
                ..0 => n,
                0..64 => n << count,
                _ => 0,
            }
        }

        // The 64 bits of `n` moved `count` places down, the sign kept:
        // `n` divided by 2^count, rounded down. A count of 64 or more
        // gives 0 for a number of 0 or more and -1 for a negative one;
        // a negative count is 0.
        fn shift_right(n: i64, count: i64) -> i64 {
            n >> count.clamp(0, 63)
        }

        fn abs(n: i64) -> Result<i64, VmError> {
            n.checked_abs()
                .ok_or_else(|| VmError::new(format!("integer overflow: abs({n})")))
        }

        fn min(a: i64, b: i64) -> i64 {
            a.min(b)
        }

        fn max(a: i64, b: i64) -> i64 {
            a.max(b)
        }

        fn clamp(x: i64, lo: i64, hi: i64) -> Result<i64, VmError> {
            if lo > hi {
                return Err(VmError::new(format!(
                    "int.clamp: invalid bounds: lo ({lo}) > hi ({hi})"
                )));
            }
            Ok(x.clamp(lo, hi))
        }
    }
}

/// `Int.from_int` and `Float.from_int`: the methods of the built-in
/// impls of `Number` (`builtins::applicative::IMPLS`).
pub(crate) mod number {
    /// `Int.from_int`: the Int itself.
    pub(crate) mod int {
        use super::super::*;

        builtins! {
            fn from_int(n: i64) -> i64 {
                n
            }
        }
    }

    /// `Float.from_int`: the Float nearest the Int, the one way to turn
    /// an Int into a Float.
    pub(crate) mod float {
        use super::super::*;

        builtins! {
            fn from_int(n: i64) -> f64 {
                n as f64
            }
        }
    }
}

/// `float.*`
pub(crate) mod float {
    use super::*;

    builtins! {
        fn parse(s: &str) -> Value {
            match s.trim().parse::<f64>() {
                // Rust also parses `"inf"`, `"NaN"` and out-of-range
                // literals such as `"1e400"` (to ±infinity). None of them
                // is a `Float`: a spelled-out infinity or NaN is
                // `NotFinite`, and an out-of-range literal overflows the
                // way `int.parse` does (`Underflow` below the range,
                // `Overflow` above it).
                Ok(n) => match Float::new(n) {
                    Some(n) => Value::variant(bv::OK, [Value::Float(n)]),
                    None => {
                        let spelled = s.trim().trim_start_matches(['+', '-']);
                        let err = if !spelled.starts_with(|c: char| c.is_ascii_digit() || c == '.')
                        {
                            Value::variant(bv::FLOAT_NOT_FINITE, [])
                        } else if n < 0.0 {
                            Value::variant(bv::FLOAT_UNDERFLOW, [])
                        } else {
                            Value::variant(bv::FLOAT_OVERFLOW, [])
                        };
                        Value::variant(bv::ERR, [err])
                    }
                },
                Err(_) => Value::variant(bv::ERR, [classify_float_parse_error(s)]),
            }
        }

        fn round(f: f64) -> f64 {
            f.round()
        }

        fn ceil(f: f64) -> f64 {
            f.ceil()
        }

        fn floor(f: f64) -> f64 {
            f.floor()
        }

        fn abs(f: f64) -> f64 {
            f.abs()
        }

        fn to_int(f: f64) -> Result<i64, VmError> {
            // The Ints are the Floats from `i64::MIN`, which is a Float,
            // to below 2^63: `i64::MAX` is no Float, and the Float
            // nearest to it is 2^63, the first that is too large. (`as`
            // would give the nearest Int for a number outside.)
            const LEAST: f64 = i64::MIN as f64;
            const TOO_LARGE: f64 = 9223372036854775808.0;
            if !(LEAST..TOO_LARGE).contains(&f) {
                // (Shown as a program prints the Float.)
                let mut shown = String::new();
                // (Writing to a String does not fail.)
                let _ = Float::write_text(f, &mut shown);
                return Err(VmError::new(format!(
                    "float.to_int: {shown} is outside the Int range"
                )));
            }
            Ok(f as i64)
        }

        fn min(a: f64, b: f64) -> f64 {
            a.min(b)
        }

        fn max(a: f64, b: f64) -> f64 {
            a.max(b)
        }

        fn clamp(x: f64, lo: Float, hi: Float) -> Result<f64, VmError> {
            if lo > hi {
                return Err(VmError::new(format!(
                    "float.clamp: invalid bounds: lo ({lo}) > hi ({hi})"
                )));
            }
            Ok(x.clamp(lo.get(), hi.get()))
        }

        // Rust's `{:.prec$}` formatter backs precision with a u16 and
        // panics with "Formatting argument out of range" for any value
        // above `u16::MAX` (65535): such a precision is an error here.
        fn format(f: f64, decimals: i64) -> Result<String, VmError> {
            if decimals < 0 {
                return Err(VmError::new(
                    "float.format: decimals must be non-negative".into(),
                ));
            }
            let Ok(prec) = u16::try_from(decimals) else {
                return Err(VmError::new(format!(
                    "float.format: decimals {decimals} exceeds maximum precision of 65535"
                )));
            };
            let text = decimal_text(f, prec.into());
            // A number that rounds to zero from below is written as
            // zero is: no Float is `-0.0`, and no text of one is.
            Ok(match text.strip_prefix('-') {
                Some(zero) if zero.bytes().all(|b| b == b'0' || b == b'.') => zero.to_string(),
                _ => text,
            })
        }
    }
}

/// `math.*`
pub(crate) mod math {
    use super::*;

    builtins! {
        fn sqrt(f: Float) -> Result<f64, VmError> {
            if f.get() < 0.0 {
                return Err(VmError::new(format!("math.sqrt of a negative number: {f}")));
            }
            Ok(f.get().sqrt())
        }

        fn pow(base: Float, exp: Float) -> Result<Value, VmError> {
            let result = base.get().powf(exp.get());
            if result.is_nan() {
                return Err(VmError::new(format!(
                    "math.pow of a negative number to a fractional power: {base} ^ {exp}"
                )));
            }
            if base.get() == 0.0 && exp.get() < 0.0 {
                return Err(VmError::new(format!(
                    "math.pow of zero to a negative power: {base} ^ {exp}"
                )));
            }
            checked_float(result, || format!("math.pow overflow: {base} ^ {exp}"))
        }

        fn log(f: Float) -> Result<f64, VmError> {
            if f.get() <= 0.0 {
                return Err(VmError::new(format!(
                    "math.log of a number that is not positive: {f}"
                )));
            }
            Ok(f.get().ln())
        }

        fn log10(f: Float) -> Result<f64, VmError> {
            if f.get() <= 0.0 {
                return Err(VmError::new(format!(
                    "math.log10 of a number that is not positive: {f}"
                )));
            }
            Ok(f.get().log10())
        }

        fn sin(f: f64) -> f64 {
            f.sin()
        }

        fn cos(f: f64) -> f64 {
            f.cos()
        }

        fn tan(f: Float) -> Result<Value, VmError> {
            checked_float(f.get().tan(), || format!("math.tan overflow: {f}"))
        }

        fn asin(f: Float) -> Result<f64, VmError> {
            if !(-1.0..=1.0).contains(&f.get()) {
                return Err(VmError::new(format!(
                    "math.asin of a number outside -1..1: {f}"
                )));
            }
            Ok(f.get().asin())
        }

        fn acos(f: Float) -> Result<f64, VmError> {
            if !(-1.0..=1.0).contains(&f.get()) {
                return Err(VmError::new(format!(
                    "math.acos of a number outside -1..1: {f}"
                )));
            }
            Ok(f.get().acos())
        }

        fn atan(f: f64) -> f64 {
            f.atan()
        }

        fn atan2(y: f64, x: f64) -> f64 {
            y.atan2(x)
        }

        fn exp(f: Float) -> Result<Value, VmError> {
            checked_float(f.get().exp(), || format!("math.exp overflow: {f}"))
        }

        fn random(vm) -> f64 {
            vm.runtime.random()
        }

        // An Int of `lo..=hi`, both ends included, each as likely.
        fn random_int(vm, lo: i64, hi: i64) -> Result<i64, VmError> {
            if lo > hi {
                return Err(VmError::new(format!(
                    "math.random_int: invalid bounds: lo ({lo}) > hi ({hi})"
                )));
            }
            Ok(vm.runtime.random_int(lo, hi))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::{Step, Vm};

    fn to_int(f: f64) -> Result<Value, VmError> {
        let mut vm = Vm::new(crate::vm::HostIo::process());
        let f = Value::Float(Float::new(f).expect("a finite number"));
        match float::to_int(&mut vm, &[f]).expect("a Float is the argument")? {
            Step::Done(value) => Ok(value),
            _ => panic!("float.to_int gives a value"),
        }
    }

    // ── float.to_int: a Float outside the Int range ───────────────

    #[test]
    fn float_to_int_rejects_positive_out_of_range() {
        let err = to_int(1.0e20).expect_err("expected out-of-range error");
        assert_eq!(
            err.message,
            "float.to_int: 100000000000000000000.0 is outside the Int range"
        );
    }

    #[test]
    fn float_to_int_rejects_negative_out_of_range() {
        let err = to_int(-1.0e20).expect_err("expected out-of-range error");
        assert_eq!(
            err.message,
            "float.to_int: -100000000000000000000.0 is outside the Int range"
        );
    }

    #[test]
    fn float_to_int_rejects_exactly_i64_max_plus_one() {
        // `i64::MAX + 1 == 9223372036854775808` is exactly representable in
        // f64 (it's 2^63) and is the first value strictly outside the i64
        // range. It must be rejected.
        let err = to_int(9_223_372_036_854_775_808.0)
            .expect_err("expected out-of-range for i64::MAX + 1");
        assert_eq!(
            err.message,
            "float.to_int: 9223372036854776000.0 is outside the Int range"
        );
    }

    #[test]
    fn float_to_int_accepts_near_i64_max_after_rounding() {
        // f64 cannot exactly represent 9_223_372_036_854_775_000; it rounds
        // *down* to 9_223_372_036_854_774_784 (the nearest representable
        // value below i64::MAX), which IS within range and must convert
        // successfully. This pins the boundary so a future tightening of
        // the range check doesn't accidentally reject it.
        let v = to_int(9_223_372_036_854_775_000.0).expect("in-range after rounding");
        assert!(matches!(v, Value::Int(_)));
    }

    #[test]
    fn float_to_int_truncates_positive_fraction() {
        let v = to_int(42.5).expect("42.5 should convert");
        assert!(matches!(v, Value::Int(42)), "expected Int(42), got {v:?}");
    }

    #[test]
    fn float_to_int_truncates_negative_fraction() {
        let v = to_int(-42.5).expect("-42.5 should convert");
        assert!(matches!(v, Value::Int(-42)), "expected Int(-42), got {v:?}");
    }

    #[test]
    fn float_to_int_accepts_zero() {
        let v = to_int(0.0).expect("0.0 should convert");
        assert!(matches!(v, Value::Int(0)), "expected Int(0), got {v:?}");
        let v = to_int(-0.0).expect("-0.0 should convert");
        assert!(matches!(v, Value::Int(0)), "expected Int(0), got {v:?}");
    }

    #[test]
    fn float_to_int_accepts_i64_min_exact() {
        // i64::MIN is exactly representable as f64.
        let v = to_int(i64::MIN as f64).expect("i64::MIN as f64 should convert");
        assert!(
            matches!(v, Value::Int(n) if n == i64::MIN),
            "expected Int(i64::MIN), got {v:?}"
        );
    }
}
