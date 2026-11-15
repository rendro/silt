#!/usr/bin/env bash
# Run unit tests of silt under one of the compiler's sanitizers, on
# x86_64 Linux with a nightly toolchain (and its `rust-src`: the
# standard library is built with the sanitizer too, -Zbuild-std, so
# that what it does is seen as well).
#
# Usage: sanitizer.sh <thread|address>
#
#   thread   ThreadSanitizer, on the tests of the scheduler and of the
#            runtime (its tasks, channels, waits, timers and I/O
#            handles) and of the shared slice of values: a data race
#            between threads, in silt's code or in what it calls, fails
#            the run.
#   address  AddressSanitizer, on the tests of the values and of the
#            runtime's I/O handles: a use after free, a read or write
#            out of bounds, a double free or a leak fails the run. The
#            handle tests make the C calls that silt's unsafe code makes
#            on sockets (send, recv, poll, eventfd); the sanitizer checks
#            the memory each call is given.
#
# The value tests that copy lists of 10,000 elements or more, or nest a
# value 10,000 deep (value::tests, the ones below), take a long time
# under a sanitizer and test no other unsafe code: they are left out.
# So are value::map_tests (maps are imbl's trees, with no unsafe code
# of silt's, and their property tests are slow here).
#
# No report is suppressed. A third-party report that cannot be fixed
# here would be suppressed in a file named by TSAN_OPTIONS or
# ASAN_OPTIONS below, each line with the reason.
set -euo pipefail

sanitizer="${1:?usage: sanitizer.sh <thread|address>}"
target=x86_64-unknown-linux-gnu

skip_slow=(
  --skip value::map_tests
  --skip a_changed_range_is_written_out_side_by_side
  --skip a_list_changed_an_element_at_a_time_copies_n_log_n_elements
  --skip a_walk_down_a_list_copies_few_of_its_elements
  --skip without_the_native_stack
)

case "$sanitizer" in
  thread)
    tests=(scheduler:: runtime:: value::thin)
    # A report fails the run when the tests end (exit status 66); all
    # of them are printed first.
    export TSAN_OPTIONS="halt_on_error=0"
    ;;
  address)
    tests=(value:: runtime::handle "${skip_slow[@]}")
    export ASAN_OPTIONS="detect_leaks=1"
    ;;
  *)
    echo "unknown sanitizer: $sanitizer (thread or address)" >&2
    exit 2
    ;;
esac

# Its own target directory: a build with a sanitizer shares nothing
# with one without.
export CARGO_TARGET_DIR="target/sanitizer-$sanitizer"
export RUSTFLAGS="-Zsanitizer=$sanitizer"
export RUSTDOCFLAGS="-Zsanitizer=$sanitizer"

cargo +nightly test --locked -Zbuild-std --target "$target" --lib -- "${tests[@]}"
