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
/// does not break the stub itself. Each call's arguments are written to
/// `$STUB_STATE/curl.log`, one call a line.
const STUB_CURL: &str = r#"#!/bin/sh
echo "$*" >> "$STUB_STATE/curl.log"
url=""
dest=""
head=""
while [ $# -gt 0 ]; do
    case "$1" in
        -fsI) head=1 ;;
        -o) shift; dest="$1" ;;
        https://*|http://*) url="$1" ;;
    esac
    shift
done
if [ -n "$head" ]; then
    printf 'HTTP/2 302\r\nlocation: https://github.com/rendro/silt/releases/tag/v9.9.9\r\n\r\n'
    exit 0
fi
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

/// A stub `gh` that is not logged in.
const STUB_GH_LOGGED_OUT: &str = "#!/bin/sh\nexit 1\n";

/// A stub `gh` that is logged in to github.com and whose `attestation
/// verify` exits with `$STUB_GH_VERIFY` (0 when unset), saying so; its
/// arguments go to `$STUB_STATE/gh.log`.
const STUB_GH: &str = r#"#!/bin/sh
echo "$*" >> "$STUB_STATE/gh.log"
case "$1 $2" in
    "attestation --help") exit 0 ;;
    "auth status") [ "$3 $4" = "--hostname github.com" ] || exit 8; exit 0 ;;
    "attestation verify")
        if [ "${STUB_GH_VERIFY:-0}" != 0 ]; then
            echo "stub gh: no attestation matches"
        fi
        exit "${STUB_GH_VERIFY:-0}"
        ;;
esac
exit 1
"#;

/// A stub `gh` as Ubuntu 24.04 ships it (2.45): logged in, but without
/// the `attestation` command, which came in 2.49.
const STUB_GH_WITHOUT_ATTESTATION: &str = r#"#!/bin/sh
echo "$*" >> "$STUB_STATE/gh.log"
case "$1 $2" in
    "auth status") exit 0 ;;
esac
echo "unknown command \"$1\" for \"gh\"" >&2
exit 1
"#;

/// The stub `wget`, for a machine without curl. `--help` names
/// `--max-redirect`, as GNU wget's does. The version lookup (`--spider`)
/// is answered with a redirect to the tag; a download from github.com
/// with a redirect to `$STUB_WGET_TO` followed by the file's name, as
/// GitHub sends a release asset to its storage, unless the URL has a
/// directory `via/`; any other URL is served as the stub curl serves it.
/// Each call's arguments go to `$STUB_STATE/wget.log`.
const STUB_WGET: &str = r#"#!/bin/sh
echo "$*" >> "$STUB_STATE/wget.log"
if [ "$1" = --help ]; then
    echo "       --max-redirect=NUMBER       maximum redirections allowed per page"
    exit 0
fi
url=""
dest=""
spider=""
while [ $# -gt 0 ]; do
    case "$1" in
        --spider) spider=1 ;;
        -O) shift; dest="$1" ;;
        https://*|http://*) url="$1" ;;
    esac
    shift
done
if [ -n "$spider" ]; then
    printf '  HTTP/1.1 302 Found\r\n  Location: https://github.com/rendro/silt/releases/tag/v9.9.9\r\n' >&2
    exit 8
