//! Event-driven wake graph for the M:N scheduler.
//!
//! Phase 4 of the watchdog rewrite: the polling layer is gone — this
//! graph is now the SOLE deadlock detector. The graph mutates on every
//! park / wake / spawn / complete and signals interested waiters
//! (`Scheduler::install_main_waiter`) the moment a state change could
//! plausibly unblock them. Main-thread waits are now indefinite
//! `condvar.wait` calls woken only by real wake events; the previous
//! 100ms polling tick + consecutive-streak escalator is deleted.
//!
//! ## Design
//!
//! The graph is a per-task map from `TaskId` (or the singleton
//! `NodeId::MAIN`) to a `ParkEdge` describing what that task is parked
//! on. Reverse indices keep per-channel listener sets so a deadlock
//! check is `O(reachable nodes via Join chains)` plus a constant-time
//! counterparty lookup per channel edge.
//!
//! ### What "starved" means
//!
//! `is_main_starved(target)` returns `true` iff the wake graph proves
//! that no scheduled task can drive `target` forward. Soundness rules:
//!
//!   1. If any live task is queued / running (i.e. has no parked edge),
//!      it is "fuel" for ANY target — we can't predict what an
//!      unparked task will do, so we must not declare deadlock.
//!   2. If `target` is a channel and the channel has a pending timer
//!      close (`Channel::has_pending_timer_close`), an external timer
//!      thread will fire `ch.close()` → drains wakers → wakes main.
//!      Not starved.
//!   3. If `target` is `Recv(ch)` and `ch_send_listeners[ch]` is
//!      non-empty, a parked sender exists; the rendezvous-handshake
//!      protocol guarantees at least one of them will be requeued
//!      (either main's recv-waker registration fires `wake_send`, or
//!      the registration already did and the requeue is in flight).
//!      Symmetric rule for `Send(ch)`.
//!   4. If `target` is `Join(handle_id)`, recursively walk the joinee's
//!      Join edges to find a runnable task. Channel edges encountered
//!      mid-walk apply rule 3. `ParkEdge::Io` is fuel (external waker).
//!
//! Otherwise — every reachable path is parked on a graph edge with no
//! counterparty and no fuel — return `true` (starved). The watchdog
//! fires `deadlock` immediately, no consecutive-tick streak required.
//!
//! ### Why a runnable task counts as universal fuel
//!
//! Pre-Phase-4, the polling fallback's `Scheduler::can_make_progress`
//! treated `live > internal_blocked` as "fuel exists". The wake-graph
//! BFS was narrower: it only counted fuel reachable through a chain
//! of edges from `target`. With polling deleted, the BFS must subsume
//! the polling semantic too — otherwise the legitimate fan-in shape
//! `spawn N senders → main parks recv` would false-positive on the
//! window where the senders are queued but haven't yet parked.
//!
//! ### Select ordering hazard
//!
//! `register_*_waker_guard` (in `src/value.rs`) can synchronously
//! invoke the waker mid-registration if a counterparty is already
//! parked on the channel. For the Select arm — which registers wakers
//! on N channels in a row — that means an inline-fire on iteration K
//! drops the task before iterations K+1..N register. The graph's
//! `on_park` for the task happens once (BEFORE the registration loop)
//! and `on_wake` clears the edge atomically once the inline-fire's
//! `requeue` runs.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use parking_lot::Mutex;

use crate::value::{Channel, TaskHandle};

/// Stable identifier for a node in the wake graph. The main thread is
/// the singleton `NodeId::MAIN`; every spawned task is `NodeId::Task(id)`
/// where `id` is the task's `TaskHandle::id`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum NodeId {
    /// The main thread (`fn main`). There is exactly one.
    Main,
    /// A spawned task identified by its `TaskHandle::id`.
    Task(usize),
}

impl NodeId {
    /// Convenience for the main-thread singleton.
    pub const MAIN: NodeId = NodeId::Main;
}

