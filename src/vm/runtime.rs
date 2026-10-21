//! VM runtime types: call frames, blocking reasons, timer manager,
//! shared runtime state, and regex cache. The I/O pool is in
//! `io_op`.

use regex::Regex;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use crate::bytecode::{Function, VmClosure};
use crate::runtime::sync::{Fired, Wait};
use crate::scheduler::Scheduler;
use crate::value::Value;

use super::io_op::IoPool;
use super::{HostIo, Vm, VmError};

// ── Call frame ────────────────────────────────────────────────────

pub(crate) struct CallFrame {
    pub(crate) closure: Arc<VmClosure>,
    pub(crate) ip: usize,
    pub(crate) base_slot: usize,
}

/// How many of the tail calls of one frame the log keeps: the latest
/// ones ([`TailCalls`]).
pub(crate) const TCO_ELIDED_CAP: usize = 32;

/// The callers that tail calls replaced: a tail call runs its callee
/// in the caller's frame, so the frames no longer say who called. For
/// each frame that has made tail calls, the log has the function the
/// frame was running and where it had come to, for the latest
/// [`TCO_ELIDED_CAP`] of them. `Vm::enrich_error` reads it, so that an
/// error's call stack names the callers a chain of tail calls went
/// through; a caller's name and place are looked up there, not here.
///
/// A frame's entries go when the frame does ([`TailCalls::prune`]): the
/// log is in the order of the frames, and the frame on top is the one
/// that logs. An entry costs no allocation, and once a frame has made as
/// many tail calls as the log keeps, an entry that takes the place of
/// one of the same function does not change the function's count
/// ([`TailCalls::log`]).
///
/// Lock: `callback_frame_capture__tail_call_chain_keeps_callers` and
/// `callback_frame_capture__tail_call_ring_buffer_caps_chain`
/// (tests/golden/lang/errors), `vm::tests::error_places::in_a_tail_call`.
#[derive(Default)]
pub(crate) struct TailCalls {
    /// For each frame that has made tail calls, the innermost last: its
    /// callers, then its record. (One list: a VM has a log whether its
    /// code makes tail calls or not, and a program can have a hundred
    /// thousand VMs.)
    entries: Vec<TailCall>,
}

enum TailCall {
    /// The function a frame was running when it made a tail call, and
    /// its instruction pointer then, past the call.
    Caller(Arc<Function>, usize),
    /// A frame that has made tail calls: its index in the VM's frames,
    /// and how many it has made. Its callers are the entries before
    /// this one, as many as it has made and at most
    /// [`TCO_ELIDED_CAP`]: in the order they were logged until there
    /// are that many; from then on each takes the place of the oldest.
    Frame { depth: usize, made: usize },
}

impl TailCalls {
    /// Whether no frame has made a tail call.
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The frame at `depth`, the one on top, makes a tail call: it was
    /// running `function` and had come to `ip`.
    ///
    /// The function is cloned only for a new entry, or for one that
    /// takes the place of another function's. When the oldest entry,
    /// the one the new one takes the place of, is of the same function,
    /// as in a function that calls itself or a cycle of functions whose
    /// length divides [`TCO_ELIDED_CAP`], only its `ip` is written: the
    /// function's count, which every task running it shares, is not
    /// touched.
    pub(crate) fn log(&mut self, depth: usize, function: &Arc<Function>, ip: usize) {
        // The frame's record is off the end while its callers change.
        let made = match self.entries.last() {
            Some(&TailCall::Frame { depth: top, made }) if top == depth => {
                self.entries.pop();
                made
            }
            _ => 0,
        };
        if made < TCO_ELIDED_CAP {
            self.entries
                .push(TailCall::Caller(Arc::clone(function), ip));
        } else {
            let start = self.entries.len() - TCO_ELIDED_CAP;
            match &mut self.entries[start + made % TCO_ELIDED_CAP] {
                TailCall::Caller(held, held_ip) if Arc::ptr_eq(held, function) => *held_ip = ip,
                oldest => *oldest = TailCall::Caller(Arc::clone(function), ip),
            }
        }
        self.entries.push(TailCall::Frame {
            depth,
            made: made + 1,
        });
    }

    /// Drop the entries of the frames at `keep_depth` and above: those
    /// frames are gone.
    pub(crate) fn prune(&mut self, keep_depth: usize) {
        while let Some(&TailCall::Frame { depth, made }) = self.entries.last()
            && depth >= keep_depth
        {
            let kept = made.min(TCO_ELIDED_CAP);
            self.entries.truncate(self.entries.len() - 1 - kept);
        }
    }

