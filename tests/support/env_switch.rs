//! The on/off environment switches of the test harnesses (`SILT_BLESS`,
//! `SILT_GOLDEN_FULL_CORPUS`, `SILT_GOLDEN_SKIP_VERDICT`, ...): a switch
//! is on when its variable is set to a value other than the empty one
//! and `0`. A script that passes `SILT_BLESS=` for a plain run must not
//! rewrite what it was to check.

use std::ffi::OsStr;

/// Whether the switch `name` is on.
pub fn on(name: &str) -> bool {
    is_on(std::env::var_os(name).as_deref())
}

/// Whether a switch whose variable holds `value` (`None`: not set) is on.
pub fn is_on(value: Option<&OsStr>) -> bool {
    value.is_some_and(|v| !v.is_empty() && v != "0")
}

#[test]
fn a_switch_is_on_only_when_set_to_a_value_other_than_empty_and_0() {
    for (value, on) in [
        (None, false),
        (Some(""), false),
        (Some("0"), false),
        (Some("1"), true),
        (Some("yes"), true),
        (Some("00"), true),
    ] {
        assert_eq!(is_on(value.map(OsStr::new)), on, "{value:?}");
    }
}
