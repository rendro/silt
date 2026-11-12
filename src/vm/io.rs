//! What a running program reaches of its host: where its output goes
//! and which clock it reads.
//!
//! A [`HostIo`] is given to [`Vm::new`](super::Vm::new) and holds for
//! everything that VM runs, in the program's thread and in its tasks:
//!
//! - `print` and `println` write to its stdout, `io.eprint` and
//!   `io.eprintln` to its stderr;
//! - what the runtime prints for the program (the report of a task
//!   that failed and was never joined, the log of an `http.serve`
//!   handler that failed) goes to its stderr;
//! - `time.now`, `time.today`, `time.sleep`, the timeouts of
//!   `channel.timeout` and `channel.receive_timeout`, and the deadline of
//!   `task.deadline` read its clock.
//!
//! ```rust
//! use silt::{Buffer, HostIo, Vm};
//!
//! // An embedder collects the output in memory.
//! let out = Buffer::new();
//! let vm = Vm::new(HostIo::buffer(&out));
//! # drop(vm);
//! assert_eq!(out.contents(), "");
//! ```

use std::io;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::runtime::sync::Cell;

/// Where text written by a program goes. Called from the output's own
/// writer thread ([`Lane`]), one call for each `print`, `println`,
/// `io.eprint`, `io.eprintln` or report, or for several of them in a
/// row, joined: a call never has a part of one of them.
pub trait Output: Send + Sync {
    /// Take `text`. An error returned for a program's `print` or
    /// `println` becomes a runtime error of the program; one returned
    /// for a write to stderr is dropped. A panic counts as an error.
    fn write(&self, text: &str) -> io::Result<()>;
}

/// An in-memory [`Output`]. Its clones share one text, so an embedder
/// keeps one clone and gives the other to the [`HostIo`].
#[derive(Clone, Default)]
pub struct Buffer {
    text: Arc<parking_lot::Mutex<String>>,
}

impl Buffer {
    /// An empty buffer.
    pub fn new() -> Buffer {
        Buffer::default()
    }

    /// What has been written so far.
    pub fn contents(&self) -> String {
        self.text.lock().clone()
    }

    /// What has been written so far, which the buffer then forgets.
    pub fn take(&self) -> String {
        std::mem::take(&mut *self.text.lock())
    }
}

impl Output for Buffer {
    fn write(&self, text: &str) -> io::Result<()> {
        self.text.lock().push_str(text);
        Ok(())
    }
}

/// The stdout of the process. A closed pipe (`silt run x.silt | head
/// -1`) ends the process quietly, with the status a death by SIGPIPE
/// gives (141), whatever the signal's disposition or mask: `println!`
/// would panic there.
struct ProcessStdout;

impl Output for ProcessStdout {
    fn write(&self, text: &str) -> io::Result<()> {
        use std::io::Write;
        match io::stdout().lock().write_all(text.as_bytes()) {
            Err(e) if e.kind() == io::ErrorKind::BrokenPipe => {
                crate::scheduler::exit_with_status(141)
            }
            result => result,
        }
    }
}

/// The stderr of the process. A pipe whose reader has gone is an error
/// of the write, which is dropped: unlike stdout, a closed stderr ends
/// nothing.
struct ProcessStderr;

impl Output for ProcessStderr {
    fn write(&self, text: &str) -> io::Result<()> {
        write_stderr(text)
    }
}

/// Write `text` to the process's stderr, a pipe whose reader has gone
/// being an error of the write ([`without_sigpipe`]): what the program
/// writes there, and what the runtime reports for it, also the report
/// of a failed program under `silt run`
/// ([`crate::scheduler::report_on_stderr`]).
pub(crate) fn write_stderr(text: &str) -> io::Result<()> {
    use std::io::Write;
    without_sigpipe(|| io::stderr().lock().write_all(text.as_bytes()))
}