/// One edge of the wake graph: what a parked node is waiting on.
/// Channel variants carry an `Arc<Channel>` so the BFS can consult
/// `Channel::has_pending_timer_close` directly when classifying a
/// parked node — a channel scheduled to close by an external timer
/// (e.g. `channel.timeout(50)`) is fuel even when nobody else is
/// listening.
#[derive(Clone)]
pub enum ParkEdge {
    /// Parked on `channel.receive(ch)` — wakes when a value lands in
    /// the channel (a parked / future sender, a buffered value, or
    /// the channel closing).
    Recv(Arc<Channel>),
    /// Parked on `channel.send(ch)` — wakes when buffer space opens
    /// or a receiver appears.
    Send(Arc<Channel>),
    /// Parked on `channel.select(...)` — wakes when ANY of the listed
    /// edges fires.
    Select(Vec<SelectEdge>),
    /// Parked on `task.join(handle_id)` — wakes when the joinee's
    /// `TaskHandle::complete` fires.
    Join(usize),
    /// Parked on external I/O — opaque to the graph; treated as
    /// always-might-unblock because an I/O completion is an external
    /// wake source the graph cannot reason about.
    Io,
}

// Manual Debug for ParkEdge — `Channel` doesn't impl Debug. Render
// channel variants as `Recv(<id>)` / `Send(<id>)`.
impl std::fmt::Debug for ParkEdge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParkEdge::Recv(ch) => write!(f, "Recv({})", ch.id),
            ParkEdge::Send(ch) => write!(f, "Send({})", ch.id),
            ParkEdge::Select(edges) => write!(f, "Select({edges:?})"),
            ParkEdge::Join(h) => write!(f, "Join({h})"),
            ParkEdge::Io => write!(f, "Io"),
        }
    }
}

/// One alternative inside a `ParkEdge::Select`. Same shape as
/// `ParkEdge::{Recv, Send}` but flat to keep the graph small.
#[derive(Clone)]
pub enum SelectEdge {
    Recv(Arc<Channel>),
    Send(Arc<Channel>),
}

impl std::fmt::Debug for SelectEdge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SelectEdge::Recv(ch) => write!(f, "Recv({})", ch.id),
            SelectEdge::Send(ch) => write!(f, "Send({})", ch.id),
        }
    }
}

/// The wake graph itself. Held inside `SchedulerInner` behind a
/// `Mutex` so every park / wake / spawn / complete site can mutate it
/// under a single lock acquisition.
///
/// All mutator entry points are designed to be called from the
/// existing scheduler call sites (`Scheduler::submit`, `worker_loop`'s
/// Blocked arms, `requeue`, terminal arms). The graph never spawns
/// threads of its own.
pub struct WakeGraph {
    inner: Mutex<GraphInner>,
}

/// Internal mutable state of the wake graph. Behind one `Mutex` so
/// every mutation is observed atomically by readers (`is_main_starved`).
///
/// Sets are `BTreeSet` (rather than `HashSet`) because the BFS in
/// `is_main_starved` benefits from deterministic iteration order: a
/// debug-build consistency check is much easier to interpret if the
/// walk visits nodes in a stable order across runs.
struct GraphInner {
    /// Per-node park edge. A node not in the map is "runnable" — it is
    /// either queued, running on a worker, or has already completed.
    /// Completed tasks are removed; runnable ones are simply absent.
    edges: BTreeMap<NodeId, ParkEdge>,
    /// Reverse index: for each channel id, the set of task / main
    /// nodes that are currently waiting to RECEIVE on it. Includes
    /// Select arms whose `SelectEdge::Recv(ch)` matches.
    ch_recv_listeners: BTreeMap<usize, BTreeSet<NodeId>>,
    /// Reverse index: for each channel id, the set of task / main
    /// nodes that are currently waiting to SEND on it. Includes
    /// Select arms.
    ch_send_listeners: BTreeMap<usize, BTreeSet<NodeId>>,
    /// Reverse index: for each handle id, the set of task / main
    /// nodes that are blocked in `task.join` on that handle.
    join_listeners: BTreeMap<usize, BTreeSet<NodeId>>,
    /// Live (not yet completed) task ids — used by `is_main_starved`
    /// to find "fuel" nodes (tasks alive but absent from `edges`).
    live_tasks: BTreeSet<usize>,
    /// True iff `MAIN` has been registered as live. Set by
    /// `register_main_present` once the program has imported anything
    /// concurrency-related; cleared by `unregister_main`. The graph
    /// only does work while main is present.
    main_present: bool,
}

