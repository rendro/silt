//! Behavioural locks for install.sh's branded failure handling
//! (rounds 79, 80 and 87).
//!
//! Every step of the installer that can fail — the temp-dir creation,
//! the binary and SHA256SUMS downloads, the archive extraction and the
//! install-dir `mkdir` / `cp` / `chmod` — must chain `|| err "..."` so a
//! failure surfaces as a branded `error: <msg>` line instead of a bare
//! `set -eu` exit with a raw tool error.
//!
//! The tests run the real script under `sh` with a stub directory at
//! the front of `PATH`. The stub `curl` serves a fake release (a
//! `Location:` header for the version lookup, an archive, and a
//! matching SHA256SUMS file) without touching the network; each test
//! then makes one step fail — by failing that download, or by adding a
//! stub for the tool that exits 1 — and checks the branded message.
//! A happy-path test proves the stubs themselves install cleanly.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// The stub `curl`. It only uses shell builtins, `cat` and `awk`, plus a
/// SHA-256 tool, so a test that stubs `cp`, `mkdir` or `tar` to fail
/// does not break the stub itself.
const STUB_CURL: &str = r#"#!/bin/sh
case "$1" in
    -fsI)
        printf 'HTTP/2 302\r\nlocation: https://github.com/rendro/silt/releases/tag/v9.9.9\r\n\r\n'
        exit 0
        ;;
esac
url="$2"
dest="$4"
case "$url" in
    *"$STUB_FAIL_FETCH"*)
        [ -n "$STUB_FAIL_FETCH" ] && exit 22
        ;;
esac
case "$url" in
    *SHA256SUMS)
        asset="$(cat "$STUB_STATE/asset")"
        if command -v sha256sum > /dev/null 2>&1; then
            hash="$(sha256sum "$STUB_ASSET" | awk '{print $1}')"
        else
            hash="$(shasum -a 256 "$STUB_ASSET" | awk '{print $1}')"
        fi
        printf '%s  %s\n' "$hash" "$asset" > "$dest"
        ;;
    *)
        printf '%s' "${url##*/}" > "$STUB_STATE/asset"
        cat "$STUB_ASSET" > "$dest"
        ;;
esac
"#;

/// A stub `uname` that reports a Windows (MSYS) host, so the script takes
/// its `.zip` / `unzip` branch.
const STUB_UNAME_WINDOWS: &str =
    "#!/bin/sh\ncase \"$1\" in\n    -s) echo MINGW64_NT-10.0 ;;\n    -m) echo x86_64 ;;\nesac\n";

struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let root =
            std::env::temp_dir().join(format!("silt_install_sh_{tag}_{}_{n}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for sub in ["stubs", "state", "home", "asset"] {
            fs::create_dir_all(root.join(sub)).unwrap();
        }
        let sandbox = Sandbox { root };
        sandbox.stub("curl", STUB_CURL);
        // A real release archive holding a `silt` binary, so the steps
        // after extraction have something to install.
        fs::write(
            sandbox.root.join("asset/silt"),
            "#!/bin/sh\necho stub silt\n",
        )
        .unwrap();
        let status = Command::new("tar")
            .args(["czf", "release.tar.gz", "-C", "asset", "silt"])
            .current_dir(&sandbox.root)
            .status()
            .expect("run tar to build the fake release");
        assert!(status.success(), "building the fake release archive failed");
        sandbox
    }

    fn stub(&self, name: &str, body: &str) {
        let path = self.root.join("stubs").join(name);
        fs::write(&path, body).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// Replace `tool` with a stub that always fails.
    fn fail_tool(&self, tool: &str) {
        self.stub(tool, "#!/bin/sh\nexit 1\n");
    }

    fn install_dir(&self) -> PathBuf {
        self.root.join("home/bin")
    }

    fn run(&self, fail_fetch: &str) -> Output {
        let install_sh = Path::new(env!("CARGO_MANIFEST_DIR")).join("install.sh");
        let path = format!(
            "{}:{}",
            self.root.join("stubs").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        Command::new("sh")
            .arg(&install_sh)
            .env("PATH", path)
            .env("HOME", self.root.join("home"))
            .env("SILT_INSTALL_DIR", self.install_dir())
            .env("STUB_STATE", self.root.join("state"))
            .env("STUB_ASSET", self.root.join("release.tar.gz"))
            .env("STUB_FAIL_FETCH", fail_fetch)
            .output()
            .expect("run install.sh")
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// The run must exit 1 with the branded `error: <expected>` line.
fn assert_branded_failure(out: &Output, expected: &str) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(1),
        "install.sh should exit 1; stdout={}\nstderr={stderr}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        stderr.contains(&format!("  error: {expected}")),
        "expected the branded error {expected:?}; stderr:\n{stderr}"
    );
}

#[test]
fn install_sh_happy_path_installs_with_the_stubs() {
    let sb = Sandbox::new("happy");
    let out = sb.run("");
    assert!(
        out.status.success(),
        "install.sh failed under the stubs; stdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(sb.install_dir().join("silt").is_file());
}

#[test]
fn install_sh_mktemp_failure_is_branded() {
    let sb = Sandbox::new("mktemp");
    sb.fail_tool("mktemp");
    assert_branded_failure(&sb.run(""), "mktemp -d failed");
}

#[test]
fn install_sh_binary_fetch_failure_is_branded() {
    let sb = Sandbox::new("binary_fetch");
    assert_branded_failure(&sb.run(".tar.gz"), "failed to download binary from");
}

#[test]
fn install_sh_sha256sums_fetch_failure_is_branded() {
    let sb = Sandbox::new("sums_fetch");
    assert_branded_failure(&sb.run("SHA256SUMS"), "failed to download SHA256SUMS");
}

#[test]
fn install_sh_tar_extract_failure_is_branded() {
    let sb = Sandbox::new("tar");
    sb.fail_tool("tar");
    assert_branded_failure(&sb.run(""), "failed to extract archive (tar");
}

#[test]
fn install_sh_unzip_extract_failure_is_branded() {
    let sb = Sandbox::new("unzip");
    sb.stub("uname", STUB_UNAME_WINDOWS);
    sb.fail_tool("unzip");
    assert_branded_failure(&sb.run(""), "failed to extract archive (unzip");
}

#[test]
fn install_sh_mkdir_install_dir_failure_is_branded() {
    let sb = Sandbox::new("mkdir");
    sb.fail_tool("mkdir");
    assert_branded_failure(&sb.run(""), "failed to create install dir");
}

#[test]
fn install_sh_cp_binary_failure_is_branded() {
    let sb = Sandbox::new("cp");
    sb.fail_tool("cp");
    assert_branded_failure(&sb.run(""), "failed to install binary to");
}

#[test]
fn install_sh_chmod_binary_failure_is_branded() {
    let sb = Sandbox::new("chmod");
    sb.fail_tool("chmod");
    assert_branded_failure(&sb.run(""), "failed to mark");
}
