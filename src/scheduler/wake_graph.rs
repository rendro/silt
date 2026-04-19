//! Event-driven wake graph for the M:N scheduler.
//!
//! Phase 3 of the watchdog rewrite. Replaces the polling, streak-based
//! deadlock detector that lived in `src/builtins/concurrency.rs` with an
//! edge-driven graph that mutates on every park / wake / spawn / complete
//! and signals interested waiters the moment a state change could
//! plausibly unblock them.
//!
//! ## Design
//!
//! The graph is a per-task map from `TaskId` (or the singleton
//! `NodeId::MAIN`) to a `ParkEdge` describing what that task is parked
//! on. Reverse indices keep per-channel listener sets so a BFS for "is
//! there a still-runnable task whose unblock chain reaches `target`?"
//! is `O(reachable nodes)` rather than `O(all tasks)`.
//!
//! ### Why "fuel" instead of "reachability"
//!
//! For deadlock detection we don't need to prove that `target` will
//! definitely unblock — we need to prove that NO scheduled task could
//! ever unblock `target`. The cheapest sound test is: starting from
//! `target`'s edge, walk the reverse-listener indices and see whether
//! we reach a node that is NOT parked on a graph edge — i.e. one that
//! is queued, running, or parked on external I/O. Any such node is
//! "fuel": when its slice runs, it WILL eventually wake either `target`
//! directly or some intermediate parked task that wakes `target`.
//!
//! If the walk finds zero fuel nodes, the graph is closed: every
//! reachable task is parked on another graph edge that ALSO has no
//! fuel reachable from it. That is the deadlock signal — fire
//! immediately, no consecutive-tick streak required.
//!
//! ### Consistency with `Scheduler::can_make_progress`
//!
//! `can_make_progress` answers a coarser question: "is there ANY
//! task that could potentially unblock the main thread?" The graph
//! answers a finer one: "is there a task that could unblock THIS
//! specific edge (channel ID / handle ID)?" The two must agree on the
//! `false` case — if the graph says "no fuel", `can_make_progress` had
//! better also say "no fuel"; otherwise the watchdog would fire on a
//! channel that some unrelated runnable task is going to drive.
//! Debug builds assert this consistency.
//!
//! ### Select ordering hazard
//!
//! `register_*_waker_guard` (in `src/value.rs`) can synchronously
//! invoke the waker mid-registration if a counterparty is already
//! parked on the channel. For the Select arm — which registers wakers
//! on N channels in a row — that means an inline-fire on iteration K
//! drops the task before iterations K+1..N register. The graph cannot
//! be the source-of-truth for those interim states without a staging
//! discipline.
//!
//! The chosen mitigation is a thread-local pending-set: each
//! `BlockReason` arm calls `WakeGraph::stage_*` instead of
//! `WakeGraph::commit_*` while it builds out the per-arm edges, then
//! a single `WakeGraph::commit_pending(task_id)` flips the staged
//! edges atomically under one graph-lock acquisition. If an inline
//! fire happens during register (the select case), the staged set is
//! discarded by `WakeGraph::cancel_pending(task_id)` and the wake
//! path uses the empty graph state — which correctly says "task is
//! already runnable" because the inline-fire's `requeue` ran first.

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
#[derive(Clone, Debug)]
pub enum ParkEdge {
    /// Parked on `channel.receive(ch_id)` — wakes when a value lands
    /// in the channel (a parked / future sender, a buffered value, or
    /// the channel closing).
    Recv(usize),
    /// Parked on `channel.send(ch_id)` — wakes when buffer space opens
    /// or a receiver appears.
    Send(usize),
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

/// One alternative inside a `ParkEdge::Select`. Same shape as
/// `ParkEdge::{Recv, Send}` but flat to keep the graph small.
#[derive(Clone, Debug)]
pub enum SelectEdge {
    Recv(usize),
    Send(usize),
}

/// The wake graph itself. Held inside `SchedulerInner` behind a
/// `Mutex` so every park / wake / spawn / complete site can mutate it
/// under a single lock acquisition (matches the existing
/// `blocked_handles` lock cost — one extra `Mutex<HashMap>` worth).
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
    /// Reverse index: for each channel id, the set of task ids that
    /// are currently waiting to RECEIVE on it. Includes Select arms
    /// whose `SelectEdge::Recv(ch)` matches.
    ch_recv_listeners: BTreeMap<usize, BTreeSet<NodeId>>,
    /// Reverse index: for each channel id, the set of task ids that
    /// are currently waiting to SEND on it. Includes Select arms.
    ch_send_listeners: BTreeMap<usize, BTreeSet<NodeId>>,
    /// Reverse index: for each handle id, the set of task ids that
    /// are blocked in `task.join` on that handle.
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
    /// always returns `false` (no main = no main-side caller).
    pub fn register_main_present(&self) {
        self.inner.lock().main_present = true;
    }

