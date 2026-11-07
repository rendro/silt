//! The numbers a type, a variant and an impl header give their parts are
//! `u16`s: a variant's ordinal and arity, the index of a type's
//! parameter, the index of an impl header's type variable as a decoder
//! numbers it. Past them a declaration is refused with the wording of
//! every compile limit; at them a program runs. (The refusal of 65,537
//! variants is a unit test of the checker; these take seconds in a debug
//! build.)

use std::path::PathBuf;
use std::process::{Command, Output};

/// `silt <command>` of `source`, written to a file of its own.
fn silt(command: &str, name: &str, source: &str) -> Output {
    let dir = std::env::temp_dir().join(format!("silt_type_limits_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory");
    let file: PathBuf = dir.join(format!("{name}.silt"));
    std::fs::write(&file, source).expect("the module is written");
    let output = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg(command)
        .arg(&file)
        .output()
        .expect("silt runs");
    let _ = std::fs::remove_file(&file);
    output
}

/// A refused module: exit status 1, `message` among the errors, and no
/// internal error.
fn assert_refused(output: &Output, message: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains(message), "{stderr}");
    assert!(!stderr.contains("internal error"), "{stderr}");
}

/// 65,536 variants fit, and the last is not the first.
#[test]
fn an_enum_of_65536_variants_runs() {
    let names: Vec<String> = (0..65_536).map(|k| format!("V{k}")).collect();
    let source = format!(
        "type Big {{ {} }}\n\
         fn name(b: Big) -> String {{\n  match b {{\n    V0 -> \"first\"\n    V65535 -> \"last\"\n    _ -> \"other\"\n  }}\n}}\n\
         fn main() {{\n  println(name(V0))\n  println(name(V65535))\n  println(V65535 == V0)\n}}\n",
        names.join(", ")
    );
    let output = silt("run", "variants", &source);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "first\nlast\nfalse\n"
    );
}

/// A variant's arity is a `u16`, and the index of a type parameter is
/// one: past them, the declaration is refused.
#[test]
fn a_variant_of_65536_fields_and_a_type_of_65537_parameters_are_refused() {
    let fields = vec!["Int"; 65_536].join(", ");
    let output = silt(
        "check",
        "fields",
        &format!("type Wide {{ W({fields}) }}\nfn main() {{}}\n"),
    );
    assert_refused(
        &output,
        "too many fields of a variant: 65536 (the limit is 65535)",
    );
    let params: Vec<String> = (0..65_537).map(|k| format!("a{k}")).collect();
    let output = silt(
        "check",
        "params",
        &format!(
            "type Many({}) {{ x: a0 }}\nfn main() {{}}\n",
            params.join(", ")
        ),
    );
    assert_refused(
        &output,
        "too many parameters of a type: 65537 (the limit is 65536)",
    );
}

/// A decoder numbers an impl header's type variables as a type's
/// parameters; a header may hold any number in tuples. An impl of more
/// than 65,536 is refused at the impl, and its use is no internal error.
#[test]
fn an_impl_of_65600_type_variables_is_refused() {
    let n = 65_600;
    let tuple = |xs: &[String]| format!("({})", xs.join(", "));
    let groups = |of: &dyn Fn(usize) -> String| {
        let all: Vec<String> = (0..n).map(of).collect();
        tuple(&all.chunks(255).map(tuple).collect::<Vec<_>>())
    };
    let header = groups(&|k| format!("a{k}"));
    let given = groups(&|_| "Int".to_string());
    let source = format!(
        "import json\ntype Box(a) {{ v: a }}\n\
         trait json.Decode for Box({header}) {{\n\
         \x20 fn decode(value: json.Value) -> Result(Self, json.JsonError) {{ Err(json.Other(\"inst\")) }}\n\
         }}\n\
         fn main() {{ println(json.parse(\"null\", Box({given}))) }}\n"
    );
    let output = silt("check", "impl", &source);
    assert_refused(
        &output,
        "too many type variables of an impl: 65600 (the limit is 65536)",
    );
}

/// A bound on a trait of the program is a parameter of the function,
/// hidden: 65,537 of them, each on a type variable of its own, are far
/// past the 255 a function has, and the checker refuses the function
/// before it reads the signature. Reading it cost memory as the square
/// of its type variables (16,000 took 1.9 GB, 65,537 more than 4 GB);
/// refused, it takes some 120 MB in a debug build, under the limit of
/// 1 GB here.
#[test]
#[cfg(target_os = "linux")]
fn a_function_of_65537_bounds_is_refused_at_little_memory() {
    use std::io::Read;
    use std::process::Stdio;

    let n = 65_537;
    let vars: Vec<String> = (0..n).map(|k| format!("a{k}")).collect();
    let tuple = |xs: &[String]| format!("({})", xs.join(", "));
    let ty = tuple(&vars.chunks(255).map(tuple).collect::<Vec<_>>());
    let bounds: Vec<String> = vars.iter().map(|v| format!("{v}: Tr")).collect();
    let source = format!(
        "trait Tr {{ fn tr(self) -> Int }}\n\
         fn f(x: {ty}) -> Int where {} {{ 1 }}\n\
         fn main() {{}}\n",
        bounds.join(", ")
    );
    let dir = std::env::temp_dir().join(format!("silt_type_limits_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory");
    let file = dir.join("bounds.silt");
    std::fs::write(&file, source).expect("the module is written");
    // (`wait4` below reaps it, for the memory it had.)
    #[allow(clippy::zombie_processes)]
    let mut child = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("check")
        .arg(&file)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("silt runs");
    let mut stderr = String::new();
    (child.stderr.take().expect("silt's errors"))
        .read_to_string(&mut stderr)
        .expect("silt's errors are text");
    // (`wait4`: the most memory the process had resident, in kB.)
    let mut status = 0;
    // SAFETY: an all-zero `rusage` is a valid value of it.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: the pid is of our child, not waited for yet; `status` and
    // `usage` are valid for writes.
    let waited = unsafe { libc::wait4(child.id() as libc::pid_t, &mut status, 0, &mut usage) };
    let _ = std::fs::remove_file(&file);
    assert_eq!(waited, child.id() as libc::pid_t, "{stderr}");
    assert!(libc::WIFEXITED(status), "{stderr}");
    assert_eq!(libc::WEXITSTATUS(status), 1, "{stderr}");
    assert!(
        stderr.contains("too many parameters and bounds of a function: 65538 (the limit is 255)"),
        "{stderr}"
    );
    assert!(!stderr.contains("internal error"), "{stderr}");
    let resident = usage.ru_maxrss;
    assert!(
        resident < 1024 * 1024,
        "a function of 65,537 bounds took {resident} kB to refuse"
    );
}