/// `write`, with SIGPIPE ignored: a write to a pipe whose reader has
/// gone fails with `BrokenPipe` instead of killing the process (the
/// `silt` command sets the signal to its default).
///
/// The disposition is the process's, not the thread's: on macOS the
/// signal of a write to a pipe goes to the process, and any thread that
/// does not block it takes it, so a mask of the writing thread alone
/// would not do. It is ignored for the time of the write and then put
/// back as it was, under the lock of the command's reports
/// (`scheduler::report`), so that no other report of the command's is
/// written meanwhile: such a write keeps its SIGPIPE. A write of the
/// program to stdout in that time fails with `BrokenPipe` instead of
/// dying by the signal, and [`ProcessStdout`] ends the process with 141
/// for it all the same. (In a process that ignores SIGPIPE, as Rust's
/// programs do, nothing changes.) Where stderr is no pipe and no socket
/// (a file, a terminal) no write raises the signal, and none of this is
/// done.
#[cfg(unix)]
fn without_sigpipe(write: impl FnOnce() -> io::Result<()>) -> io::Result<()> {
    if !stderr_raises_sigpipe() {
        return write();
    }
    crate::scheduler::with_report_lock(|| {
        // SAFETY: the `sigaction`s are zeroed, which is a valid value of
        // the C struct, and the handler and mask of the one that is set
        // are set before it is passed; every pointer passed is valid or
        // null. The disposition put back is the one read here.
        let old = unsafe {
            let mut ignore: libc::sigaction = std::mem::zeroed();
            ignore.sa_sigaction = libc::SIG_IGN;
            libc::sigemptyset(&mut ignore.sa_mask);
            let mut old: libc::sigaction = std::mem::zeroed();
            libc::sigaction(libc::SIGPIPE, &ignore, &mut old);
            old
        };
        let result = write();
        // SAFETY: as above.
        unsafe {
            libc::sigaction(libc::SIGPIPE, &old, std::ptr::null_mut());
        }
        result
    })
}

/// Whether a write to the process's stderr can raise SIGPIPE: whether
/// it is a pipe or a socket. Asked once; a stderr that cannot be asked
/// counts as one.
#[cfg(unix)]
fn stderr_raises_sigpipe() -> bool {
    use std::os::fd::AsFd;
    use std::os::unix::fs::FileTypeExt;
    static RAISES: OnceLock<bool> = OnceLock::new();
    *RAISES.get_or_init(|| {
        // (A closed stderr cannot be copied: a write to it fails, and
        // raises nothing.)
        let Ok(copy) = io::stderr().as_fd().try_clone_to_owned() else {
            return false;
        };
        match std::fs::File::from(copy).metadata() {
            Ok(meta) => meta.file_type().is_fifo() || meta.file_type().is_socket(),
            Err(_) => true,
        }
    })
}

/// Elsewhere a write to a closed pipe is an error of the write.
#[cfg(not(unix))]
fn without_sigpipe(write: impl FnOnce() -> io::Result<()>) -> io::Result<()> {
    write()
}

/// The clock a program reads. Called from the thread that runs the
/// program and from the runtime's threads. If one of its methods
/// panics, the clock is not called again and the program ends with the
/// runtime error `the clock panicked: ...`.
pub trait Clock: Send + Sync {
    /// The time of day, as the time since the Unix epoch
    /// (1970-01-01T00:00:00Z). `time.now` and `time.today` give it.
    fn now(&self) -> Duration;

    /// The time since some fixed moment, which never goes back. Every
    /// wait of a program is measured on it: a deadline is the reading
    /// at which the wait ends.
    fn monotonic(&self) -> Duration;

    /// Block the calling thread until `duration` has passed on this
    /// clock. A `time.sleep` outside a task calls it.
    fn sleep(&self, duration: Duration);
}

