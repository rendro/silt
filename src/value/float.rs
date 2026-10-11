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
#[derive(Clone, Copy, PartialEq, PartialOrd)]
pub struct Float(f64);

impl Float {
    /// The Float `f`, with `-0.0` as `0.0`; `None` for a NaN or an
    /// infinity, which are no Floats.
    pub fn new(f: f64) -> Option<Float> {
        // (`-0.0 == 0.0`, so the one test finds both zeros.)
        f.is_finite().then_some(Float(if f == 0.0 { 0.0 } else { f }))
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

impl fmt::Display for Float {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::Debug for Float {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
