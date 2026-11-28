//! The stop signals a program asks for (`process.stop_signals`): SIGINT
//! (Ctrl-C) and SIGTERM (what a service manager stops a process with) on
//! Unix, Ctrl-C and Ctrl-Break on Windows.
//!
//! Until a program asks, nothing is installed, and a stop signal ends the
//! process as the system ends it. Once it has asked, the first stop
//! signal is a message on each channel asked for since the last one,
//! each channel is then closed, and the system's own way is put back: a
//! second signal ends the process, so a program whose shutdown is stuck
//! can still be stopped. Asking again after that installs the handler
//! again.
//!
//! On Unix the handler only writes the signal's number into a pipe (and
//! puts the system's way back), which is all a signal handler may do; a
//! thread of its own reads the pipe and sends the message. On Windows
//! the system calls the handler on a thread of its own, which sends it.
//!
//! A channel asked for is fed from outside the program
//! (`Channel::set_fed_from_outside`): a task that waits on it is never
//! judged deadlocked, as a signal may come at any time.
//!
//! The command that runs the programs can ask too (`when_untaken`):
//! `silt test` is told of a stop signal that no channel waits for, and
//! says which test it stopped in.

use std::sync::{Arc, Mutex, OnceLock, Weak};

use crate::runtime::sync::{Channel, Close};
use crate::scheduler::Scheduler;
use crate::typeinfo::bv;
use crate::value::Value;

/// The channels that the next stop signal is sent into, each with the
/// scheduler of its program. Held weakly: a channel that the program has
/// let go of waits for nothing.
static WAITING: Mutex<Vec<(Weak<Channel>, Weak<Scheduler>)>> = Mutex::new(Vec::new());

/// What a stop signal that no channel waits for does, once the command
/// has asked (`when_untaken`): called with `interrupt` as `arrived` is,
/// on the thread that reads the signals.
static UNTAKEN: OnceLock<fn(bool)> = OnceLock::new();

/// A stop signal that no channel of a program waits for calls `action`
/// (`silt test`'s, which ends the process), instead of ending the
/// process as the system ends it. The first `action` given stays.
/// Called again, it listens again where a signal that a program took
/// has put the system's way back. Where the signals cannot be listened
/// for, nothing is done: the system's way stays.
pub(crate) fn when_untaken(action: fn(bool)) {
    if !os::HAS_STOP_SIGNALS {
        return;
    }
    let _ = UNTAKEN.set(action);
    let _waiting = WAITING.lock().unwrap_or_else(|e| e.into_inner());
    let _ = os::listen();
}

/// The next stop signal is sent into `ch`, a channel of `scheduler`'s
/// program, which is then closed. `Err` with the reason when the
/// signals cannot be listened for. Where the system has no stop signals
/// (the playground) nothing is ever sent.
pub(super) fn send_the_next_into(
    ch: &Arc<Channel>,
    scheduler: &Arc<Scheduler>,
) -> Result<(), String> {
    if !os::HAS_STOP_SIGNALS {
        return Ok(());
    }
    // Held while the handler is installed and the channel is entered: a
    // signal that comes in between is sent once the channel is in.
    let mut waiting = WAITING.lock().unwrap_or_else(|e| e.into_inner());
    os::listen()?;
    ch.set_fed_from_outside();
    waiting.retain(|(ch, _)| ch.strong_count() > 0);
    waiting.push((Arc::downgrade(ch), Arc::downgrade(scheduler)));
    Ok(())
}

/// A stop signal came (`interrupt`: SIGINT or Ctrl-C; else SIGTERM or
/// Ctrl-Break): each channel that waits for it gets it, and is closed.
/// A channel that is full already (the program sent a stop into it
/// itself) or closed keeps what it has.
#[cfg_attr(not(any(unix, windows)), allow(dead_code))]
fn arrived(interrupt: bool) {
    let waiting = std::mem::take(&mut *WAITING.lock().unwrap_or_else(|e| e.into_inner()));
    let signal = match interrupt {
        true => bv::SIGNAL_INTERRUPT,
        false => bv::SIGNAL_TERMINATE,
    };
    let mut taken = false;
    for (ch, scheduler) in waiting {
        let (Some(ch), Some(scheduler)) = (ch.upgrade(), scheduler.upgrade()) else {
            continue;
        };
        taken = true;
        let _ = ch.try_send(Value::variant(signal, []), scheduler.wake());
        ch.close(Close::default(), scheduler.wake());
    }
    if !taken && let Some(action) = UNTAKEN.get() {
        action(interrupt);
    }
}

#[cfg(unix)]
mod os {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

    pub(super) const HAS_STOP_SIGNALS: bool = true;

    /// The end of the pipe the handler writes into: -1 until the reader
    /// has started.
    static WRITE_END: AtomicI32 = AtomicI32::new(-1);
    /// Whether the handler is installed for each of the two signals,
    /// SIGINT and SIGTERM. A signal that the process ignored when the
    /// program asked stays ignored (`nohup`, a background job of a
    /// shell), as `silt watch` leaves it.
    static INSTALLED: [AtomicBool; 2] = [AtomicBool::new(false), AtomicBool::new(false)];
    const SIGNALS: [libc::c_int; 2] = [libc::SIGINT, libc::SIGTERM];