    /// The callers, each with its frame's depth: the innermost frame's
    /// first, and a frame's latest tail call first.
    pub(crate) fn newest_first(&self) -> impl Iterator<Item = (usize, &Function, usize)> + '_ {
        let mut end = self.entries.len();
        std::iter::from_fn(move || {
            // The record of the next frame down, after its callers.
            let record = end.checked_sub(1)?;
            let TailCall::Frame { depth, made } = self.entries[record] else {
                unreachable!("a frame's record is after its callers")
            };
            let kept = made.min(TCO_ELIDED_CAP);
            let callers = &self.entries[record - kept..record];
            end = record - kept;
            // The latest entry is before the place of the next one.
            let next = made % TCO_ELIDED_CAP;
            Some((0..kept).map(
                move |back| match &callers[(next + kept - 1 - back) % kept] {
                    TailCall::Caller(function, ip) => (depth, &**function, *ip),
                    TailCall::Frame { .. } => {
                        unreachable!("a frame's callers are before its record")
                    }
                },
            ))
        })
        .flatten()
    }
}

// ── Frames ───────────────────────────────────────────────────────

/// One frame of a VM: a silt function being run, or a builtin that
/// calls back into silt code or waits.
pub(crate) enum Frame {
    Code(CallFrame),
    Native(Box<dyn Native>),
}

/// A builtin that does not finish in one go: it calls silt functions
/// (`list.map`), or waits (`channel.each`). It is a state machine and a
/// frame of the VM, so a call it makes is a frame above it in the one
/// instruction loop, never a loop of its own on the host stack.
pub(crate) trait Native: Send {
    /// The builtin's name, `list.map`.
    fn name(&self) -> &str;

    /// Go on. `input` is unit the first time and after the frame
    /// parked; after a [`Step::Call`], the value the call returned.
    fn resume(&mut self, vm: &mut Vm, input: Value) -> Result<Step, VmError>;

    /// The frame is dropped because an error passes through it: undo
    /// what the builtin did to the VM for the time of its call.
    fn abandon(&mut self, _vm: &mut Vm) {}
}

/// What a builtin, or a [`Native`] frame, does next.
pub(crate) enum Step {
    /// It is finished, with this value.
    Done(Value),
    /// Call `callee` with the top `argc` values of the stack, which
    /// are above a slot of the call's own; the value it returns is the
    /// frame's next input. Made by [`Vm::call`].
    Call { callee: Value, argc: usize },
    /// Go on as this frame, whose value is the builtin's.
    Run(Box<dyn Native>),
    /// The task waits for this. When the wait has ended the frame is
    /// resumed, with unit, and finds how it ended in [`Vm::woken`].
    Park(Wait),
    /// The task's slice ends here, to give way to the other tasks; the
    /// frame is resumed, with unit, when the task runs again.
    Yield,
}

/// A frame that waits once: [`Vm::park`].
struct Await<F> {
    name: &'static str,
    /// The wait, until the frame has parked.
    wait: Option<Wait>,
    then: F,
}

impl<F> Native for Await<F>
where
    F: FnMut(&mut Vm, Fired) -> Result<Step, VmError> + Send,
{
    fn name(&self) -> &str {
        self.name
    }

    fn resume(&mut self, vm: &mut Vm, _input: Value) -> Result<Step, VmError> {
        if let Some(wait) = self.wait.take() {
            return Ok(Step::Park(wait));
        }
        let fired = vm.woken()?;
        let step = (self.then)(vm, fired)?;
        // A clock that has panicked ended every wait that was pending
        // on it: the waiter fails as a builtin that reads the clock
        // does, unless it has an error of its own.
        match vm.runtime.io.clock_failure() {
            Some(failure) => Err(VmError::new(failure)),
            None => Ok(step),
        }
    }
}

impl Vm {
    /// The step of a builtin `name` that waits for `wait` and goes on
    /// with `then`, which gets how the wait ended.
    pub(crate) fn park(
        &mut self,
        name: &'static str,
        wait: Wait,
        then: impl FnMut(&mut Vm, Fired) -> Result<Step, VmError> + Send + 'static,
    ) -> Step {
        Step::Run(Box::new(Await {
            name,
            wait: Some(wait),
            then,
        }))
    }

