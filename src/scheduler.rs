//! M:N scheduler mapping lightweight tasks onto a fixed-size thread pool.
//!
//! Spawned tasks run cooperatively on worker threads. Channel operations
//! park tasks instead of blocking OS threads, and wakers re-enqueue
//! them when data arrives.

use parking_lot::{Condvar, Mutex};
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use crate::value::{TaskHandle, Value};
use crate::vm::{BlockReason, SelectOpKind, Vm, VmError};

/// Maximum number of live (active + blocked + queued) tasks the scheduler allows.
const MAX_TASKS: usize = 100_000;

/// Result of running a task's VM for one time slice.
pub enum SliceResult {
    /// Time slice expired; task is still runnable.
    Yielded,
    /// Task completed with a value.
    Completed(Value),
    /// Task failed with an error.
    Failed(VmError),
    /// Task is blocked on a channel/join operation.
    /// The block_reason on the VM describes what it's waiting for.
    Blocked,
}

/// A lightweight task scheduled on the M:N thread pool.
pub struct Task {
    pub id: usize,
    pub vm: Vm,
    pub handle: Arc<TaskHandle>,
}

/// Shared state for the M:N scheduler.
pub struct Scheduler {
    inner: Arc<SchedulerInner>,
    /// Worker thread handles, created lazily on first submit.
    workers: Mutex<Option<Vec<thread::JoinHandle<()>>>>,
}

