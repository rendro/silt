//! M:N scheduler mapping lightweight tasks onto a fixed-size thread pool.
//!
//! Spawned tasks run cooperatively on worker threads. Channel operations
//! park tasks instead of blocking OS threads, and wakers re-enqueue
//! them when data arrives.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

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
            }),
            workers: Mutex::new(None),
        }
    }

    /// Ensure worker threads are running.
    fn ensure_workers(&self) {
        let mut guard = self.workers.lock().unwrap();
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

    /// Submit a runnable task to the scheduler.
    pub fn submit(&self, task: Task) {
        self.ensure_workers();
        self.inner.live_tasks.fetch_add(1, Ordering::SeqCst);
        let mut queue = self.inner.run_queue.lock().unwrap();
        queue.push_back(task);
        self.inner.condvar.notify_one();
    }
}

impl Drop for Scheduler {
    fn drop(&mut self) {
        self.inner.shutdown.store(true, Ordering::SeqCst);
        self.inner.condvar.notify_all();
        if let Some(workers) = self.workers.lock().unwrap().take() {
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
            let mut queue = inner.run_queue.lock().unwrap();
            loop {
                if inner.shutdown.load(Ordering::SeqCst) {
                    return;
                }
                if let Some(task) = queue.pop_front() {
                    break task;
                }
                queue = inner.condvar.wait(queue).unwrap();
            }
        };

        let Task { id, mut vm, handle } = task;
        let time_slice = 2000;

        let result = vm.execute_slice(time_slice);

        match result {
            SliceResult::Yielded => {
                // Task still runnable — put it back.
                let mut queue = inner.run_queue.lock().unwrap();
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

                match reason {
                    Some(BlockReason::Receive(ch)) => {
                        let slot = task_slot.clone();
                        let inner2 = inner.clone();
                        ch.register_recv_waker(Box::new(move || {
                            if let Some(task) = slot.lock().unwrap().take() {
                                requeue(&inner2, task);
                            }
                        }));
                    }
                    Some(BlockReason::Send(ch)) => {
                        let slot = task_slot.clone();
                        let inner2 = inner.clone();
                        ch.register_send_waker(Box::new(move || {
                            if let Some(task) = slot.lock().unwrap().take() {
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
                                if let Some(task) = slot.lock().unwrap().take() {
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
                            if let Some(task) = slot.lock().unwrap().take() {
                                requeue(&inner2, task);
                            }
                        }));
                    }
                    None => {
                        // No block reason — shouldn't happen but treat as yield.
                        if let Some(task) = task_slot.lock().unwrap().take() {
                            let mut queue = inner.run_queue.lock().unwrap();
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
    let mut queue = inner.run_queue.lock().unwrap();
    queue.push_back(task);
    inner.condvar.notify_one();
}