    /// How the wait of the frame that is resumed after a
    /// [`Step::Park`] ended.
    pub(crate) fn woken(&mut self) -> Result<Fired, VmError> {
        self.woken.take().ok_or_else(|| {
            VmError::new("internal VM error: a frame was resumed with its wait open".into())
        })
    }

    /// Whether the task this is the VM of takes nothing new: it was
    /// cancelled, or its time is over (it is the function of a
    /// `task.deadline` that has passed, and a slice of it has begun
    /// since: `TaskHandle::set_over`). The steps such a task still
    /// has are for what it
    /// holds, which it may give (a send that is ready), not for new
    /// work. So every operation that would take something from outside
    /// the task asks this before it looks, and ends the task where it
    /// holds ([`Vm::end_here`], [`Vm::no_wait`]): a receive in each of
    /// its forms, the take of `channel.each` and of a stream's stage or
    /// sink, and the start of an I/O operation ([`Vm::io_op`], which
    /// hands the same flags to the operation: `Wanted`). So
    /// does what would start a task whose one purpose is to take in
    /// its place: a stream's stage, and `task.deadline`, which waits
    /// for the function it starts. The arms of a wait ask the same of
    /// the wait's flag (`runtime::sync::park`).
    ///
    /// It is the one condition of that rule, for a cancel and for a
    /// deadline alike. Neither is ever taken back, so an operation
    /// that found it true is refused the wait it then begins: as
    /// cancelled, or as ended by the task's deadline.
    ///
    /// It reads two flags and no clock: a loop of takes under a
    /// deadline was 15% slower with a reading of the clock at each. So
    /// the time is over, for this, from the first slice that begins
    /// past it, which is the moment the count of a late task's steps
    /// begins too. In the slice in which the time passes a take of what
    /// is there may still complete, and is the task's like what a wait
    /// that ended just before had given it; a take that has to wait is
    /// refused by the wait, which reads the clock.
    pub(crate) fn takes_nothing_new(&self) -> bool {
        let set = |flag: &Arc<std::sync::atomic::AtomicBool>| {
            flag.load(std::sync::atomic::Ordering::SeqCst)
        };
        self.cancelled.as_ref().is_some_and(set) || self.over.as_ref().is_some_and(set)
    }

    /// The wait that ends a task which takes nothing new
    /// ([`Vm::takes_nothing_new`]) where it is: a wait with no arm,
    /// which such a task is refused, as it is refused every wait that
    /// cannot end at once.
    pub(crate) fn no_wait() -> Wait {
        Wait::new(Vec::new())
    }

    /// The step of a builtin `name` that ends the task here
    /// ([`Vm::no_wait`]), for a builtin that has no frame of its own.
    pub(crate) fn end_here(&mut self, name: &'static str) -> Step {
        self.park(name, Vm::no_wait(), |_, _| {
            Err(VmError::new(
                "internal VM error: a task went on that was to take nothing new".into(),
            ))
        })
    }
}

// ── Runtime (shared state) ───────────────────────────────────────

/// What the VM that runs the program's own code has of the program,
/// for the VMs of its tasks to take. A REPL session's VM takes in a
/// program for each entry, each with the slots and tables of the
/// entries before it and its own after them; a task started in an
/// earlier entry finds here what the later ones added.
#[derive(Default)]
pub(crate) struct Newest {
    /// How often `view` was written. A VM that has the view of this
    /// count has the newest: it need not take the lock to know.
    pub(crate) count: std::sync::atomic::AtomicU64,
    pub(crate) view: parking_lot::Mutex<View>,
}

/// What a VM has of the program: the values of its global slots, and
/// what each slot is.
#[derive(Clone)]
pub(crate) struct View {
    /// The values, each unset until its definition's code has run. There
    /// may be more slots than the program has definitions: room for
    /// those of the next REPL entries.
    pub(crate) slots: Arc<[std::sync::OnceLock<Value>]>,
    /// What each global slot is, and the slot of each impl method: what
    /// the VM looks a method up in. For the VM of a task this is what
    /// STANDS of the program ([`crate::session::Program::settled`]):
    /// nothing in it is ever taken back or changed, so what the VM
    /// finds in it needs no second look.
    pub(crate) table: Arc<crate::bytecode::Globals>,
    /// All of the program that runs now: `table`, and what the REPL
    /// entry being run adds to it, which is gone again if the entry
    /// fails. The VM of a task looks here where `table` has nothing.
    /// (The VM that runs the entry itself has this as its `table`.)
    pub(crate) entry: Arc<crate::bytecode::Globals>,
}