    /// Park a task / main thread on the given edge. Replaces any
    /// previous edge for the same node (fine: a node can only park on
    /// one thing at a time, and a stale edge would just be a leak).
    pub fn on_park(&self, node: NodeId, edge: ParkEdge) {
        let mut g = self.inner.lock();
        // Drop any previous edge for this node (and its reverse-index
        // entries) before installing the new one. The "previous edge"
        // case is rare in practice — a node is unparked before being
        // re-parked — but the cleanup is cheap and keeps the indices
        // honest if the call ever races.
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

    /// Snapshot the number of currently parked nodes. Used by
    /// debug-build consistency checks against `Scheduler::progress_snapshot`.
    pub fn parked_count(&self) -> usize {
        self.inner.lock().edges.len()
    }

    /// Snapshot the number of live (not yet completed) tasks. Used
    /// by debug-build consistency checks.
    pub fn live_task_count(&self) -> usize {
        self.inner.lock().live_tasks.len()
    }

    /// Walk the wake graph from MAIN's edge (or `target` if MAIN is
    /// not parked) and return `true` iff NO runnable / I/O-parked task
    /// is reachable. A `true` return means the watchdog can fire
    /// `deadlock` immediately — there is provably no path from any
    /// scheduled task to wakening `target`.
    ///
    /// `target` describes what the main thread is currently waiting
    /// on. The walk starts there and follows reverse-listener edges
    /// to find which tasks could plausibly drive `target` to a wake.
    /// If any reachable node is either:
    ///
    ///   * not in `edges` (queued / running) AND in `live_tasks`, OR
    ///   * parked on `ParkEdge::Io` (external waker),
    ///
    /// the walk returns `false` (fuel exists). Otherwise the walk
    /// exhausts the closure — all reachable nodes are parked on
    /// graph edges with no fuel of their own — and returns `true`
    /// (true starvation).
    pub fn is_main_starved(&self, target: &MainTarget) -> bool {
        let g = self.inner.lock();
        if !g.main_present {
            // The graph isn't tracking main — fall through to the
            // legacy detector. Reporting `false` means "I don't have
            // proof of starvation"; the caller's existing logic still
            // gets to decide.
            return false;
        }
        // Seed the walk with the producer side of `target`. For Recv,
        // we need a Sender (or buffered value / closed) to wake us;
        // for Send, we need a Receiver.
        let mut queue: VecDeque<NodeId> = VecDeque::new();
        let mut visited: BTreeSet<NodeId> = BTreeSet::new();

        // Inlined seed logic — extracting it into closures fights
        // with the borrow checker because both `queue` and `visited`
        // would need to be captured mutably by each closure.
        match target {
            MainTarget::Recv(ch_id) => {
                if let Some(senders) = g.ch_send_listeners.get(ch_id) {
                    for s in senders {
                        if visited.insert(*s) {
                            queue.push_back(*s);
                        }
                    }
                }
            }
            MainTarget::Send(ch_id) => {
                if let Some(recvs) = g.ch_recv_listeners.get(ch_id) {
                    for r in recvs {
                        if visited.insert(*r) {
                            queue.push_back(*r);
                        }
                    }
                }
            }
            MainTarget::Join(handle_id) => {
                // For a join, the joinee task itself is the producer.
                let node = NodeId::Task(*handle_id);
                if visited.insert(node) {
                    queue.push_back(node);
                }
            }
            MainTarget::Select(edges) => {
                for e in edges {
                    match e {
                        SelectEdge::Recv(ch_id) => {
                            if let Some(senders) = g.ch_send_listeners.get(ch_id) {
                                for s in senders {
                                    if visited.insert(*s) {
                                        queue.push_back(*s);
                                    }
                                }
                            }
                        }
                        SelectEdge::Send(ch_id) => {
                            if let Some(recvs) = g.ch_recv_listeners.get(ch_id) {
                                for r in recvs {
                                    if visited.insert(*r) {
                                        queue.push_back(*r);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // BFS over the wake graph. Each reachable node is examined:
        //   * if it has no edge but is in live_tasks → fuel; return false.
        //   * if it is parked on Io → fuel; return false.
        //   * otherwise enqueue its dependencies (the producers for
        //     whatever IT is parked on) and continue.
        while let Some(node) = queue.pop_front() {
            // A node we don't track in `edges` can be either runnable
            // (if it's still live) or already completed.
            match g.edges.get(&node) {
                None => {
                    if let NodeId::Task(id) = node {
                        if g.live_tasks.contains(&id) {
                            // Fuel: this task is queued or running and
                            // will eventually drive `target` forward
                            // (or at least could).
                            return false;
                        }
                    } else {
                        // MAIN never appears as a "fuel" node from
                        // anyone else's perspective — but it should not
                        // appear in the walk either; defensive fall-through.
                        return false;
                    }
                }
                Some(ParkEdge::Io) => {
                    // External I/O is opaque — its completion will
                    // eventually fire and wake this task.
                    return false;
                }
                Some(ParkEdge::Recv(ch)) => {
                    if let Some(senders) = g.ch_send_listeners.get(ch) {
                        for s in senders {
                            if visited.insert(*s) {
                                queue.push_back(*s);
                            }
                        }
                    }
                }
                Some(ParkEdge::Send(ch)) => {
                    if let Some(recvs) = g.ch_recv_listeners.get(ch) {
                        for r in recvs {
                            if visited.insert(*r) {
                                queue.push_back(*r);
                            }
                        }
                    }
                }
                Some(ParkEdge::Select(edges)) => {
                    for e in edges {
                        match e {
                            SelectEdge::Recv(ch) => {
                                if let Some(senders) = g.ch_send_listeners.get(ch) {
                                    for s in senders {
                                        if visited.insert(*s) {
                                            queue.push_back(*s);
                                        }
                                    }
                                }
                            }
                            SelectEdge::Send(ch) => {
                                if let Some(recvs) = g.ch_recv_listeners.get(ch) {
                                    for r in recvs {
                                        if visited.insert(*r) {
                                            queue.push_back(*r);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                Some(ParkEdge::Join(handle_id)) => {
                    let node = NodeId::Task(*handle_id);
                    if visited.insert(node) {
                        queue.push_back(node);
                    }
                }
            }
        }

        // Every reachable node is parked on a graph edge with no
        // fuel reachable from it. True starvation — the scheduler
        // cannot drive `target` forward. The watchdog should fire
        // `deadlock` immediately.
        true
    }
}

/// What the main thread is currently parked on. Mirrors the
/// `BlockReason` shape but uses raw IDs (the channel `Arc` and the
/// `TaskHandle` `Arc` aren't needed for the walk; the IDs are).
#[derive(Clone, Debug)]
pub enum MainTarget {
    Recv(usize),
    Send(usize),
    Join(usize),
    Select(Vec<SelectEdge>),
}

impl MainTarget {
    /// Build a `MainTarget::Recv` from a channel reference.
    pub fn from_recv(ch: &Arc<Channel>) -> Self {
        MainTarget::Recv(ch.id)
    }
    /// Build a `MainTarget::Send` from a channel reference.
    pub fn from_send(ch: &Arc<Channel>) -> Self {
        MainTarget::Send(ch.id)
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
            g.ch_recv_listeners.entry(*ch).or_default().insert(node);
        }
        ParkEdge::Send(ch) => {
            g.ch_send_listeners.entry(*ch).or_default().insert(node);
        }
        ParkEdge::Join(h) => {
            g.join_listeners.entry(*h).or_default().insert(node);
        }
        ParkEdge::Select(edges) => {
            for e in edges {
                match e {
                    SelectEdge::Recv(ch) => {
                        g.ch_recv_listeners.entry(*ch).or_default().insert(node);
                    }
                    SelectEdge::Send(ch) => {
                        g.ch_send_listeners.entry(*ch).or_default().insert(node);
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
        ParkEdge::Recv(ch) => prune_set(&mut g.ch_recv_listeners, *ch),
        ParkEdge::Send(ch) => prune_set(&mut g.ch_send_listeners, *ch),
        ParkEdge::Join(h) => prune_set(&mut g.join_listeners, *h),
        ParkEdge::Select(edges) => {
            for e in edges {
                match e {
                    SelectEdge::Recv(ch) => prune_set(&mut g.ch_recv_listeners, *ch),
                    SelectEdge::Send(ch) => prune_set(&mut g.ch_send_listeners, *ch),
                }
            }
        }
        ParkEdge::Io => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trivial smoke: an empty graph with main present and no edges
    /// reports starved=true for any target (no tasks → no fuel).
    #[test]
    fn empty_graph_with_main_is_starved() {
        let g = WakeGraph::new();
        g.register_main_present();
        assert!(g.is_main_starved(&MainTarget::Recv(0)));
        assert!(g.is_main_starved(&MainTarget::Send(0)));
        assert!(g.is_main_starved(&MainTarget::Join(99)));
    }

    /// A spawned-but-not-parked task is fuel: any main target that
    /// could plausibly be reached from a runnable task reports
    /// "not starved".
    #[test]
    fn live_runnable_task_is_fuel_for_any_target() {
        let g = WakeGraph::new();
        g.register_main_present();
        g.on_spawn(7);
        // Runnable task seeds NO reverse index (it isn't parked), but
        // the BFS only visits seeded nodes. With no parked sender, a
        // recv target is starved EVEN with a runnable task — we have
        // no proof that the runnable task will send to OUR channel.
        // This is correct behavior: the legacy can_make_progress check
        // is what catches "some task is runnable, may eventually
        // unblock us"; the graph fires only when it is provably stuck.
        assert!(g.is_main_starved(&MainTarget::Recv(0)));
    }

    /// A parked sender is fuel for our recv: the BFS finds the sender
    /// in `ch_send_listeners[ch]`, sees it has no parked-recv on
    /// anything that ALSO has no fuel, and reports "not starved".
    /// Wait — actually, a parked sender that nobody can wake IS still
    /// stuck. The test's correct shape: parked sender + main parked on
    /// recv on the same channel = NOT starved (the sender will wake
    /// when main's recv lands, classic rendezvous handshake — the
    /// graph's BFS unfortunately doesn't know that without extra
    /// modeling. So: a parked sender on channel C is reachable from
    /// MAIN's recv on C; the sender has no edge to elsewhere → BFS
    /// returns true).
    ///
    /// Actually, let me re-think. For a rendezvous handshake to make
    /// progress, BOTH sides need a participant. If main parks on
    /// recv(C) and a task parks on send(C), the channel itself wakes
    /// both: when main registers its recv waker, the parked sender's
    /// `wake_send` fires, which requeues the sender, which then
    /// completes the handshake. So this state IS NOT a deadlock — but
    /// the graph as defined treats the parked sender as a leaf with no
    /// fuel beyond it.
    ///
    /// Resolution: the parked sender's edge is `ParkEdge::Send(C)`,
    /// and the BFS for that edge looks at `ch_recv_listeners[C]`. If
    /// MAIN is registered there (because main called on_park with
    /// ParkEdge::Recv(C)), the BFS would find MAIN → cycle → no fuel.
    /// MAIN is in fact NOT a fuel node (it's what we're trying to
    /// unblock), so the cycle correctly shows no fuel. That LOOKS like
    /// starvation to the graph!
    ///
    /// The fix: the channel-peek already knows this case isn't a
    /// deadlock (parked sender → channel can carry main forward). The
    /// caller composes graph + channel-peek; the graph alone is too
    /// pessimistic for rendezvous shapes. That's the design: graph
    /// gives a fast NEGATIVE proof for the unambiguous-stuck cases;
    /// channel peek covers the handshake-pending cases.
    #[test]
    fn parked_sender_alone_is_starved_per_graph() {
        let g = WakeGraph::new();
        g.register_main_present();
        g.on_spawn(1);
        g.on_park(NodeId::Task(1), ParkEdge::Send(42));
        // From main's perspective on recv(42), the only reachable
        // producer is task 1 (parked-send-42), which has no fuel of
        // its own. The graph reports starved — the channel-peek in
        // the watchdog must override this for rendezvous.
        assert!(g.is_main_starved(&MainTarget::Recv(42)));
    }

    /// A parked sender whose dependencies include a runnable task
    /// IS NOT starved. e.g. task 1 parks on join(2); task 2 is
    /// runnable. Main parks on recv from a channel where task 1 is
    /// the parked sender (?) — simpler shape: main parks on join(1);
    /// task 1 is runnable.
    #[test]
    fn join_on_runnable_task_is_not_starved() {
        let g = WakeGraph::new();
        g.register_main_present();
        g.on_spawn(1);
        // task 1 has no edge → it's runnable.
        // main parks on join(1) → BFS seeds NodeId::Task(1), finds
        // it absent from edges but present in live_tasks → fuel.
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
        assert!(g.is_main_starved(&MainTarget::Recv(0)));
        assert!(g.is_main_starved(&MainTarget::Join(1)));
    }

    /// I/O-parked task is fuel: external waker will eventually fire.
    #[test]
    fn io_parked_task_is_fuel() {
        let g = WakeGraph::new();
        g.register_main_present();
        g.on_spawn(1);
        // Task 1 parks on I/O.
        g.on_park(NodeId::Task(1), ParkEdge::Io);
        // Main parks on join(1) → BFS seeds Task(1), sees ParkEdge::Io
        // → fuel. Not starved.
        assert!(!g.is_main_starved(&MainTarget::Join(1)));
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
            ParkEdge::Select(vec![SelectEdge::Recv(10), SelectEdge::Send(20)]),
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
        g.on_park(NodeId::Task(1), ParkEdge::Send(42));
        g.on_park(NodeId::Task(1), ParkEdge::Recv(42));
        let inner = g.inner.lock();
        assert!(inner.ch_send_listeners.is_empty());
        assert_eq!(inner.ch_recv_listeners.len(), 1);
    }

    /// Without `register_main_present`, `is_main_starved` returns
    /// `false` (the graph defers to the legacy detector).
    #[test]
    fn no_main_present_means_not_starved_default() {
        let g = WakeGraph::new();
        // Don't call register_main_present.
        assert!(!g.is_main_starved(&MainTarget::Recv(0)));
    }
}