/// The clock of the operating system.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Duration {
        // A system clock set before 1970 reads as 1970.
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
    }

    fn monotonic(&self) -> Duration {
        static START: OnceLock<Instant> = OnceLock::new();
        START.get_or_init(Instant::now).elapsed()
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// How many bytes of output may wait for an output's writer
/// ([`Lane`]). A print that finds them there waits for room, without a
/// thread: the program's other tasks run on. (A text larger than this
/// goes when nothing else waits.)
const QUEUED_MAX: usize = 64 * 1024;

/// How long a writer thread that has nothing to write waits for more
/// before it ends.
#[cfg(not(target_arch = "wasm32"))]
const WRITER_IDLE: Duration = Duration::from_secs(5);

/// What a print came to ([`HostIo::print`]).
pub(crate) enum Printed {
    /// The text is the writer's: it is written in its turn.
    Queued,
    /// The writer has as much as it may hold: the text waits, and goes
    /// to the writer when the cell is completed. The print waits for
    /// that (`Vm::park`): a wait that ends otherwise (a deadline, a
    /// cancel) leaves the text unwritten.
    Waits(Arc<Cell<()>>),
    /// An earlier write failed: the output takes nothing more.
    Failed(String),
}

/// Which output of a [`HostIo`].
#[derive(Clone, Copy)]
pub(crate) enum Stream {
    Stdout,
    Stderr,
}

/// One output and the thread that writes to it: the program's prints
/// are put in its queue, in the order of the calls, and the thread
/// writes them out, whole and in that order. A print does not wait for
/// the output: one that finds the queue full waits for room as any
/// wait of a task does, and the other tasks run. So a reader that is
/// slow, or gone quiet, holds up the tasks that print, and nothing
/// else.
///
/// The thread is started at the first text and ends after it has had
/// nothing to write for a while. (Where there are no threads, the
/// print writes itself.)
pub(crate) struct Lane {
    output: Arc<dyn Output>,
    state: parking_lot::Mutex<LaneState>,
    /// The writer waits here for texts.
    work: parking_lot::Condvar,
    /// Who waits for the queue to be written out waits here.
    idle: parking_lot::Condvar,
}

#[derive(Default)]
struct LaneState {
    /// The texts the writer has, oldest first, and their bytes.
    queue: std::collections::VecDeque<String>,
    queued: usize,
    /// The texts that wait for room, oldest first.
    waiting: std::collections::VecDeque<Waiting>,
    /// The writer is writing a batch.
    writing: bool,
    /// The writer thread runs.
    thread: bool,
    /// The writer thread waits for texts: a print wakes it. (One that
    /// writes finds the new texts when it is done.)
    asleep: bool,
    /// How many batches have been written: a drain that sees it move
    /// knows the reader takes them.
    written: u64,
    /// The first failure of a write, which every later print gives.
    failed: Option<String>,
}

/// A text that waits for room ([`Printed::Waits`]).
struct Waiting {
    text: String,
    room: Arc<Cell<()>>,
    /// Who wakes the print, and what keeps its program from being seen
    /// as stuck meanwhile: the writer completes the wait from outside
    /// its tasks.
    scheduler: Arc<crate::scheduler::Scheduler>,
    external: crate::scheduler::External,
}

impl Lane {
    fn new(output: Arc<dyn Output>) -> Arc<Lane> {
        Arc::new(Lane {
            output,
            state: parking_lot::Mutex::default(),
            work: parking_lot::Condvar::new(),
            idle: parking_lot::Condvar::new(),
        })
    }

    /// Put `text` in the queue, or have it wait for room. A runtime's
    /// report (`room` none) never waits: it goes in the queue whatever
    /// it holds, after what is there.
    fn print(
        self: &Arc<Self>,
        text: String,
        room: Option<&Arc<crate::scheduler::Scheduler>>,
    ) -> Printed {
        #[cfg(target_arch = "wasm32")]
        {
            let _ = room;
            return match write_caught(&*self.output, &text) {
                Ok(()) => Printed::Queued,
                Err(e) => Printed::Failed(crate::diagnostic::io_error_text(&e)),
            };
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut state = self.state.lock();
            if let Some(failed) = &state.failed
                && room.is_some()
            {
                return Printed::Failed(failed.clone());
            }
            let fits = state.queued == 0 || state.queued + text.len() <= QUEUED_MAX;
            match room {
                Some(scheduler) if !(state.waiting.is_empty() && fits) => {
                    let cell = Cell::new();
                    state.waiting.push_back(Waiting {
                        text,
                        room: cell.clone(),
                        scheduler: scheduler.clone(),
                        external: scheduler.external(),
                    });
                    Printed::Waits(cell)
                }
                _ => {
                    state.queued += text.len();
                    state.queue.push_back(text);
                    self.start(&mut state);
                    Printed::Queued
                }
            }
        }
    }

    /// Wake the writer, or start one.
    #[cfg(not(target_arch = "wasm32"))]
    fn start(self: &Arc<Self>, state: &mut LaneState) {
        if state.thread {
            if std::mem::take(&mut state.asleep) {
                self.work.notify_one();
            }
            return;
        }
        let lane = self.clone();
        let started = std::thread::Builder::new()
            .name("silt-output".into())
            .spawn(move || lane.write_out());
        match started {
            Ok(_) => state.thread = true,
            // No thread: the texts are written here, in their order.
            Err(_) => {
                let batch: String = state.queue.drain(..).collect();
                state.queued = 0;
                if let Err(e) = write_caught(&*self.output, &batch) {
                    state
                        .failed
                        .get_or_insert(crate::diagnostic::io_error_text(&e));
                }
            }
        }
    }

    /// The writer thread: write what is queued, in batches, until there
    /// has been nothing for a while.
    #[cfg(not(target_arch = "wasm32"))]
    fn write_out(self: Arc<Self>) {
        loop {
            let mut state = self.state.lock();
            while state.queue.is_empty() {
                self.idle.notify_all();
                state.asleep = true;
                let timed_out = self.work.wait_for(&mut state, WRITER_IDLE).timed_out();
                state.asleep = false;
                if timed_out && state.queue.is_empty() {
                    state.thread = false;
                    return;
                }
            }
            let batch: String = state.queue.drain(..).collect();
            state.writing = true;
            drop(state);
            let result = write_caught(&*self.output, &batch);
            let mut state = self.state.lock();
            state.writing = false;
            state.written += 1;
            state.queued -= batch.len();
            if let Err(e) = result {
                state
                    .failed
                    .get_or_insert(crate::diagnostic::io_error_text(&e));
            }
            // The texts that waited go in, oldest first, as far as there
            // is room.
            let mut given = Vec::new();
            while let Some(next) = state.waiting.front() {
                // One whose print has stopped waiting is not written.
                if Arc::strong_count(&next.room) == 1 {
                    let _ = state.waiting.pop_front();
                    continue;
                }
                if state.queued != 0 && state.queued + next.text.len() > QUEUED_MAX {
                    break;
                }
                let Some(next) = state.waiting.pop_front() else {
                    break;
                };
                let Waiting {
                    text,
                    room,
                    scheduler,
                    external,
                } = next;
                state.queued += text.len();
                state.queue.push_back(text);
                given.push((room, scheduler, external));
            }
            drop(state);
            // Woken outside the lock; each print goes on, and only then
            // does its program count without it.
            for (room, scheduler, external) in given {
                let _ = room.complete((), scheduler.wake());
                drop(external);
            }
        }
    }

    /// Wait until what is queued has been written. `stall`: give up
    /// when no batch has been written for that long (a reader that
    /// takes nothing); none waits as long as it takes.
    fn drain(&self, stall: Option<Duration>) {
        let mut state = self.state.lock();
        loop {
            if state.queue.is_empty() && !state.writing {
                return;
            }
            let before = state.written;
            match stall {
                None => self.idle.wait(&mut state),
                Some(stall) => {
                    if self.idle.wait_for(&mut state, stall).timed_out() && state.written == before
                    {
                        return;
                    }
                }
            }
        }
    }
}

/// The lanes of the process's stdout and stderr, one each for every
/// program of the process ([`HostIo::process`]).
fn process_lanes() -> &'static (Arc<Lane>, Arc<Lane>) {
    static LANES: OnceLock<(Arc<Lane>, Arc<Lane>)> = OnceLock::new();
    LANES.get_or_init(|| {
        (
            Lane::new(Arc::new(ProcessStdout)),
            Lane::new(Arc::new(ProcessStderr)),
        )
    })
}