impl Default for WakeGraph {
    fn default() -> Self {
        Self::new()
    }
}

impl WakeGraph {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(GraphInner {
                edges: BTreeMap::new(),
                ch_recv_listeners: BTreeMap::new(),
                ch_send_listeners: BTreeMap::new(),
                join_listeners: BTreeMap::new(),
                live_tasks: BTreeSet::new(),
                main_present: false,
            }),
        }
    }

    /// Note that a fresh task with `task_id` has been spawned. It is
    /// runnable (no edge); the membership in `live_tasks` is what
    /// makes it count as "fuel" for any other parked node's BFS.
    pub fn on_spawn(&self, task_id: usize) {
        let mut g = self.inner.lock();
        g.live_tasks.insert(task_id);
    }

    /// Note that `task_id` has finished executing (Completed / Failed /
    /// cancelled). Removes it from `live_tasks` and tears down any
    /// edge it was parked on. A removed node cannot be reached by any
    /// future BFS, so this is what flips a graph from "fuel reachable"
    /// to "starved" when the last task completes without sending.
    pub fn on_complete(&self, task_id: usize) {
        let mut g = self.inner.lock();
        let node = NodeId::Task(task_id);
        if let Some(edge) = g.edges.remove(&node) {
            remove_edge_indices(&mut g, node, &edge);
        }
        g.live_tasks.remove(&task_id);
    }

    /// Register the main thread as a live participant. Safe to call
    /// multiple times — idempotent. Without this, `is_main_starved`
    /// always returns `false` (the graph defers to "no proof" rather
    /// than firing on a program that never set up concurrency).
    pub fn register_main_present(&self) {
        self.inner.lock().main_present = true;
    }

    /// Park a task / main thread on the given edge. Replaces any
    /// previous edge for the same node (fine: a node can only park on
    /// one thing at a time, and a stale edge would just be a leak).
    pub fn on_park(&self, node: NodeId, edge: ParkEdge) {
        let mut g = self.inner.lock();
        if let Some(prev) = g.edges.remove(&node) {
            remove_edge_indices(&mut g, node, &prev);
        }
        insert_edge_indices(&mut g, node, &edge);
        g.edges.insert(node, edge);
    }

    /// Unpark a task / main thread (its waker fired). Removes the
    /// node from `edges` and from any reverse-index entries.
    pub fn on_wake(&self, node: NodeId) {
        let mut g = self.inner.lock();
        if let Some(edge) = g.edges.remove(&node) {
            remove_edge_indices(&mut g, node, &edge);
        }
    }

    /// Walk the wake graph from MAIN's target and return `true` iff NO
    /// runnable / I/O-parked task / pending-counterparty edge is
    /// reachable. A `true` return is the event-driven deadlock signal —
    /// the watchdog fires `deadlock` immediately, no consecutive-tick
    /// streak required.
    ///
    /// See the module-level docs for the four soundness rules.
    pub fn is_main_starved(&self, target: &MainTarget) -> bool {
        let g = self.inner.lock();
        if !g.main_present {
            return false;
        }

        // Rule 1: any "non-stuck" live task is universal fuel — we
        // can't predict what queued / running / I/O-parked tasks will
        // do. This subsumes the legacy `Scheduler::can_make_progress`
        // check that the polling watchdog used to call separately.
        //
        // "Stuck" here means a task that is parked on an internal
        // graph edge (Recv / Send / Select / Join). A task absent
        // from `edges` is queued / running / between-slices; a task
        // with `ParkEdge::Io` has an external waker that will fire
        // independently. Either way, it's fuel for ANY target.
        let internally_parked_task_count = g
            .edges
            .iter()
            .filter(|(node, edge)| matches!(node, NodeId::Task(_)) && !matches!(edge, ParkEdge::Io))
            .count();
        if g.live_tasks.len() > internally_parked_task_count {
            return false;
        }

        // Rules 2–4: target-specific reasoning. For channel targets,
        // a pending timer close or a parked counterparty is fuel; for
        // join targets, walk Join chains until a runnable / I/O /
        // pending-counterparty node is reached.
        match target {
            MainTarget::Recv(ch) => {
                if ch.has_pending_timer_close() {
                    return false;
                }
                if has_listeners(&g.ch_send_listeners, ch.id) {
                    return false;
                }
                true
            }
            MainTarget::Send(ch) => {
                if ch.has_pending_timer_close() {
                    return false;
                }
                if has_listeners(&g.ch_recv_listeners, ch.id) {
                    return false;
                }
                true
            }
            MainTarget::Select(edges) => {
                for e in edges {
                    let (ch, listeners) = match e {
                        SelectEdge::Recv(ch) => (ch, &g.ch_send_listeners),
                        SelectEdge::Send(ch) => (ch, &g.ch_recv_listeners),
                    };
                    if ch.has_pending_timer_close() {
                        return false;
                    }
                    if has_listeners(listeners, ch.id) {
                        return false;
                    }
                }
                true
            }
            MainTarget::Join(handle_id) => bfs_join_starved(&g, *handle_id),
        }
    }
}

