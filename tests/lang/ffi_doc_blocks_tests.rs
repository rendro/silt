//! Every Rust block of docs/ffi.md, compiled (and run where it can
//! run): each test below holds one block as the guide writes it, with
//! what it needs around it. `every_block_of_the_guide_is_here` fails
//! when the guide has a block these tests do not hold, so a block of the
//! guide that does not compile cannot stay. (The copies are made by
//! taking the blocks from the file; change the guide, then the copy.)

#![allow(unused_mut, dead_code, clippy::let_unit_value)]

use std::path::Path;

/// A `Vm` and a program for the blocks that run one.
fn a_vm_and_a_program() -> (silt::Vm, silt::session::Program) {
    use silt::session::{Builtins, Config, Entry, LockPolicy, ProjectSetup, Session};
    let mut session = Session::new(Config {
        project: ProjectSetup::None,
        lock: LockPolicy::ReadOnly,
        host: vec![],
        builtins: Builtins::All,
    });
    let file = session.set_overlay(Path::new("main.silt"), "fn main() { }".to_string());
    let program = session.compile(file, Entry::Main).expect("compiles");
    let out = silt::Buffer::new();
    (silt::Vm::new(silt::HostIo::buffer(&out)), program)
}

#[test]
fn quick_start() {
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use silt::session::{Builtins, Config, Entry, HostModule, LockPolicy, ProjectSetup, Session};
    use silt::{Buffer, HostIo, Vm};

    // 1. Declare the host module: each function by its silt signature. A
    //    program's `main` returns nothing: it hands its result to the host
    //    through a function of the host's, `mylib.give` here.
    let given = Arc::new(Mutex::new(0));
    let slot = given.clone();
    let mylib = HostModule::new("mylib")
        .fn1("fn double(x: Int) -> Int", |x: i64| x * 2)
        .fn1("fn give(x: Int) -> ()", move |x: i64| *slot.lock().unwrap() = x);

    // 2. Open the program in a session that knows the module.
    let mut session = Session::new(Config {
        project: ProjectSetup::None,
        lock: LockPolicy::ReadOnly,
        host: vec![mylib],
        builtins: Builtins::All,
    });
    let source = "import mylib\nfn main() { mylib.give(mylib.double(21)) }";
    let file = session.set_overlay(Path::new("main.silt"), source.to_string());

    // 3. Check it. Every static error is in the analysis.
    let analysis = session.analyze(file);
    for d in &analysis.diagnostics {
        eprintln!("{}", d.message);
    }
    assert!(!analysis.has_errors());

    // 4. Compile it and run it. What the program prints is collected in `out`.
    let program = session.compile(file, Entry::Main).expect("compiles");
    let out = Buffer::new();
    Vm::new(HostIo::buffer(&out)).run_program(&program).unwrap();
    assert_eq!(*given.lock().unwrap(), 42);
}

#[test]
fn declaring_a_host_module() {
    use silt::session::HostModule;
    let mylib = HostModule::new("mylib")
        .fn0("fn answer() -> Int", || 42_i64)
        .fn1("fn double(x: Int) -> Int", |x: i64| x * 2)
        .fn2("fn add(a: Int, b: Int) -> Int", |a: i64, b: i64| a + b);
    let _ = mylib;
}

#[test]
fn functions_on_values() {
    use silt::session::{HostCall, HostModule};
    use silt::{IntoValue, Value, VmError};
    let lists = HostModule::new("lists").function(
        "fn first(xs: List(a)) -> Option(a)",
        |call: &HostCall| {
            let Value::List(xs) = &call.args[0] else {
                return Err(VmError::new("expected a list".into()));
            };
            xs.first().into_value().map_err(VmError::new)
        },
    );
    let _ = lists;
}

#[test]
fn host_types() {
    use silt::session::{HostCall, HostModule};
    use silt::{Value, VmError};

    // The field `name` of the record `value`, an Int.
    fn int(value: &Value, name: &str) -> Result<i64, VmError> {
        match value {
            Value::Record(record) => match record.get(name) {
                Some(Value::Int(n)) => Ok(*n),
                _ => Err(VmError::new(format!("no Int field `{name}`"))),
            },
            _ => Err(VmError::new("expected a record".into())),
        }
    }

    let geo = HostModule::new("geo")
        .declare_type("type Point { x: Int, y: Int }")
        .declare_type("type Shape { Circle(Int), Rect(Point, Point) }")
        .function("fn origin() -> Point", |call: &HostCall| {
            call.record("Point", [("x", Value::Int(0)), ("y", Value::Int(0))])
        })
        .function("fn norm1(p: Point) -> Int", |call: &HostCall| {
            let p = &call.args[0];
            Ok(Value::Int(int(p, "x")?.abs() + int(p, "y")?.abs()))
        })
        .function("fn circle(r: Int) -> Shape", |call: &HostCall| {
            call.variant("Shape", "Circle", vec![call.args[0].clone()])
        });
    let _ = geo;
}

#[test]
fn supported_types() {
    use silt::session::HostModule;
    let users = HostModule::new("users")
        .fn1("fn find(id: Int) -> Option(String)", |id: i64| {
            if id == 1 { Some("alice".to_string()) } else { None }
        })
        .fn1("fn parse_int(s: String) -> Result(Int, String)", |s: String| {
            s.parse::<i64>().map_err(|e| e.to_string())
        });
    let _ = users;
}

