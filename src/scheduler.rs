use std::cell::RefCell;
use std::rc::Rc;

use crate::ast::Expr;
use crate::env::Env;
use crate::value::{Channel, TaskHandle, Value};

// ── Task ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum TaskState {
    Ready,
    BlockedSend(usize),    // channel id
    BlockedReceive(usize), // channel id
    Completed,
    Cancelled,
}

pub struct Task {
    pub id: usize,
    pub body: Expr,
    pub env: Env,
    pub state: TaskState,
    pub handle: Rc<TaskHandle>,
}

// ── Scheduler ───────────────────────────────────────────────────────

pub struct Scheduler {
    tasks: Vec<Task>,
    next_task_id: usize,
    next_channel_id: usize,
}

impl Scheduler {
    pub fn new() -> Self {
        Self {
            tasks: Vec::new(),
            next_task_id: 0,
            next_channel_id: 0,
        }
    }

    /// Create a new channel with the given capacity.
    pub fn create_channel(&mut self, capacity: usize) -> Value {
        let id = self.next_channel_id;
        self.next_channel_id += 1;
        Value::Channel(Rc::new(Channel::new(id, capacity)))
    }

    /// Spawn a new task. Returns a Handle value.
    pub fn spawn(&mut self, body: Expr, env: Env) -> Value {
        let id = self.next_task_id;
        self.next_task_id += 1;
        let handle = Rc::new(TaskHandle {
            id,
            result: RefCell::new(None),
        });
        let task = Task {
            id,
            body,
            env,
            state: TaskState::Ready,
            handle: handle.clone(),
        };
        self.tasks.push(task);
        Value::Handle(handle)
    }

    /// Take all ready tasks out for execution, in FIFO order.
    /// Combined with the scheduler's round-robin yield behavior (yielded
    /// tasks are re-enqueued at the end), this naturally rotates which
    /// task runs first across scheduling rounds.
    pub fn take_ready_tasks(&mut self) -> Vec<Task> {
        let mut ready = Vec::new();
        let mut remaining = Vec::new();
        for task in self.tasks.drain(..) {
            if task.state == TaskState::Ready {
                ready.push(task);
            } else {
                remaining.push(task);
            }
        }
        self.tasks = remaining;
        ready
    }

    /// Return tasks that were not completed back to the scheduler.
    pub fn return_tasks(&mut self, tasks: Vec<Task>) {
        for task in tasks {
            if task.state != TaskState::Completed && task.state != TaskState::Cancelled {
                self.tasks.push(task);
            }
        }
    }

    /// Check if a task with the given handle id is completed.
    pub fn is_completed(&self, handle_id: usize) -> bool {
        // Check in remaining tasks
        for task in &self.tasks {
            if task.id == handle_id {
                return task.state == TaskState::Completed;
            }
        }
        // Task not found in scheduler means it was already completed and removed
        true
    }

    /// Cancel a task by handle id.
    pub fn cancel(&mut self, handle_id: usize) {
        for task in &mut self.tasks {
            if task.id == handle_id {
                task.state = TaskState::Cancelled;
                *task.handle.result.borrow_mut() = Some(Err("cancelled".to_string()));
                return;
            }
        }
    }

    /// Check if there are any pending (non-completed, non-cancelled) tasks.
    pub fn has_pending_tasks(&self) -> bool {
        self.tasks.iter().any(|t| {
            t.state != TaskState::Completed && t.state != TaskState::Cancelled
        })
    }

    /// Try to unblock tasks that are waiting on channels.
    pub fn try_unblock(&mut self, channels: &[Rc<Channel>]) {
        for task in &mut self.tasks {
            match &task.state {
                TaskState::BlockedSend(ch_id) => {
                    if let Some(ch) = channels.iter().find(|c| c.id == *ch_id) {
                        // Unblock if there's room, or if the channel was closed
                        // (the send will then fail with an error, but the task
                        // needs to run to observe that).
                        if ch.closed.get() {
                            task.state = TaskState::Ready;
                        } else {
                            let buf = ch.buffer.borrow();
                            if buf.len() < ch.capacity {
                                task.state = TaskState::Ready;
                            }
                        }
                    }
                }
                TaskState::BlockedReceive(ch_id) => {
                    if let Some(ch) = channels.iter().find(|c| c.id == *ch_id) {
                        if !ch.buffer.borrow().is_empty() || ch.closed.get() {
                            task.state = TaskState::Ready;
                        }
                    }
                }
                _ => {}
            }
        }
    }
}
