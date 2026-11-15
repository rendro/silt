//! The payload of a `Regex` value. Its functions are the regex
//! module's (`crate::builtins::regex`).

use std::sync::Arc;

use regex_automata::meta::Regex;

/// A `Regex` value (`regex.compile`): the pattern as it was written,
/// and the regex it compiled to. Two of one pattern are equal.
pub struct Compiled {
    pub(crate) pattern: Box<str>,
    pub(crate) re: Arc<Regex>,
}