fi
case "$url" in
    */via/*) ;;
    https://github.com/*)
        printf '  HTTP/1.1 302 Found\r\n  Location: %s%s\r\nLocation: %s%s [following]\n0 redirections exceeded.\n' \
            "$STUB_WGET_TO" "${url##*/}" "$STUB_WGET_TO" "${url##*/}" >&2
        : > "$dest"
        exit 8
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
        // The GitHub CLI of the machine must not take part: by default a
        // `gh` that is not logged in, so the attestation is not checked.
        sandbox.stub("gh", STUB_GH_LOGGED_OUT);
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
        self.run_with(fail_fetch, &[])
    }

    fn run_with(&self, fail_fetch: &str, env: &[(&str, &str)]) -> Output {
        let path = format!(
            "{}:{}",
            self.root.join("stubs").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        self.run_on_path(&path, fail_fetch, env)
    }

    /// Run with the stub `wget` and no curl: PATH is the stubs and a
    /// directory of links to the machine's tools that install.sh uses,
    /// curl not among them.
    fn run_with_wget(&self, env: &[(&str, &str)]) -> Output {
        let path = self.wget_path();
        self.run_on_path(&path, "", env)
    }

    /// The PATH of [`Sandbox::run_with_wget`], with the stub `wget` in
    /// place and curl gone.
    fn wget_path(&self) -> String {
        self.stub("wget", STUB_WGET);
        let _ = fs::remove_file(self.root.join("stubs/curl"));
        let tools = self.root.join("tools");
        fs::create_dir_all(&tools).unwrap();
        let path_var = std::env::var("PATH").unwrap_or_default();
        for tool in [
            "sh",
            "uname",
            "mktemp",
            "rm",
            "awk",
            "tar",
            "gzip",
            "cp",
            "mkdir",
            "chmod",
            "mv",
            "sha256sum",
            "shasum",
            "tr",
            "grep",
            "cat",
            "sed",
            "head",
        ] {
            let found = std::env::split_paths(&path_var)
                .map(|dir| dir.join(tool))
                .find(|candidate| candidate.is_file());
            if let Some(found) = found {
                let _ = std::os::unix::fs::symlink(found, tools.join(tool));
            }
        }
        format!("{}:{}", self.root.join("stubs").display(), tools.display())
    }

    fn run_on_path(&self, path: &str, fail_fetch: &str, env: &[(&str, &str)]) -> Output {
        self.run_under_umask(None, path, fail_fetch, env)
    }

    /// Run with the stubs, under the umask `umask` when one is given.
    fn run_under_umask(
        &self,
        umask: Option<&str>,
        path: &str,
        fail_fetch: &str,
        env: &[(&str, &str)],
    ) -> Output {
        let install_sh = Path::new(env!("CARGO_MANIFEST_DIR")).join("install.sh");
        let mut command = Command::new("sh");
        if let Some(umask) = umask {
            command.args(["-c", &format!("umask {umask}; exec sh \"$0\"")]);
        }
        command
            .arg(&install_sh)
            .env("PATH", path)
            .env("HOME", self.root.join("home"))
            .env("SILT_INSTALL_DIR", self.install_dir())
            .env("STUB_STATE", self.root.join("state"))
            .env("STUB_ASSET", self.root.join("release.tar.gz"))
            .env("STUB_FAIL_FETCH", fail_fetch)
            .envs(env.iter().copied())
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

#[test]
fn install_sh_downloads_only_over_https() {
    // Every curl call refuses a protocol but https, a redirect to http
    // included, and a TLS older than 1.2.
    let sb = Sandbox::new("https_only");
    let out = sb.run("");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let log = fs::read_to_string(sb.root.join("state/curl.log")).unwrap();
    assert_eq!(
        log.lines().count(),
        3,
        "the version, the archive, the sums:\n{log}"
    );
    for call in log.lines() {
        assert!(
            call.starts_with("--proto =https --tlsv1.2 "),
            "a curl call without --proto =https --tlsv1.2: {call}"
        );
    }
}

#[test]
fn install_sh_without_a_logged_in_gh_says_the_attestation_is_not_checked() {
    let sb = Sandbox::new("attest_unchecked");
    let out = sb.run("");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains(
            "Build attestation not checked: that needs gh 2.49 or later, logged in to github.com"
        ),
        "{stdout}"
    );
    assert!(sb.install_dir().join("silt").is_file());
}

#[test]
fn install_sh_with_a_logged_in_gh_verifies_the_attestation() {
    let sb = Sandbox::new("attest_ok");
    sb.stub("gh", STUB_GH);
    let out = sb.run("");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("Verified the build attestation"),
        "{stdout}"
    );
    let log = fs::read_to_string(sb.root.join("state/gh.log")).unwrap();
    assert!(
        log.lines()
            .any(|call| call.starts_with("attestation verify ")
                && call.ends_with(" --repo rendro/silt")),
        "{log}"
    );
    assert!(sb.install_dir().join("silt").is_file());
}

#[test]
fn install_sh_refuses_an_archive_whose_attestation_does_not_verify() {
    let sb = Sandbox::new("attest_bad");
    sb.stub("gh", STUB_GH);
    let out = sb.run_with("", &[("STUB_GH_VERIFY", "1")]);
    assert_branded_failure(&out, "the build attestation of silt-v9.9.9-");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("stub gh: no attestation matches"),
        "{stderr}"
    );
    assert!(!sb.install_dir().join("silt").exists());
}