/// Walk Join chains starting from `seed_handle_id`. Returns `true`
/// iff every reachable joinee is parked on an edge with no fuel
/// (no counterparty, no I/O, no pending external close, no
/// live-runnable producer through the chain).
///
/// Channel edges encountered mid-walk apply the rendezvous-handshake
/// rule (a parked Recv with a parked Send counterparty — or vice
/// versa — is fuel, the handshake will resolve) AND the timer-close
/// rule (a parked Recv/Send on a channel scheduled to close is
/// fuel, the timer thread will fire `wake_*` when it runs).
fn bfs_join_starved(g: &GraphInner, seed_handle_id: usize) -> bool {
    let mut queue: VecDeque<NodeId> = VecDeque::new();
    let mut visited: BTreeSet<NodeId> = BTreeSet::new();
    let seed = NodeId::Task(seed_handle_id);
    visited.insert(seed);
    queue.push_back(seed);

    while let Some(node) = queue.pop_front() {
        match g.edges.get(&node) {
            None => {
                if let NodeId::Task(id) = node
                    && g.live_tasks.contains(&id)
                {
                    // Joinee (or transitive joinee) is queued / running.
                    return false;
                }
                // Else: completed task. Through a Join edge, this
                // means the joinee is gone — main's `try_get` on the
                // next wake will see the result and return cleanly.
                // Not fuel through this path; continue BFS (other
                // queued nodes may still be fuel via Rule 1, which
                // is_main_starved already checked).
            }
            Some(ParkEdge::Io) => return false,
            Some(ParkEdge::Recv(ch)) => {
                if ch.has_pending_timer_close() {
                    return false;
                }
                if has_listeners(&g.ch_send_listeners, ch.id) {
                    return false;
                }
            }
            Some(ParkEdge::Send(ch)) => {
                if ch.has_pending_timer_close() {
                    return false;
                }
                if has_listeners(&g.ch_recv_listeners, ch.id) {
                    return false;
                }
            }
            Some(ParkEdge::Select(edges)) => {
                for e in edges {
                    match e {
                        SelectEdge::Recv(ch) => {
                            if ch.has_pending_timer_close() {
                                return false;
                            }
                            if has_listeners(&g.ch_send_listeners, ch.id) {
                                return false;
                            }
                        }
                        SelectEdge::Send(ch) => {
                            if ch.has_pending_timer_close() {
                                return false;
                            }
                            if has_listeners(&g.ch_recv_listeners, ch.id) {
                                return false;
                            }
                        }
                    }
                }
            }
            Some(ParkEdge::Join(handle_id)) => {
                let next = NodeId::Task(*handle_id);
                if visited.insert(next) {
                    queue.push_back(next);
                }
            }
        }
    }
    true
}

