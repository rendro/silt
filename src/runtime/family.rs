//! Who spawned whom, for `task.cancel`: a cancel of a task also cancels
//! every task it spawned that still runs, and theirs, whether or not the
//! tasks between have ended (`Scheduler::cancel`).
//!
//! A task joins the tree when a task spawns it (`Scheduler::submit`),
//! its spawner with it: a task that only `main` (or a front end) spawns,
//! and that spawns nothing, is never in it. A node stays while its
//! task's handle lives, so that `task.cancel(h)` of a task that has
//! returned still reaches what it spawned. When the handle is dropped
//! the node is taken out and its children go to its parent: a task
//! that nobody can name any more links its ancestors to its
//! descendants and holds nothing itself. So the tree holds the live
//! handles, and no more.
//!
//! One lock for the whole tree, never held while a handle is dropped
//! (dropping one takes the lock) or while anything is woken.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Weak};

use parking_lot::Mutex;

use super::handle::TaskHandle;

/// A task in the tree.
struct Node {
    /// The task that spawned it, or the nearest ancestor whose handle
    /// lives; `None` for a task that `main` or a front end spawned.
    parent: Option<u64>,
    /// The tasks it spawned, and those that went to it from a child
    /// whose handle was dropped.
    kids: HashSet<u64>,
    handle: Weak<TaskHandle>,
}

/// Every task in the tree, by the key of its handle's [`Member`].
static TREE: LazyLock<Mutex<HashMap<u64, Node>>> = LazyLock::new(Default::default);

/// The keys of the tree's nodes: never 0, never used twice.
static NEXT_KEY: AtomicU64 = AtomicU64::new(1);

/// A handle's place in the tree: its key, 0 while it has none. Dropped
/// with the handle, which takes the node out of the tree.
#[derive(Default)]
pub(crate) struct Member(AtomicU64);

impl Drop for Member {
    fn drop(&mut self) {
        let key = *self.0.get_mut();
        if key != 0 {
            forget(key);
        }
    }
}

/// The node of `handle`, made if it has none: its key.
fn node_of(tree: &mut HashMap<u64, Node>, handle: &Arc<TaskHandle>) -> u64 {
    let member = &handle.family().0;
    let key = member.load(Ordering::Acquire);
    if key != 0 {
        return key;
    }
    let key = NEXT_KEY.fetch_add(1, Ordering::Relaxed);
    tree.insert(
        key,
        Node {
            parent: None,
            kids: HashSet::new(),
            handle: Arc::downgrade(handle),
        },
    );
    member.store(key, Ordering::Release);
    key
}

/// `parent` spawned `child`. True when `parent` has been cancelled: the
/// child is then to be cancelled at once, by the caller, as the cancel
/// of `parent` did not see it. (A cancel marks the tasks it reaches in
/// the same hold of the lock in which it finds them, so a task spawned
/// by one of them after that sees its spawner's mark here.)
pub(crate) fn spawned(parent: &Arc<TaskHandle>, child: &Arc<TaskHandle>) -> bool {
    let mut tree = TREE.lock();
    let parent_key = node_of(&mut tree, parent);
    let child_key = node_of(&mut tree, child);
    if let Some(node) = tree.get_mut(&child_key) {
        node.parent = Some(parent_key);
    }
    if let Some(node) = tree.get_mut(&parent_key) {
        node.kids.insert(child_key);
    }
    parent.is_cancelled()
}

