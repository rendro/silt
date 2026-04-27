#!/usr/bin/env bash
# Run a partitioned subset of `cargo test` for CI parallelisation.
#
# Usage: run-test-partition.sh <heavy|concurrency|rest>
#
# Partitioning strategy:
#   - heavy:        integration.rs + integration_concurrency.rs
#                   (~180s wall-clock; the largest single binaries).
#   - concurrency:  scheduler / channel / concurrency-stress / docs-stdlib
#                   walker / TLS — tests whose runtime is dominated by
#                   spawning silt scheduler threads or subprocesses, and
#                   which interact poorly with parallel binary contention.
#   - rest:         everything else — lib + bins unit tests + every
#                   other tests/*.rs binary. Default partition.
#
# Each test binary in tests/*.rs is a separate cargo test target; we
# pass an explicit `--test NAME` list per partition. The `rest`
# partition runs `cargo test --lib --bins` plus every test binary not
# in `heavy` or `concurrency`.
#
# Adding a new test file:
#   - For a small binary (<5s), do nothing — it joins `rest`
#     automatically via the diff-the-directory logic below.
#   - For a slow concurrency-leaning binary, append it to
#     CONCURRENCY_TESTS below.
#   - For another monolith on the integration scale, append to
#     HEAVY_TESTS.
set -euo pipefail

partition="${1:?missing partition arg: heavy|concurrency|rest}"

HEAVY_TESTS=(
  integration
  integration_concurrency
)

CONCURRENCY_TESTS=(
  scheduler_cancel_setup_race_tests
  scheduler_deadlock_detector_tests
  scheduler_race_tests
  concurrency_stress_property_tests
  docs_stdlib_println_parity_tests
  channel_timeout_tests
  channel_op_shape_negative_tests
  cancel_path_waker_leak_tests
  call_method_yield_tests
  callback_frame_capture_tests
  main_thread_waker_leak_tests
  nested_invoke_yield_tests
  select_waker_cleanup_tests
  task_deadline_tests
  time_sleep_cooperative_tests
  tcp_module_tests
  tcp_mtls_tests
  tcp_shutdown_tests
  tcp_tls_tests
  http_bind_default_tests
  http_dispatch_parity_round36_tests
  http_hardening_tests
)

case "$partition" in
  heavy)
    args=()
    for t in "${HEAVY_TESTS[@]}"; do
      args+=("--test" "$t")
    done
    set -x
    exec cargo test --all-features "${args[@]}"
    ;;
  concurrency)
    args=()
    for t in "${CONCURRENCY_TESTS[@]}"; do
      args+=("--test" "$t")
    done
    set -x
    exec cargo test --all-features "${args[@]}"
    ;;
  rest)
    # Discover every tests/*.rs basename; exclude HEAVY + CONCURRENCY.
    excluded_list="$(printf '%s\n' "${HEAVY_TESTS[@]}" "${CONCURRENCY_TESTS[@]}")"
    rest_tests=()
    while IFS= read -r f; do
      base="$(basename "$f" .rs)"
      if ! printf '%s\n' "$excluded_list" | grep -qFx -- "$base"; then
        rest_tests+=("--test" "$base")
      fi
    done < <(find tests -maxdepth 1 -name '*.rs' -type f | sort)
    set -x
    # `--lib` runs unit tests in src/; `--bins` runs unit tests in src/main.rs.
    exec cargo test --all-features --lib --bins "${rest_tests[@]}"
    ;;
  *)
    echo "unknown partition: $partition (expected: heavy|concurrency|rest)" >&2
    exit 2
    ;;
esac
