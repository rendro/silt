//! The Float value.
//!
//! A Float is finite, and is never `-0.0`: an operation whose result
//! would be a NaN or an infinity is an error where it is made, and
//! there is one zero. So any two Floats are in order, and equal Floats
//! are the same bits: a Float is compared, ordered and hashed as it
//! is, with no case of its own anywhere.
//!
//! [`Float::new`] is the one way to make a Float of a number that may
//! be none; what cannot leave the Floats (an Int as a Float, a Float
//! negated) is made here.

use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};

/// A Float: a finite `f64` that is not `-0.0`.
#[derive(Clone, Copy, PartialEq)]
pub struct Float(f64);

impl Float {
    /// The Float `f`, with `-0.0` as `0.0`; `None` for a NaN or an
    /// infinity, which are no Floats.
    pub fn new(f: f64) -> Option<Float> {
        // (`-0.0 == 0.0`, so the one test finds both zeros.)
        f.is_finite()
            .then_some(Float(if f == 0.0 { 0.0 } else { f }))
    }

    /// The number.
    pub fn get(self) -> f64 {
        self.0
    }
}

/// An Int as a Float: the Float nearest to it.
impl From<i64> for Float {
    fn from(n: i64) -> Float {
        Float(n as f64)
    }
}

/// A Float negated is a Float; the zero is its own.
impl std::ops::Neg for Float {
    type Output = Float;

    fn neg(self) -> Float {
        Float(if self.0 == 0.0 { 0.0 } else { -self.0 })
    }
}

impl Eq for Float {}

impl PartialOrd for Float {
    fn partial_cmp(&self, other: &Float) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Float {
    fn cmp(&self, other: &Float) -> Ordering {
        // (Neither is a NaN, and there is one zero: the order of the
        // bits is the order of the numbers.)
        self.0.total_cmp(&other.0)
    }
}

impl Hash for Float {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.to_bits().hash(state);
    }
}

impl Float {
    /// Write the number `n` as a Float is shown: its digits with a
    /// point among them, and no exponent. A whole number ends in `.0`,
    /// so the text is never an Int's; a number of twenty-two digits is
    /// written with all of them. This is the one place that makes the
    /// text. (The lexer writes the number of a Float token with it: it
    /// makes no token of a literal that is not finite.)
    pub(crate) fn write_text(n: f64, out: &mut impl fmt::Write) -> fmt::Result {
        // (Rust writes a whole number without a point, and any other
        // with one.)
        match n.fract() == 0.0 {
            true => write!(out, "{n}.0"),
            false => write!(out, "{n}"),
        }
    }
}

/// The text of a Float, wherever one is shown (`Float::write_text`).
/// A width and an alignment place that text (`{:>8}`); a precision asks
/// for its own decimals, and the number is written with exactly those
/// (`{:.2}`), as an `f64` is.
impl fmt::Display for Float {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if f.precision().is_some() {
            return fmt::Display::fmt(&self.0, f);
        }
        if f.width().is_none() {
            return Float::write_text(self.0, f);
        }
        let mut text = String::new();
        Float::write_text(self.0, &mut text)?;
        f.pad(&text)
    }
}

impl fmt::Debug for Float {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}