impl Default for View {
    fn default() -> Self {
        // (One table for both: a VM whose `table` is its `entry` has
        // all of its program, see `Vm::wider_table`.)
        let table: Arc<crate::bytecode::Globals> = Arc::default();
        View {
            slots: Arc::from([]),
            entry: table.clone(),
            table,
        }
    }
}

impl View {
    /// The value of the global slot `slot`, if it is set.
    pub(crate) fn global(&self, slot: u16) -> Option<&Value> {
        self.slots.get(usize::from(slot))?.get()
    }

    /// Make this view the program's newest if it is not (`count` says
    /// which it is). See [`Vm::look_again`], which is this for a VM.
    pub(crate) fn renew(&mut self, count: &mut u64, newest: &Newest) -> bool {
        use std::sync::atomic::Ordering;
        if newest.count.load(Ordering::Relaxed) == *count {
            return false;
        }
        let view = newest.view.lock();
        *self = view.clone();
        *count = newest.count.load(Ordering::Relaxed);
        true
    }

    /// A read of the global slot `slot` found no value: its value in
    /// the program's newest slots, or the error.
    ///
    /// The VM of a task comes here for a slot of a REPL entry later
    /// than the one it was started in (it runs a closure of that
    /// entry). Nothing else does: the checker orders the top-level
    /// `let`s so that each is set before anything can read it, and the
    /// definitions are set before any `let` runs. That holds for a task
    /// as for the program's own code, since a task has the code of a
    /// later entry only from that entry's code, which ran after what it
    /// reads was set. A slot that is not set in the newest slots either
    /// is a bug of silt, and said so.
    #[cold]
    #[inline(never)]
    pub(crate) fn global_of_newest(
        &mut self,
        count: &mut u64,
        newest: &Newest,
        slot: u16,
    ) -> Result<Value, VmError> {
        self.renew(count, newest);
        self.global(slot).cloned().ok_or_else(|| {
            VmError::type_confusion(format!(
                "the global '{}' is read before it is set",
                self.entry.name(slot)
            ))
        })
    }
}

/// The VM's own copies of the closures it has read from the global
/// slots (top-level functions, impl methods), by slot.
///
/// A closure in a global slot is one allocation for every VM of the
/// program, and each call of it counts a reference to it up and down:
/// tasks on several cores that call one function would each take the
/// cache line of that count from the others, every call. So a VM calls
/// a copy of its own, made the first time it reads the slot: the same
/// `Function`, the same captures, and so the same value
/// (src/value/key.rs).
///
/// With each copy is kept the closure it was copied from, and a copy is
/// used only for that one: a slot that is set again (an embedder runs
/// another program on the VM, see [`Vm::set_global`]) holds another
/// closure, which is copied in turn. (The closure kept also keeps its
/// address from being that of another.)
///
/// The copies are in a table of their own size, not one of the
/// program's slots: a task that calls one function of a program of
/// thousands holds one copy. The copy of the slot `s` is at the place
/// `s` modulo the number of places, or at the first place after it
/// that is free or has it; at most half of the places are taken.
#[derive(Default)]
pub(crate) struct OwnClosures {
    /// A power of two of places, or none.
    places: Vec<Option<Copied>>,
    /// How many places are taken.
    taken: usize,
}

/// The copy of the closure of a global slot.
struct Copied {
    slot: u16,
    /// The closure in the slot when it was copied.
    shared: Arc<VmClosure>,
    own: Arc<VmClosure>,
}

impl OwnClosures {
    /// This VM's copy of `shared`, the closure in the global slot
    /// `slot`.
    #[inline(always)]
    pub(crate) fn of(&mut self, slot: u16, shared: &Arc<VmClosure>) -> Arc<VmClosure> {
        // (With no places, the mask is all ones and the first look
        // finds none.)
        let mask = self.places.len().wrapping_sub(1);
        let mut at = usize::from(slot) & mask;
        while let Some(Some(copied)) = self.places.get(at) {
            if copied.slot == slot {
                if Arc::ptr_eq(&copied.shared, shared) {
                    return copied.own.clone();
                }
                break;
            }
            at = (at + 1) & mask;
        }
        self.copy(slot, shared)
    }

