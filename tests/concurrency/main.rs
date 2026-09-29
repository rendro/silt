//! Test suite: tasks, channels, streams, the scheduler and deadlock detection.
//!
//! One test binary; each module was a separate test crate before.

mod cancel_path_join_io_waker_leak_tests;
mod cancel_path_waker_leak_tests;
mod channel_op_shape_negative_tests;
mod channel_timeout_tests;
mod concurrency_main_join_msg_tests;
mod concurrency_stress_property_tests;
mod docs_channel_select_send_tests;
mod docs_concurrency_postgres_tests;
mod docs_task_cancel_tests;
mod main_thread_each_deadlock_tests;
mod main_thread_select_deadlock_tests;
mod main_thread_waker_leak_tests;
mod round74_concurrency_doc_snippets_tests;
mod round77_concurrency_doc_parity_tests;
mod round80_stream_error_parity_tests;
mod round82_channel_equality_tests;
mod round86_closed_channel_send_err_helper_tests;
mod round93_concurrency_recheck_extraction_tests;
mod round93_scheduler_cancel_cleanup_extraction_tests;
mod scheduler_cancel_setup_race_tests;
mod scheduler_deadlock_detector_tests;
mod scheduler_deadlock_false_positive_tests;
mod scheduler_race_tests;
mod select_waker_cleanup_tests;
mod stream_channel_main_thread_wait_tests;
mod stream_module_tests;
mod task_deadline_tests;
mod wave1_channels_tasks_tests;
mod wave2_tasks_tests;
