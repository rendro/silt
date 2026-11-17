//! `silt init <word>` takes no argument; the line under the error says
//! what to do instead, and a command in it works as it stands, or the
//! advice is said in words. On Windows, where `&&` and `mkdir -p` depend
//! on the shell, it is always words.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A fresh directory holding `dirs` (directories) and `files` (empty).
fn fixture(tag: &str, dirs: &[&str], files: &[&str]) -> PathBuf {
    let root = std::env::temp_dir().join(format!("silt-init-suggest-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let dir = root.join("work");
    std::fs::create_dir_all(&dir).unwrap();
    for d in dirs {
        std::fs::create_dir_all(dir.join(d)).unwrap();
    }
    for f in files {
        std::fs::write(dir.join(f), "").unwrap();
    }
    dir
}

/// The line under `silt init`'s error for `args`, run in `dir`.
fn advice(dir: &Path, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("init")
        .args(args)
        .current_dir(dir)
        .env("NO_COLOR", "1")
        .output()
        .expect("silt runs");
    assert_eq!(out.status.code(), Some(1), "silt init {args:?}");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(!dir.join("silt.toml").exists(), "nothing is written");
    stderr.lines().nth(1).unwrap_or_default().trim().to_string()
}

/// Each kind of argument gets advice that is right for it. FAIL on the
/// base: `sub/dir` was offered a plain `mkdir` (no parent), `..` and an
/// existing directory a `mkdir` of what is there, a file a `mkdir` of its
/// name, `io` a directory whose package name silt refuses, and `--lib`
/// was dropped.
#[test]
fn the_advice_for_init_with_an_argument_fits_the_argument() {
    let dir = fixture("kinds", &["existing", "pkg"], &["afile", "pkg/silt.toml"]);
    let windows = cfg!(windows);
    let cases: [(&[&str], &str, &str); 9] = [
        (
            &["new"],
            "to create a package in a new directory, run: mkdir new && cd new && silt init",
            "to create a package in a new directory, make the directory, go into it and run silt init there",
        ),
        (
            &["sub/dir"],
            "to create a package in a new directory, run: mkdir -p sub/dir && cd sub/dir && silt init",
            "to create a package in a new directory, make the directory, go into it and run silt init there",
        ),
        (
            &["--lib", "new"],
            "to create a package in a new directory, run: mkdir new && cd new && silt init --lib",
            "to create a package in a new directory, make the directory, go into it and run silt init --lib there",
        ),
        (
            &["."],
            "to create the package in this directory, run: silt init",
            "to create the package in this directory, run: silt init",
        ),
        (
            &[".."],
            "'..' is a directory; to create the package in it, run: cd .. && silt init",
            "'..' is a directory; to create the package in it, go into it and run silt init there",
        ),
        (
            &["existing"],
            "'existing' is a directory; to create the package in it, run: cd existing && silt init",
            "'existing' is a directory; to create the package in it, go into it and run silt init there",
        ),
        (
            &["pkg"],
            "'pkg' already holds a silt package (its silt.toml)",
            "'pkg' already holds a silt package (its silt.toml)",
        ),
        (
            &["afile"],
            "'afile' is a file; to create a package in a new directory, make one with another name, go into it and run silt init there",
            "'afile' is a file; to create a package in a new directory, make one with another name, go into it and run silt init there",
        ),
        (
            &["io"],
            "a package is named after its directory, and in a directory 'io' it would be: package name `io` collides with builtin module `io`; make a directory with another name, go into it and run silt init there",
            "a package is named after its directory, and in a directory 'io' it would be: package name `io` collides with builtin module `io`; make a directory with another name, go into it and run silt init there",
        ),
    ];
    for (args, unix, win) in cases {
        let expected = if windows { win } else { unix };
        assert_eq!(advice(&dir, args), expected, "silt init {args:?}");
    }
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
}
