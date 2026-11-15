#!/usr/bin/env bash
# Run under Miri the unit tests that reach silt's own unsafe memory
# code: the shared slice of values in src/value/thin.rs (allocation,
# the header, the count, reads and writes through raw pointers, the
# drop by the last holder) and the values built on it.
#
# Usage: miri.sh            (needs a nightly toolchain with `miri`)
#
# Miri finds undefined behaviour in what it runs: a use after free, a
# double drop, a read of memory never written, a pointer used outside
# what it may reach, a data race. It runs a program about a thousand
# times slower than a debug build, and it cannot call into C or make
# most system calls, so it runs a chosen set of tests:
#
# 1. The slice's own tests, value::thin, each under 16 schedules of its
#    threads (-Zmiri-many-seeds): their threads share, clone and drop
#    slices, and a data race or a free before the last drop shows in
#    some interleaving.
# 2. The tests of the values, value::tests and value::obj: lists,
#    tuples, records and variants made, changed, compared and dropped,
#    which keep their elements in such slices. Some are left out, each
#    in `skips` below with its reason.
#
# The rest of silt's unsafe code calls into the C library (a socket's
# send and receive, poll, eventfd, pipe, sigaction, the environment) or
# sets up the binary's allocator: Miri cannot run those, and the
# sanitizer runs cover what of them the unit tests reach
# (sanitizer.sh).
#
# A failure prints Miri's report: the operation, where it happened, and
# where the memory was made and freed.
set -euo pipefail

# Stricter pointer rules than the default: an integer is never taken
# for a pointer.
flags="-Zmiri-strict-provenance"

echo "== value::thin, 16 schedules"
MIRIFLAGS="$flags -Zmiri-many-seeds=0..16" \
  cargo +nightly miri test --locked --no-default-features --lib -- value::thin

# Left out, each with its reason:
skips=(
  # They measure how many elements a change copies, on lists of 10,000
  # to 64,000 elements (hours under Miri); the small lists of the
  # other tests take the same paths.
  --skip a_changed_range_is_written_out_side_by_side
  --skip a_list_changed_an_element_at_a_time_copies_n_log_n_elements
  --skip a_walk_down_a_list_copies_few_of_its_elements
  # A property test: 400 cases of lists of up to 1,500 elements and up
  # to 40 changes, not done after 15 minutes under Miri.
  --skip a_list_is_the_list_of_its_elements_however_it_is_kept
  # They build values nested 10,000 to 200,000 deep, to show that no
  # walk uses the native stack; a deep nest of slices is dropped in
  # value::thin (a_deep_nest_of_lists_is_dropped).
  --skip without_the_native_stack
  # It compares and hashes values of 7 shapes nested past the depth
  # where the walk leaves the native stack (64), each pair of shapes:
  # not done after 20 minutes under Miri.
  --skip the_key_is_the_same_where_the_walk_changes_its_stack
  # It compares, hashes and shows each pair of 88 lists, with their runs
  # cut five ways: not done after 15 minutes under Miri.
  --skip where_a_list_s_runs_end_is_nothing_a_value_shows
)

echo "== value::tests and value::obj"
MIRIFLAGS="$flags" \
  cargo +nightly miri test --locked --no-default-features --lib -- value::tests value::obj "${skips[@]}"
