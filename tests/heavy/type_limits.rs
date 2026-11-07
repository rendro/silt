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

/// `silt check` of `source`, written to a file of its own: its exit
/// status, its errors, the most memory it had resident (kB) and how long
/// it took.
#[cfg(target_os = "linux")]
fn check_measured(name: &str, source: &str) -> (i32, String, i64, std::time::Duration) {
    use std::io::Read;
    use std::process::Stdio;

    let dir = std::env::temp_dir().join(format!("silt_type_limits_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory");
    let file = dir.join(format!("{name}.silt"));
    std::fs::write(&file, source).expect("the module is written");
    let started = std::time::Instant::now();
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
    let mut status = 0;
    // SAFETY: an all-zero `rusage` is a valid value of it.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: the pid is of our child, not waited for yet; `status` and
    // `usage` are valid for writes.
    let waited = unsafe { libc::wait4(child.id() as libc::pid_t, &mut status, 0, &mut usage) };
    let took = started.elapsed();
    let _ = std::fs::remove_file(&file);
    assert_eq!(waited, child.id() as libc::pid_t, "{stderr}");
    assert!(libc::WIFEXITED(status), "{stderr}");
    assert!(!stderr.contains("internal error"), "{stderr}");
    (libc::WEXITSTATUS(status), stderr, usage.ru_maxrss, took)
}

/// The parameter `x: ((a0, .., a254), ..)` of `n` type variables.
#[cfg(target_os = "linux")]
fn many_variables(n: usize) -> (Vec<String>, String) {
    let vars: Vec<String> = (0..n).map(|k| format!("a{k}")).collect();
    let tuple = |xs: &[String]| format!("({})", xs.join(", "));
    let ty = tuple(&vars.chunks(255).map(tuple).collect::<Vec<_>>());
    (vars, ty)
}

/// A bound on a trait of the program is a parameter of the function,
/// hidden: 65,537 of them, each on a type variable of its own, are far
/// past the 255 a function has, and the checker refuses the function.
/// Reading its signature cost memory as the square of its type
/// variables (16,000 took 1.9 GB, 65,537 more than 4 GB); it takes some
/// 160 MB in a debug build now, under the limit of 1 GB here.
#[test]
#[cfg(target_os = "linux")]
fn a_function_of_65537_bounds_is_refused_at_little_memory() {
    let (vars, ty) = many_variables(65_537);
    let bounds: Vec<String> = vars.iter().map(|v| format!("{v}: Tr")).collect();
    let source = format!(
        "trait Tr {{ fn tr(self) -> Int }}\n\
         fn f(x: {ty}) -> Int where {} {{ 1 }}\n\
         fn main() {{}}\n",
        bounds.join(", ")
    );
    let (code, stderr, resident, _) = check_measured("bounds", &source);
    assert_eq!(code, 1, "{stderr}");
    assert!(
        stderr.contains("too many parameters and bounds of a function: 65538 (the limit is 255)"),
        "{stderr}"
    );
    assert!(
        resident < 1024 * 1024,
        "a function of 65,537 bounds took {resident} kB to refuse"
    );
}

/// A signature of 65,537 type variables and no bound is read in time
/// and memory in proportion to them: it took 147 s in a debug build,
/// comparing each variable with all, and takes some 2.5 s and 150 MB.
/// (The limit of 60 s is far from both.)
#[test]
#[cfg(target_os = "linux")]
fn a_signature_of_65537_type_variables_is_read_in_linear_time() {
    let (_, ty) = many_variables(65_537);
    let source = format!("fn f(x: {ty}) -> Int {{ 1 }}\nfn main() {{}}\n");
    let (code, stderr, resident, took) = check_measured("variables", &source);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        resident < 1024 * 1024,
        "a signature of 65,537 type variables took {resident} kB"
    );
    assert!(
        took < std::time::Duration::from_secs(60),
        "a signature of 65,537 type variables took {took:?}"
    );
}

