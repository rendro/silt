//! A SIGINT from outside (`kill -INT`) while `silt repl` waits at its
//! prompt acts as Ctrl-C at the keyboard: the entry being typed is
//! dropped, `^C` is shown and a fresh prompt reads the next one. Driven
//! over a pseudo-terminal, as the line editor reads a terminal only.

#![cfg(all(unix, feature = "repl"))]

use std::io::{Read, Write};
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// `silt repl` on the far side of a new pseudo-terminal: the child, and
/// the terminal's near side to write keys to and read the screen from.
fn repl_on_a_pty(home: &std::path::Path) -> (Child, std::fs::File) {
    // SAFETY: plain libc calls on descriptors this function owns; the
    // pre_exec closure calls only async-signal-safe functions.
    unsafe {
        let master = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
        assert!(master >= 0, "posix_openpt");
        assert_eq!(libc::grantpt(master), 0, "grantpt");
        assert_eq!(libc::unlockpt(master), 0, "unlockpt");
        let name = std::ffi::CStr::from_ptr(libc::ptsname(master))
            .to_string_lossy()
            .into_owned();
        let slave = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&name)
            .expect("the pty's far side opens");
        let child = Command::new(env!("CARGO_BIN_EXE_silt"))
            .arg("repl")
            .current_dir(home)
            .env("HOME", home)
            .env("XDG_DATA_HOME", home.join("data"))
            .env("TERM", "xterm-256color")
            .env("NO_COLOR", "1")
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave))
            .pre_exec(|| {
                // Its own session, with the pty as its terminal.
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            })
            .spawn()
            .expect("silt repl starts");
        let fd = OwnedFd::from_raw_fd(master);
        (child, std::fs::File::from(fd))
    }
}

/// Read the terminal into `shown` until it holds `needle` after byte
/// `from` of it, or `limit` passes. Whether it came.
fn wait_for(
    master: &mut std::fs::File,
    shown: &mut String,
    from: usize,
    needle: &str,
    limit: Duration,
) -> bool {
    let fd = std::os::fd::AsRawFd::as_raw_fd(master);
    let end = Instant::now() + limit;
    loop {
        if shown.get(from..).is_some_and(|rest| rest.contains(needle)) {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        let mut poll = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd.
        let ready = unsafe { libc::poll(&mut poll, 1, 50) };
        if ready > 0 {
            let mut buf = [0u8; 4096];
            match master.read(&mut buf) {
                Ok(0) | Err(_) => return false,
                Ok(n) => shown.push_str(&String::from_utf8_lossy(&buf[..n]).replace('\r', "")),
            }
        }
    }
}

/// The answers the REPL printed on lines of their own.
fn answers(text: &str) -> Vec<String> {
    (text.lines())
        .map(str::trim)
        .filter(|line| *line == "42" || *line == "^C" || line.contains("undefined variable"))
        .map(str::to_string)
        .collect()
}

/// Each step waits for what the terminal shows before the next: no fixed
/// sleep decides it on a slow machine.
const LIMIT: Duration = Duration::from_secs(20);

/// Type `lines` (each waited for as `(keys, then the screen shows)`),
/// interrupt (by `interrupt`), type `6 * 7` and Enter: what the REPL
/// answered.
fn run(
    tag: &str,
    lines: &[(&str, &str)],
    interrupt: impl Fn(&Child, &mut std::fs::File),
) -> Vec<String> {
    let home = std::env::temp_dir().join(format!("silt-repl-sigint-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let (mut child, mut master) = repl_on_a_pty(&home);
    let mut shown = String::new();
    assert!(
        wait_for(&mut master, &mut shown, 0, "silt> ", LIMIT),
        "the first prompt: {shown:?}"
    );
    for (keys, then) in lines {
        let from = shown.len();
        master.write_all(keys.as_bytes()).unwrap();
        assert!(
            wait_for(&mut master, &mut shown, from, then, LIMIT),
            "after {keys:?}: {shown:?}"
        );
    }
    let from = shown.len();
    interrupt(&child, &mut master);
    // The answer to the interrupt, then a fresh prompt (or, without
    // either, whatever comes: the assertion of the caller says what).
    wait_for(
        &mut master,
        &mut shown,
        from,
        "silt> ",
        Duration::from_secs(5),
    );
    let from = shown.len();
    master.write_all(b"6 * 7\r").unwrap();
    wait_for(&mut master, &mut shown, from, "\n42\n", LIMIT);
    wait_for(&mut master, &mut shown, from, "silt> ", LIMIT);
    master.write_all(b"\x04").unwrap();
    let from = shown.len();
    // (Whatever Ctrl-D still prints; nothing is waited for.)
    let _ = wait_for(
        &mut master,
        &mut shown,
        from,
        "\u{0}",
        Duration::from_secs(2),
    );
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&home);
    answers(&shown)
}

fn outside_sigint(child: &Child, _: &mut std::fs::File) {
    // SAFETY: a signal to our own child.
    unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) };
}

fn keyboard_ctrl_c(_: &Child, master: &mut std::fs::File) {
    master.write_all(b"\x03").unwrap();
}

/// FAIL with rustyline 19 alone (70717235): the signal was lost, `typed`
/// stayed in the line, and `6 * 7` made `typed6 * 7`.
#[test]
fn an_outside_sigint_at_the_prompt_acts_as_ctrl_c_at_the_keyboard() {
    let keyboard = run("keyboard", &[("typed", "typed")], keyboard_ctrl_c);
    assert_eq!(keyboard, ["^C", "42"], "Ctrl-C at the keyboard");
    let outside = run("outside", &[("typed", "typed")], outside_sigint);
    assert_eq!(outside, ["^C", "42"], "a SIGINT from outside");
}

/// Inside an entry over lines the whole entry is dropped, as by Ctrl-C.
#[test]
fn an_outside_sigint_inside_an_entry_over_lines_drops_the_entry() {
    let outside = run(
        "multi",
        &[("let xs = [\r", "  ... "), ("1,\r", "  ... ")],
        outside_sigint,
    );
    assert_eq!(outside, ["^C", "42"]);
}
