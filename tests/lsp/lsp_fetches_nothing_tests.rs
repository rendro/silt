//! The language server fetches nothing: opening a file of a project
//! whose `silt.toml` names a git dependency that is not in the cache runs
//! no git. The dependency is an error on its `silt.toml` line that says
//! to run `silt check`; once `silt check` has fetched it, the server
//! reads it from the cache. FAIL on the base commit: the server cloned
//! the dependency into the cache when the file was opened.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use crate::support::LspClient;

fn path_to_uri(p: &Path) -> String {
    let s = p.to_str().expect("utf8 path");
    if s.starts_with('/') {
        format!("file://{s}")
    } else {
        format!("file:///{}", s.replace('\\', "/"))
    }
}

/// The variables that keep git and silt's checkout cache inside `ws`.
fn hermetic_env(ws: &Path) -> Vec<(&'static str, String)> {
    let cache = ws.join("cache").display().to_string();
    vec![
        ("XDG_CACHE_HOME", cache.clone()),
        ("LOCALAPPDATA", cache),
        ("GIT_CONFIG_NOSYSTEM", "1".to_string()),
        (
            "GIT_CONFIG_GLOBAL",
            ws.join("gitconfig").display().to_string(),
        ),
        ("GIT_TERMINAL_PROMPT", "0".to_string()),
    ]
}

fn git(ws: &Path, cwd: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(["-c", "user.name=silt tests"])
        .args(["-c", "user.email=tests@silt.local"])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(cwd)
        .envs(hermetic_env(ws))
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn git_installed() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|out| out.status.success())
}

/// Every directory under `dir`, recursively.
fn dirs_under(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        if entry.path().is_dir() {
            found.push(entry.path());
            found.extend(dirs_under(&entry.path()));
        }
    }
    found
}

fn messages(published: &Value) -> Vec<String> {
    published
        .pointer("/params/diagnostics")
        .and_then(|d| d.as_array())
        .into_iter()
        .flatten()
        .filter_map(|d| d.get("message").and_then(|m| m.as_str()).map(String::from))
        .collect()
}

#[test]
fn opening_a_file_fetches_no_git_dependency() {
    if !git_installed() {
        eprintln!("SKIP: `git` is not installed");
        return;
    }
    let ws = std::env::temp_dir().join(format!("silt_lsp_no_fetch_{}", std::process::id()));
    let _ = fs::remove_dir_all(&ws);
    fs::create_dir_all(&ws).unwrap();
    let ws = ws.canonicalize().unwrap();
    fs::write(ws.join("gitconfig"), "").unwrap();

    // A repository holding a library package.
    let repo = ws.join("dep");
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(
        repo.join("silt.toml"),
        "[package]\nname = \"dep\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(repo.join("src/lib.silt"), "pub fn two() -> Int {\n  2\n}\n").unwrap();
    git(&ws, &repo, &["init", "-q", "-b", "main"]);
    git(&ws, &repo, &["add", "."]);
    git(&ws, &repo, &["commit", "-q", "-m", "dep"]);

    // A project that depends on it, with no silt.lock.
    let app = ws.join("app");
    fs::create_dir_all(app.join("src")).unwrap();
    let url = repo.display().to_string().replace('\\', "/");
    fs::write(
        app.join("silt.toml"),
        format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\n\
             dep = {{ git = \"{url}\", branch = \"main\" }}\n"
        ),
    )
    .unwrap();
    let main = "import dep\n\nfn main() {\n  println(dep.two())\n}\n";
    fs::write(app.join("src/main.silt"), main).unwrap();
    let main_uri = path_to_uri(&app.join("src/main.silt"));
    let manifest_uri = path_to_uri(&app.join("silt.toml"));

    let env = hermetic_env(&ws);
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let mut client = LspClient::spawn_with_root_and_env(Some(&path_to_uri(&app)), &env);
    client.did_open(&main_uri, main);
    let published = client.wait_for_diagnostics(&manifest_uri);
    let found = messages(&published);
    assert!(
        found.iter().any(|m| m.contains("git dependency `dep`")
            && m.contains("): not fetched: run `silt check`\n")),
        "got {published}"
    );
    client.shutdown();
    // Nothing was cloned into the cache.
    let cloned: Vec<PathBuf> = dirs_under(&ws.join("cache"))
        .into_iter()
        .filter(|d| d.join("silt.toml").is_file())
        .collect();
    assert!(cloned.is_empty(), "the server fetched: {cloned:?}");

    // `silt check` fetches it and pins it; the server then reads it.
    let out = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("check")
        .current_dir(&app)
        .envs(hermetic_env(&ws))
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let mut client = LspClient::spawn_with_root_and_env(Some(&path_to_uri(&app)), &env);
    let published = client.did_open_and_wait(&main_uri, main);
    assert_eq!(
        messages(&published),
        Vec::<String>::new(),
        "got {published}"
    );
    client.shutdown();
    let _ = fs::remove_dir_all(&ws);
}
