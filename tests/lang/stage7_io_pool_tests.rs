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
use std::time::Duration;

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

/// Every thread of the VM's I/O pool that exists is a free one: none
/// is still in an operation that nobody waits for. (A thread that was
/// to end and has not is one that exists and is not the pool's.)
/// Without the test hooks nothing is asserted here: the behaviour
/// that the test saw before is what it proves.
fn no_thread_is_left_in_an_operation(vm: &Vm) {
    #[cfg(feature = "test-hooks")]
    until(
        "a thread is still in an operation that nobody waits for",
        || silt::vm::io_pool_live_threads(vm) == silt::vm::io_pool_threads(vm),
    );
    let _ = vm;
}

/// Wait, for at most ten seconds, until `done` holds.
#[cfg(feature = "test-hooks")]
fn until(what: &str, done: impl Fn() -> bool) {
    use std::time::Instant;
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
    no_thread_is_left_in_an_operation(&vm);
    drop(vm);
}

/// Eight reads time out (`task.deadline`). Their connections are shut
/// down, so their threads end; a file read afterwards runs at once,
/// and the connection of a read that timed out is closed. (A read
/// whose time was over before it began has timed out as well, with the
/// connection shut down and no thread to end: the test does not
/// depend on which of the two a read was.)
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
    no_thread_is_left_in_an_operation(&vm);
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
    let (vm, out) = run(source).expect("the program ended");
    assert_eq!(out, "done\n");
    no_thread_is_left_in_an_operation(&vm);
    drop(vm);
}

/// An accept that waits costs nothing: its thread sleeps in the
/// system's call until a connection comes or the accept is given up,
/// and nothing else in the process wakes either. No thread of the
/// program is scheduled once in five seconds (see `quiet`); the heavy
/// suite looks for 65 seconds.
#[test]
#[cfg(target_os = "linux")]
fn an_idle_accept_is_not_woken() {
    crate::quiet::assert_never_woken(crate::quiet::ACCEPTS, Duration::from_secs(5));
}

/// A server that nobody connects to costs as little: `http.serve` is
/// an accept on the listener and nothing else, with no thread of its
/// own and nothing that wakes now and then to look.
#[test]
#[cfg(all(target_os = "linux", feature = "http"))]
fn an_idle_server_is_not_woken() {
    crate::quiet::assert_never_woken(crate::quiet::SERVER, Duration::from_secs(5));
}

/// The look that the two tests above take does see a wake-up: a
/// program in which one task wakes once a second, between waits that
/// are as quiet as theirs, fails it.
#[test]
#[cfg(target_os = "linux")]
#[should_panic(expected = "were woken within")]
fn a_program_that_wakes_now_and_then_is_caught() {
    let wakes = r#"
import task
import tcp
import time

fn tick() {
  time.sleep(time.ms(1000))
  tick()
}

fn main() {
  when let Ok(other) = tcp.listen("127.0.0.1:0") else { panic("cannot listen") }
  let _ = task.spawn(tick)
  println("waiting")
  let _ = tcp.accept(other)
}
"#;
    crate::quiet::assert_never_woken(wakes, Duration::from_secs(3));
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

/// An accept is given up in the same instant as a client connects, a
/// thousand times. The client's connection is never lost: if the
/// accept that was given up had it already and its task got it, that
/// task hands it on; if the accept had it and its task was gone, or
/// was given up with it in hand, the next accept gets it. Either way
/// the client is served.
#[test]
fn a_client_is_never_lost_to_an_accept_that_was_given_up() {
    let source = r#"
import bytes
import channel
import task
import tcp

fn acceptor(listener, started, accepted) {
  task.spawn { ->
    channel.send(started, ())
    match tcp.accept(listener) {
      Ok(conn) -> channel.send(accepted, conn)
      Err(_) -> ()
    }
  }
}

fn round(listener, port, started, n, served) {
  match n {
    0 -> served
    _ -> {
      let accepted = channel.new(2)
      let given_up = acceptor(listener, started, accepted)
      let _ = channel.receive(started)
      let client = task.spawn { ->
        when let Ok(conn) = tcp.connect("127.0.0.1:{port}") else { panic("the client cannot connect") }
        when let Ok(_) = tcp.write(conn, bytes.from_string("x")) else { panic("the client cannot write") }
        when let Ok(echo) = tcp.read_exact(conn, 1) else { panic("the client was not served") }
        tcp.close(conn)
        bytes.length(echo)
      }
      task.cancel(given_up)
      let next = acceptor(listener, started, accepted)
      let _ = channel.receive(started)
      when let channel.Message(conn) = channel.receive(accepted) else { panic("no connection") }
      when let Ok(byte) = tcp.read_exact(conn, 1) else { panic("the server cannot read") }
      when let Ok(_) = tcp.write(conn, byte) else { panic("the server cannot write") }
      let got = task.join(client)
      tcp.close(conn)
      task.cancel(next)
      round(listener, port, started, n - 1, served + got)
    }
  }
}

fn main() {
  when let Ok(listener) = tcp.listen("127.0.0.1:0") else { panic("cannot listen") }
  println(round(listener, tcp.local_port(listener), channel.new(0), 1000, 0))
}
"#;
    let (_vm, out) = run(source).expect("the program ended");
    assert_eq!(out, "1000\n");
}

/// A write blocks: the peer reads nothing and the buffers are full.
/// When the deadline around it passes the task goes on, the connection
/// is closed, the thread of the write ends, and a file read afterwards
/// is not held up. (If the deadline passes between two writes, before
/// one has blocked, the next write times out where it begins, with the
/// connection closed all the same: the test does not depend on where
/// the deadline falls.)
#[test]
fn a_blocked_write_ends_when_its_deadline_passes() {
    let (listener, addr) = listener();
    let _peer = silent_peer(listener, 1);
    let path = temp_file("blocked_write.txt", "the file");
    let source = format!(
        r#"
import bytes
import io
import string
import task
import tcp
import time

fn fill(conn, chunk, n) {{
  match n {{
    0 -> Ok(())
    _ -> match tcp.write(conn, chunk) {{
      Ok(_) -> fill(conn, chunk, n - 1)
      Err(e) -> Err(e)
    }}
  }}
}}

fn main() {{
  when let Ok(conn) = tcp.connect("{addr}") else {{ panic("cannot connect") }}
  let chunk = bytes.from_string(string.repeat("0123456789abcdef", 65536))
  match task.deadline(time.ms(300), {{ -> fill(conn, chunk, 256) }}) {{
    Err(tcp.TcpTimeout) -> println("timed out")
    Ok(_) -> println("the peer took everything")
    Err(e) -> println("a write failed: {{e.message()}}")
  }}
  match tcp.write(conn, chunk) {{
    Err(tcp.TcpClosed) -> println("closed")
    _ -> println("something else")
  }}
  when let Ok(text) = io.read_file("{path}") else {{ panic("cannot read the file") }}
  println(text)
}}
"#
    );
    let (vm, out) = run(&source).expect("the program ended");
    assert_eq!(out, "timed out\nclosed\nthe file\n");
    no_thread_is_left_in_an_operation(&vm);
    drop(vm);
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
    no_thread_is_left_in_an_operation(&vm);
    drop(vm);
}
