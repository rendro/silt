//! Test suite: tasks, channels, streams, the scheduler and deadlock detection.
//!
//! One test binary; each module was a separate test crate before.
//!
//! Run it with at most 4 tests at once: `cargo test --test concurrency --
//! --test-threads=4` (CI does the same through .config/nextest.toml). Its
//! tests spawn scheduler threads and assert on deadlock verdicts and
//! timeouts; all at once, they oversubscribe the CPU and fail.

mod cancel_path_join_io_waker_leak_tests;
mod cancel_path_waker_leak_tests;
mod concurrency_stress_property_tests;
mod main_thread_waker_leak_tests;
mod scheduler_deadlock_detector_tests;
mod scheduler_race_tests;
mod select_waker_cleanup_tests;
mod stream_channel_main_thread_wait_tests;
mod task_deadline_tests;
mod wave1_channels_tasks_tests;
mod wave2_tasks_tests;