#[test]
fn install_sh_with_a_gh_that_has_no_attestation_command_installs_unchecked() {
    // Ubuntu 24.04's gh (2.45) is logged in but has no `attestation`:
    // the genuine release installs, the check is said to be skipped.
    let sb = Sandbox::new("attest_old_gh");
    sb.stub("gh", STUB_GH_WITHOUT_ATTESTATION);
    let out = sb.run("");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("Build attestation not checked"), "{stdout}");
    assert!(sb.install_dir().join("silt").is_file());
}

#[test]
fn install_sh_with_wget_follows_a_redirect_to_https() {
    let sb = Sandbox::new("wget_https");
    let out = sb.run_with_wget(&[("STUB_WGET_TO", "https://release-assets.example/")]);
    assert!(
        out.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(sb.install_dir().join("silt").is_file());
    let log = fs::read_to_string(sb.root.join("state/wget.log")).unwrap();
    assert!(
        log.lines().any(
            |call| call.ends_with("https://release-assets.example/SHA256SUMS")
                || call.contains("https://release-assets.example/silt-v9.9.9-")
        ),
        "{log}"
    );
}

#[test]
fn install_sh_with_wget_refuses_a_redirect_to_http() {
    // wget would follow it (its --https-only is for recursive downloads);
    // install.sh follows redirects itself and never asks the http URL.
    let sb = Sandbox::new("wget_http");
    let out = sb.run_with_wget(&[("STUB_WGET_TO", "http://release-assets.example/")]);
    assert_branded_failure(&out, "failed to download binary");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("which is not https"), "{stderr}");
    let log = fs::read_to_string(sb.root.join("state/wget.log")).unwrap();
    assert!(
        !log.contains("http://"),
        "a plain http URL was asked for:\n{log}"
    );
    assert!(!sb.install_dir().join("silt").exists());
}