#[test]
fn output_and_clock_vm() {
    use silt::{Buffer, HostIo, Vm};
    let out = Buffer::new();
    let vm = Vm::new(HostIo::buffer(&out));
    let _ = vm;
}

#[test]
fn an_output_of_ones_own() {
    use silt::Output;
    struct Lines(std::sync::mpsc::Sender<String>);

    impl Output for Lines {
        fn write(&self, text: &str) -> std::io::Result<()> {
            self.0.send(text.to_string()).map_err(std::io::Error::other)
        }
    }
    let (send, _keep) = std::sync::mpsc::channel();
    let _ = Lines(send).write("x");
}

#[test]
fn the_clock_trait() {
    use std::time::Duration;
    pub trait Clock: Send + Sync {
        /// The time since the Unix epoch.
        fn now(&self) -> Duration;
        /// The time since some fixed moment; it never goes back.
        fn monotonic(&self) -> Duration;
        /// Block the calling thread until `duration` has passed.
        fn sleep(&self, duration: Duration);
    }
}

#[test]
fn a_clock_of_ones_own() {
    use std::path::Path;
    use std::time::Duration;

    use silt::session::{Builtins, Config, Entry, LockPolicy, ProjectSetup, Session};
    use silt::{Buffer, Clock, HostIo, Vm};

    // A clock that starts at a fixed time and moves only when the
    // program sleeps.
    struct Simulated(std::sync::Mutex<Duration>);

    impl Clock for Simulated {
        fn now(&self) -> Duration {
            // 2026-10-05T12:00:00Z, plus what has passed.
            Duration::from_secs(1_791_201_600) + self.monotonic()
        }
        fn monotonic(&self) -> Duration {
            *self.0.lock().unwrap()
        }
        fn sleep(&self, duration: Duration) {
            *self.0.lock().unwrap() += duration;
        }
    }

    let mut session = Session::new(Config {
        project: ProjectSetup::None,
        lock: LockPolicy::ReadOnly,
        host: vec![],
        builtins: Builtins::All,
    });
    let source = r#"
    import time
    import result
    fn main() {
      println("started")
      time.sleep(time.minutes(90))
      println(time.now() |> time.to_datetime(0) |> time.format("%H:%M") |> result.unwrap_or(""))
    }
    "#;
    let file = session.set_overlay(Path::new("main.silt"), source.to_string());
    let program = session.compile(file, Entry::Main).expect("compiles");

    // The program's output is collected in `out`; it reads `Simulated`.
    let out = Buffer::new();
    let io = HostIo::buffer(&out).clock(Simulated(Default::default()));
    Vm::new(io).run_program(&program).unwrap();
    assert_eq!(out.contents(), "started\n13:30\n");
}

#[test]
fn process_exit() {
    let (mut vm, program) = a_vm_and_a_program();
    match vm.run_program(&program) {
        Err(e) if e.exit.is_some() => println!("exit code {:?}", e.exit),
        Err(e) => eprintln!("silt error: {}", e.message),
        Ok(()) => {}
    }
}

#[test]
fn builtin_modules() {
    use silt::session::{Builtins, Config, LockPolicy, ProjectSetup, Session};
    let session = Session::new(Config {
        project: ProjectSetup::None,
        lock: LockPolicy::ReadOnly,
        host: vec![],
        builtins: Builtins::Only(vec!["list".into(), "string".into(), "math".into()]),
    });
    let _ = session;
}

#[test]
fn error_surfacing() {
    let (mut vm, program) = a_vm_and_a_program();
    match vm.run_program(&program) {
        Ok(()) => println!("the program ran to its end"),
        Err(e) => eprintln!("silt error: {}", e.message),
    }
}

/// The lines of `text` that are not blank, without their indentation.
fn lines(text: &str) -> Vec<&str> {
    (text.lines())
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect()
}

#[test]
fn every_block_of_the_guide_is_here() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let guide = std::fs::read_to_string(dir.join("docs/ffi.md")).expect("the guide is read");
    let here = std::fs::read_to_string(dir.join("tests/lang/ffi_doc_blocks_tests.rs"))
        .expect("this file is read");
    let here = lines(&here);
    let mut blocks = Vec::new();
    let mut block: Option<String> = None;
    for line in guide.lines() {
        match (&mut block, line.trim_end()) {
            (None, "```rust") => block = Some(String::new()),
            (Some(text), "```") => {
                blocks.push(std::mem::take(text));
                block = None;
            }
            (Some(text), line) => {
                text.push_str(line);
                text.push('\n');
            }
            (None, _) => {}
        }
    }
    assert!(blocks.len() >= 12, "{} blocks", blocks.len());
    let missing: Vec<&String> = (blocks.iter())
        .filter(|block| {
            let block = lines(block);
            !here.windows(block.len()).any(|window| window == block.as_slice())
        })
        .collect();
    assert!(
        missing.is_empty(),
        "blocks of docs/ffi.md that no test here compiles (copy each into a test): {missing:#?}"
    );
}
