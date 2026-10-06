//! Test suite: tasks, channels, streams, the scheduler and deadlock detection.
//!
//! One test binary; each module was a separate test crate before.
//!
//! Run it with at most 4 tests at once: `cargo test --test concurrency --
//! --test-threads=4` (CI does the same through .config/nextest.toml). Its
//! tests spawn scheduler threads and assert on deadlock verdicts and
//! timeouts; all at once, they oversubscribe the CPU and fail.

mod concurrency_stress_property_tests;
mod stream_channel_main_thread_wait_tests;
mod task_deadline_tests;
mod wave1_channels_tasks_tests;
mod wave2_tasks_tests;
