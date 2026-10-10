//! The builtin modules, one `module!` each: the rows in the order of
//! the module's summary table.
//!
//! A function row is `f(signature, summary, body)`: the body is the
//! function of that name among the module's typed bodies
//! (`crate::builtins::typed`); with `.feature("name")` it needs a
//! cargo feature beyond its module's, with `.optional_last()` its last
//! parameter may be left out. A constant is `k("name: Type", summary,
//! value)`.

use super::{Module, RowSpec, build_module, f, k, module};
use crate::builtins::collections::{list, map, set};
use crate::builtins::numeric::{float, int, math};
#[cfg(feature = "postgres")]
use crate::builtins::postgres;
use crate::builtins::prelude;
#[cfg(feature = "tcp")]
use crate::builtins::tcp;
use crate::builtins::{
    bytes, concurrency, core, crypto, encoding, http, io, json, regex, stream, string, time, toml,
    uuid,
};

/// The functions of the prelude: called by their bare names, each
/// shows its argument as `Display` says.
pub(super) fn prelude() -> Vec<RowSpec> {
    vec![
        f(
            "fn print(value: a) -> () where a: Display",
            "Write a value to stdout",
            prelude::print,
        ),
        f(
            "fn println(value: a) -> () where a: Display",
            "Write a value and a newline to stdout",
            prelude::println,
        ),
        f(
            "fn panic(message: a) -> Never where a: Display",
            "Stop the program with an error",
            prelude::panic,
        ),
    ]
}