/// The names of a type's, a trait's and an impl head's type variables are
/// told apart in a set, in time in proportion to them: a type of 65,537
/// parameters took 10.9 s in a debug build to be refused, comparing each
/// with all before it, and an impl head of 65,537 binders and one
/// written twice 35.5 s; each takes under a second now. (The limits of
/// 10 s and 1 GB are far from both.)
#[test]
#[cfg(target_os = "linux")]
fn the_type_variables_of_a_declaration_are_told_apart_in_linear_time() {
    let (vars, ty) = many_variables(65_537);
    let twice = format!("{}, a0", vars.join(", "));
    let cases = [
        (
            "type_parameters",
            format!(
                "type Many({}) {{ x: a0 }}\nfn main() {{}}\n",
                vars.join(", ")
            ),
            "too many parameters of a type: 65537 (the limit is 65536)",
        ),
        (
            "type_parameter_twice",
            format!("type Many({twice}) {{ x: a0 }}\nfn main() {{}}\n"),
            "duplicate type variable 'a0' in type declaration",
        ),
        (
            "trait_parameter_twice",
            format!("trait Many({twice}) {{ fn m(self) -> Int }}\nfn main() {{}}\n"),
            "duplicate type variable 'a0' in trait declaration",
        ),
        (
            "impl_binder_twice",
            format!(
                "trait Tr {{ fn tr(self) -> Int }}\ntype Box(a) {{ v: a }}\n\
                 trait Tr for Box(({ty}, a0)) {{ fn tr(self) -> Int {{ 1 }} }}\nfn main() {{}}\n"
            ),
            "duplicate type variable 'a0' in impl target",
        ),
    ];
    for (name, source, message) in cases {
        let (code, stderr, resident, took) = check_measured(name, &source);
        assert_eq!(code, 1, "{name}: {stderr}");
        assert!(stderr.contains(message), "{name}: {stderr}");
        assert!(
            resident < 1024 * 1024,
            "{name} took {resident} kB to refuse"
        );
        assert!(
            took < std::time::Duration::from_secs(10),
            "{name} took {took:?} to refuse"
        );
    }
}

/// The source of a trait `Go` whose method `go` takes `x: {ty}` with the
/// bounds `bounds`, with a default body or an impl for `Box` that writes
/// the same.
#[cfg(target_os = "linux")]
fn method_of_bounds(ty: &str, bounds: &str, default: bool) -> String {
    let tr = "trait Tr {\n  fn tr(self) -> Int\n}\n\ntrait Tr for Int {\n  fn tr(self) -> Int { self + 1 }\n}\n";
    let main = "fn main() {\n  println(1)\n}\n";
    match default {
        true => format!(
            "{tr}\ntrait Go {{\n  fn go(self, x: {ty}) -> Int where {bounds} {{ 0 }}\n}}\n\n{main}"
        ),
        false => format!(
            "{tr}\ntrait Go {{\n  fn go(self, x: {ty}) -> Int where {bounds}\n}}\n\n\
             type Box {{ n: Int }}\n\n\
             trait Go for Box {{\n  fn go(self, x: {ty}) -> Int where {bounds} {{ 0 }}\n}}\n\n{main}"
        ),
    }
}

/// A method's bounds are its parameters too: an impl's method and a
/// trait's default method of 65,537 bounds are refused by the checker,
/// with the 255 of a function, before it compares each bound the impl
/// writes with all the trait declares (16,384 on one variable took
/// 129 s), in seconds and at little memory.
#[test]
#[cfg(target_os = "linux")]
fn methods_of_65537_bounds_are_refused_at_little_memory() {
    let (vars, ty) = many_variables(65_537);
    let distinct: Vec<String> = vars.iter().map(|v| format!("{v}: Tr")).collect();
    let one = format!("a: {}", vec!["Tr"; 65_537].join(" + "));
    let cases = [
        ("impl_one_variable", method_of_bounds("a", &one, false)),
        (
            "impl_variables",
            method_of_bounds(&ty, &distinct.join(", "), false),
        ),
        (
            "default_variables",
            method_of_bounds(&ty, &distinct.join(", "), true),
        ),
    ];
    for (name, source) in cases {
        let (code, stderr, resident, took) = check_measured(name, &source);
        assert_eq!(code, 1, "{name}: {stderr}");
        assert!(
            stderr
                .contains("too many parameters and bounds of a function: 65539 (the limit is 255)"),
            "{name}: {stderr}"
        );
        assert!(
            !stderr.contains("too many bounds of a definition"),
            "{name}: {stderr}"
        );
        assert!(
            resident < 1024 * 1024,
            "{name} took {resident} kB to refuse"
        );
        assert!(
            took < std::time::Duration::from_secs(60),
            "{name} took {took:?} to refuse"
        );
    }
}