/// Write out what the programs of the process have printed and is
/// still queued, as `process.exit` ends the process: a writer that
/// writes nothing for `stall` (its reader takes nothing) is not waited
/// for longer.
pub(crate) fn drain_process_output(stall: Duration) {
    let (stdout, stderr) = process_lanes();
    stdout.drain(Some(stall));
    stderr.drain(Some(stall));
}

/// The output and the clock of a [`Vm`](super::Vm), set when the VM is
/// made. See the module documentation.
#[derive(Clone)]
pub struct HostIo {
    stdout: Arc<Lane>,
    stderr: Arc<Lane>,
    clock: Arc<dyn Clock>,
    /// Whether `clock` is [`SystemClock`], on which a duration takes
    /// that long: the runtime's threads then wait a deadline out
    /// instead of reading the clock again and again.
    system_clock: bool,
    /// The message of the first panic of `clock`, once it has
    /// panicked. From then on the clock is not called again, its
    /// readings are zero, and whatever the program does next fails with
    /// [`HostIo::clock_failure`]: a clock that panics must not leave a
    /// wait that never ends.
    clock_panic: Arc<OnceLock<String>>,
}

impl HostIo {
    /// Output to `stdout` and `stderr`, and the system clock. Each is
    /// written by a thread of its own ([`Lane`]): what a program prints
    /// is there once the code the VM runs has returned, or the VM has
    /// settled ([`Vm::settle`](super::Vm::settle)).
    pub fn new(stdout: impl Output + 'static, stderr: impl Output + 'static) -> HostIo {
        HostIo::of_lanes(Lane::new(Arc::new(stdout)), Lane::new(Arc::new(stderr)))
    }

