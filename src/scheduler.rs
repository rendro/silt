//! M:N scheduler mapping lightweight tasks onto a fixed-size thread pool.
//!
//! Spawned tasks run cooperatively on worker threads. Channel operations
//! park tasks instead of blocking OS threads, and wakers re-enqueue
//! them when data arrives.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use parking_lot::{Condvar, Mutex};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::value::{TaskHandle, Value};
use crate::vm::{BlockReason, SelectOpKind, Vm, VmError};

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
    /// Number of tasks currently parked (blocked on channel/join).
    blocked_tasks: AtomicUsize,
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

    /// Submit a runnable task to the scheduler.
    pub fn submit(&self, task: Task) {
        self.ensure_workers();
        self.inner.live_tasks.fetch_add(1, Ordering::SeqCst);
        let mut queue = self.inner.run_queue.lock();
        queue.push_back(task);
        self.inner.condvar.notify_one();
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
                let wait_result = inner
                    .condvar
                    .wait_for(&mut queue, Duration::from_secs(1));
                if wait_result.timed_out() {
                    let live = inner.live_tasks.load(Ordering::SeqCst);
                    let blocked = inner.blocked_tasks.load(Ordering::SeqCst);
                    if live > 0 && blocked >= live && queue.is_empty() {
                        // All live tasks are blocked with no runnable work — deadlock.
                        if !inner.deadlock_detected.swap(true, Ordering::SeqCst) {
                            eprintln!(
                                "deadlock: all {live} live tasks are blocked with nothing runnable"
                            );
                            // Complete all blocked task handles with a deadlock error
                            // so that joiners (including the main thread) unblock.
                            let handles: Vec<Arc<TaskHandle>> = {
                                let mut guard = inner.blocked_handles.lock();
                                std::mem::take(&mut *guard)
                            };
                            for handle in handles {
                                handle.complete(Err(
                                    "deadlock: all tasks are blocked with no progress possible"
                                        .to_string(),
                                ));
                            }
                        }
                        // Signal shutdown so all workers exit cleanly.
                        inner.shutdown.store(true, Ordering::SeqCst);
                        inner.condvar.notify_all();
                        return;
                    }
                }
            }
        };

        let Task { id, mut vm, handle } = task;
        let time_slice = 2000;

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
                handle.complete(Err(err.message));
                inner.live_tasks.fetch_sub(1, Ordering::SeqCst);
            }
            SliceResult::Blocked => {
                // Take the block reason from the VM.
                let reason = vm.take_block_reason();
                let task_slot: Arc<Mutex<Option<Task>>> =
                    Arc::new(Mutex::new(Some(Task { id, vm, handle })));

                // Track that this task is now blocked (unless no block reason,
                // which is treated as a yield and re-enqueued immediately).
                if reason.is_some() {
                    inner.blocked_tasks.fetch_add(1, Ordering::SeqCst);
                    // Store the handle so we can complete it on deadlock.
                    let handle_for_registry =
                        task_slot.lock().as_ref().unwrap().handle.clone();
                    inner
                        .blocked_handles
                        .lock()
                        .push(handle_for_registry);
                }

                match reason {
                    Some(BlockReason::Receive(ch)) => {
                        let slot = task_slot.clone();
                        let inner2 = inner.clone();
                        ch.register_recv_waker(Box::new(move || {
                            if let Some(task) = slot.lock().take() {
                                requeue(&inner2, task);
                            }
                        }));
                    }
                    Some(BlockReason::Send(ch)) => {
                        let slot = task_slot.clone();
                        let inner2 = inner.clone();
                        ch.register_send_waker(Box::new(move || {
                            if let Some(task) = slot.lock().take() {
                                requeue(&inner2, task);
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
                                    requeue(&inner2, task);
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
                                requeue(&inner2, task);
                            }
                        }));
                    }
                    Some(BlockReason::Io(completion)) => {
                        let slot = task_slot.clone();
                        let inner2 = inner.clone();
                        completion.register_waker(Box::new(move || {
                            if let Some(task) = slot.lock().take() {
                                requeue(&inner2, task);
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
fn requeue(inner: &Arc<SchedulerInner>, task: Task) {
    inner.blocked_tasks.fetch_sub(1, Ordering::SeqCst);
    // Remove this task's handle from the blocked registry.
    {
        let task_id = task.id;
        let mut handles = inner.blocked_handles.lock();
        if let Some(pos) = handles.iter().position(|h| h.id == task_id) {
            handles.swap_remove(pos);
        }
    }
    let mut queue = inner.run_queue.lock();
    queue.push_back(task);
    inner.condvar.notify_one();
}