#[test]
fn install_sh_mv_binary_failure_is_branded() {
    let sb = Sandbox::new("mv");
    sb.fail_tool("mv");
    assert_branded_failure(&sb.run(""), "failed to replace");
    // The copy staged beside it is removed.
    let left: Vec<_> = fs::read_dir(sb.install_dir())
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

/// A silt that is running (an editor's `silt lsp`) is replaced: the new
/// binary is copied beside it and moved onto its name, and nothing else
/// is left in the install directory. FAILS on the base: `cp` onto the
/// running binary failed with "Text file busy" (reported as permission
/// denied). Linux only: there, writing to a running binary is refused.
#[cfg(target_os = "linux")]
#[test]
fn install_sh_replaces_a_running_binary() {
    let sb = Sandbox::new("busy");
    // A real executable as the released `silt`: `sleep`, which runs as
    // long as it is told.
    let sleep = ["/bin/sleep", "/usr/bin/sleep"]
        .into_iter()
        .map(Path::new)
        .find(|p| p.is_file())
        .expect("a sleep binary");
    fs::copy(sleep, sb.root.join("asset/silt")).unwrap();
    let status = Command::new("tar")
        .args(["czf", "release.tar.gz", "-C", "asset", "silt"])
        .current_dir(&sb.root)
        .status()
        .unwrap();
    assert!(status.success());
    let out = sb.run("");
    assert!(out.status.success(), "{out:?}");
    let installed = sb.install_dir().join("silt");
    let mut running = Command::new(&installed).arg("30").spawn().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(200));
    if running.try_wait().unwrap().is_some() {
        // (A multi-call `sleep` does not run under another name.)
        eprintln!("SKIP: the copied sleep binary does not run as `silt`");
        return;
    }
    let out = sb.run("");
    let _ = running.kill();
    let _ = running.wait();
    assert!(
        out.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let mode = fs::metadata(&installed).unwrap().permissions().mode();
    assert_eq!(mode & 0o111, 0o111, "not executable: {mode:o}");
    let left: Vec<_> = fs::read_dir(sb.install_dir())
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert_eq!(left, ["silt"], "{left:?}");
}

/// A signal between the copy and the move ends the script with its
/// EXIT trap: the staged copy is removed and the old binary stays.
/// FAILS on the base of this fix: under dash, which runs no EXIT trap
/// when a signal ends the script, `.silt.new.<pid>` was left behind.
#[test]
fn install_sh_interrupted_before_the_move_leaves_no_staged_copy() {
    let sb = Sandbox::new("signal");
    let out = sb.run("");
    assert!(out.status.success(), "{out:?}");
    // `mv` signals the script (its parent) and fails.
    sb.stub("mv", "#!/bin/sh\nkill -TERM $PPID\nsleep 1\nexit 1\n");
    let out = sb.run("");
    assert_ne!(out.status.code(), Some(0), "{out:?}");
    let left: Vec<_> = fs::read_dir(sb.install_dir())
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert_eq!(left, ["silt"], "{left:?}");
}

/// An install path that is a directory is refused: the new binary is not
/// moved into it. FAILS on the base: `mv` put `.silt.new.<pid>` inside
/// the directory and the script said "installed".
#[test]
fn install_sh_refuses_an_install_path_that_is_a_directory() {
    let sb = Sandbox::new("dir_target");
    fs::create_dir_all(sb.install_dir().join("silt")).unwrap();
    let expected = format!(
        "{}/silt is a directory: remove it",
        sb.install_dir().display()
    );
    assert_branded_failure(&sb.run(""), &expected);
    let inside: Vec<_> = fs::read_dir(sb.install_dir().join("silt"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert!(inside.is_empty(), "{inside:?}");
}

/// The binary can be run by everyone whatever the umask, as one in a bin
/// directory is. FAILS on the base: under umask 077 `chmod +x` gave 700.
#[test]
fn install_sh_installs_a_binary_everyone_can_run_whatever_the_umask() {
    let sb = Sandbox::new("umask");
    let path = format!(
        "{}:{}",
        sb.root.join("stubs").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = sb.run_under_umask(Some("077"), &path, "", &[]);
    assert!(out.status.success(), "{out:?}");
    let mode = fs::metadata(sb.install_dir().join("silt"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o755, "{mode:o}");
}

/// A Location without a scheme is followed as a browser reads it:
/// `//host/path` over https on that host, a relative path beside the URL
/// asked for. FAILS on the base: `//host` became a path on github.com and
/// a relative path was refused as "not https".
#[test]
fn install_sh_with_wget_follows_a_redirect_without_a_scheme() {
    for (tag, to, asked) in [
        (
            "wget_slashes",
            "//release-assets.example/via/",
            "https://release-assets.example/via/",
        ),
        (
            "wget_relative",
            "via/",
            "https://github.com/rendro/silt/releases/download/v9.9.9/via/",
        ),
    ] {
        let sb = Sandbox::new(tag);
        let out = sb.run_with_wget(&[("STUB_WGET_TO", to)]);
        assert!(
            out.status.success(),
            "{to}: stdout={}\nstderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(sb.install_dir().join("silt").is_file());
        let log = fs::read_to_string(sb.root.join("state/wget.log")).unwrap();
        assert!(
            log.lines()
                .any(|call| call.ends_with(&format!("{asked}silt-v9.9.9-SHA256SUMS"))),
            "{to}:\n{log}"
        );
    }
}

/// A download that is redirected again and again ends after ten hops,
/// saying so. FAILS on the base: it ended without a word of its own.
#[test]
fn install_sh_with_wget_says_when_a_download_is_redirected_too_often() {
    let sb = Sandbox::new("wget_loop");
    let out = sb.run_with_wget(&[("STUB_WGET_TO", "https://github.com/loop/")]);
    assert_branded_failure(&out, "failed to download binary");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("  error: a download was redirected more than 10 times"),
        "{stderr}"
    );
}

/// A wget that cannot be told not to follow a redirect (busybox's) is not
/// used: the script says what it needs. FAILS on the base: "could not
/// determine latest version".
#[test]
fn install_sh_with_a_busybox_wget_says_what_it_needs() {
    let sb = Sandbox::new("wget_busybox");
    let path = sb.wget_path();
    sb.stub(
        "wget",
        "#!/bin/sh\necho 'BusyBox v1.36.1 multi-call binary.' >&2\n\
         echo 'Usage: wget [-cqS] [-O FILE] URL' >&2\nexit 1\n",
    );
    let out = sb.run_on_path(&path, "", &[]);
    assert_branded_failure(&out, "this wget cannot be kept to https");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("install curl or GNU wget"), "{stderr}");
}