    /// The value `value` of the global slot `slot` as this VM uses it:
    /// its copy if it is a closure.
    pub(crate) fn value(&mut self, slot: u16, value: Value) -> Value {
        match value {
            Value::VmClosure(shared) => Value::VmClosure(self.of(slot, &shared)),
            value => value,
        }
    }

    /// Copy `shared`, the closure in the global slot `slot`, for this
    /// VM, in place of any copy the slot had.
    #[cold]
    #[inline(never)]
    fn copy(&mut self, slot: u16, shared: &Arc<VmClosure>) -> Arc<VmClosure> {
        let own = Arc::new(VmClosure {
            function: shared.function.clone(),
            upvalues: shared.upvalues.clone(),
        });
        self.forget(slot);
        if 2 * (self.taken + 1) > self.places.len() {
            let places = (2 * self.places.len()).max(8);
            let copies = std::mem::replace(&mut self.places, (0..places).map(|_| None).collect());
            copies
                .into_iter()
                .flatten()
                .for_each(|copied| self.put(copied));
        }
        self.put(Copied {
            slot,
            shared: shared.clone(),
            own: own.clone(),
        });
        self.taken += 1;
        own
    }

    /// Put `copied` at its place, in a table with a free place and no
    /// copy of its slot.
    fn put(&mut self, copied: Copied) {
        let mask = self.places.len() - 1;
        let mut at = usize::from(copied.slot) & mask;
        while self.places[at].is_some() {
            at = (at + 1) & mask;
        }
        self.places[at] = Some(copied);
    }

    /// How many places the table has.
    #[cfg(test)]
    pub(crate) fn places(&self) -> usize {
        self.places.len()
    }

    /// Let go of the copy of the closure that was in the global slot
    /// `slot`, if there is one: the slot is set again. (Seldom: the
    /// copies are put in their places again without it.)
    pub(crate) fn forget(&mut self, slot: u16) {
        let had = |copied: &Option<Copied>| matches!(copied, Some(copied) if copied.slot == slot);
        if !self.places.iter().any(had) {
            return;
        }
        let places = self.places.len();
        let copies = std::mem::replace(&mut self.places, (0..places).map(|_| None).collect());
        for copied in copies.into_iter().flatten() {
            match copied.slot == slot {
                true => self.taken -= 1,
                false => self.put(copied),
            }
        }
    }
}

/// Shared, read-only-after-init state for a Silt program.
/// Created once during initialization, then shared across spawned tasks via `Arc`.
pub struct Runtime {
    // ── M:N scheduler ──────────────────────────────────────────
    /// The scheduler of the program's tasks.
    pub(crate) scheduler: Arc<Scheduler>,

    // ── I/O pool ────────────────────────────────────────────────
    /// Thread pool for async I/O operations.
    pub(crate) io_pool: IoPool,

    // ── Host ────────────────────────────────────────────────────
    /// Where the program's output goes and which clock it reads.
    pub(crate) io: HostIo,
    /// The slice an embedder set ([`Vm::set_time_slice`]); 0 while it
    /// has set none.
    pub(crate) time_slice: std::sync::atomic::AtomicUsize,
    /// The steps the program may still run ([`Vm::set_step_budget`]);
    /// `u64::MAX` while it has no budget.
    pub(crate) steps_left: std::sync::atomic::AtomicU64,

    // ── The program ─────────────────────────────────────────────
    /// The global slots and the tables of the newest program the VM
    /// that owns this runtime took in, for the VMs of its tasks
    /// ([`Vm::look_again`]).
    pub(crate) newest: Newest,
    /// The program's types, by id: the decoders find the record types
    /// of record fields here ([`Vm::type_info`]). One table for the VM
    /// and the VMs of its tasks; a REPL entry adds its types to it.
    pub(crate) types: parking_lot::Mutex<crate::typeinfo::TypeTable>,
    /// Whether the decoders of a family have a value of a type of the
    /// program, by the type's id and the family: the message of their
    /// refusal, or `None` (`crate::builtins::decode::refusal`). Decided
    /// once per type and family, and forgotten when a program is taken in
    /// (`Vm::load`: a REPL entry's definitions are new ones, with ids of
    /// their own, but a description taken in anew replaces the old).
    /// By the type's number, then the family's (`Family as usize`): no
    /// key is hashed on a call.
    pub(crate) refusals: parking_lot::RwLock<
        Vec<[Option<Option<String>>; crate::builtins::registry::decoding::Family::ALL.len()]>,
    >,

