//! Who spawned whom, for `task.cancel`: a cancel of a task also cancels
//! every task it spawned that still runs, and theirs, whether or not the
//! tasks between have ended (`Scheduler::cancel`).
//!
//! A task joins a tree when a task spawns it (`Scheduler::submit`), its
//! spawner with it: a task that only `main` (or a front end) spawns, and
//! that spawns nothing, is never in one. A spawner that is in no tree
//! yet is the root of a new one, and every task below it is in that
//! tree: the trees never meet, and each has a lock of its own, so tasks
//! that `main` started spawn side by side without waiting for each
//! other. A node stays while its task's handle lives, so that
//! `task.cancel(h)` of a task that has returned still reaches what it
//! spawned. When the handle is dropped the node is taken out and its
//! children go to its parent: a task that nobody can name any more
//! links its ancestors to its descendants and holds nothing itself. So
//! a tree holds the live handles, and no more.
//!
//! A tree's lock is never held while a handle is dropped (dropping one
//! takes it) or while anything is woken.

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::atomic::{AtomicU64, Ordering, fence};
use std::sync::{Arc, OnceLock, Weak};

use parking_lot::Mutex;

use super::handle::TaskHandle;

/// The hasher of the keys, which are unique counts: one multiplication.
#[derive(Default)]
struct KeyHasher(u64);

impl Hasher for KeyHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 = (self.0 ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3);
        }
    }
    fn write_u64(&mut self, key: u64) {
        self.0 = key.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    }
}

type Keys = BuildHasherDefault<KeyHasher>;

/// A task in a tree.
struct Node {
    /// The task that spawned it, or the nearest ancestor whose handle
    /// lives; `None` for the root and for a task whose ancestors' handles
    /// are all dropped.
    parent: Option<u64>,
    /// The tasks it spawned, and those that went to it from a child
    /// whose handle was dropped.
    kids: HashSet<u64, Keys>,
    handle: Weak<TaskHandle>,
}

/// The tasks of one tree, by the key of their handles' [`Member`].
type Tree = Mutex<HashMap<u64, Node, Keys>>;

/// The keys of the nodes: never used twice.
static NEXT_KEY: AtomicU64 = AtomicU64::new(1);

/// A handle's place in a tree: the tree and its key there, once it has
/// one. Dropped with the handle, which takes the node out of the tree.
#[derive(Default)]
pub(crate) struct Member(OnceLock<(Arc<Tree>, u64)>);

impl Drop for Member {
    fn drop(&mut self) {
        if let Some((tree, key)) = self.0.get() {
            forget(tree, *key);
        }
    }
}

/// A node for `handle` in `tree`, under `parent`: its key.
fn insert(
    tree: &mut HashMap<u64, Node, Keys>,
    handle: &Arc<TaskHandle>,
    parent: Option<u64>,
) -> u64 {
    let key = NEXT_KEY.fetch_add(1, Ordering::Relaxed);
    tree.insert(
        key,
        Node {
            parent,
            kids: HashSet::default(),
            handle: Arc::downgrade(handle),
        },
    );
    key
}

/// `parent` spawned `child`. True when `parent` has been cancelled: the
/// child is then to be cancelled at once, by the caller, as the cancel
/// of `parent` did not see it. A cancel marks the tasks it reaches in
/// the same hold of the tree's lock in which it finds them, so a task
/// spawned by one of them after that sees its spawner's mark here; and
/// the cancel of a spawner that was in no tree yet marks it before it
/// looks for its tree, which a first spawn makes before it reads the
/// mark (`running_below`).
pub(crate) fn spawned(parent: &Arc<TaskHandle>, child: &Arc<TaskHandle>) -> bool {
    let member = &parent.family().0;
    let (tree, parent_key) = match member.get() {
        Some((tree, key)) => (tree.clone(), *key),
        None => {
            // The parent's first spawn: a new tree with it as the root.
            // (Only the parent's own slice spawns its children: nobody
            // else sets this.)
            let tree: Arc<Tree> = Arc::default();
            let key = insert(&mut tree.lock(), parent, None);
            let _ = member.set((tree.clone(), key));
            (tree, key)
        }
    };
    let mut nodes = tree.lock();
    let child_key = insert(&mut nodes, child, Some(parent_key));
    let _ = child.family().0.set((tree.clone(), child_key));
    if let Some(node) = nodes.get_mut(&parent_key) {
        node.kids.insert(child_key);
    }
    // (Against the cancel of a parent that had no tree when it looked.)
    fence(Ordering::SeqCst);
    parent.is_cancelled()
}