    fn of_lanes(stdout: Arc<Lane>, stderr: Arc<Lane>) -> HostIo {
        HostIo {
            stdout,
            stderr,
            clock: Arc::new(SystemClock),
            system_clock: true,
            clock_panic: Arc::default(),
        }
    }

    /// Both stdout and stderr into `buffer`, and the system clock: what
    /// an embedder starts from.
    pub fn buffer(buffer: &Buffer) -> HostIo {
        HostIo::new(buffer.clone(), buffer.clone())
    }

    /// The stdout and the stderr of the process, and the system clock:
    /// what the `silt` command runs programs with. A program that
    /// writes to a closed stdout pipe ends the process, quietly, with
    /// status 141. Every program of the process prints through the
    /// same two writers.
    pub fn process() -> HostIo {
        let (stdout, stderr) = process_lanes();
        HostIo::of_lanes(stdout.clone(), stderr.clone())
    }

    /// The same output, with `clock` instead of the system clock.
    ///
    /// The runtime's own threads still wait in real time: a task's
    /// `time.sleep`, a `channel.timeout`, a `channel.receive_timeout` and
    /// the wait of a `task.deadline` end within a millisecond of the
    /// moment `clock` reaches their deadline.
    pub fn clock(self, clock: impl Clock + 'static) -> HostIo {
        HostIo {
            clock: Arc::new(clock),
            system_clock: false,
            clock_panic: Arc::default(),
            ..self
        }
    }