    // ── Per-VM generators ───────────────────────────────────────
    /// The state of `math.random`; `None` until the first call seeds it
    /// from the host clock.
    pub(crate) rng: parking_lot::Mutex<Option<u64>>,
    /// The counter that keeps the `uuid.v7`s minted within one
    /// millisecond of the host clock in order.
    pub(crate) uuid_v7: std::sync::Mutex<uuid::ContextV7>,
}

impl Runtime {
    /// The description of the program's type `id`, or of the builtin
    /// type. (The table is the program's, not a VM's: the VM of a task
    /// finds a type of a REPL entry later than its own. Its lock is
    /// held for the lookup, not while anything is built of what is
    /// found.)
    pub(crate) fn type_info(
        &self,
        id: crate::defs::TypeId,
    ) -> Option<Arc<crate::typeinfo::TypeInfo>> {
        self.types.lock().get(id).cloned()
    }

    /// End the threads that serve the program: the scheduler's workers
    /// and timer thread and the I/O workers. Called when
    /// the VM that was made for the program is dropped. Tasks that have
    /// not ended never run again, and pending timers never fire.
    ///
    /// The threads are told to end, not waited for: a worker inside a
    /// builtin that blocks ends when the builtin returns.
    pub(super) fn shutdown(&self) {
        self.scheduler.shutdown();
        self.io_pool.stop();
    }

    /// Take `used` steps, those of a slice that ran its full length,
    /// off the step budget. The error of a program whose budget is
    /// used up, from then on at every call.
    pub(crate) fn charge(&self, used: usize) -> Result<(), VmError> {
        use std::sync::atomic::Ordering::Relaxed;
        if self.steps_left.load(Relaxed) == u64::MAX {
            return Ok(());
        }
        let used = used as u64;
        let mut left = self.steps_left.load(Relaxed);
        // (A loop of its own: `fetch_update` is deprecated on newer
        // toolchains, and its new name is not on older ones.)
        while let Err(now) =
            self.steps_left
                .compare_exchange_weak(left, left.saturating_sub(used), Relaxed, Relaxed)
        {
            left = now;
        }
        match left > used {
            true => Ok(()),
            false => Err(VmError::budget_used_up()),
        }
    }

    /// The next value of `math.random`, in `[0, 1)`.
    pub(crate) fn random(&self) -> f64 {
        let mut state = self.rng.lock();
        let mut s = state.unwrap_or_else(|| {
            // splitmix64 of the host clock's time: clocks that read
            // close to each other, or close to zero, still start far
            // apart. xorshift64 must not be seeded with 0.
            let mut z = (self.io.now().as_nanos() as u64).wrapping_add(0x9E37_79B9_7F4A_7C15);
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            (z ^ (z >> 31)).max(1)
        });
        // xorshift64
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        *state = Some(s);
        // Convert to [0.0, 1.0)
        (s >> 11) as f64 / ((1u64 << 53) as f64)
    }
}

// ── Regex cache ──────────────────────────────────────────────────

/// Bounded cache for compiled regex patterns.
///
/// Tracks insertion order with a `VecDeque`. When the cache exceeds
/// `MAX_ENTRIES`, the oldest 25% of entries are evicted instead of
/// clearing the entire cache.
pub(crate) struct RegexCache {
    map: HashMap<String, Regex>,
    order: VecDeque<String>,
}

impl RegexCache {
    const MAX_ENTRIES: usize = 256;
    const EVICT_COUNT: usize = 64; // 25% of MAX_ENTRIES

    pub(super) fn new() -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    /// Return a reference to the cached `Regex` for `pattern`, compiling and
    /// caching it if necessary; the error of a pattern that does not
    /// compile.
    pub(super) fn get(&mut self, pattern: &str) -> Result<&Regex, regex::Error> {
        if !self.map.contains_key(pattern) {
            let re = Regex::new(pattern)?;
            if self.map.len() >= Self::MAX_ENTRIES {
                // Evict the oldest 25% of entries.
                for _ in 0..Self::EVICT_COUNT {
                    if let Some(old_key) = self.order.pop_front() {
                        self.map.remove(&old_key);
                    }
                }
            }
            self.order.push_back(pattern.to_string());
            self.map.insert(pattern.to_string(), re);
        }
        Ok(self.map.get(pattern).expect("pattern was just inserted"))
    }
}
