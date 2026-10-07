//! The I/O pool is elastic, a blocked operation on a socket is ended
//! when nobody waits for it any more, and a plain TCP connection is
//! read and written at the same time.
//!
//! The peers are held by the tests (a listener of the test's own, on a
//! port the OS gave), so nothing here depends on a fixed port. Where a
//! program sleeps, the sleep only gives the old code time to go wrong:
//! the new code passes without it.

#![cfg(feature = "tcp")]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use silt::session::testing::compile_str;
use silt::{Buffer, HostIo, Value, Vm};

/// How long a program may take before the test calls it hung.
const BUDGET: Duration = Duration::from_secs(30);

/// Run `source` to its end (`settle` included) on a thread of its own.
/// Gives the VM, still alive, and what the program printed; `None` if
/// it did not end within [`BUDGET`].
fn run(source: &str) -> Option<(Vm, String)> {
    let program = compile_str(source).unwrap_or_else(|errors| panic!("{errors:?}"));
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let out = Buffer::new();
        let mut vm = Vm::new(HostIo::buffer(&out));
        let result = vm.run_program(&program).map_err(|e| e.message);
        vm.settle();
        assert_eq!(result, Ok(Value::Unit), "output: {:?}", out.contents());
        let _ = tx.send((vm, out.contents()));
    });
    rx.recv_timeout(BUDGET).ok()
}

/// A listener of the test's own, and its address.
fn listener() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local_addr").to_string();
    (listener, addr)
}

/// Accept `n` connections and keep them open, saying nothing, until
/// the sender that is returned is dropped.
fn silent_peer(listener: TcpListener, n: usize) -> mpsc::Sender<()> {
    let (done, wait) = mpsc::channel::<()>();
    thread::spawn(move || {
        let held: Vec<TcpStream> = (0..n)
            .filter_map(|_| listener.accept().ok())
            .map(|c| c.0)
            .collect();
        let _ = wait.recv();
        drop(held);
    });
    done
}

/// The threads of this process (Linux), for the tests that say a
/// thread has ended.
#[cfg(target_os = "linux")]
fn os_threads() -> usize {
    std::fs::read_dir("/proc/self/task")
        .expect("/proc/self/task")
        .count()
}

/// Wait, for at most ten seconds, until `done` holds.
fn until(what: &str, done: impl Fn() -> bool) {
    let limit = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < limit, "{what}");
        thread::sleep(Duration::from_millis(5));
    }
}

