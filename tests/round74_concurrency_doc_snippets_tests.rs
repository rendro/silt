//! Round-74 DOC fix #1 + #2: corrected wildcard-select and
//! variable-binding-select snippets in `docs/concurrency.md`
//! type-check.
//!
//! - Round-74 finding #1: the wildcard-select snippet at
//!   `docs/concurrency.md:427-430` originally had only 2 arms over the
//!   4-variant `(channel, ChannelResult(a))` tuple. The corrected
//!   snippet adds a final `_` catch-all so exhaustiveness passes.
//! - Round-74 finding #2: the variable-binding-select snippet at
//!   `docs/concurrency.md:437-441` originally interpolated `{source}`
//!   (a `Channel(_)` value, which has no `Display` impl) and was
//!   non-exhaustive. The corrected snippet drops the `{source}`
//!   interpolation and adds the missing `_` arm.
//!
//! Both snippets are pinned via include_str! source-grep against the
//! committed file; the lifted forms are then run through `silt check`
//! to prove they typecheck.

use std::path::{Path, PathBuf};
use std::process::Command;

const CONCURRENCY_DOC_SRC: &str = include_str!("../docs/concurrency.md");

fn silt_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

fn scratch_dir(suffix: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("silt_round74_{suffix}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_main(dir: &Path, src: &str) -> PathBuf {
    let path = dir.join("main.silt");
    std::fs::write(&path, src).unwrap();
    path
}

fn check_ok(dir_suffix: &str, src: &str) {
    let dir = scratch_dir(dir_suffix);
    let main = write_main(&dir, src);
    let output = silt_bin()
        .args(["check", main.to_str().unwrap()])
        .output()
        .expect("silt check");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "silt check should succeed for {dir_suffix} snippet.\n\
         stdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

#[test]
fn finding1_wildcard_select_snippet_present_with_catchall() {
    // Pin the corrected snippet by source-grep so a regression that
    // drops the catch-all is caught at the doc level.
    let needle = "match channel.select([Recv(ch1), Recv(ch2)]) {\n  (_, Message(val)) -> println(\"got {val} from somewhere\")\n  (_, Closed)       -> println(\"all done\")\n  _                 -> ()\n}";
    assert!(
        CONCURRENCY_DOC_SRC.contains(needle),
        "concurrency.md wildcard-select snippet must include the final `_` \
         catch-all arm (covers Sent / Empty for exhaustiveness)."
    );
}

#[test]
fn finding1_wildcard_select_snippet_typechecks() {
    // Lift the corrected snippet into a runnable program and assert it
    // type-checks.
    let src = r#"
import channel

fn main() {
  let ch1 = channel.new(1)
  let ch2 = channel.new(1)
  match channel.select([Recv(ch1), Recv(ch2)]) {
    (_, Message(val)) -> println("got {val} from somewhere")
    (_, Closed)       -> println("all done")
    _                 -> ()
  }
}
"#;
    check_ok("finding1_wildcard", src);
}

#[test]
fn finding2_variable_binding_select_snippet_drops_source_interpolation() {
    // Old broken snippet interpolated `{source}` (a Channel value with
    // no Display impl). The corrected snippet drops it.
    assert!(
        !CONCURRENCY_DOC_SRC.contains("got {val} from channel {source}"),
        "concurrency.md must not interpolate `{{source}}` (Channel values \
         do not auto-derive Display)."
    );
    // And the prose must call out the underlying reason so a future
    // editor doesn't reintroduce the same bug.
    assert!(
        CONCURRENCY_DOC_SRC.contains("Channels\ndo not auto-derive `Display`")
            || CONCURRENCY_DOC_SRC.contains("Channels do not auto-derive `Display`")
            || CONCURRENCY_DOC_SRC.contains("do not auto-derive `Display`"),
        "concurrency.md should explain why the channel itself isn't \
         interpolated (no auto-derived Display)."
    );
}

#[test]
fn finding2_variable_binding_select_snippet_typechecks() {
    let src = r#"
import channel

fn main() {
  let ch1 = channel.new(1)
  let ch2 = channel.new(1)
  match channel.select([Recv(ch1), Recv(ch2)]) {
    (_source, Message(val)) -> println("got {val}")
    (_, Closed)             -> println("all done")
    _                       -> ()
  }
}
"#;
    check_ok("finding2_var_bind", src);
}
