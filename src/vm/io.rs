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

/// Where text written by a program goes. Called from the thread that
/// runs the program and from the scheduler's threads, one call for each
/// `print`, `println`, `io.eprint`, `io.eprintln` or report.
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
/// would panic there. Not once the program ends by a failure
/// ([`HostIo::drain_failing`]): the failure is reported then, and the
/// write is an error.
struct ProcessStdout;

/// The program ends by a failure ([`HostIo::drain_failing`]).
static FAILING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

impl Output for ProcessStdout {
    fn write(&self, text: &str) -> io::Result<()> {
        use std::io::Write;
        match io::stdout().lock().write_all(text.as_bytes()) {
            Err(e)
                if e.kind() == io::ErrorKind::BrokenPipe
                    && !FAILING.load(std::sync::atomic::Ordering::SeqCst) =>
            {
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

/// The size of a piece of a print that goes in pieces: what the writer's
/// queue holds.
pub(crate) const PIECE: usize = QUEUED_MAX;

/// How long the writer thread of a lane made for a test waits for more
/// before it ends. (Those of the process's outputs live with the
/// process, asleep while there is nothing to write.)
#[cfg(test)]
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
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stream {
    Stdout,
    Stderr,
}

/// The two outputs of a [`HostIo`] and the one thread that writes to
/// them: the program's prints, to stdout and to stderr, are put in one
/// queue, in the order of the calls, and the thread writes them out,
/// whole and in that order, each to its output. So a line on stderr
/// stays between the stdout lines it was printed between (`2>&1`). A
/// print does not wait for the outputs: one that finds the queue full
/// waits for room as any wait of a task does, and the other tasks run.
/// So a reader that is slow, or gone quiet, holds up the tasks that
/// print, and nothing else.
///
/// The process's stdout and stderr are written so ([`HostIo::process`]);
/// the thread starts at the first text and sleeps while there is
/// nothing to write. An embedder's outputs ([`HostIo::new`]) are written
/// by the print itself, as is everything where there are no threads.
pub(crate) struct Lane {
    /// stdout and stderr.
    outputs: [Arc<dyn Output>; 2],
    /// Whether a writer thread writes the texts; else the print does.
    queued: bool,
    /// Test-only: the writer thread ends when it has had nothing to
    /// write for [`WRITER_IDLE`].
    #[cfg(test)]
    ends_when_idle: bool,
    state: parking_lot::Mutex<LaneState>,
    /// The writer waits here for texts.
    work: parking_lot::Condvar,
    /// Who waits for the queue to be written out waits here.
    idle: parking_lot::Condvar,
}

#[derive(Default)]
struct LaneState {
    /// The texts the writer has, oldest first, each with its output, and
    /// their bytes.
    queue: std::collections::VecDeque<(Stream, String)>,
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
    /// The print whose text goes in pieces now ([`Lane::print_piece`]):
    /// the texts of other prints wait until its last piece is in, or
    /// until it ends otherwise (a deadline, a cancel).
    stream: Option<u64>,
    /// The number the next such print gets.
    next_stream: u64,
    /// The first failure of a write to stdout, which every later print
    /// to stdout gives. (One to stderr is dropped: there is nowhere left
    /// to report it.)
    failed: Option<String>,
    /// Whether a print has given that failure.
    raised: bool,
}

/// A text that waits for room ([`Printed::Waits`]).
struct Waiting {
    stream: Stream,
    text: String,
    /// For a piece of a print in pieces: that print.
    of: Option<u64>,
    room: Arc<Cell<()>>,
    /// Who wakes the print, and what keeps its program from being seen
    /// as stuck meanwhile: the writer completes the wait from outside
    /// its tasks.
    scheduler: Arc<crate::scheduler::Scheduler>,
    external: crate::scheduler::External,
}

impl Lane {
    fn new(stdout: Arc<dyn Output>, stderr: Arc<dyn Output>, queued: bool) -> Arc<Lane> {
        Arc::new(Lane {
            outputs: [stdout, stderr],
            queued,
            #[cfg(test)]
            ends_when_idle: false,
            state: parking_lot::Mutex::default(),
            work: parking_lot::Condvar::new(),
            idle: parking_lot::Condvar::new(),
        })
    }

    /// The output a text of `stream` goes to.
    fn output(&self, stream: Stream) -> &dyn Output {
        match stream {
            Stream::Stdout => &*self.outputs[0],
            Stream::Stderr => &*self.outputs[1],
        }
    }

    /// Write `text` to its output, here: a failure of a write to
    /// stdout is kept.
    fn write_one(&self, stream: Stream, text: &str, failed: &mut Option<String>) {
        if let Err(e) = write_caught(self.output(stream), text)
            && stream == Stream::Stdout
        {
            failed.get_or_insert(crate::diagnostic::io_error_text(&e));
        }
    }

    /// Put `text` for `stream` in the queue, or have it wait for room. A
    /// runtime's report (`room` none) never waits: it goes in the queue
    /// whatever it holds, after what is there.
    fn print(
        self: &Arc<Self>,
        stream: Stream,
        text: String,
        room: Option<&Arc<crate::scheduler::Scheduler>>,
    ) -> Printed {
        self.print_of(stream, text, room, None)
    }

    /// [`Lane::print`] of a text that is a piece of the print `of`, if
    /// one: its first piece waits behind the texts before it, and makes
    /// the texts after it wait until its last piece is in; each other
    /// piece goes before the texts that wait.
    fn print_of(
        self: &Arc<Self>,
        stream: Stream,
        text: String,
        room: Option<&Arc<crate::scheduler::Scheduler>>,
        of: Option<u64>,
    ) -> Printed {
        if !self.queued || cfg!(target_arch = "wasm32") {
            let mut failed = None;
            self.write_one(stream, &text, &mut failed);
            return match failed {
                None => Printed::Queued,
                Some(why) => Printed::Failed(why),
            };
        }
        let mut state = self.state.lock();
        if stream == Stream::Stdout
            && room.is_some()
            && let Some(failed) = state.failed.clone()
        {
            state.raised = true;
            return Printed::Failed(failed);
        }
        let room_for = state.queued == 0 || state.queued + text.len() <= QUEUED_MAX;
        // (A print goes now if nothing waits before it and no print in
        // pieces is under way, or if it is a piece of the one that is.)
        let goes = match (state.stream, of) {
            (Some(current), Some(of)) => current == of && room_for,
            (Some(_), None) => false,
            (None, _) => state.waiting.is_empty() && room_for,
        };
        match room {
            Some(scheduler) if !goes => {
                let cell = Cell::new();
                state.waiting.push_back(Waiting {
                    stream,
                    text,
                    of,
                    room: cell.clone(),
                    scheduler: scheduler.clone(),
                    external: scheduler.external(),
                });
                Printed::Waits(cell)
            }
            _ => {
                if of.is_some() && room.is_some() {
                    state.stream = of;
                }
                state.queued += text.len();
                state.queue.push_back((stream, text));
                self.start(&mut state);
                Printed::Queued
            }
        }
    }

    /// A number for a print whose text goes in pieces.
    fn stream_number(&self) -> u64 {
        let mut state = self.state.lock();
        state.next_stream += 1;
        state.next_stream
    }

    /// The print in pieces `of` has given its last piece, or has ended
    /// otherwise: the texts that waited behind it go in.
    fn end_stream(self: &Arc<Self>, of: u64) {
        let mut state = self.state.lock();
        // (Its pieces that still wait are not written: it has ended.)
        state.waiting.retain(|waiting| waiting.of != Some(of));
        if state.stream == Some(of) {
            state.stream = None;
        }
        let given = Self::admit(&mut state);
        if !state.queue.is_empty() {
            self.start(&mut state);
        }
        drop(state);
        for (room, scheduler, external) in given {
            let _ = room.complete((), scheduler.wake());
            drop(external);
        }
    }

    /// Wake the writer, or start one.
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
                state.queued = 0;
                let mut failed = state.failed.take();
                for (stream, text) in std::mem::take(&mut state.queue) {
                    self.write_one(stream, &text, &mut failed);
                }
                state.failed = failed;
            }
        }
    }

    /// The writer thread: write what is queued, one text a call, oldest
    /// first, each to its output.
    fn write_out(self: Arc<Self>) {
        loop {
            let mut state = self.state.lock();
            while state.queue.is_empty() {
                self.idle.notify_all();
                state.asleep = true;
                #[cfg(test)]
                if self.ends_when_idle {
                    let timed_out = self.work.wait_for(&mut state, WRITER_IDLE).timed_out();
                    state.asleep = false;
                    if timed_out && state.queue.is_empty() {
                        state.thread = false;
                        return;
                    }
                    continue;
                }
                self.work.wait(&mut state);
                state.asleep = false;
            }
            let batch = std::mem::take(&mut state.queue);
            state.writing = true;
            drop(state);
            let mut failed = None;
            let mut bytes = 0;
            for (stream, text) in batch {
                bytes += text.len();
                self.write_one(stream, &text, &mut failed);
            }
            let mut state = self.state.lock();
            state.writing = false;
            state.written += 1;
            state.queued -= bytes;
            if let Some(failed) = failed {
                state.failed.get_or_insert(failed);
            }
            // The texts that waited go in, oldest first, as far as there
            // is room.
            let given = Self::admit(&mut state);
            drop(state);
            // Woken outside the lock; each print goes on, and only then
            // does its program count without it.
            for (room, scheduler, external) in given {
                let _ = room.complete((), scheduler.wake());
                drop(external);
            }
        }
    }

    /// Put the texts that wait for room in the queue, oldest first, as
    /// far as there is room, and none while a print streams: the prints
    /// to wake, each with its scheduler.
    #[allow(clippy::type_complexity)]
    fn admit(
        state: &mut LaneState,
    ) -> Vec<(
        Arc<Cell<()>>,
        Arc<crate::scheduler::Scheduler>,
        crate::scheduler::External,
    )> {
        let mut given = Vec::new();
        loop {
            // (One whose print has stopped waiting is not written.)
            state
                .waiting
                .retain(|waiting| Arc::strong_count(&waiting.room) > 1);
            // While a print goes in pieces, only its pieces go in.
            let at = match state.stream {
                Some(current) => state.waiting.iter().position(|w| w.of == Some(current)),
                None => (!state.waiting.is_empty()).then_some(0),
            };
            let Some(at) = at else {
                break;
            };
            let len = state.waiting[at].text.len();
            if state.queued != 0 && state.queued + len > QUEUED_MAX {
                break;
            }
            let Some(next) = state.waiting.remove(at) else {
                break;
            };
            let Waiting {
                stream,
                text,
                of,
                room,
                scheduler,
                external,
            } = next;
            if of.is_some() {
                state.stream = of;
            }
            state.queued += text.len();
            state.queue.push_back((stream, text));
            given.push((room, scheduler, external));
        }
        given
    }

    /// The failure of a write to stdout that no print has given yet.
    /// Given once.
    fn unraised_stdout_failure(&self) -> Option<String> {
        let mut state = self.state.lock();
        if state.raised {
            return None;
        }
        let failed = state.failed.clone()?;
        state.raised = true;
        Some(failed)
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

/// A print whose text goes in pieces ([`HostIo::begin_stream`]): when
/// it is dropped, at the end of the print or when its wait ends
/// otherwise (a deadline, a cancel), the texts that wait behind it go
/// in.
pub(crate) struct StreamEnd {
    lane: Arc<Lane>,
    of: u64,
}

impl Drop for StreamEnd {
    fn drop(&mut self) {
        self.lane.end_stream(self.of);
    }
}

/// The lane of the process's stdout and stderr, one for every program
/// of the process ([`HostIo::process`]).
fn process_lane() -> &'static Arc<Lane> {
    static LANE: OnceLock<Arc<Lane>> = OnceLock::new();
    LANE.get_or_init(|| Lane::new(Arc::new(ProcessStdout), Arc::new(ProcessStderr), true))
}

/// Write out what the programs of the process have printed and is
/// still queued, as `process.exit` ends the process: a writer that
/// writes nothing for `stall` (its reader takes nothing) is not waited
/// for longer.
pub(crate) fn drain_process_output(stall: Duration) {
    process_lane().drain(Some(stall));
}

/// The failure of a write to the process's stdout that no print has
/// given yet ([`Lane::unraised_stdout_failure`]): `process.exit` ends
/// the program with it, as the end of `run_program` does.
pub(crate) fn unraised_process_stdout_failure() -> Option<String> {
    process_lane().unraised_stdout_failure()
}

/// The output and the clock of a [`Vm`](super::Vm), set when the VM is
/// made. See the module documentation.
#[derive(Clone)]
pub struct HostIo {
    /// stdout and stderr, and who writes them.
    lane: Arc<Lane>,
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
    /// Output to `stdout` and `stderr`, and the system clock. A print
    /// writes to them itself.
    pub fn new(stdout: impl Output + 'static, stderr: impl Output + 'static) -> HostIo {
        HostIo::of_lane(Lane::new(Arc::new(stdout), Arc::new(stderr), false))
    }

    /// Test-only: output to `stdout` and `stderr` through one writer
    /// thread, as the process's output goes ([`HostIo::process`]).
    #[cfg(test)]
    pub(crate) fn queued(stdout: impl Output + 'static, stderr: impl Output + 'static) -> HostIo {
        let mut lane = Lane::new(Arc::new(stdout), Arc::new(stderr), true);
        if let Some(lane) = Arc::get_mut(&mut lane) {
            lane.ends_when_idle = true;
        }
        HostIo::of_lane(lane)
    }

    fn of_lane(lane: Arc<Lane>) -> HostIo {
        HostIo {
            lane,
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
    /// same writer, which keeps the order of the prints to the two.
    pub fn process() -> HostIo {
        HostIo::of_lane(process_lane().clone())
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
        self.lane.print(stream, text, Some(scheduler))
    }

    /// Whether a print's text may go to the outputs in pieces: where
    /// one thread writes the queue (the process's stdout and stderr). An
    /// embedder's outputs, and the outputs of a build for WebAssembly,
    /// are written by the print itself, one write a print.
    pub(crate) fn streams(&self) -> bool {
        self.lane.queued && !cfg!(target_arch = "wasm32")
    }

    /// A number for a print whose text goes in pieces
    /// ([`HostIo::print_piece`]), and what ends it when dropped.
    pub(crate) fn begin_stream(&self) -> StreamEnd {
        StreamEnd {
            lane: self.lane.clone(),
            of: self.lane.stream_number(),
        }
    }

    /// [`HostIo::print`] of `text`, a piece of the print `of`: its
    /// pieces go to the writer whole and in order, with no other print's
    /// text between them; each waits for room as a print does.
    pub(crate) fn print_piece(
        &self,
        of: &StreamEnd,
        stream: Stream,
        text: String,
        scheduler: &Arc<crate::scheduler::Scheduler>,
    ) -> Printed {
        self.lane
            .print_of(stream, text, Some(scheduler), Some(of.of))
    }

    /// Write `text` to stderr: a report of the runtime's for the
    /// program. It never waits: it goes to the writer after what is
    /// there. A write error, or a panic of the output, is ignored:
    /// there is nowhere left to report it.
    pub(crate) fn err(&self, text: &str) {
        let _ = self.lane.print(Stream::Stderr, text.to_string(), None);
    }

    /// The failure of a write to stdout that no print has given yet: the
    /// last text the writer had failed. Given once.
    pub(crate) fn unraised_stdout_failure(&self) -> Option<String> {
        self.lane.unraised_stdout_failure()
    }

    /// Wait until what the program has printed has been written out,
    /// to stdout and to stderr, however long the readers take.
    pub(crate) fn drain(&self) {
        self.lane.drain(None);
    }

    /// The program ends by a failure: wait for what it printed only while
    /// the reader takes some of it within `stall`, and from now on a
    /// reader of stdout that has gone ends the process no more (the
    /// failure is reported, with its own status, not 141).
    pub(crate) fn drain_failing(&self, stall: Duration) {
        FAILING.store(true, std::sync::atomic::Ordering::SeqCst);
        self.lane.drain(Some(stall));
    }

    /// Whether the program ends by a failure ([`HostIo::drain_failing`]).
    pub(crate) fn is_failing(&self) -> bool {
        FAILING.load(std::sync::atomic::Ordering::SeqCst)
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
