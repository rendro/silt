//! The tuple value.
//!
//! A tuple's elements are shared by its clones, and a clone is a
//! count. How they are stored is this file's own: the rest of silt has
//! them as a slice.

use std::ops::Deref;

use super::Value;
use super::deep::Buf;

/// A tuple: `(1, "a")`.
#[derive(Clone)]
pub struct Tuple(Buf);

impl Deref for Tuple {
    type Target = [Value];

    fn deref(&self) -> &[Value] {
        &self.0
    }
}

impl From<Vec<Value>> for Tuple {
    fn from(items: Vec<Value>) -> Tuple {
        Tuple(Buf::from(items))
    }
}

impl FromIterator<Value> for Tuple {
    fn from_iter<I: IntoIterator<Item = Value>>(items: I) -> Tuple {
        Tuple(items.into_iter().collect())
    }
}