/// `true` iff `map[ch_id]` exists and is non-empty.
fn has_listeners(map: &BTreeMap<usize, BTreeSet<NodeId>>, ch_id: usize) -> bool {
    map.get(&ch_id).is_some_and(|s| !s.is_empty())
}

/// What the main thread is currently parked on. Channel variants
/// carry an `Arc<Channel>` so the graph can consult the channel's
/// `has_pending_timer_close` flag without reaching back into the
/// scheduler — the timer is an external waker that the BFS would
/// otherwise misclassify as starvation.
#[derive(Clone)]
pub enum MainTarget {
    Recv(Arc<Channel>),
    Send(Arc<Channel>),
    Join(usize),
    Select(Vec<SelectEdge>),
}

// Manual Debug — `Channel` doesn't impl Debug (it owns mutexes /
// VecDeques of waker closures whose Debug bounds we don't want to
// require). Render channel variants as their channel id only.
impl std::fmt::Debug for MainTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MainTarget::Recv(ch) => write!(f, "Recv({})", ch.id),
            MainTarget::Send(ch) => write!(f, "Send({})", ch.id),
            MainTarget::Join(h) => write!(f, "Join({h})"),
            MainTarget::Select(edges) => write!(f, "Select({edges:?})"),
        }
    }
}

impl MainTarget {
    /// Build a `MainTarget::Recv` from a channel reference.
    pub fn from_recv(ch: &Arc<Channel>) -> Self {
        MainTarget::Recv(ch.clone())
    }
    /// Build a `MainTarget::Send` from a channel reference.
    pub fn from_send(ch: &Arc<Channel>) -> Self {
        MainTarget::Send(ch.clone())
    }
    /// Build a `MainTarget::Join` from a task handle reference.
    pub fn from_join(handle: &Arc<TaskHandle>) -> Self {
        MainTarget::Join(handle.id)
    }
}

// ── Internal index maintenance ────────────────────────────────────

fn insert_edge_indices(g: &mut GraphInner, node: NodeId, edge: &ParkEdge) {
    match edge {
        ParkEdge::Recv(ch) => {
            g.ch_recv_listeners.entry(ch.id).or_default().insert(node);
        }
        ParkEdge::Send(ch) => {
            g.ch_send_listeners.entry(ch.id).or_default().insert(node);
        }
        ParkEdge::Join(h) => {
            g.join_listeners.entry(*h).or_default().insert(node);
        }
        ParkEdge::Select(edges) => {
            for e in edges {
                match e {
                    SelectEdge::Recv(ch) => {
                        g.ch_recv_listeners.entry(ch.id).or_default().insert(node);
                    }
                    SelectEdge::Send(ch) => {
                        g.ch_send_listeners.entry(ch.id).or_default().insert(node);
                    }
                }
            }
        }
        ParkEdge::Io => {
            // No reverse index for I/O edges — they're opaque to the
            // graph; an I/O-parked node is treated as fuel during BFS
            // (the I/O completion will eventually wake it).
        }
    }
}