#[rustfmt::skip]
pub(super) fn modules() -> Vec<Module> {
    vec![
        module! {
            name: "io",
            page: "io-fs.md",
            types: "\
                pub type IoError { IoNotFound(String), IoPermissionDenied(String), IoAlreadyExists(String), IoInvalidInput(String), IoInterrupted, IoUnexpectedEof, IoWriteZero, IoUnknown(String) }\n\
            ",
            error: "IoError",
            rows: [
                f("fn args() -> List(String)", "Command-line arguments", io::args),
                f("fn inspect(x: a) -> String", "Debug representation of any value", io::inspect),
                f("fn read_file(path: String) -> Result(String, IoError)", "Read entire file as string", io::read_file),
                f("fn read_line() -> Result(String, IoError)", "Read one line from stdin", io::read_line),
                f("fn write_file(path: String, contents: String) -> Result((), IoError)", "Write string to file", io::write_file),
            ],
        },
        module! {
            name: "string",
            page: "string.md",
            rows: [
                f("fn char_code(s: String) -> Int", "Unicode code point of first character", string::char_code),
                f("fn chars(s: String) -> List(String)", "Split string into single-character strings", string::chars),
                f("fn contains(s: String, sub: String) -> Bool", "Check if substring exists", string::contains),
                f("fn ends_with(s: String, suffix: String) -> Bool", "Check suffix", string::ends_with),
                f("fn from(x: a) -> String where a: Display", "Convert any value to its display string", string::from),
                f("fn from_char_code(code: Int) -> String", "Character from Unicode code point", string::from_char_code),
                f("fn index_of(s: String, needle: String) -> Option(Int)", "Character index of first occurrence", string::index_of),
                f("fn byte_length(s: String) -> Int", "Length in bytes", string::byte_length),
                f("fn is_alnum(s: String) -> Bool", "All chars are alphanumeric", string::is_alnum),
                f("fn is_alpha(s: String) -> Bool", "All chars are alphabetic", string::is_alpha),
                f("fn is_digit(s: String) -> Bool", "All chars are ASCII digits", string::is_digit),
                f("fn is_empty(s: String) -> Bool", "String has zero length", string::is_empty),
                f("fn is_lower(s: String) -> Bool", "All chars are lowercase", string::is_lower),
                f("fn is_upper(s: String) -> Bool", "All chars are uppercase", string::is_upper),
                f("fn is_whitespace(s: String) -> Bool", "All chars are whitespace", string::is_whitespace),
                f("fn join(xs: List(String), sep: String) -> String", "Join list with separator", string::join),
                f("fn last_index_of(s: String, needle: String) -> Option(Int)", "Character index of last occurrence", string::last_index_of),
                f("fn length(s: String) -> Int", "Length in characters", string::length),
                f("fn lines(s: String) -> List(String)", "Split on `\\n` (strips trailing `\\r`, no empty final element)", string::lines),
                f("fn pad_left(s: String, width: Int, pad: String) -> String", "Pad to width on the left", string::pad_left),
                f("fn pad_right(s: String, width: Int, pad: String) -> String", "Pad to width on the right", string::pad_right),
                f("fn repeat(s: String, n: Int) -> String", "Repeat string n times", string::repeat),
                f("fn replace(s: String, from: String, to: String) -> String", "Replace all occurrences", string::replace),
                f("fn slice(s: String, start: Int, end: Int) -> String", "Substring by character indices", string::slice),
                f("fn split(s: String, separator: String) -> List(String)", "Split on separator", string::split),
                f("fn split_at(s: String, idx: Int) -> (String, String)", "Split into two strings at character index", string::split_at),
                f("fn starts_with(s: String, prefix: String) -> Bool", "Check prefix", string::starts_with),
                f("fn starts_with_at(s: String, offset: Int, prefix: String) -> Bool", "Check prefix at a given character offset", string::starts_with_at),
                f("fn to_lower(s: String) -> String", "Convert to lowercase", string::to_lower),
                f("fn to_upper(s: String) -> String", "Convert to uppercase", string::to_upper),
                f("fn trim(s: String) -> String", "Remove leading and trailing whitespace", string::trim),
                f("fn trim_end(s: String) -> String", "Remove trailing whitespace", string::trim_end),
                f("fn trim_start(s: String) -> String", "Remove leading whitespace", string::trim_start),
            ],
        },
        module! {
            name: "int",
            page: "int-float.md",
            types: "\
                pub type ParseError { ParseEmpty, ParseInvalidDigit(Int), ParseOverflow, ParseUnderflow }\n\
            ",
            error: "ParseError",
            rows: [
                f("fn abs(n: Int) -> Int", "Absolute value", int::abs),
                f("fn clamp(x: Int, lo: Int, hi: Int) -> Int", "Clamp value to `[lo, hi]`", int::clamp),
                f("fn max(a: Int, b: Int) -> Int", "Larger of two values", int::max),
                f("fn min(a: Int, b: Int) -> Int", "Smaller of two values", int::min),
                f("fn parse(s: String) -> Result(Int, ParseError)", "Parse string to integer", int::parse),
                f("fn to_float(n: Int) -> Float", "Convert to float", int::to_float),
                f("fn to_string(n: Int) -> String", "Convert to string", int::to_string),
            ],
        },
        module! {
            name: "float",
            page: "int-float.md",
            shares: [("int", "ParseError")],
            rows: [
                f("fn abs(f: Float) -> Float", "Absolute value", float::abs),
                f("fn ceil(f: Float) -> Float", "Round up to nearest integer (as Float)", float::ceil),
                f("fn clamp(x: Float, lo: Float, hi: Float) -> Float", "Clamp value to `[lo, hi]`", float::clamp),
                f("fn floor(f: Float) -> Float", "Round down to nearest integer (as Float)", float::floor),
                f("fn max(a: Float, b: Float) -> Float", "Larger of two values", float::max),
                f("fn min(a: Float, b: Float) -> Float", "Smaller of two values", float::min),
                f("fn parse(s: String) -> Result(Float, ParseError)", "Parse string to float", float::parse),
                f("fn round(f: Float) -> Float", "Round to nearest integer (as Float)", float::round),
                f("fn to_int(f: Float) -> Int", "Truncate to integer", float::to_int),
                f("fn to_string(f: Float, decimals: Int) -> String", "Shortest round-trippable representation; with `decimals`, that many decimal places", float::to_string).optional_last(),
                k("max_value: Float", "Maximum finite value (`1.7976931348623157e+308`)", f64::MAX),
                k("min_value: Float", "Minimum finite value (`-1.7976931348623157e+308`)", f64::MIN),
                k("epsilon: Float", "Machine epsilon (`2.220446049250313e-16`)", f64::EPSILON),
                k("min_positive: Float", "Smallest positive normal (`2.2250738585072014e-308`)", f64::MIN_POSITIVE),
            ],
        },
        module! {
            name: "list",
            page: "list.md",
            types: "\
                pub type Step(a) { Stop(a), Continue(a) }\n\
            ",
            rows: [
                f("fn all(xs: List(a), f: Fn(a) -> Bool) -> Bool", "True if predicate holds for every element", list::all),
                f("fn any(xs: List(a), f: Fn(a) -> Bool) -> Bool", "True if predicate holds for at least one element", list::any),
                f("fn append(xs: List(a), elem: a) -> List(a)", "Add an element to the end", list::append),
                f("fn concat(xs: List(a), ys: List(a)) -> List(a)", "Concatenate two lists", list::concat),
                f("fn contains(xs: List(a), elem: a) -> Bool where a: Equal", "Check if element is in list", list::contains),
                f("fn drop(xs: List(a), n: Int) -> List(a)", "Remove first n elements", list::drop),
                f("fn each(xs: List(a), f: Fn(a) -> ()) -> ()", "Call function for each element (side effects)", list::each),
                f("fn enumerate(xs: List(a)) -> List((Int, a))", "Pair each element with its index", list::enumerate),
                f("fn filter(xs: List(a), f: Fn(a) -> Bool) -> List(a)", "Keep elements matching predicate", list::filter),
                f("fn filter_map(xs: List(a), f: Fn(a) -> Option(b)) -> List(b)", "Filter and transform in one pass", list::filter_map),
                f("fn find(xs: List(a), f: Fn(a) -> Bool) -> Option(a)", "First element matching predicate", list::find),
                f("fn flat_map(xs: List(a), f: Fn(a) -> List(b)) -> List(b)", "Map then flatten", list::flat_map),
                f("fn flatten(xs: List(List(a))) -> List(a)", "Flatten one level of nesting", list::flatten),
                f("fn fold(xs: List(a), init: b, f: Fn(b, a) -> b) -> b", "Reduce to a single value", list::fold),
                f("fn fold_until(xs: List(a), init: b, f: Fn(b, a) -> Step(b)) -> b", "Fold with early termination", list::fold_until),
                f("fn get(xs: List(a), i: Int) -> Option(a)", "Element at index, or None", list::get),
                f("fn group_by(xs: List(a), f: Fn(a) -> b) -> Map(b, List(a)) where b: Hash", "Group elements by key function", list::group_by),
                f("fn head(xs: List(a)) -> Option(a)", "First element, or None", list::head),
                f("fn index_of(xs: List(a), target: a) -> Option(Int) where a: Equal", "Index of first matching element, or None", list::index_of),
                f("fn intersperse(xs: List(a), sep: a) -> List(a)", "Insert separator between elements", list::intersperse),
                f("fn last(xs: List(a)) -> Option(a)", "Last element, or None", list::last),
                f("fn length(xs: List(a)) -> Int", "Number of elements", list::length),
                f("fn map(xs: List(a), f: Fn(a) -> b) -> List(b)", "Transform each element", list::map),
                f("fn max_by(xs: List(a), key: Fn(a) -> b) -> Option(a) where b: Compare", "Element with largest key, or None", list::max_by),
                f("fn min_by(xs: List(a), key: Fn(a) -> b) -> Option(a) where b: Compare", "Element with smallest key, or None", list::min_by),
                f("fn prepend(xs: List(a), elem: a) -> List(a)", "Add an element to the front", list::prepend),
                f("fn product(xs: List(Int)) -> Int", "Product of a list of ints (1 on empty)", list::product),
                f("fn product_float(xs: List(Float)) -> Float", "Product of a list of floats (1.0 on empty)", list::product_float),
                f("fn remove_at(xs: List(a), index: Int) -> List(a)", "Remove element at index (panics if out of range)", list::remove_at),
                f("fn reverse(xs: List(a)) -> List(a)", "Reverse element order", list::reverse),
                f("fn scan(xs: List(a), init: b, f: Fn(b, a) -> b) -> List(b)", "Prefix fold; returns all intermediate accumulators", list::scan),
                f("fn set(xs: List(a), index: Int, value: a) -> List(a)", "Return new list with element at index replaced", list::set),
                f("fn sort(xs: List(a)) -> List(a) where a: Compare", "Sort in natural order", list::sort),
                f("fn sort_by(xs: List(a), key: Fn(a) -> b) -> List(a) where b: Compare", "Sort by key function", list::sort_by),
                f("fn sum(xs: List(Int)) -> Int", "Sum a list of ints (0 on empty)", list::sum),
                f("fn sum_float(xs: List(Float)) -> Float", "Sum a list of floats (0.0 on empty)", list::sum_float),
                f("fn tail(xs: List(a)) -> List(a)", "All elements except the first", list::tail),
                f("fn take(xs: List(a), n: Int) -> List(a)", "Keep first n elements", list::take),
                f("fn unfold(seed: a, f: Fn(a) -> Option((b, a))) -> List(b)", "Build a list from a seed", list::unfold),
                f("fn unique(xs: List(a)) -> List(a) where a: Equal", "Remove duplicates, preserving first occurrence", list::unique),
                f("fn zip(xs: List(a), ys: List(b)) -> List((a, b))", "Pair elements from two lists", list::zip),
            ],
        },
        module! {
            name: "map",
            page: "map.md",
            rows: [
                f("fn contains(m: Map(a, b), key: a) -> Bool where a: Hash", "Check if key exists", map::contains),
                f("fn delete(m: Map(a, b), key: a) -> Map(a, b) where a: Hash", "Remove a key", map::delete),
                f("fn each(m: Map(a, b), f: Fn(a, b) -> ()) -> ()", "Iterate over all entries", map::each),
                f("fn entries(m: Map(a, b)) -> List((a, b))", "All key-value pairs as tuples", map::entries),
                f("fn filter(m: Map(a, b), f: Fn(a, b) -> Bool) -> Map(a, b)", "Keep entries matching predicate", map::filter),
                f("fn from_entries(entries: List((a, b))) -> Map(a, b) where a: Hash", "Build map from tuple list", map::from_entries),
                f("fn get(m: Map(a, b), k: a) -> Option(b) where a: Hash", "Look up value by key", map::get),
                f("fn keys(m: Map(a, b)) -> List(a)", "All keys as a list", map::keys),
                f("fn length(m: Map(a, b)) -> Int", "Number of entries", map::length),
                f("fn map(m: Map(a, b), f: Fn(a, b) -> (c, d)) -> Map(c, d) where c: Hash", "Transform all entries", map::map),
                f("fn merge(m1: Map(a, b), m2: Map(a, b)) -> Map(a, b) where a: Hash", "Merge two maps (right wins)", map::merge),
                f("fn set(m: Map(a, b), k: a, v: b) -> Map(a, b) where a: Hash", "Insert or update a key", map::set),
                f("fn update(m: Map(a, b), key: a, default: b, f: Fn(b) -> b) -> Map(a, b) where a: Hash", "Update existing or insert default", map::update),
                f("fn values(m: Map(a, b)) -> List(b)", "All values as a list", map::values),
            ],
        },
        module! {
            name: "result",
            page: "result-option.md",
            rows: [
                f("fn flat_map(r: Result(a, b), f: Fn(a) -> Result(c, b)) -> Result(c, b)", "Chain fallible operations", core::result::flat_map),
                f("fn flatten(r: Result(Result(a, b), b)) -> Result(a, b)", "Remove one nesting level", core::result::flatten),
                f("fn is_err(r: Result(a, b)) -> Bool", "True if Err", core::result::is_err),
                f("fn is_ok(r: Result(a, b)) -> Bool", "True if Ok", core::result::is_ok),
                f("fn map_err(r: Result(a, b), f: Fn(b) -> c) -> Result(a, c)", "Transform the error", core::result::map_err),
                f("fn map_ok(r: Result(a, b), f: Fn(a) -> c) -> Result(c, b)", "Transform the success value", core::result::map_ok),
                f("fn unwrap_or(r: Result(a, b), default: a) -> a", "Extract value or use default", core::result::unwrap_or),
            ],
        },
        module! {
            name: "option",
            page: "result-option.md",
            rows: [
                f("fn flat_map(opt: Option(a), f: Fn(a) -> Option(b)) -> Option(b)", "Chain optional operations", core::option::flat_map),
                f("fn is_none(opt: Option(a)) -> Bool", "True if None", core::option::is_none),
                f("fn is_some(opt: Option(a)) -> Bool", "True if Some", core::option::is_some),
                f("fn map(opt: Option(a), f: Fn(a) -> b) -> Option(b)", "Transform the inner value", core::option::map),
                f("fn to_result(opt: Option(a), error: b) -> Result(a, b)", "Convert to Result with error value", core::option::to_result),
                f("fn unwrap_or(opt: Option(a), default: a) -> a", "Extract value or use default", core::option::unwrap_or),
            ],
        },
        module! {
            name: "test",
            page: "test.md",
            rows: [
                f("fn assert(condition: Bool, message: String) -> ()", "Assert value is truthy", core::test::assert).optional_last(),
                f("fn assert_eq(left: a, right: a, message: String) -> () where a: Equal + Display", "Assert two values are equal", core::test::assert_eq).optional_last(),
                f("fn assert_ne(left: a, right: a, message: String) -> () where a: Equal + Display", "Assert two values are not equal", core::test::assert_ne).optional_last(),
            ],
        },
        module! {
            name: "channel",
            page: "channel-task.md",
            types: "\
                pub type ChannelResult(a) { Message(a), Closed, Sent, Empty }\n\
                pub type ChannelOp(a) { Recv(Channel(a)), Send(Channel(a), a) }\n\
                pub type ChannelError { ChannelTimeout, ChannelClosed }\n\
            ",
            derives: [("ChannelOp", &[])],
            error: "ChannelError",
            rows: [
                f("fn close(ch: Channel(a)) -> ()", "Close the channel", concurrency::channel::close),
                f("fn each(ch: Channel(a), f: Fn(a) -> b) -> ()", "Iterate until channel closes", concurrency::channel::each),
                f("fn new(capacity: Int) -> Channel(a)", "Create a channel (0 = rendezvous, N = buffered)", concurrency::channel::new).optional_last(),
                f("fn receive(ch: Channel(a)) -> ChannelResult(a)", "Blocking receive", concurrency::channel::receive),
                f("fn recv_timeout(ch: Channel(a), dur: Duration) -> Result(a, ChannelError)", "Blocking receive with a timeout", concurrency::channel::recv_timeout),
                f("fn select(ops: List(ChannelOp(a))) -> (Channel(a), ChannelResult(a))", "Wait on multiple channels (each op is `Recv(ch)` or `Send(ch, v)`)", concurrency::channel::select),
                f("fn send(ch: Channel(a), value: a) -> ()", "Blocking send", concurrency::channel::send),
                f("fn timeout(ms: Int) -> Channel(a)", "Create a channel that closes after N ms", concurrency::channel::timeout),
                f("fn try_receive(ch: Channel(a)) -> ChannelResult(a)", "Non-blocking receive", concurrency::channel::try_receive),
                f("fn try_send(ch: Channel(a), value: a) -> Bool", "Non-blocking send", concurrency::channel::try_send),
            ],
        },
        module! {
            name: "task",
            page: "channel-task.md",
            opaque: [("Handle", 1)],
            rows: [
                f("fn cancel(handle: Handle(a)) -> ()", "Request cancellation of a task (cooperative; see details below)", concurrency::task::cancel),
                f("fn deadline(dur: Duration, f: Fn() -> a) -> a", "Run a callback with a scoped I/O deadline", concurrency::task::deadline),
                f("fn join(handle: Handle(a)) -> a", "Wait for a task to complete", concurrency::task::join),
                f("fn spawn(f: Fn() -> a) -> Handle(a)", "Spawn a new lightweight task", concurrency::task::spawn),
                f("fn spawn_until(dur: Duration, f: Fn() -> a) -> Handle(a)", "Spawn a task scoped by a deadline", concurrency::task::spawn_until),
            ],
        },
        module! {
            name: "regex",
            page: "regex.md",
            types: "\
                pub type RegexError { RegexInvalidPattern(String, Int), RegexTooBig }\n\
            ",
            error: "RegexError",
            rows: [
                f("fn captures(pattern: String, text: String) -> Option(List(String))", "Capture groups from first match", regex::captures),
                f("fn captures_all(pattern: String, text: String) -> List(List(String))", "Capture groups from all matches", regex::captures_all),
                f("fn captures_named(pattern: String, text: String) -> Option(Map(String, String))", "Named capture groups from first match", regex::captures_named),
                f("fn find(pattern: String, text: String) -> Option(String)", "First match", regex::find),
                f("fn find_all(pattern: String, text: String) -> List(String)", "All matches", regex::find_all),
                f("fn is_match(pattern: String, text: String) -> Bool", "Test if pattern matches", regex::is_match),
                f("fn replace(pattern: String, text: String, replacement: String) -> String", "Replace first match", regex::replace),
                f("fn replace_all(pattern: String, text: String, replacement: String) -> String", "Replace all matches", regex::replace_all),
                f("fn replace_all_with(pattern: String, text: String, f: Fn(String) -> String) -> String", "Replace all with callback", regex::replace_all_with),
                f("fn split(pattern: String, text: String) -> List(String)", "Split on pattern", regex::split),
            ],
        },
        module! {
            name: "json",
            page: "json.md",
            types: "\
                pub type JsonError { JsonSyntax(String, Int), JsonTypeMismatch(String, String), JsonMissingField(String), JsonUnknown(String) }\n\
            ",
            error: "JsonError",
            rows: [
                f("fn parse(s: String, type a) -> Result(a, JsonError)", "Parse JSON object into record", json::parse),
                f("fn parse_list(s: String, type a) -> Result(List(a), JsonError)", "Parse JSON array into record list", json::parse_list),
                f("fn parse_map(s: String, type a) -> Result(Map(String, a), JsonError)", "Parse JSON object into map", json::parse_map),
                f("fn pretty(value: a) -> String", "Pretty-print value as JSON", json::pretty),
                f("fn stringify(value: a) -> String", "Serialize value as compact JSON", json::stringify),
            ],
        },
        module! {
            name: "toml",
            page: "toml.md",
            types: "\
                pub type TomlError { TomlSyntax(String, Int), TomlTypeMismatch(String, String), TomlMissingField(String), TomlUnknown(String) }\n\
            ",
            error: "TomlError",
            rows: [
                f("fn parse(s: String, type a) -> Result(a, TomlError)", "Parse a TOML document (top-level table) into a record", toml::parse),
                f("fn parse_list(s: String, type a) -> Result(List(a), TomlError)", "Parse a single `[[items]]` section into a list of records", toml::parse_list),
                f("fn parse_map(s: String, type a) -> Result(Map(String, a), TomlError)", "Parse a top-level table into a map", toml::parse_map),
                f("fn pretty(value: a) -> Result(String, TomlError)", "Pretty-print a value as TOML", toml::pretty),
                f("fn stringify(value: a) -> Result(String, TomlError)", "Serialize a value as TOML", toml::stringify),
            ],
        },
        module! {
            name: "set",
            page: "set.md",
            rows: [
                f("fn contains(s: Set(a), elem: a) -> Bool where a: Hash", "Check membership", set::contains),
                f("fn difference(a: Set(a), b: Set(a)) -> Set(a)", "Elements in first but not second", set::difference),
                f("fn each(s: Set(a), f: Fn(a) -> ()) -> ()", "Iterate over all elements", set::each),
                f("fn filter(s: Set(a), f: Fn(a) -> Bool) -> Set(a)", "Keep elements matching predicate", set::filter),
                f("fn fold(s: Set(a), init: b, f: Fn(b, a) -> b) -> b", "Reduce to a single value", set::fold),
                f("fn from_list(xs: List(a)) -> Set(a) where a: Hash", "Create set from list", set::from_list),
                f("fn insert(s: Set(a), elem: a) -> Set(a) where a: Hash", "Add an element", set::insert),
                f("fn intersection(a: Set(a), b: Set(a)) -> Set(a)", "Elements in both sets", set::intersection),
                f("fn is_subset(a: Set(a), b: Set(a)) -> Bool", "True if first is subset of second", set::is_subset),
                f("fn length(s: Set(a)) -> Int", "Number of elements", set::length),
                f("fn map(s: Set(a), f: Fn(a) -> b) -> Set(b) where b: Hash", "Transform each element", set::map),
                f("fn new() -> Set(a)", "Create an empty set", set::new),
                f("fn remove(s: Set(a), elem: a) -> Set(a) where a: Hash", "Remove an element", set::remove),
                f("fn symmetric_difference(a: Set(a), b: Set(a)) -> Set(a)", "Elements in exactly one of the two sets", set::symmetric_difference),
                f("fn to_list(s: Set(a)) -> List(a)", "Convert set to sorted list", set::to_list),
                f("fn union(a: Set(a), b: Set(a)) -> Set(a)", "Combine all elements", set::union),
            ],
        },
        module! {
            name: "math",
            page: "math.md",
            rows: [
                f("fn acos(x: Float) -> Float", "Arccosine (radians)", math::acos),
                f("fn asin(x: Float) -> Float", "Arcsine (radians)", math::asin),
                f("fn atan(x: Float) -> Float", "Arctangent (radians)", math::atan),
                f("fn atan2(y: Float, x: Float) -> Float", "Two-argument arctangent", math::atan2),
                f("fn cos(x: Float) -> Float", "Cosine", math::cos),
                k("e: Float", "Euler's number (2.71828...)", std::f64::consts::E),
                f("fn exp(x: Float) -> Float", "Exponential (e^x)", math::exp),
                f("fn log(x: Float) -> Float", "Natural logarithm (ln)", math::log),
                f("fn log10(x: Float) -> Float", "Base-10 logarithm", math::log10),
                k("pi: Float", "Pi (3.14159...)", std::f64::consts::PI),
                f("fn pow(base: Float, exponent: Float) -> Float", "Exponentiation", math::pow),
                f("fn random() -> Float", "Random float in [0.0, 1.0)", math::random),
                f("fn sin(x: Float) -> Float", "Sine", math::sin),
                f("fn sqrt(x: Float) -> Float", "Square root", math::sqrt),
                f("fn tan(x: Float) -> Float", "Tangent", math::tan),
            ],
        },
        module! {
            name: "time",
            page: "time.md",
            types: "\
                pub type Instant { epoch_ns: Int }\n\
                pub type Date { year: Int, month: Int, day: Int }\n\
                pub type Time { hour: Int, minute: Int, second: Int, ns: Int }\n\
                pub type DateTime { date: Date, time: Time }\n\
                pub type Duration { ns: Int }\n\
                pub type Weekday { Monday, Tuesday, Wednesday, Thursday, Friday, Saturday, Sunday }\n\
                pub type TimeError { TimeParseFormat(String), TimeOutOfRange(String) }\n\
            ",
            error: "TimeError",
            rows: [
                f("fn now() -> Instant", "Current UTC time as nanosecond epoch", time::now),
                f("fn today() -> Date", "Current local date", time::today),
                f("fn date(year: Int, month: Int, day: Int) -> Result(Date, TimeError)", "Validated date from year, month, day", time::date),
                f("fn time(hour: Int, min: Int, sec: Int) -> Result(Time, TimeError)", "Validated time from hour, min, sec (ns=0)", time::time),
                f("fn datetime(date: Date, time: Time) -> DateTime", "Combine date and time (infallible)", time::datetime),
                f("fn to_datetime(instant: Instant, offset_minutes: Int) -> DateTime", "Convert instant to local datetime with UTC offset in minutes", time::to_datetime),
                f("fn to_instant(datetime: DateTime, offset_minutes: Int) -> Instant", "Convert local datetime to instant with UTC offset in minutes", time::to_instant),
                f("fn to_utc(instant: Instant) -> DateTime", "Convert instant to UTC datetime (shorthand for offset=0)", time::to_utc),
                f("fn from_utc(datetime: DateTime) -> Instant", "Convert UTC datetime to instant (shorthand for offset=0)", time::from_utc),
                f("fn format(datetime: DateTime, pattern: String) -> String", "Format datetime with strftime pattern", time::format),
                f("fn format_date(date: Date, pattern: String) -> String", "Format date with strftime pattern", time::format_date),
                f("fn parse(s: String, pattern: String) -> Result(DateTime, TimeError)", "Parse string into datetime with strftime pattern", time::parse),
                f("fn parse_date(s: String, pattern: String) -> Result(Date, TimeError)", "Parse string into date with strftime pattern", time::parse_date),
                f("fn add_days(date: Date, days: Int) -> Date", "Add/subtract days from a date", time::add_days),
                f("fn add_months(date: Date, months: Int) -> Date", "Add/subtract months, clamping to end-of-month", time::add_months),
                f("fn add(instant: Instant, duration: Duration) -> Instant", "Add duration to an instant", time::add),
                f("fn since(from: Instant, to: Instant) -> Duration", "Signed duration between two instants (to − from)", time::since),
                f("fn hours(n: Int) -> Duration", "Create duration from hours", time::hours),
                f("fn minutes(n: Int) -> Duration", "Create duration from minutes", time::minutes),
                f("fn seconds(n: Int) -> Duration", "Create duration from seconds", time::seconds),
                f("fn ms(n: Int) -> Duration", "Create duration from milliseconds", time::ms),
                f("fn micros(n: Int) -> Duration", "Create duration from microseconds", time::micros),
                f("fn nanos(n: Int) -> Duration", "Create duration from nanoseconds", time::nanos),
                f("fn weekday(date: Date) -> Weekday", "Day of the week", time::weekday),
                f("fn days_between(from: Date, to: Date) -> Int", "Signed number of days between two dates", time::days_between),
                f("fn days_in_month(year: Int, month: Int) -> Int", "Days in month for given year and month", time::days_in_month),
                f("fn is_leap_year(year: Int) -> Bool", "Check if a year is a leap year", time::is_leap_year),
                f("fn sleep(duration: Duration) -> ()", "Fiber-aware sleep", time::sleep),
            ],
        },
        module! {
            name: "http",
            page: "http.md",
            types: "\
                pub type Method { GET, POST, PUT, PATCH, DELETE, HEAD, OPTIONS }\n\
                pub type Response { status: Int, body: String, headers: Map(String, String) }\n\
                pub type Request { method: Method, path: String, query: String, headers: Map(String, String), body: String }\n\
                pub type HttpError { HttpConnect(String), HttpTls(String), HttpTimeout, HttpInvalidUrl(String), HttpInvalidResponse(String), HttpClosedEarly, HttpStatusCode(Int, String), HttpUnknown(String) }\n\
            ",
            derives: [("Response", &["Equal", "Hash", "Display"]), ("Request", &["Equal", "Hash", "Display"])],
            error: "HttpError",
            rows: [
                f("fn get(url: String) -> Result(Response, HttpError)", "HTTP GET request", http::get).feature("http"),
                f("fn request(method: Method, url: String, body: String, headers: Map(String, String)) -> Result(Response, HttpError)", "HTTP request with method, URL, body, headers", http::request).feature("http"),
                f("fn serve(listener: TcpListener, handler: Fn(Request) -> Response) -> ()", "Serve HTTP on a listener made with `tcp.listen`, a task per connection", http::serve).feature("http"),
                f("fn segments(path: String) -> List(String)", "Split URL path into segments", http::segments),
                f("fn parse_query(query: String) -> Map(String, List(String))", "Parse a URL query string into a multi-value map", http::parse_query),
            ],
        },
        module! {
            name: "fs",
            page: "io-fs.md",
            types: "\
                pub type FileStat { size: Int, is_file: Bool, is_dir: Bool, is_symlink: Bool, modified: Int, readonly: Bool, mode: Int, accessed: Option(DateTime), created: Option(DateTime) }\n\
            ",
            rows: [
                f("fn copy(from: String, to: String) -> Result((), IoError)", "Copy a file", io::fs::copy),
                f("fn exists(path: String) -> Bool", "Check if path exists", io::fs::exists),
                f("fn glob(pattern: String) -> Result(List(String), IoError)", "Match paths by glob pattern", io::fs::glob),
                f("fn is_dir(path: String) -> Bool", "Check if path is a directory", io::fs::is_dir),
                f("fn is_file(path: String) -> Bool", "Check if path is a file", io::fs::is_file),
                f("fn is_symlink(path: String) -> Bool", "Check if path is a symlink (without following)", io::fs::is_symlink),
                f("fn list_dir(path: String) -> Result(List(String), IoError)", "List entries in a directory", io::fs::list_dir),
                f("fn mkdir(path: String) -> Result((), IoError)", "Create a directory (and parents)", io::fs::mkdir),
                f("fn read_link(path: String) -> Result(String, IoError)", "Read a symlink's target (without following)", io::fs::read_link),
                f("fn remove(path: String) -> Result((), IoError)", "Remove a file or empty directory", io::fs::remove),
                f("fn rename(from: String, to: String) -> Result((), IoError)", "Rename / move a file or directory", io::fs::rename),
                f("fn stat(path: String) -> Result(FileStat, IoError)", "Fetch filesystem metadata for a path", io::fs::stat),
                f("fn walk(root: String) -> Result(List(String), IoError)", "Recursively list all paths under a directory", io::fs::walk),
            ],
        },
        module! {
            name: "env",
            page: "io-fs.md",
            rows: [
                f("fn get(key: String) -> Option(String)", "Read an environment variable", io::env::get),
                f("fn set(key: String, value: String) -> ()", "Set an environment variable", io::env::set),
                f("fn remove(name: String) -> ()", "Unset an environment variable (idempotent)", io::env::remove),
                f("fn vars() -> List((String, String))", "Snapshot every environment variable", io::env::vars),
            ],
        },
        module! {
            name: "postgres",
            feature: "postgres",
            page: "postgres.md",
            types: "\
                pub type PgError { PgConnect(String), PgTls(String), PgAuthFailed(String), PgQuery(String, String), PgTypeMismatch(String, String, String), PgNoSuchColumn(String), PgClosed, PgTimeout, PgTxnAborted, PgUnknown(String) }\n\
            ",
            opaque: [("PgPool", 0), ("PgTx", 0), ("PgCursor", 0), ("QueryResult", 0), ("ExecResult", 0), ("Value", 0)],
            error: "PgError",
            rows: [
                f("fn connect(url: String) -> Result(PgPool, PgError)", "Open a connection pool from a `postgresql://` URL (uses r2d2 defaults)", postgres::connect),
                f("fn connect_with(url: String, opts: Map(String, Int)) -> Result(PgPool, PgError)", "Like `connect` with a tunable options bag (see [Connect options](#connect-options))", postgres::connect_with),
                f("fn query(conn: a, sql: String, params: List(Value)) -> Result(QueryResult, PgError)", "Run a SELECT-style statement and materialize rows", postgres::query),
                f("fn execute(conn: a, sql: String, params: List(Value)) -> Result(ExecResult, PgError)", "Run an INSERT/UPDATE/DELETE and return affected-row count", postgres::execute),
                f("fn transact(pool: PgPool, f: Fn(PgTx) -> Result(a, PgError)) -> Result(a, PgError)", "Pin a single connection for a transaction; callback runs inside BEGIN/COMMIT", postgres::transact),
                f("fn close(pool: PgPool) -> ()", "Drop the pool; future ops on it error", postgres::close),
                f("fn stream(conn: a, sql: String, params: List(Value)) -> Result(Channel(b), PgError)", "Stream rows through a bounded channel (backpressured)", postgres::stream),
                f("fn cursor(tx: PgTx, sql: String, params: List(Value), batch_size: Int) -> Result(PgCursor, PgError)", "Declare a server-side cursor with batch size", postgres::cursor),
                f("fn cursor_next(cursor: PgCursor) -> Result(List(Map(String, Value)), PgError)", "Fetch the next batch of rows from a cursor", postgres::cursor_next),
                f("fn cursor_close(cursor: PgCursor) -> Result((), PgError)", "Release a cursor and its underlying connection", postgres::cursor_close),
                f("fn listen(pool: PgPool, channel: String) -> Result(Channel(a), PgError)", "LISTEN on a channel; delivers async notifications", postgres::listen),
                f("fn notify(conn: a, channel: String, payload: String) -> Result((), PgError)", "NOTIFY a channel with a payload", postgres::notify),
                f("fn uuidv7() -> String", "Generate a time-ordered UUIDv7 (RFC 9562)", postgres::uuidv7),
            ],
        },
        module! {
            name: "bytes",
            page: "bytes.md",
            types: "\
                pub type BytesError { BytesInvalidUtf8(Int), BytesInvalidHex(String), BytesInvalidBase64(String), BytesByteOutOfRange(Int), BytesOutOfBounds(Int) }\n\
            ",
            error: "BytesError",
            rows: [
                f("fn concat(a: Bytes, b: Bytes) -> Bytes", "Concatenate two byte sequences", bytes::concat),
                f("fn concat_all(parts: List(Bytes)) -> Bytes", "Concatenate every element of a list", bytes::concat_all),
                f("fn empty() -> Bytes", "Zero-length byte sequence", bytes::empty),
                f("fn ends_with(b: Bytes, suffix: Bytes) -> Bool", "True if `b` ends with `suffix`", bytes::ends_with),
                f("fn eq(a: Bytes, b: Bytes) -> Bool", "Structural byte-by-byte comparison", bytes::eq),
                f("fn from_base64(s: String) -> Result(Bytes, BytesError)", "Decode base64 string", bytes::from_base64),
                f("fn from_hex(s: String) -> Result(Bytes, BytesError)", "Decode hex string (case-insensitive)", bytes::from_hex),
                f("fn from_list(xs: List(Int)) -> Result(Bytes, BytesError)", "Build from a list of byte values (0..=255)", bytes::from_list),
                f("fn from_string(s: String) -> Bytes", "UTF-8 encode a string", bytes::from_string),
                f("fn get(b: Bytes, i: Int) -> Result(Int, BytesError)", "Read a single byte at index", bytes::get),
                f("fn index_of(b: Bytes, needle: Bytes) -> Option(Int)", "First offset at which `needle` appears", bytes::index_of),
                f("fn length(b: Bytes) -> Int", "Number of bytes", bytes::length),
                f("fn slice(b: Bytes, start: Int, end: Int) -> Result(Bytes, BytesError)", "Half-open `[start, end)` slice", bytes::slice),
                f("fn split(b: Bytes, sep: Bytes) -> List(Bytes)", "Split on every occurrence of `sep` (panics if `sep` is empty)", bytes::split),
                f("fn starts_with(b: Bytes, prefix: Bytes) -> Bool", "True if `b` begins with `prefix`", bytes::starts_with),
                f("fn to_base64(b: Bytes) -> String", "Encode as base64", bytes::to_base64),
                f("fn to_hex(b: Bytes) -> String", "Encode as lowercase hex", bytes::to_hex),
                f("fn to_list(b: Bytes) -> List(Int)", "Materialize as a list of byte values", bytes::to_list),
                f("fn to_string(b: Bytes) -> Result(String, BytesError)", "UTF-8 decode (errors on invalid UTF-8)", bytes::to_string),
            ],
        },
        module! {
            name: "crypto",
            page: "crypto.md",
            rows: [
                f("fn sha256(data: Bytes) -> Bytes", "SHA-256 digest (32 bytes)", crypto::sha256),
                f("fn sha512(data: Bytes) -> Bytes", "SHA-512 digest (64 bytes)", crypto::sha512),
                f("fn md5(data: Bytes) -> Bytes", "MD5 digest (16 bytes) — **legacy / non-security use only**", crypto::md5),
                f("fn md5_hex(data: Bytes) -> String", "MD5 digest as lower-case hex (32 chars)", crypto::md5_hex),
                f("fn blake2b(data: Bytes) -> Bytes", "BLAKE2b-512 digest (64 bytes), RFC 7693", crypto::blake2b),
                f("fn blake2b_hex(data: Bytes) -> String", "BLAKE2b-512 digest as lower-case hex (128 chars)", crypto::blake2b_hex),
                f("fn hmac_sha256(key: Bytes, msg: Bytes) -> Bytes", "HMAC-SHA256 over `(key, msg)` (32 bytes)", crypto::hmac_sha256),
                f("fn hmac_sha512(key: Bytes, msg: Bytes) -> Bytes", "HMAC-SHA512 over `(key, msg)` (64 bytes)", crypto::hmac_sha512),
                f("fn random_bytes(n: Int) -> Result(Bytes, String)", "OS CSPRNG, `0..=1_048_576` bytes", crypto::random_bytes),
                f("fn constant_time_eq(a: Bytes, b: Bytes) -> Bool", "Timing-safe comparison (lengths leak)", crypto::constant_time_eq),
            ],
        },
        module! {
            name: "encoding",
            page: "encoding.md",
            rows: [
                f("fn url_encode(s: String) -> String", "Percent-encode per RFC 3986 (unreserved = `ALPHA` / `DIGIT` / `-._~`)", encoding::url_encode),
                f("fn url_decode(s: String) -> Result(String, String)", "Inverse. Errors on malformed `%HH` or invalid UTF-8 after decoding", encoding::url_decode),
                f("fn form_encode(pairs: List((String, String))) -> String", "Build an `application/x-www-form-urlencoded` body", encoding::form_encode),
                f("fn form_decode(body: String) -> Result(List((String, String)), String)", "Parse an `application/x-www-form-urlencoded` body into pairs", encoding::form_decode),
            ],
        },
        module! {
            name: "tcp",
            feature: "tcp",
            page: "tcp.md",
            types: "\
                pub type TcpError { TcpConnect(String), TcpTls(String), TcpClosed, TcpTimeout, TcpUnknown(String) }\n\
            ",
            opaque: [("TcpListener", 0), ("TcpStream", 0)],
            error: "TcpError",
            rows: [
                f("fn accept(listener: TcpListener) -> Result(TcpStream, TcpError)", "Wait for an incoming connection (cooperative I/O)", tcp::accept),
                f("fn close(stream: TcpStream) -> ()", "Shut the connection down: operations in flight on it return, later ones give `Err(TcpClosed)`", tcp::close),
                f("fn connect(addr: String) -> Result(TcpStream, TcpError)", "Open a TCP connection to `host:port` (cooperative I/O)", tcp::connect),
                f("fn listen(addr: String) -> Result(TcpListener, TcpError)", "Bind a TCP listener to `host:port`", tcp::listen),
                f("fn local_port(listener: TcpListener) -> Int", "The port the listener is bound to: the one the system chose for port `0`", tcp::local_port),
                f("fn peer_addr(stream: TcpStream) -> Result(String, TcpError)", "The address of the other end, as `ip:port`", tcp::peer_addr),
                f("fn read(stream: TcpStream, max: Int) -> Result(Bytes, TcpError)", "Read up to `max` bytes (cooperative)", tcp::read),
                f("fn read_exact(stream: TcpStream, n: Int) -> Result(Bytes, TcpError)", "Read exactly `n` bytes (cooperative; loops)", tcp::read_exact),
                f("fn set_nodelay(stream: TcpStream, on: Bool) -> Result((), TcpError)", "Send small writes at once (`true`) instead of gathering them (Nagle's algorithm, the default)", tcp::set_nodelay),
                f("fn write(stream: TcpStream, data: Bytes) -> Result((), TcpError)", "Write the entire buffer and flush (cooperative)", tcp::write),
                f("fn accept_tls(listener: TcpListener, cert_pem: Bytes, key_pem: Bytes) -> Result(TcpStream, TcpError)", "Accept a connection and complete the TLS server handshake using the supplied PEM cert chain + key", tcp::accept_tls).feature("tcp-tls"),
                f("fn accept_tls_mtls(listener: TcpListener, cert_pem: Bytes, key_pem: Bytes, client_ca_pem: Bytes) -> Result(TcpStream, TcpError)", "Like `accept_tls`, but also requires the client to present a cert chaining to the supplied CA PEM bundle (mutual TLS)", tcp::accept_tls_mtls).feature("tcp-tls"),
                f("fn connect_tls(addr: String, hostname: String) -> Result(TcpStream, TcpError)", "Open a TCP connection then complete the TLS client handshake against `hostname`", tcp::connect_tls).feature("tcp-tls"),
            ],
        },
        module! {
            name: "stream",
            page: "stream.md",
            rows: [
                f("fn from_list(xs: List(a)) -> Channel(a)", "Emit list elements then close", stream::from_list),
                f("fn from_range(lo: Int, hi: Int) -> Channel(Int)", "Emit `lo..=hi` then close", stream::from_range),
                f("fn repeat(x: a) -> Channel(a)", "Infinite — pair with `take`", stream::repeat),
                f("fn unfold(seed: a, f: Fn(a) -> Option((b, a))) -> Channel(b)", "Generator (closes on `None`)", stream::unfold),
                f("fn file_chunks(path: String, size: Int) -> Channel(Result(Bytes, IoError))", "Read file in chunks", stream::file_chunks),
                f("fn file_lines(path: String) -> Channel(Result(String, IoError))", "Read file line-by-line", stream::file_lines),
                f("fn tcp_chunks(stream: TcpStream, size: Int) -> Channel(Result(Bytes, TcpError))", "Read TCP in chunks", stream::tcp_chunks).feature("tcp"),
                f("fn tcp_lines(stream: TcpStream) -> Channel(Result(String, TcpError))", "Read TCP line-by-line", stream::tcp_lines).feature("tcp"),
                f("fn map(ch: Channel(a), f: Fn(a) -> b) -> Channel(b)", "Apply `f` to each element", stream::map),
                f("fn map_ok(ch: Channel(Result(a, b)), f: Fn(a) -> c) -> Channel(Result(c, b))", "Apply `f` to each `Ok` element; `Err` passes through", stream::map_ok),
                f("fn filter(ch: Channel(a), pred: Fn(a) -> Bool) -> Channel(a)", "Keep the elements `pred` accepts", stream::filter),
                f("fn filter_ok(ch: Channel(Result(a, b)), pred: Fn(a) -> Bool) -> Channel(Result(a, b))", "Keep the `Ok` elements `pred` accepts; `Err` passes through", stream::filter_ok),
                f("fn flat_map(ch: Channel(a), f: Fn(a) -> List(b)) -> Channel(b)", "Emit each element of `f(x)`", stream::flat_map),
                f("fn take(ch: Channel(a), n: Int) -> Channel(a)", "The first `n` elements", stream::take),
                f("fn drop(ch: Channel(a), n: Int) -> Channel(a)", "Skip the first `n` elements", stream::drop),
                f("fn take_while(ch: Channel(a), pred: Fn(a) -> Bool) -> Channel(a)", "Elements until `pred` first rejects one", stream::take_while),
                f("fn drop_while(ch: Channel(a), pred: Fn(a) -> Bool) -> Channel(a)", "Skip elements until `pred` first rejects one", stream::drop_while),
                f("fn chunks(ch: Channel(a), n: Int) -> Channel(List(a))", "Group elements into lists of `n`", stream::chunks),
                f("fn scan(ch: Channel(a), init: b, f: Fn(b, a) -> b) -> Channel(b)", "Running fold: emit each accumulator", stream::scan),
                f("fn dedup(ch: Channel(a)) -> Channel(a) where a: Equal", "Drop consecutive duplicates", stream::dedup),
                f("fn buffered(ch: Channel(a), n: Int) -> Channel(a)", "Decouple producer and consumer with a buffer of `n`", stream::buffered),
                f("fn merge(chs: List(Channel(a))) -> Channel(a)", "Interleave several streams as elements arrive", stream::merge),
                f("fn concat(chs: List(Channel(a))) -> Channel(a)", "One stream after another", stream::concat),
                f("fn zip(a: Channel(a), b: Channel(b)) -> Channel((a, b))", "Pair elements of two streams", stream::zip),
                f("fn collect(ch: Channel(a)) -> List(a)", "Drain into a list", stream::collect),
                f("fn fold(ch: Channel(a), init: b, f: Fn(b, a) -> b) -> b", "Reduce to a single value", stream::fold),
                f("fn each(ch: Channel(a), f: Fn(a) -> ()) -> ()", "Call `f` for each element", stream::each),
                f("fn count(ch: Channel(a)) -> Int", "Count the elements", stream::count),
                f("fn first(ch: Channel(a)) -> Option(a)", "The first element, or `None`", stream::first),
                f("fn last(ch: Channel(a)) -> Option(a)", "The last element, or `None`", stream::last),
                f("fn write_to_file(ch: Channel(Bytes), path: String) -> Result((), IoError)", "Write each chunk to a file", stream::write_to_file),
                f("fn write_to_tcp(ch: Channel(Bytes), stream: TcpStream) -> Result((), TcpError)", "Write each chunk to a TCP stream", stream::write_to_tcp).feature("tcp"),
            ],
        },
        module! {
            name: "uuid",
            page: "uuid.md",
            rows: [
                f("fn v4() -> String", "Random UUID (version 4, CSPRNG-backed)", uuid::v4),
                f("fn v7() -> String", "Time-ordered UUID (version 7, RFC 9562)", uuid::v7),
                f("fn parse(s: String) -> Result(String, String)", "Validate + canonicalize any-version UUID", uuid::parse),
                f("fn nil() -> String", "The all-zero UUID sentinel", uuid::nil),
                f("fn is_valid(s: String) -> Bool", "Predicate form of `parse`", uuid::is_valid),
            ],
        },
    ]
}