    extern "C" fn on_signal(signal: libc::c_int) {
        // The system's way is back for the next signal, then the reader
        // is told which one came (1 for SIGINT, 0 for SIGTERM).
        // SAFETY: signal(2) and write(2) are async-signal-safe, and so
        // are the atomics (lock-free); the buffer is the handler's own.
        unsafe {
            for (installed, signal) in INSTALLED.iter().zip(SIGNALS) {
                if installed.swap(false, Ordering::SeqCst) {
                    libc::signal(signal, libc::SIG_DFL);
                }
            }
            let byte = u8::from(signal == libc::SIGINT);
            let _ = libc::write(
                WRITE_END.load(Ordering::SeqCst),
                (&raw const byte).cast::<libc::c_void>(),
                1,
            );
        }
    }

    /// The pipe and the thread that reads it, made once.
    fn start_reader() -> Result<(), String> {
        static STARTED: OnceLock<Result<(), String>> = OnceLock::new();
        STARTED
            .get_or_init(|| {
                let mut ends: [libc::c_int; 2] = [0; 2];
                // SAFETY: `ends` has room for the two descriptors pipe(2)
                // writes; fcntl(2) is given descriptors that are open.
                unsafe {
                    if libc::pipe(ends.as_mut_ptr()) != 0 {
                        return Err(std::io::Error::last_os_error().to_string());
                    }
                    for end in ends {
                        libc::fcntl(end, libc::F_SETFD, libc::FD_CLOEXEC);
                    }
                    // A full pipe drops a signal rather than block the handler.
                    let flags = libc::fcntl(ends[1], libc::F_GETFL);
                    libc::fcntl(ends[1], libc::F_SETFL, flags | libc::O_NONBLOCK);
                }
                let read_end = ends[0];
                std::thread::Builder::new()
                    .name("silt-stop-signals".into())
                    .spawn(move || {
                        loop {
                            let mut byte = 0u8;
                            // SAFETY: the buffer is one byte, and the
                            // descriptor stays open for the process.
                            let read = unsafe {
                                libc::read(read_end, (&raw mut byte).cast::<libc::c_void>(), 1)
                            };
                            match read {
                                1 => super::arrived(byte == 1),
                                _ if std::io::Error::last_os_error().kind()
                                    == std::io::ErrorKind::Interrupted => {}
                                _ => break,
                            }
                        }
                    })
                    .map_err(|e| e.to_string())?;
                WRITE_END.store(ends[1], Ordering::SeqCst);
                Ok(())
            })
            .clone()
    }

    /// Install the handler for SIGINT and SIGTERM, where it is not
    /// installed already and the signal is not ignored.
    pub(super) fn listen() -> Result<(), String> {
        start_reader()?;
        for (installed, signal) in INSTALLED.iter().zip(SIGNALS) {
            if installed.load(Ordering::SeqCst) {
                continue;
            }
            // SAFETY: sigaction(2) is given a zeroed action of the
            // handler's own and an out-parameter of the right type; the
            // handler only makes async-signal-safe calls.
            unsafe {
                let mut old: libc::sigaction = std::mem::zeroed();
                if libc::sigaction(signal, std::ptr::null(), &mut old) != 0
                    || old.sa_sigaction == libc::SIG_IGN
                {
                    continue;
                }
                let mut action: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
                // A call of another thread that the signal interrupts
                // goes on where the system can restart it.
                action.sa_flags = libc::SA_RESTART;
                libc::sigemptyset(&mut action.sa_mask);
                installed.store(true, Ordering::SeqCst);
                if libc::sigaction(signal, &action, std::ptr::null_mut()) != 0 {
                    installed.store(false, Ordering::SeqCst);
                    return Err(std::io::Error::last_os_error().to_string());
                }
            }
        }
        Ok(())
    }
}

#[cfg(windows)]
mod os {
    use std::sync::atomic::{AtomicBool, Ordering};

    use windows_sys::Win32::System::Console::{
        CTRL_BREAK_EVENT, CTRL_C_EVENT, SetConsoleCtrlHandler,
    };
    use windows_sys::core::BOOL;

    pub(super) const HAS_STOP_SIGNALS: bool = true;

    /// Whether the handler is installed.
    static INSTALLED: AtomicBool = AtomicBool::new(false);

    /// Ctrl-C and Ctrl-Break, while the handler is installed: it removes
    /// itself, so that the next one ends the process as the system does,
    /// and sends the signal. Any other event (the console closed, a
    /// logoff, a shutdown) goes to the system's handler.
    unsafe extern "system" fn on_ctrl(event: u32) -> BOOL {
        let interrupt = match event {
            CTRL_C_EVENT => true,
            CTRL_BREAK_EVENT => false,
            _ => return 0,
        };
        if !INSTALLED.swap(false, Ordering::SeqCst) {
            return 0;
        }
        // SAFETY: a handler may remove itself; `on_ctrl` is the routine
        // that was added.
        unsafe { SetConsoleCtrlHandler(Some(on_ctrl), 0) };
        super::arrived(interrupt);
        1
    }

    /// Install the handler, where it is not installed already.
    pub(super) fn listen() -> Result<(), String> {
        if INSTALLED.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        // SAFETY: `on_ctrl` has the signature of a handler routine.
        match unsafe { SetConsoleCtrlHandler(Some(on_ctrl), 1) } {
            0 => {
                INSTALLED.store(false, Ordering::SeqCst);
                Err(std::io::Error::last_os_error().to_string())
            }
            _ => Ok(()),
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod os {
    pub(super) const HAS_STOP_SIGNALS: bool = false;

    pub(super) fn listen() -> Result<(), String> {
        Ok(())
    }
}
