//! A program that only waits is woken by nothing, for longer than any
//! period a runtime might poll with (the HTTP server that silt used to
//! have woke every 30 seconds).

#![cfg(all(target_os = "linux", feature = "tcp"))]

use std::time::Duration;

use crate::quiet::{ACCEPTS, assert_never_woken};

/// Tasks that wait in `tcp.accept`: no thread of the process is
/// scheduled in 65 seconds.
#[test]
fn idle_accepts_are_not_woken_in_65_seconds() {
    assert_never_woken(ACCEPTS, Duration::from_secs(65));
}

/// An `http.serve` that nobody connects to, likewise.
#[test]
#[cfg(feature = "http")]
fn an_idle_server_is_not_woken_in_65_seconds() {
    assert_never_woken(crate::quiet::SERVER, Duration::from_secs(65));
}