fn remove_edge_indices(g: &mut GraphInner, node: NodeId, edge: &ParkEdge) {
    let prune_set = |map: &mut BTreeMap<usize, BTreeSet<NodeId>>, key: usize| {
        if let Some(set) = map.get_mut(&key) {
            set.remove(&node);
            if set.is_empty() {
                map.remove(&key);
            }
        }
    };
    match edge {
        ParkEdge::Recv(ch) => prune_set(&mut g.ch_recv_listeners, ch.id),
        ParkEdge::Send(ch) => prune_set(&mut g.ch_send_listeners, ch.id),
        ParkEdge::Join(h) => prune_set(&mut g.join_listeners, *h),
        ParkEdge::Select(edges) => {
            for e in edges {
                match e {
                    SelectEdge::Recv(ch) => prune_set(&mut g.ch_recv_listeners, ch.id),
                    SelectEdge::Send(ch) => prune_set(&mut g.ch_send_listeners, ch.id),
                }
            }
        }
        ParkEdge::Io => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(id: usize) -> Arc<Channel> {
        Arc::new(Channel::new(id, 4))
    }

    fn rdv(id: usize) -> Arc<Channel> {
        Arc::new(Channel::new(id, 0))
    }

    /// Empty graph with main present and no edges: starved for any
    /// channel target (no fuel); for Join, also starved (joinee absent).
    #[test]
    fn empty_graph_with_main_is_starved() {
        let g = WakeGraph::new();
        g.register_main_present();
        assert!(g.is_main_starved(&MainTarget::Recv(ch(0))));
        assert!(g.is_main_starved(&MainTarget::Send(ch(0))));
        assert!(g.is_main_starved(&MainTarget::Join(99)));
    }

    /// Rule 1: a spawned-but-not-parked task is universal fuel for
    /// any target — the polling-era `can_make_progress` semantics.
    /// This is the fan-in shape: senders submitted but not yet parked.
    #[test]
    fn live_runnable_task_is_universal_fuel() {
        let g = WakeGraph::new();
        g.register_main_present();
        g.on_spawn(7);
        // Task 7 is queued (in live_tasks, no edge) — counts as fuel
        // for ANY target.
        assert!(!g.is_main_starved(&MainTarget::Recv(ch(0))));
        assert!(!g.is_main_starved(&MainTarget::Send(ch(0))));
        assert!(!g.is_main_starved(&MainTarget::Join(99)));
    }

    /// Rule 3: parked sender on the channel main wants to recv is
    /// fuel — the rendezvous-handshake protocol guarantees a wake.
    /// Pre-Phase-4 the BFS classified this as starved (no fuel
    /// reachable through the cycle); the polling layer overrode with
    /// `Channel::watchdog_might_unblock_recv`. Now the graph itself
    /// recognises the handshake-pending state.
    #[test]
    fn parked_sender_is_fuel_for_main_recv() {
        let g = WakeGraph::new();
        g.register_main_present();
        g.on_spawn(1);
        let c = rdv(42);
        g.on_park(NodeId::Task(1), ParkEdge::Send(c.clone()));
        assert!(!g.is_main_starved(&MainTarget::Recv(c)));
    }

    /// Rule 3 symmetric: parked receiver on main's send target is fuel.
    #[test]
    fn parked_receiver_is_fuel_for_main_send() {
        let g = WakeGraph::new();
        g.register_main_present();
        g.on_spawn(1);
        let c = rdv(42);
        g.on_park(NodeId::Task(1), ParkEdge::Recv(c.clone()));
        assert!(!g.is_main_starved(&MainTarget::Send(c)));
    }

    /// Rule 4: join on a runnable task is not starved.
    #[test]
    fn join_on_runnable_task_is_not_starved() {
        let g = WakeGraph::new();
        g.register_main_present();
        g.on_spawn(1);
        // Task 1 has no edge → it's runnable. Rule 1 also catches
        // this, but the Join BFS would too via the seed.
        assert!(!g.is_main_starved(&MainTarget::Join(1)));
    }

    /// Spawned task that completed without sending anywhere: live_tasks
    /// is empty, channel has no listeners. Main parks on recv → starved.
    #[test]
    fn completed_task_leaves_graph_starved() {
        let g = WakeGraph::new();
        g.register_main_present();
        g.on_spawn(1);
        g.on_complete(1);
        assert!(g.is_main_starved(&MainTarget::Recv(ch(0))));
        assert!(g.is_main_starved(&MainTarget::Join(1)));
    }

    /// I/O-parked task is fuel: external waker will eventually fire.
    #[test]
    fn io_parked_task_is_fuel() {
        let g = WakeGraph::new();
        g.register_main_present();
        g.on_spawn(1);
        g.on_park(NodeId::Task(1), ParkEdge::Io);
        // Rule 4: Join BFS visits Task(1), sees ParkEdge::Io → fuel.
        assert!(!g.is_main_starved(&MainTarget::Join(1)));
    }

    /// Pending timer close: main parks on recv, channel is scheduled
    /// to close (e.g. `channel.receive(ch, timeout(50))`). The timer
    /// thread will fire `ch.close()` and wake main. Not starved.
    #[test]
    fn pending_timer_close_is_fuel() {
        let g = WakeGraph::new();
        g.register_main_present();
        let c = ch(0);
        c.mark_pending_timer_close();
        assert!(!g.is_main_starved(&MainTarget::Recv(c)));
    }

    /// A select that includes a recv on a channel with a parked
    /// sender registers in ch_recv_listeners. After the select wakes,
    /// the listener entry must be cleared so a future BFS does not
    /// see a phantom recv listener.
    #[test]
    fn select_park_then_wake_clears_indices() {
        let g = WakeGraph::new();
        g.register_main_present();
        g.on_spawn(1);
        g.on_park(
            NodeId::Task(1),
            ParkEdge::Select(vec![SelectEdge::Recv(ch(10)), SelectEdge::Send(ch(20))]),
        );
        {
            let inner = g.inner.lock();
            assert_eq!(inner.ch_recv_listeners.len(), 1);
            assert_eq!(inner.ch_send_listeners.len(), 1);
        }
        g.on_wake(NodeId::Task(1));
        let inner = g.inner.lock();
        assert!(inner.ch_recv_listeners.is_empty());
        assert!(inner.ch_send_listeners.is_empty());
    }

    /// `on_park` with a previous edge cleans up the previous reverse
    /// index entries so an Initial Send → switch to Recv leaves
    /// `ch_send_listeners` empty.
    #[test]
    fn re_parking_replaces_previous_edge_indices() {
        let g = WakeGraph::new();
        g.register_main_present();
        g.on_spawn(1);
        let c = rdv(42);
        g.on_park(NodeId::Task(1), ParkEdge::Send(c.clone()));
        g.on_park(NodeId::Task(1), ParkEdge::Recv(c));
        let inner = g.inner.lock();
        assert!(inner.ch_send_listeners.is_empty());
        assert_eq!(inner.ch_recv_listeners.len(), 1);
    }

    /// Without `register_main_present`, `is_main_starved` returns
    /// `false` (the graph defers — no main means nobody to declare
    /// deadlock for).
    #[test]
    fn no_main_present_means_not_starved_default() {
        let g = WakeGraph::new();
        assert!(!g.is_main_starved(&MainTarget::Recv(ch(0))));
    }

    /// Transitive Join chain with a real deadlock: A joins B; B
    /// parks-Recv(10); nobody sends to 10. Main joins A. Starved.
    #[test]
    fn transitive_join_chain_starved_at_recv() {
        let g = WakeGraph::new();
        g.register_main_present();
        g.on_spawn(1);
        g.on_spawn(2);
        g.on_park(NodeId::Task(1), ParkEdge::Join(2));
        g.on_park(NodeId::Task(2), ParkEdge::Recv(rdv(10)));
        assert!(g.is_main_starved(&MainTarget::Join(1)));
    }

    /// Transitive Join chain with a parked counterparty downstream:
    /// A joins B; B parks-Recv(10); C parks-Send(10). The handshake
    /// will resolve, B will unblock, A will unblock. Not starved.
    #[test]
    fn transitive_join_chain_with_handshake_is_fuel() {
        let g = WakeGraph::new();
        g.register_main_present();
        g.on_spawn(1);
        g.on_spawn(2);
        g.on_spawn(3);
        let c = rdv(10);
        g.on_park(NodeId::Task(1), ParkEdge::Join(2));
        g.on_park(NodeId::Task(2), ParkEdge::Recv(c.clone()));
        g.on_park(NodeId::Task(3), ParkEdge::Send(c));
        assert!(!g.is_main_starved(&MainTarget::Join(1)));
    }

    /// Mutual Join cycle: A joins B; B joins A. Real deadlock.
    #[test]
    fn mutual_join_cycle_is_starved() {
        let g = WakeGraph::new();
        g.register_main_present();
        g.on_spawn(1);
        g.on_spawn(2);
        g.on_park(NodeId::Task(1), ParkEdge::Join(2));
        g.on_park(NodeId::Task(2), ParkEdge::Join(1));
        assert!(g.is_main_starved(&MainTarget::Join(1)));
    }
}