/// The tasks below `handle` in its tree that have no result yet, each
/// marked as cancelled ([`TaskHandle::mark_cancelled`]) before the lock
/// is released. Below a task that has ended the search goes on: what
/// that task spawned may still run. The caller has marked `handle`
/// itself.
pub(crate) fn running_below(handle: &TaskHandle) -> Vec<Arc<TaskHandle>> {
    // (After the mark of `handle`: a first spawn of its task either is
    // seen here or sees the mark.)
    fence(Ordering::SeqCst);
    let Some((tree, key)) = handle.family().0.get() else {
        return Vec::new();
    };
    let mut running = Vec::new();
    // The handles of the tasks that have ended, dropped once the lock
    // is released: the last of them may be dropped here.
    let mut ended = Vec::new();
    {
        let nodes = tree.lock();
        let mut todo: Vec<u64> = match nodes.get(key) {
            Some(node) => node.kids.iter().copied().collect(),
            None => Vec::new(),
        };
        while let Some(key) = todo.pop() {
            let Some(node) = nodes.get(&key) else {
                continue;
            };
            todo.extend(node.kids.iter().copied());
            let Some(task) = node.handle.upgrade() else {
                continue;
            };
            if task.has_result() {
                ended.push(task);
            } else {
                task.mark_cancelled();
                running.push(task);
            }
        }
    }
    drop(ended);
    running
}

/// The handle of the node `key` has been dropped: its children go to
/// its parent.
fn forget(tree: &Tree, key: u64) {
    let mut nodes = tree.lock();
    let Some(node) = nodes.remove(&key) else {
        return;
    };
    for kid in &node.kids {
        if let Some(kid) = nodes.get_mut(kid) {
            kid.parent = node.parent;
        }
    }
    if let Some(parent) = node.parent.and_then(|parent| nodes.get_mut(&parent)) {
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

    /// Whether the tree of `handle` holds the node of `key`.
    fn holds(handle: &TaskHandle, key: u64) -> bool {
        let (tree, _) = handle.family().0.get().expect("a task in a tree");
        tree.lock().contains_key(&key)
    }

    fn key_of(handle: &TaskHandle) -> u64 {
        handle.family().0.get().expect("a task in a tree").1
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
        assert_eq!(ids(&below), ids(std::slice::from_ref(&leaf)));
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
        let middle_key = key_of(&middle);
        drop(middle);
        assert!(!holds(&root, middle_key));
        assert_eq!(ids(&running_below(&root)), ids(std::slice::from_ref(&leaf)));
        // And the leaf's own handle, dropped, leaves the tree too.
        let leaf_key = key_of(&leaf);
        drop(leaf);
        assert!(!holds(&root, leaf_key));
        assert!(running_below(&root).is_empty());
    }

    /// Tasks below two roots are in two trees: a cancel of one does not
    /// look at the other.
    #[test]
    fn two_roots_have_two_trees() {
        let (one, two, under_one, under_two) = (task(), task(), task(), task());
        assert!(!spawned(&one, &under_one));
        assert!(!spawned(&two, &under_two));
        let tree = |handle: &TaskHandle| Arc::as_ptr(&handle.family().0.get().unwrap().0);
        assert_ne!(tree(&one), tree(&two));
        assert_eq!(tree(&one), tree(&under_one));
        assert_eq!(
            ids(&running_below(&one)),
            ids(std::slice::from_ref(&under_one))
        );
        assert!(!under_two.is_cancelled());
    }

    /// A task spawned by one that has been cancelled is to be cancelled
    /// at once, also by its first spawn.
    #[test]
    fn a_task_spawned_by_a_cancelled_task_is_told_so() {
        let (root, child, grandchild) = (task(), task(), task());
        root.mark_cancelled();
        assert!(spawned(&root, &child));
        child.mark_cancelled();
        assert!(spawned(&child, &grandchild));
    }
}