struct SchedulerInner {
    run_queue: Mutex<VecDeque<Task>>,
    condvar: Condvar,
    shutdown: AtomicBool,
    /// Number of tasks that haven't yet completed (active + blocked + queued).
    live_tasks: AtomicUsize,
    /// Number of tasks currently parked (blocked on channel/join/io).
    blocked_tasks: AtomicUsize,
    /// Number of tasks currently parked on external I/O (a strict subset of
    /// `blocked_tasks`). These do not count toward deadlock detection because
    /// an external waker (I/O pool completion) will unblock them — the
    /// scheduler can't prove the graph is stuck while work is in flight.
    /// Matches Go's philosophy: netpoll-blocked goroutines are not deadlocked.
    io_blocked_tasks: AtomicUsize,
    /// Set to true once a deadlock has been detected and reported.
    deadlock_detected: AtomicBool,
    /// Handles of tasks that are currently blocked. When deadlock is detected,
    /// all of these are completed with a deadlock error so joiners unblock.
    blocked_handles: Mutex<Vec<Arc<TaskHandle>>>,
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl Scheduler {
    /// Create a new scheduler (does NOT start worker threads yet).
    pub fn new() -> Self {
        Scheduler {
            inner: Arc::new(SchedulerInner {
                run_queue: Mutex::new(VecDeque::new()),
                condvar: Condvar::new(),
                shutdown: AtomicBool::new(false),
                live_tasks: AtomicUsize::new(0),
                blocked_tasks: AtomicUsize::new(0),
                io_blocked_tasks: AtomicUsize::new(0),
                deadlock_detected: AtomicBool::new(false),
                blocked_handles: Mutex::new(Vec::new()),
            }),
            workers: Mutex::new(None),
        }
    }

    /// Ensure worker threads are running.
    fn ensure_workers(&self) {
        let mut guard = self.workers.lock();
        if guard.is_some() {
            return;
        }

        let num_workers = thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .max(2); // At least 2 workers to avoid deadlocks

        let mut handles = Vec::with_capacity(num_workers);
        for _ in 0..num_workers {
            let inner = self.inner.clone();
            handles.push(thread::spawn(move || {
                worker_loop(inner);
            }));
        }
        *guard = Some(handles);
    }

    /// Returns true if a deadlock has been detected.
    pub fn deadlock_detected(&self) -> bool {
        self.inner.deadlock_detected.load(Ordering::SeqCst)
    }

    /// Snapshot `(live_tasks, internal_blocked)` used by the main-thread
    /// channel watchdog to decide whether any scheduled task could still
    /// make progress. `internal_blocked` excludes I/O-blocked tasks — an
    /// external waker (I/O pool completion) will eventually unblock them
    /// and they might then reach the channel the main thread is waiting on.
    /// When `live > internal_blocked`, at least one task is either queued,
    /// running, or parked on external I/O; when `live <= internal_blocked`,
    /// no scheduled counterparty can make progress.
    pub fn progress_snapshot(&self) -> (usize, usize) {
        let blocked = self.inner.blocked_tasks.load(Ordering::SeqCst);
        let io_blocked = self.inner.io_blocked_tasks.load(Ordering::SeqCst);
        (
            self.inner.live_tasks.load(Ordering::SeqCst),
            blocked.saturating_sub(io_blocked),
        )
    }

    /// Submit a runnable task to the scheduler.
    ///
    /// Returns an error if the live-task count has reached [`MAX_TASKS`].
    pub fn submit(&self, task: Task) -> Result<(), String> {
        self.ensure_workers();
        let current = self.inner.live_tasks.load(Ordering::SeqCst);
        if current >= MAX_TASKS {
            return Err(format!(
                "task limit exceeded: {} tasks running (max {MAX_TASKS})",
                current
            ));
        }
        self.inner.live_tasks.fetch_add(1, Ordering::SeqCst);
        let mut queue = self.inner.run_queue.lock();
        queue.push_back(task);
        self.inner.condvar.notify_one();
        Ok(())
    }
}

impl Drop for Scheduler {
    fn drop(&mut self) {
        self.inner.shutdown.store(true, Ordering::SeqCst);
        self.inner.condvar.notify_all();
        if let Some(workers) = self.workers.lock().take() {
            for w in workers {
                let _ = w.join();
            }
        }
    }
}

/// The worker loop: dequeue tasks, run them for a time slice, handle results.
fn worker_loop(inner: Arc<SchedulerInner>) {
    let time_slice: usize = std::env::var("SILT_TIME_SLICE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2000);

    loop {
        // Dequeue a task.
        let task = {
            let mut queue = inner.run_queue.lock();
            loop {
                if inner.shutdown.load(Ordering::SeqCst) {
                    return;
                }
                if let Some(task) = queue.pop_front() {
                    break task;
                }
                // Use a timeout so we can periodically check for deadlock.
                let wait_result = inner.condvar.wait_for(&mut queue, Duration::from_secs(1));
                if wait_result.timed_out() {
                    // Deadlock fires only when all live tasks are blocked on
                    // INTERNAL graph edges (channels, joins). I/O-blocked
                    // tasks have external wakers and are not deadlocked in
                    // any sense the scheduler can prove — matches Go's
                    // netpoll exemption.
                    let live = inner.live_tasks.load(Ordering::SeqCst);
                    let blocked = inner.blocked_tasks.load(Ordering::SeqCst);
                    let io_blocked = inner.io_blocked_tasks.load(Ordering::SeqCst);
                    let internal_blocked = blocked.saturating_sub(io_blocked);
                    if live > 0 && internal_blocked >= live && queue.is_empty() {
                        // Double-check: release lock, yield, re-acquire, check again
                        // to avoid TOCTOU false positives.
                        drop(queue);
                        std::thread::yield_now();
                        queue = inner.run_queue.lock();
                        let live2 = inner.live_tasks.load(Ordering::SeqCst);
                        let blocked2 = inner.blocked_tasks.load(Ordering::SeqCst);
                        let io_blocked2 = inner.io_blocked_tasks.load(Ordering::SeqCst);
                        let internal_blocked2 = blocked2.saturating_sub(io_blocked2);
                        if live2 > 0 && internal_blocked2 >= live2 && queue.is_empty() {
                            // Confirmed deadlock — all live tasks are blocked
                            // with no runnable work.
                            if !inner.deadlock_detected.swap(true, Ordering::SeqCst) {
                                eprintln!(
                                    "deadlock: all {live2} live tasks are blocked with nothing runnable"
                                );
                                // Complete all blocked task handles with a deadlock error
                                // so that joiners (including the main thread) unblock.
                                let handles: Vec<Arc<TaskHandle>> = {
                                    let mut guard = inner.blocked_handles.lock();
                                    std::mem::take(&mut *guard)
                                };
                                for handle in handles {
                                    handle.complete(Err(VmError::new(
                                        "deadlock: all tasks are blocked with no progress possible"
                                            .to_string(),
                                    )));
                                }
                            }
                            // Signal shutdown so all workers exit cleanly.
                            inner.shutdown.store(true, Ordering::SeqCst);
                            inner.condvar.notify_all();
                            return;
                        }
                    }
                }
            }
        };

        let Task { id, mut vm, handle } = task;

        let result = vm.execute_slice(time_slice);

        match result {
            SliceResult::Yielded => {
                // Task still runnable — put it back.
                let mut queue = inner.run_queue.lock();
                queue.push_back(Task { id, vm, handle });
                inner.condvar.notify_one();
            }
            SliceResult::Completed(val) => {
                handle.complete(Ok(val));
                inner.live_tasks.fetch_sub(1, Ordering::SeqCst);
            }
            SliceResult::Failed(err) => {
                handle.complete(Err(vm.enrich_error(err)));
                inner.live_tasks.fetch_sub(1, Ordering::SeqCst);
            }
            SliceResult::Blocked => {
                // Take the block reason from the VM.
                let reason = vm.take_block_reason();
                let task_slot: Arc<Mutex<Option<Task>>> =
                    Arc::new(Mutex::new(Some(Task { id, vm, handle })));

                // Track whether this block is on external I/O. I/O blocks
                // bump `io_blocked_tasks` in addition to `blocked_tasks` so
                // the deadlock detector can exclude them.
                let was_io = matches!(reason, Some(BlockReason::Io(_)));

                // Track that this task is now blocked (unless no block reason,
                // which is treated as a yield and re-enqueued immediately).
                if reason.is_some() {
                    inner.blocked_tasks.fetch_add(1, Ordering::SeqCst);
                    if was_io {
                        inner.io_blocked_tasks.fetch_add(1, Ordering::SeqCst);
                    }
                    // Store the handle so we can complete it on deadlock.
                    // SAFETY: task_slot was just created with Some(Task{..}) above.
                    let handle_for_registry = task_slot
                        .lock()
                        .as_ref()
                        .expect("task_slot just initialized")
                        .handle
                        .clone();
                    inner
                        .blocked_handles
                        .lock()
                        .push(handle_for_registry.clone());

                    // Register cancel cleanup: if the task is cancelled while
                    // blocked, take it from the slot (making the waker a no-op),
                    // decrement blocked_tasks, and remove from blocked_handles.
                    let cancel_slot = task_slot.clone();
                    let cancel_inner = inner.clone();
                    let cancel_task_id = id;
                    handle_for_registry.set_cancel_cleanup(Box::new(move || {
                        // Take the task so the waker closure becomes a no-op.
                        // If the slot is empty, the waker already fired and the
                        // task is either running or already accounted for — do
                        // not double-decrement the counters in that case.
                        if cancel_slot.lock().take().is_none() {
                            return;
                        }
                        // This blocked task is being dropped: decrement
                        // blocked_tasks AND live_tasks (the Completed/Failed
                        // path is unreachable once we drop the task, so
                        // live_tasks would otherwise leak).
                        cancel_inner.blocked_tasks.fetch_sub(1, Ordering::SeqCst);
                        if was_io {
                            cancel_inner.io_blocked_tasks.fetch_sub(1, Ordering::SeqCst);
                        }
                        cancel_inner.live_tasks.fetch_sub(1, Ordering::SeqCst);
                        let mut handles = cancel_inner.blocked_handles.lock();
                        if let Some(pos) = handles.iter().position(|h| h.id == cancel_task_id) {
                            handles.swap_remove(pos);
                        }
                    }));
                }

                match reason {
                    Some(BlockReason::Receive(ch)) => {
                        let slot = task_slot.clone();
                        let inner2 = inner.clone();
                        ch.register_recv_waker(Box::new(move || {
                            if let Some(task) = slot.lock().take() {
                                requeue(&inner2, task, false);
                            }
                        }));
                    }
                    Some(BlockReason::Send(ch)) => {
                        let slot = task_slot.clone();
                        let inner2 = inner.clone();
                        ch.register_send_waker(Box::new(move || {
                            if let Some(task) = slot.lock().take() {
                                requeue(&inner2, task, false);
                            }
                        }));
                    }
                    Some(BlockReason::Select(ops)) => {
                        // Register waker on ALL channels. First waker to fire
                        // wakes the task; the rest check the cancel token and
                        // return early, dropping their Arc references promptly.
                        let cancelled = Arc::new(AtomicBool::new(false));
                        for (ch, kind) in &ops {
                            let slot = task_slot.clone();
                            let inner2 = inner.clone();
                            let cancelled2 = cancelled.clone();
                            let waker = Box::new(move || {
                                if cancelled2.load(Ordering::Acquire) {
                                    return; // Another waker already fired
                                }
                                if let Some(task) = slot.lock().take() {
                                    cancelled2.store(true, Ordering::Release);
                                    requeue(&inner2, task, false);
                                }
                            });
                            match kind {
                                SelectOpKind::Receive => ch.register_recv_waker(waker),
                                SelectOpKind::Send => ch.register_send_waker(waker),
                            }
                        }
                    }
                    Some(BlockReason::Join(target_handle)) => {
                        let slot = task_slot.clone();
                        let inner2 = inner.clone();
                        target_handle.register_join_waker(Box::new(move || {
                            if let Some(task) = slot.lock().take() {
                                requeue(&inner2, task, false);
                            }
                        }));
                    }
                    Some(BlockReason::Io(completion)) => {
                        let slot = task_slot.clone();
                        let inner2 = inner.clone();
                        completion.register_waker(Box::new(move || {
                            if let Some(task) = slot.lock().take() {
                                requeue(&inner2, task, true);
                            }
                        }));
                    }
                    None => {
                        // No block reason — shouldn't happen but treat as yield.
                        if let Some(task) = task_slot.lock().take() {
                            let mut queue = inner.run_queue.lock();
                            queue.push_back(task);
                            inner.condvar.notify_one();
                        }
                    }
                }
            }
        }
    }
}

/// Re-enqueue a parked task on the scheduler's run queue.
///
/// `was_io` indicates whether the task was parked on external I/O (so
/// `io_blocked_tasks` needs decrementing) or on an internal graph edge
/// (channel/select/join). The waker site knows which arm it's in.
fn requeue(inner: &Arc<SchedulerInner>, task: Task, was_io: bool) {
    inner.blocked_tasks.fetch_sub(1, Ordering::SeqCst);
    if was_io {
        inner.io_blocked_tasks.fetch_sub(1, Ordering::SeqCst);
    }
    // Remove this task's handle from the blocked registry.
    {
        let task_id = task.id;
        let mut handles = inner.blocked_handles.lock();
        if let Some(pos) = handles.iter().position(|h| h.id == task_id) {
            handles.swap_remove(pos);
        }
    }
    // Clear the stale cancel-cleanup so it won't double-decrement
    // blocked_tasks when the task completes normally.
    task.handle.clear_cancel_cleanup();
    let mut queue = inner.run_queue.lock();
    queue.push_back(task);
    inner.condvar.notify_one();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytecode::VmClosure;
    use crate::compiler::Compiler;
    use crate::lexer::Lexer;
    use crate::parser::Parser;
    use crate::vm::CallFrame;

    /// Compile a Silt snippet and return a VM ready for execute_slice.
    fn make_vm(src: &str) -> Vm {
        let tokens = Lexer::new(src).tokenize().expect("lexer error");
        let mut program = Parser::new(tokens).parse_program().expect("parse error");
        let _ = crate::typechecker::check(&mut program);
        let mut compiler = Compiler::new();
        let functions = compiler.compile_program(&program).expect("compile error");
        let script = Arc::new(functions.into_iter().next().unwrap());
        let mut vm = Vm::new();
        vm.is_scheduled_task = true;
        let closure = Arc::new(VmClosure {
            function: script,
            upvalues: vec![],
        });
        vm.frames.push(CallFrame {
            closure,
            ip: 0,
            base_slot: 0,
        });
        vm
    }

    fn make_task(id: usize, src: &str) -> (Task, Arc<TaskHandle>) {
        let handle = Arc::new(TaskHandle::new(id));
        let vm = make_vm(src);
        (
            Task {
                id,
                vm,
                handle: handle.clone(),
            },
            handle,
        )
    }

    // ── Basic lifecycle ────────────────────────────────────────────

    #[test]
    fn test_submit_and_join_single_task() {
        let scheduler = Scheduler::new();
        let (task, handle) = make_task(1, "fn main() { 42 }");
        scheduler.submit(task).unwrap();
        let result = handle.join();
        assert_eq!(result.ok(), Some(Value::Int(42)));
    }

    #[test]
    fn test_submit_multiple_tasks_all_complete() {
        let scheduler = Scheduler::new();
        let mut handles = Vec::new();
        for i in 0..10 {
            let src = format!("fn main() {{ {} }}", i);
            let (task, handle) = make_task(i, &src);
            scheduler.submit(task).unwrap();
            handles.push((i as i64, handle));
        }
        for (expected, handle) in handles {
            assert_eq!(handle.join().ok(), Some(Value::Int(expected)));
        }
    }

    #[test]
    fn test_failed_task_reports_error() {
        let scheduler = Scheduler::new();
        let (task, handle) = make_task(1, "fn main() { 1 / 0 }");
        scheduler.submit(task).unwrap();
        let result = handle.join();
        assert!(result.is_err(), "expected error from division by zero");
        assert!(
            result.unwrap_err().message.contains("division"),
            "error should mention division"
        );
    }

    // ── Counter bookkeeping ────────────────────────────────────────

    #[test]
    fn test_live_tasks_counter_reaches_zero() {
        let scheduler = Scheduler::new();
        let mut handles = Vec::new();
        for i in 0..5 {
            let (task, handle) = make_task(i, "fn main() { 1 }");
            scheduler.submit(task).unwrap();
            handles.push(handle);
        }
        for h in &handles {
            let _ = h.join();
        }
        // Give workers a moment to decrement.
        std::thread::sleep(Duration::from_millis(50));
        let live = scheduler.inner.live_tasks.load(Ordering::SeqCst);
        assert_eq!(
            live, 0,
            "live_tasks should be 0 after all tasks complete, got {live}"
        );
    }

    #[test]
    fn test_failed_task_decrements_live_counter() {
        let scheduler = Scheduler::new();
        let (task, handle) = make_task(1, "fn main() { 1 / 0 }");
        scheduler.submit(task).unwrap();
        let _ = handle.join();
        std::thread::sleep(Duration::from_millis(50));
        let live = scheduler.inner.live_tasks.load(Ordering::SeqCst);
        assert_eq!(
            live, 0,
            "live_tasks should be 0 after failed task, got {live}"
        );
    }

    // ── Shutdown ───────────────────────────────────────────────────

    #[test]
    fn test_drop_joins_workers_cleanly() {
        let scheduler = Scheduler::new();
        let (task, handle) = make_task(1, "fn main() { 1 }");
        scheduler.submit(task).unwrap();
        let _ = handle.join();
        // Drop the scheduler — should not hang or panic.
        drop(scheduler);
    }

    #[test]
    fn test_drop_empty_scheduler_is_noop() {
        // No tasks submitted — drop should be immediate.
        let scheduler = Scheduler::new();
        drop(scheduler);
    }

    // ── Deadlock detection ─────────────────────────────────────────

    #[test]
    fn test_progress_snapshot_excludes_io_blocked_tasks() {
        // Unit-level invariant: progress_snapshot returns `(live,
        // internal_blocked)` where internal_blocked = blocked - io_blocked.
        // I/O-blocked tasks have external wakers and must not count toward
        // a deadlock-style "no progress possible" signal.
        let scheduler = Scheduler::new();
        // Simulate: 3 live tasks, 3 blocked total, 2 of which are on I/O.
        scheduler.inner.live_tasks.store(3, Ordering::SeqCst);
        scheduler.inner.blocked_tasks.store(3, Ordering::SeqCst);
        scheduler.inner.io_blocked_tasks.store(2, Ordering::SeqCst);
        let (live, internal_blocked) = scheduler.progress_snapshot();
        assert_eq!(live, 3);
        assert_eq!(
            internal_blocked, 1,
            "internal_blocked should exclude I/O tasks: 3 - 2 = 1"
        );
        // Sanity: if io_blocked == blocked, internal_blocked should be 0.
        scheduler.inner.io_blocked_tasks.store(3, Ordering::SeqCst);
        let (_, internal_blocked) = scheduler.progress_snapshot();
        assert_eq!(internal_blocked, 0);
        // Reset so the Drop path doesn't see non-zero counters.
        scheduler.inner.live_tasks.store(0, Ordering::SeqCst);
        scheduler.inner.blocked_tasks.store(0, Ordering::SeqCst);
        scheduler.inner.io_blocked_tasks.store(0, Ordering::SeqCst);
    }

    #[test]
    fn test_io_blocked_task_does_not_trigger_deadlock() {
        // Regression lock for the channel-only deadlock split: a task that
        // performs real I/O should complete successfully, and the scheduler
        // must NOT have marked the session as deadlocked during the I/O
        // park window. Tests the I/O path end-to-end through the counter
        // bookkeeping changes in requeue/cancel_cleanup.
        let scheduler = Scheduler::new();
        // Write a fixture file (small, completes fast).
        let path = std::env::temp_dir().join("silt_sched_io_block_test.txt");
        std::fs::write(&path, "hello").unwrap();
        let src = format!(
            r#"
import io
fn main() {{
  match io.read_file("{}") {{
    Ok(s) -> s
    Err(_) -> "fail"
  }}
}}
        "#,
            path.display()
        );
        let (task, handle) = make_task(1, &src);
        scheduler.submit(task).unwrap();
        let result = handle.join();
        assert_eq!(result.ok(), Some(Value::String("hello".into())));
        assert!(
            !scheduler.deadlock_detected(),
            "I/O-blocked task must not trigger deadlock detection"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_deadlock_detected_flag() {
        // Two tasks that each receive from a channel nobody sends to.
        // The scheduler should detect this as a deadlock.
        let scheduler = Scheduler::new();
        let src = r#"
import channel
fn main() {
  let ch = channel.new(0)
  channel.receive(ch)
}
        "#;
        let (task1, handle1) = make_task(1, src);
        let (task2, handle2) = make_task(2, src);
        scheduler.submit(task1).unwrap();
        scheduler.submit(task2).unwrap();
        // Both should complete (with deadlock error).
        let r1 = handle1.join();
        let r2 = handle2.join();
        assert!(r1.is_err(), "task1 should fail with deadlock");
        assert!(r2.is_err(), "task2 should fail with deadlock");
        assert!(
            scheduler.deadlock_detected(),
            "deadlock_detected flag should be set"
        );
    }
}