    /// A program's `print` (`println`, `io.eprint`, `io.eprintln`) of
    /// `text` to `stream`: the text goes to the writer, or waits for
    /// room there, which the print then waits for, woken by
    /// `scheduler`.
    pub(crate) fn print(
        &self,
        stream: Stream,
        text: String,
        scheduler: &Arc<crate::scheduler::Scheduler>,
    ) -> Printed {
        self.lane(stream).print(text, Some(scheduler))
    }

    /// Write `text` to stderr: a report of the runtime's for the
    /// program. It never waits: it goes to the writer after what is
    /// there. A write error, or a panic of the output, is ignored:
    /// there is nowhere left to report it.
    pub(crate) fn err(&self, text: &str) {
        let _ = self.stderr.print(text.to_string(), None);
    }

    /// Wait until what the program has printed has been written out,
    /// to stdout and to stderr, however long the readers take.
    pub(crate) fn drain(&self) {
        self.stdout.drain(None);
        self.stderr.drain(None);
    }

    fn lane(&self, stream: Stream) -> &Arc<Lane> {
        match stream {
            Stream::Stdout => &self.stdout,
            Stream::Stderr => &self.stderr,
        }
    }

    /// Call the clock with a panic caught: the first one is kept as
    /// the clock's failure, and the call gives `T::default()` then and
    /// ever after.
    fn read_clock<T: Default>(&self, read: impl FnOnce(&dyn Clock) -> T) -> T {
        if self.clock_panic.get().is_some() {
            return T::default();
        }
        let read = std::panic::AssertUnwindSafe(|| read(&*self.clock));
        match crate::scheduler::catch_as_error(read) {
            Ok(value) => value,
            Err(payload) => {
                let _ = self.clock_panic.set(panic_message(&payload).to_string());
                T::default()
            }
        }
    }

    /// The runtime error of a program whose clock has panicked; `None`
    /// while it has not. Checked after every builtin call, and by the
    /// runtime's threads, which then end every wait they hold.
    pub(crate) fn clock_failure(&self) -> Option<String> {
        self.clock_panic
            .get()
            .map(|message| format!("the clock panicked: {message}"))
    }

    /// The time since the Unix epoch.
    pub(crate) fn now(&self) -> Duration {
        self.read_clock(|clock| clock.now())
    }

    /// The clock's monotonic reading.
    pub(crate) fn monotonic(&self) -> Duration {
        self.read_clock(|clock| clock.monotonic())
    }

    /// The reading at which a wait of `duration` that starts now ends;
    /// `None` when it is out of range.
    pub(crate) fn deadline_after(&self, duration: Duration) -> Option<Duration> {
        self.monotonic().checked_add(duration)
    }

    /// Block the calling thread for `duration` on the clock.
    pub(crate) fn sleep(&self, duration: Duration) {
        self.read_clock(|clock| clock.sleep(duration))
    }

    /// How long a thread of the runtime waits, in real time, before it
    /// looks at the clock again, when the next deadline is `left` away
    /// on the clock.
    pub(crate) fn real_wait(&self, left: Duration) -> Duration {
        /// How often an embedder's clock is read while a deadline is
        /// pending on it.
        const POLL: Duration = Duration::from_millis(1);
        if self.system_clock {
            left
        } else {
            left.min(POLL)
        }
    }
}

/// `output.write(text)`, with a panic turned into an error that carries
/// the panic's message.
fn write_caught(output: &dyn Output, text: &str) -> io::Result<()> {
    let write = std::panic::AssertUnwindSafe(|| output.write(text));
    match crate::scheduler::catch_as_error(write) {
        Ok(result) => result,
        Err(payload) => {
            let message = panic_message(&payload);
            Err(io::Error::other(format!("the output panicked: {message}")))
        }
    }
}

/// What stands for the message of a panic that was raised with
/// something other than a text.
pub(crate) const NO_PANIC_TEXT: &str = "<non-string panic payload>";

/// The message a panic was raised with.
pub(crate) fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> &str {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        s
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.as_str()
    } else {
        NO_PANIC_TEXT
    }
}
