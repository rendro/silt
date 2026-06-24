#!/usr/bin/env bash
# Run a partitioned subset of `cargo test` for CI parallelisation.
#
# Usage: run-test-partition.sh <heavy|concurrency|rest1|rest2>
#
# Partitioning strategy:
#   - heavy:        integration.rs + integration_concurrency.rs
#                   (~180s wall-clock; the largest single binaries).
#   - concurrency:  scheduler / channel / concurrency-stress / docs-stdlib
#                   walker / TLS — tests whose runtime is dominated by
#                   spawning silt scheduler threads or subprocesses, and
#                   which interact poorly with parallel binary contention.
#   - rest1/rest2:  everything else — every other tests/*.rs binary,
#                   split across two shards by index parity (rest1 also
#                   carries the lib + bins unit tests). Default bucket.
#
# Each test binary in tests/*.rs is a separate cargo test target; we
# pass an explicit `--test NAME` list per partition. The `rest1`/`rest2`
# shards run every test binary not in `heavy` or `concurrency`, split
# evenly; `rest1` additionally runs `cargo test --lib --bins`.
#
# Adding a new test file:
#   - For a small binary (<5s), do nothing — it joins `rest1`/`rest2`
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
  # ~135s on Windows runners (3-5x slower I/O / spawn) for the property
  # cases generated_int_programs_*. Pushes the rest bucket past the
  # 25-min wall-clock cap when left there.
  vm_small_program_property_tests
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

runner="${SILT_TEST_RUNNER:-nextest}"

case "$partition" in
  heavy)
    args=()
    for t in "${HEAVY_TESTS[@]}"; do
      args+=("--test" "$t")
    done
    set -x
    if [[ "$runner" == "nextest" ]] && command -v cargo-nextest >/dev/null 2>&1; then
      exec cargo nextest run --all-features "${args[@]}"
    else
      exec cargo test --all-features "${args[@]}"
    fi
    ;;
  concurrency)
    args=()
    for t in "${CONCURRENCY_TESTS[@]}"; do
      args+=("--test" "$t")
    done
    set -x
    if [[ "$runner" == "nextest" ]] && command -v cargo-nextest >/dev/null 2>&1; then
      exec cargo nextest run --all-features "${args[@]}"
    else
      exec cargo test --all-features "${args[@]}"
    fi
    ;;
  rest1 | rest2)
    # Discover every tests/*.rs basename; exclude HEAVY + CONCURRENCY, then
    # split the remainder across two shards by index parity. Interleaving
    # (rest1 = even indices, rest2 = odd) — rather than a first-half/second-half
    # cut — spreads alphabetically-clustered slow suites (e.g. the round93_*
    # subprocess tests) evenly, so neither shard inherits a hot block. Windows
    # `rest` had outgrown the 30-min wall-clock cap as one bucket; halved, each
    # shard lands ~15 min with headroom for future growth.
    if [[ "$partition" == "rest2" ]]; then want_parity=1; else want_parity=0; fi
    excluded_list="$(printf '%s\n' "${HEAVY_TESTS[@]}" "${CONCURRENCY_TESTS[@]}")"
    args=()
    idx=0
    while IFS= read -r f; do
      base="$(basename "$f" .rs)"
      if printf '%s\n' "$excluded_list" | grep -qFx -- "$base"; then
        continue
      fi
      if (( idx % 2 == want_parity )); then
        args+=("--test" "$base")
      fi
      idx=$((idx + 1))
    done < <(find tests -maxdepth 1 -name '*.rs' -type f | sort)
    set -x
    # The `--lib` (src/) and `--bin silt` (src/main.rs) unit tests ride along on
    # rest1 only, so they run exactly once across the two shards.
    if [[ "$runner" == "nextest" ]] && command -v cargo-nextest >/dev/null 2>&1; then
      # nextest spelling: --lib + --bin <name> (not --bins).
      if [[ "$partition" == "rest1" ]]; then
        exec cargo nextest run --all-features --lib --bin silt "${args[@]}"
      else
        exec cargo nextest run --all-features "${args[@]}"
      fi
    else
      if [[ "$partition" == "rest1" ]]; then
        exec cargo test --all-features --lib --bins "${args[@]}"
      else
        exec cargo test --all-features "${args[@]}"
      fi
    fi
    ;;
  *)
    echo "unknown partition: $partition (expected: heavy|concurrency|rest1|rest2)" >&2
    exit 2
    ;;
esac