fn temp_file(name: &str, text: &str) -> String {
    let dir = std::env::temp_dir().join(format!("silt_stage7_io_pool_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    path.to_string_lossy().replace('\\', "/")
}

/// Eight tasks each wait for bytes from a peer that sends none. A
/// file read does not wait for any of them: it has a thread of its
/// own. (With a pool of at most four threads it waited for ever.)
/// When the tasks are cancelled their reads end, and so do the
/// threads.
#[test]
fn idle_readers_do_not_hold_up_a_file_read() {
    let (listener, addr) = listener();
    let _peer = silent_peer(listener, 8);
    let path = temp_file("idle_readers.txt", "the file");
    let source = format!(
        r#"
import channel
import io
import list
import task
import tcp
import time

fn main() {{
  let connected = channel.new(0)
  let readers = 1..8 |> list.map {{ _ ->
    task.spawn {{ ->
      when let Ok(conn) = tcp.connect("{addr}") else {{ panic("cannot connect") }}
      channel.send(connected, ())
      tcp.read(conn, 16)
    }}
  }}
  1..8 |> list.each {{ _ ->
    let _ = channel.receive(connected)
  }}
  time.sleep(time.ms(100))
  when let Ok(text) = io.read_file("{path}") else {{ panic("cannot read the file") }}
  println(text)
  readers |> list.each {{ h -> task.cancel(h) }}
}}
"#
    );
    let (vm, out) = run(&source).expect("the file read did not wait for the idle readers");
    assert_eq!(out, "the file\n");
    #[cfg(feature = "test-hooks")]
    until("the threads of the cancelled reads left the pool", || {
        silt::vm::io_pool_threads(&vm) <= 1
    });
    drop(vm);
}

/// Eight reads time out (`task.deadline`). Their connections are shut
/// down, so their threads end; a file read afterwards runs at once,
/// and the connection of a read that timed out is closed.
#[test]
fn timed_out_reads_free_their_threads() {
    let (listener, addr) = listener();
    let _peer = silent_peer(listener, 8);
    let path = temp_file("timed_out_reads.txt", "the file");
    let source = format!(
        r#"
import io
import list
import task
import tcp
import time

fn main() {{
  let readers = 1..8 |> list.map {{ _ ->
    task.spawn {{ ->
      when let Ok(conn) = tcp.connect("{addr}") else {{ panic("cannot connect") }}
      let first = match task.deadline(time.ms(30), {{ -> tcp.read(conn, 16) }}) {{
        Err(tcp.TcpTimeout) -> "timed out"
        _ -> "something else"
      }}
      let second = match tcp.read(conn, 16) {{
        Err(tcp.TcpClosed) -> "closed"
        _ -> "something else"
      }}
      "{{first}}, then {{second}}"
    }}
  }}
  readers |> list.each {{ h -> println(task.join(h)) }}
  when let Ok(text) = io.read_file("{path}") else {{ panic("cannot read the file") }}
  println(text)
}}
"#
    );
    #[cfg(target_os = "linux")]
    let before = os_threads();
    let (vm, out) = run(&source).expect("the program ended");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 9, "{out:?}");
    assert!(
        lines[..8]
            .iter()
            .all(|line| *line == "timed out, then closed"),
        "{out:?}"
    );
    assert_eq!(lines[8], "the file");
    // The eight threads are gone, not parked in a read for as long
    // as the peer says nothing. What may remain: the scheduler's
    // workers and timer, and pool threads that wait for work.
    #[cfg(target_os = "linux")]
    until("the threads of the timed-out reads ended", || {
        os_threads() <= before + 12
    });
    #[cfg(feature = "test-hooks")]
    until("they left the pool", || silt::vm::io_pool_threads(&vm) <= 2);
    drop(vm);
}

/// One task waits in a read, another writes on the same connection:
/// the write is not held up by the read. The peer answers only when
/// it has got the write, so with one lock for both the program hung.
#[test]
fn a_connection_is_read_and_written_at_the_same_time() {
    let (listener, addr) = listener();
    let peer = thread::spawn(move || {
        let (mut conn, _) = listener.accept().expect("accept");
        let mut ping = [0u8; 4];
        conn.read_exact(&mut ping).expect("the ping");
        assert_eq!(&ping, b"ping");
        conn.write_all(b"pong").expect("the pong");
        // Until the program has closed its end.
        let mut rest = Vec::new();
        let _ = conn.read_to_end(&mut rest);
    });
    let source = format!(
        r#"
import bytes
import channel
import task
import tcp
import time

fn main() {{
  when let Ok(conn) = tcp.connect("{addr}") else {{ panic("cannot connect") }}
  let reading = channel.new(0)
  let reader = task.spawn {{ ->
    channel.send(reading, ())
    tcp.read_exact(conn, 4)
  }}
  let _ = channel.receive(reading)
  time.sleep(time.ms(100))
  when let Ok(_) = tcp.write(conn, bytes.from_string("ping")) else {{ panic("cannot write") }}
  when let Ok(answer) = task.join(reader) else {{ panic("cannot read") }}
  println(bytes.length(answer))
  tcp.close(conn)
}}
"#
    );
    let (_vm, out) = run(&source).expect("the write was not held up by the read");
    assert_eq!(out, "4\n");
    peer.join()
        .expect("the peer got the ping and sent the pong");
}

/// A task that waits in `tcp.accept` is cancelled, a thousand times in
/// a row. Each accept gives up, and its thread ends: the number of
/// threads stays level.
#[test]
fn cancelled_accepts_leave_no_thread() {
    let source = r#"
import channel
import task
import tcp

fn go(listener, started, n) {
  match n {
    0 -> ()
    _ -> {
      let acceptor = task.spawn { ->
        channel.send(started, ())
        tcp.accept(listener)
      }
      let _ = channel.receive(started)
      task.cancel(acceptor)
      go(listener, started, n - 1)
    }
  }
}

fn main() {
  when let Ok(listener) = tcp.listen("127.0.0.1:0") else { panic("cannot listen") }
  go(listener, channel.new(0), 1000)
  println("done")
}
"#;
    #[cfg(target_os = "linux")]
    let before = os_threads();
    let (vm, out) = run(source).expect("the program ended");
    assert_eq!(out, "done\n");
    #[cfg(target_os = "linux")]
    until("the threads of the cancelled accepts ended", || {
        os_threads() <= before + 12
    });
    #[cfg(feature = "test-hooks")]
    until("they left the pool", || silt::vm::io_pool_threads(&vm) <= 2);
    drop(vm);
}

/// An accept that waits costs nothing: its thread is in the OS call
/// and is not woken until a connection comes or the accept is given
/// up. Over two seconds it uses no CPU time to speak of and is never
/// scheduled.
#[test]
#[cfg(target_os = "linux")]
fn an_idle_accept_is_not_woken() {
    /// The threads of the I/O pool: (voluntary context switches,
    /// nanoseconds on a CPU) of each.
    fn io_threads() -> Vec<(u64, u64)> {
        let mut threads = Vec::new();
        for task in std::fs::read_dir("/proc/self/task").expect("/proc/self/task") {
            let dir = task.expect("a task").path();
            let name = std::fs::read_to_string(dir.join("comm")).unwrap_or_default();
            if name.trim() != "silt-io" {
                continue;
            }
            let status = std::fs::read_to_string(dir.join("status")).unwrap_or_default();
            let switches = status
                .lines()
                .find_map(|line| line.strip_prefix("voluntary_ctxt_switches:"))
                .and_then(|n| n.trim().parse().ok())
                .expect("voluntary_ctxt_switches");
            let on_cpu = std::fs::read_to_string(dir.join("schedstat"))
                .ok()
                .and_then(|stat| stat.split_whitespace().next()?.parse().ok())
                .expect("schedstat");
            threads.push((switches, on_cpu));
        }
        threads
    }

    let source = r#"
import task
import tcp
import time

fn main() {
  when let Ok(listener) = tcp.listen("127.0.0.1:0") else { panic("cannot listen") }
  let acceptor = task.spawn { -> tcp.accept(listener) }
  time.sleep(time.ms(4000))
  task.cancel(acceptor)
  println("done")
}
"#;
    let program = compile_str(source).unwrap_or_else(|errors| panic!("{errors:?}"));
    let running = thread::spawn(move || {
        let out = Buffer::new();
        let mut vm = Vm::new(HostIo::buffer(&out));
        let result = vm.run_program(&program).map_err(|e| e.message);
        vm.settle();
        (result, out.contents())
    });
    until("the accept has its thread", || io_threads().len() == 1);
    // Let it get into the call.
    thread::sleep(Duration::from_millis(300));
    let before = io_threads();
    thread::sleep(Duration::from_secs(2));
    let after = io_threads();
    assert_eq!(before.len(), 1, "one thread in accept");
    assert_eq!(after.len(), 1, "one thread in accept");
    let (switches, on_cpu) = (after[0].0 - before[0].0, after[0].1 - before[0].1);
    assert_eq!(
        switches, 0,
        "the thread of the accept was woken while it waited"
    );
    assert!(
        on_cpu < 10_000_000,
        "the accept used {on_cpu} ns of CPU in two seconds"
    );
    let (result, out) = running.join().expect("the program ran");
    assert_eq!(result, Ok(Value::Unit));
    assert_eq!(out, "done\n");
}

/// Two tasks wait in `tcp.accept` on one listener; the first is
/// cancelled. The connection that wakes it reaches it and no other:
/// the second still gets the client that connects afterwards, and
/// only that one.
#[test]
fn giving_up_one_accept_leaves_the_others() {
    let source = r#"
import bytes
import channel
import task
import tcp

fn main() {
  when let Ok(listener) = tcp.listen("127.0.0.1:0") else { panic("cannot listen") }
  let started = channel.new(0)
  let first = task.spawn { ->
    channel.send(started, ())
    tcp.accept(listener)
  }
  let _ = channel.receive(started)
  let second = task.spawn { ->
    channel.send(started, ())
    when let Ok(conn) = tcp.accept(listener) else { panic("the second accept failed") }
    when let Ok(greeting) = tcp.read_exact(conn, 5) else { panic("cannot read") }
    bytes.length(greeting)
  }
  let _ = channel.receive(started)
  task.cancel(first)
  when let Ok(client) = tcp.connect("127.0.0.1:{tcp.local_port(listener)}") else { panic("cannot connect") }
  when let Ok(_) = tcp.write(client, bytes.from_string("hello")) else { panic("cannot write") }
  println(task.join(second))
}
"#;
    let (_vm, out) = run(source).expect("the program ended");
    assert_eq!(out, "5\n");
}

/// `main` fails while a task waits in `tcp.read`. Nothing is waited
/// for (`stop_tasks`): the task is dropped, its connection is shut
/// down, and the thread of the read ends although the peer never says
/// a word.
#[test]
fn a_read_dropped_with_its_task_leaves_no_thread() {
    let (listener, addr) = listener();
    let _peer = silent_peer(listener, 1);
    let source = format!(
        r#"
import channel
import task
import tcp

fn main() {{
  let connected = channel.new(0)
  let _reader = task.spawn {{ ->
    when let Ok(conn) = tcp.connect("{addr}") else {{ panic("cannot connect") }}
    channel.send(connected, ())
    tcp.read(conn, 16)
  }}
  let _ = channel.receive(connected)
  panic("main gives up")
}}
"#
    );
    let program = compile_str(&source).unwrap_or_else(|errors| panic!("{errors:?}"));
    #[cfg(target_os = "linux")]
    let before = os_threads();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let err = Buffer::new();
        let mut vm = Vm::new(HostIo::new(Buffer::new(), err.clone()));
        let result = vm.run_program(&program).map_err(|e| e.message);
        vm.stop_tasks();
        let _ = tx.send((vm, result));
    });
    let (vm, result) = rx.recv_timeout(BUDGET).expect("the tasks were stopped");
    assert_eq!(result, Err("panic: main gives up".to_string()));
    #[cfg(feature = "test-hooks")]
    until("the read left the pool", || {
        silt::vm::io_pool_threads(&vm) <= 1
    });
    #[cfg(target_os = "linux")]
    until("the thread of the read ended", || {
        os_threads() <= before + 12
    });
    drop(vm);
}
