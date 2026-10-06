//! What a running program reaches of its host: where its output goes
//! and which clock it reads.
//!
//! A [`HostIo`] is given to [`Vm::new`](super::Vm::new) and holds for
//! everything that VM runs, in the program's thread and in its tasks:
//!
//! - `print` and `println` write to its stdout;
//! - what the runtime prints for the program (the report of a task
//!   that failed and was never joined, the log of an `http.serve`
//!   handler that failed) goes to its stderr;
//! - `time.now`, `time.today`, `time.sleep`, the timeouts of
//!   `channel.timeout` and `channel.recv_timeout`, and the deadlines of
//!   `task.deadline` and `task.spawn_until` read its clock.
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

/// Where text written by a program goes. Called from the thread that
/// runs the program and from the scheduler's threads, one call for each
/// `print`, `println` or report.
pub trait Output: Send + Sync {
    /// Take `text`. An error returned for a program's `print` or
    /// `println` becomes a runtime error of the program; one returned
    /// for a report on stderr is dropped. A panic counts as an error.
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
            Err(e) if e.kind() == io::ErrorKind::BrokenPipe => std::process::exit(141),
            result => result,
        }
    }
}

/// The stderr of the process.
struct ProcessStderr;

impl Output for ProcessStderr {
    fn write(&self, text: &str) -> io::Result<()> {
        use std::io::Write;
        io::stderr().lock().write_all(text.as_bytes())
    }
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

/// The output and the clock of a [`Vm`](super::Vm), set when the VM is
/// made. See the module documentation.
#[derive(Clone)]
pub struct HostIo {
    stdout: Arc<dyn Output>,
    stderr: Arc<dyn Output>,
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
    /// Output to `stdout` and `stderr`, and the system clock.
    pub fn new(stdout: impl Output + 'static, stderr: impl Output + 'static) -> HostIo {
        HostIo {
            stdout: Arc::new(stdout),
            stderr: Arc::new(stderr),
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
    /// status 141.
    pub fn process() -> HostIo {
        HostIo::new(ProcessStdout, ProcessStderr)
    }

    /// The same output, with `clock` instead of the system clock.
    ///
    /// The runtime's own threads still wait in real time: a task's
    /// `time.sleep`, a `channel.timeout`, a `channel.recv_timeout` and
    /// a wait for I/O under a `task.deadline` end within a millisecond
    /// of the moment `clock` reaches their deadline.
    pub fn clock(self, clock: impl Clock + 'static) -> HostIo {
        HostIo {
            clock: Arc::new(clock),
            system_clock: false,
            clock_panic: Arc::default(),
            ..self
        }
    }

    /// Write `text` to stdout. A panic of the output is an error like
    /// one it returns: it must not take down the thread that runs the
    /// program or a task.
    pub(crate) fn out(&self, text: &str) -> io::Result<()> {
        write_caught(&*self.stdout, text)
    }

    /// Write `text` to stderr. A write error, or a panic of the output,
    /// is ignored: there is nowhere left to report it.
    pub(crate) fn err(&self, text: &str) {
        let _ = write_caught(&*self.stderr, text);
    }

    /// Call the clock with a panic caught: the first one is kept as
    /// the clock's failure, and the call gives `T::default()` then and
    /// ever after.
    fn read_clock<T: Default>(&self, read: impl FnOnce(&dyn Clock) -> T) -> T {
        if self.clock_panic.get().is_some() {
            return T::default();
        }
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| read(&*self.clock))) {
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
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| output.write(text))) {
        Ok(result) => result,
        Err(payload) => {
            let message = panic_message(&payload);
            Err(io::Error::other(format!("the output panicked: {message}")))
        }
    }
}

/// The message a panic was raised with.
fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> &str {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        s
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.as_str()
    } else {
        "<non-string panic payload>"
    }
}