/// The tasks below `handle` in the tree that have no result yet, each
/// marked as cancelled ([`TaskHandle::mark_cancelled`]) before the lock
/// is released. Below a task that has ended the search goes on: what
/// that task spawned may still run.
pub(crate) fn running_below(handle: &TaskHandle) -> Vec<Arc<TaskHandle>> {
    let key = handle.family().0.load(Ordering::Acquire);
    if key == 0 {
        return Vec::new();
    }
    let mut running = Vec::new();
    // The handles of the tasks that have ended, dropped once the lock
    // is released: the last of them may be dropped here.
    let mut ended = Vec::new();
    {
        let tree = TREE.lock();
        let mut todo: Vec<u64> = match tree.get(&key) {
            Some(node) => node.kids.iter().copied().collect(),
            None => Vec::new(),
        };
        while let Some(key) = todo.pop() {
            let Some(node) = tree.get(&key) else {
                continue;
            };
            todo.extend(node.kids.iter().copied());
            let Some(task) = node.handle.upgrade() else {
                continue;
            };
            if task.try_get().is_none() {
                task.mark_cancelled();
                running.push(task);
            } else {
                ended.push(task);
            }
        }
    }
    drop(ended);
    running
}

/// The handle of the node `key` has been dropped: its children go to
/// its parent.
fn forget(key: u64) {
    let mut tree = TREE.lock();
    let Some(node) = tree.remove(&key) else {
        return;
    };
    for kid in &node.kids {
        if let Some(kid) = tree.get_mut(kid) {
            kid.parent = node.parent;
        }
    }
    if let Some(parent) = node.parent.and_then(|parent| tree.get_mut(&parent)) {
        parent.kids.remove(&key);
        parent.kids.extend(node.kids.iter().copied());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::sync::Wake;

    fn task() -> Arc<TaskHandle> {
        Arc::new(TaskHandle::new(0))
    }

    fn ids(tasks: &[Arc<TaskHandle>]) -> Vec<*const TaskHandle> {
        let mut ids: Vec<_> = tasks.iter().map(Arc::as_ptr).collect();
        ids.sort();
        ids
    }

    /// A scheduler's waker, for the results the tests set.
    fn wake() -> Arc<crate::scheduler::Scheduler> {
        crate::vm::Vm::new(crate::HostIo::process())
            .scheduler()
            .clone()
    }

    /// The search goes below a task that has ended, and gives the tasks
    /// that run, each marked as cancelled; one that has ended is not.
    #[test]
    fn the_tasks_below_are_found_through_a_task_that_ended() {
        let scheduler = wake();
        let wake: &dyn Wake = scheduler.wake();
        let (root, middle, leaf, done) = (task(), task(), task(), task());
        assert!(!spawned(&root, &middle));
        assert!(!spawned(&middle, &leaf));
        assert!(!spawned(&root, &done));
        assert!(middle.complete(Ok(crate::value::Value::Unit), wake));
        assert!(done.complete(Ok(crate::value::Value::Unit), wake));
        let below = running_below(&root);
        assert_eq!(ids(&below), ids(&[leaf.clone()]));
        assert!(leaf.is_cancelled());
        assert!(!done.is_cancelled() && !middle.is_cancelled());
        assert!(running_below(&leaf).is_empty());
    }

    /// When the handle of a task between two is dropped, what it spawned
    /// goes to its spawner: a cancel of that one still reaches it.
    #[test]
    fn a_dropped_handle_hands_its_children_to_its_spawner() {
        let (root, middle, leaf) = (task(), task(), task());
        assert!(!spawned(&root, &middle));
        assert!(!spawned(&middle, &leaf));
        let middle_key = middle.family().0.load(Ordering::Acquire);
        drop(middle);
        assert!(!TREE.lock().contains_key(&middle_key));
        assert_eq!(ids(&running_below(&root)), ids(&[leaf.clone()]));
        // And the leaf's own handle, dropped, leaves the tree too.
        let leaf_key = leaf.family().0.load(Ordering::Acquire);
        drop(leaf);
        assert!(!TREE.lock().contains_key(&leaf_key));
        assert!(running_below(&root).is_empty());
    }

    /// A task spawned by one that has been cancelled is to be cancelled
    /// at once.
    #[test]
    fn a_task_spawned_by_a_cancelled_task_is_told_so() {
        let (root, child) = (task(), task());
        root.mark_cancelled();
        assert!(spawned(&root, &child));
    }
}
