//! `silt add <name> --path <path>` or
//! `silt add <name> --git <url> [--rev|--branch|--tag <ref>]` —
//! append a new dependency entry to the current package's
//! `silt.toml` and regenerate `silt.lock`.

use std::fs;
use std::path::PathBuf;
use std::process;

use silt::intern;
use silt::lockfile::Lockfile;
use silt::manifest::Manifest;

use crate::cli::package::find_project_root;
use crate::cli::paths::{normalize_path, relative_from};

/// Dispatch `silt add <name> <source-flag> [<ref-flag>]`.
pub(crate) fn dispatch(args: &[String]) {
    if args[2..].iter().any(|a| a == "--help" || a == "-h") {
        println!("Usage: silt add <name> --path <path>");
        println!("       silt add <name> --git <url> --rev <sha>");
        println!("       silt add <name> --git <url> --branch <name>");
        println!("       silt add <name> --git <url> --tag <name>");
        println!();
        println!("Add a dependency to the current package's silt.toml,");
        println!("then regenerate silt.lock to include the new dep.");
        println!();
        println!("Arguments:");
        println!("  <name>             The local name to import the dep as.");
        println!("                     Must be a valid silt identifier and must not");
        println!("                     collide with a builtin module.");
        println!("  --path <path>      Path to the dep's package root (the directory");
        println!("                     containing its silt.toml).");
        println!("  --git <url>        URL of a git repository hosting a silt package.");
        println!("                     Must be paired with exactly one of");
        println!("                     --rev, --branch, or --tag.");
        println!("  --rev <sha>        Pin to a specific commit SHA (7-40 hex chars).");
        println!("  --branch <name>    Track a branch; resolved to the current HEAD SHA");
        println!("                     and re-resolved on each `silt update`.");
        println!("  --tag <name>       Track a tag; resolved at lock time and");
        println!("                     re-resolved on `silt update` if the tag moves.");
        println!();
        println!("Examples:");
        println!("  silt add calc --path ../calc");
        println!("  silt add calc --git https://github.com/foo/calc --branch main");
        println!("  silt add calc --git https://github.com/foo/calc --tag v1.0.0");
        println!("  silt add calc --git https://github.com/foo/calc --rev abc1234");
        process::exit(0);
    }
    if let Err(e) = run_add_command(&args[2..]) {
        eprintln!("error: {e}");
        process::exit(1);
    }
}

/// Source kind selected on the `silt add` command line. Mirrors the two
/// arms of `Dependency` in `src/manifest.rs`; we keep this local enum
/// so the parser's "exactly one source flag" invariant lives close to
/// the parser itself.
enum AddSource {
    Path(String),
    Git {
        url: String,
        ref_spec: silt::git::GitRef,
    },
}

/// Implementation of `silt add <name> --path <path>` and
/// `silt add <name> --git <url> [--rev|--branch|--tag <ref>]`.
///
/// Edits the current package's `silt.toml` in place to add a new
/// dependency entry, then regenerates `silt.lock` so the next compile
/// picks up the new dep. Uses `toml_edit` so user formatting
/// (comments, blank lines, key ordering) is preserved.
///
/// Validation order: argument shape → name → URL/path well-formedness
/// → (git only) `verify_reachable` → (git only) `resolve_ref` → manifest
/// write → lockfile regen. Both path and git deps now flow through the
/// same lockfile-regen step (git deps fetch into `<silt-cache>/git/...`
/// and pin the resolved SHA in `silt.lock`).
///
/// Errors are returned rather than printed so the caller can wrap them
/// in the dispatch's standard "error: ..." prefix and exit code.
fn run_add_command(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    // ── Argument parsing ──────────────────────────────────────────────
    //
    // Positional name + one source flag (`--path <p>` OR `--git <url>`
    // with exactly one of `--rev` / `--branch` / `--tag`); reject
    // anything else so typos like `--paths` surface immediately rather
    // than being silently swallowed.
    let mut name: Option<String> = None;
    let mut path_arg: Option<String> = None;
    let mut git_arg: Option<String> = None;
    let mut rev_arg: Option<String> = None;
    let mut branch_arg: Option<String> = None;
    let mut tag_arg: Option<String> = None;
    let mut i = 0;
    // Helper: capture a flag's value, supporting both `--flag VALUE`
    // and `--flag=VALUE` forms, and complaining on duplicates.
    fn take_flag_value(
        args: &[String],
        i: &mut usize,
        slot: &mut Option<String>,
        flag: &str,
    ) -> Result<(), String> {
        if slot.is_some() {
            return Err(format!("{flag} was specified more than once"));
        }
        if *i + 1 >= args.len() {
            return Err(format!("{flag} requires a value"));
        }
        *slot = Some(args[*i + 1].clone());
        *i += 2;
        Ok(())
    }
    while i < args.len() {
        let arg = &args[i];
        if arg == "--path" {
            take_flag_value(args, &mut i, &mut path_arg, "--path")?;
        } else if let Some(rest) = arg.strip_prefix("--path=") {
            if path_arg.is_some() {
                return Err("--path was specified more than once".into());
            }
            path_arg = Some(rest.to_string());
            i += 1;
        } else if arg == "--git" {
            take_flag_value(args, &mut i, &mut git_arg, "--git")?;
        } else if let Some(rest) = arg.strip_prefix("--git=") {
            if git_arg.is_some() {
                return Err("--git was specified more than once".into());
            }
            git_arg = Some(rest.to_string());
            i += 1;
        } else if arg == "--rev" {
            take_flag_value(args, &mut i, &mut rev_arg, "--rev")?;
        } else if let Some(rest) = arg.strip_prefix("--rev=") {
            if rev_arg.is_some() {
                return Err("--rev was specified more than once".into());
            }
            rev_arg = Some(rest.to_string());
            i += 1;
        } else if arg == "--branch" {
            take_flag_value(args, &mut i, &mut branch_arg, "--branch")?;
        } else if let Some(rest) = arg.strip_prefix("--branch=") {
            if branch_arg.is_some() {
                return Err("--branch was specified more than once".into());
            }
            branch_arg = Some(rest.to_string());
            i += 1;
        } else if arg == "--tag" {
            take_flag_value(args, &mut i, &mut tag_arg, "--tag")?;
        } else if let Some(rest) = arg.strip_prefix("--tag=") {
            if tag_arg.is_some() {
                return Err("--tag was specified more than once".into());
            }
            tag_arg = Some(rest.to_string());
            i += 1;
        } else if arg.starts_with('-') {
            // Round-26 G6: every other subcommand emits a `Run 'silt <sub> --help'
            // for usage.` nudge on a second stderr line. Match that shape here
            // by embedding the nudge in the error string (the top-level
            // dispatch renders `error: {e}` with `\n` passthrough).
            return Err(format!(
                "silt add: unknown flag '{arg}'\nRun 'silt add --help' for usage."
            )
            .into());
        } else if name.is_none() {
            name = Some(arg.clone());
            i += 1;
        } else {
            return Err(format!("silt add: unexpected extra argument '{arg}'").into());
        }
    }
    let name = name.ok_or("silt add: missing required <name> argument")?;

    // ── Source selection (path vs git) ────────────────────────────────
    //
    // Mutually exclusive: zero source flags or both at once is a usage
    // error. For `--git` we additionally require exactly one ref form.
    let source = match (path_arg.is_some(), git_arg.is_some()) {
        (true, true) => {
            return Err("silt add: --path and --git are mutually exclusive; pick one".into());
        }
        (false, false) => {
            return Err(
                "silt add: missing source flag; use --path <path> or --git <url> \
                 [--rev|--branch|--tag <ref>]"
                    .into(),
            );
        }
        (true, false) => {
            // Bare ref flags without --git make no sense — surface a
            // dedicated error rather than silently ignoring them.
            for (val, flag) in [
                (&rev_arg, "--rev"),
                (&branch_arg, "--branch"),
                (&tag_arg, "--tag"),
            ] {
                if val.is_some() {
                    return Err(format!(
                        "silt add: {flag} requires --git (it has no meaning with --path)"
                    )
                    .into());
                }
            }
            AddSource::Path(path_arg.expect("checked above"))
        }
        (false, true) => {
            let url = git_arg.expect("checked above");
            // Tally the ref forms so the multiple-vs-missing diagnostics
            // can be tailored.
            let mut chosen: Vec<(&str, String)> = Vec::new();
            if let Some(v) = rev_arg {
                chosen.push(("rev", v));
            }
            if let Some(v) = branch_arg {
                chosen.push(("branch", v));
            }
            if let Some(v) = tag_arg {
                chosen.push(("tag", v));
            }
            let ref_spec = match chosen.len() {
                0 => {
                    return Err(
                        "silt add: --git requires exactly one of --rev, --branch, or --tag".into(),
                    );
                }
                1 => {
                    let (kind, value) = chosen.into_iter().next().unwrap();
                    match kind {
                        "rev" => silt::git::GitRef::Rev(value),
                        "branch" => silt::git::GitRef::Branch(value),
                        "tag" => silt::git::GitRef::Tag(value),
                        _ => unreachable!("kinds restricted above"),
                    }
                }
                _ => {
                    let mentioned: Vec<&str> = chosen.iter().map(|(k, _)| *k).collect();
                    return Err(format!(
                        "silt add: --git takes exactly one ref form, but multiple were given: {}",
                        mentioned
                            .iter()
                            .map(|k| format!("--{k}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                    .into());
                }
            };
            AddSource::Git { url, ref_spec }
        }
    };

    // ── Manifest discovery ─────────────────────────────────────────────
    let cwd = std::env::current_dir().map_err(|e| format!("failed to determine cwd: {e}"))?;
    let (root, manifest) = match find_project_root(&cwd)? {
        Some(pair) => pair,
        None => return Err("silt add must be run inside a silt package".into()),
    };

    // ── Name validation ────────────────────────────────────────────────
    //
    // Identifier rules first (cheap, deterministic), then collisions:
    // a name that's both invalid AND a builtin should report invalid
    // (the user's bigger problem).
    if !silt::manifest::is_silt_identifier(&name) {
        return Err(format!(
            "silt add: invalid dependency name `{name}`: \
             must match silt identifier rules `[a-z_][a-z0-9_]*`"
        )
        .into());
    }
    if silt::module::is_builtin_module(&name) {
        return Err(format!(
            "silt add: dependency name `{name}` collides with builtin module `{name}`; \
             pick a different name"
        )
        .into());
    }
    let already_present = manifest
        .dependencies
        .keys()
        .any(|sym| intern::resolve(*sym) == name);
    if already_present {
        return Err(format!("silt add: dependency '{name}' is already declared").into());
    }

    // ── Source validation + the rendered TOML inline-table ────────────
    //
    // Path deps validate filesystem state; git deps do shape checks +
    // an `ls-remote HEAD` reachability ping + a ref-existence check
    // before we mutate anything on disk.
    let (success_summary, inline) = match source {
        AddSource::Path(path_arg) => {
            // Resolve the user-provided path against cwd (so `silt add
            // foo --path ../foo` works regardless of where in the
            // package tree they're sitting), then verify the
            // destination is actually a silt package. Both checks
            // deliberately use `is_file` / `is_dir` rather than
            // `exists()` so a stray symlink doesn't trip a misleading
            // error.
            let user_path = PathBuf::from(&path_arg);
            let absolute_dep_path = if user_path.is_absolute() {
                user_path.clone()
            } else {
                cwd.join(&user_path)
            };
            let absolute_dep_path = normalize_path(&absolute_dep_path);
            if !absolute_dep_path.exists() {
                return Err(format!(
                    "silt add: path does not exist: {}",
                    absolute_dep_path.display()
                )
                .into());
            }
            let dep_manifest = absolute_dep_path.join("silt.toml");
            if !dep_manifest.is_file() {
                return Err(format!(
                    "silt add: path is not a silt package (no silt.toml found): {}",
                    absolute_dep_path.display()
                )
                .into());
            }

            // We always store the path relative-to-manifest-dir when
            // possible; this keeps freshly-checked-out workspaces
            // portable across machines. If the dep lives outside the
            // manifest's tree (e.g. an absolute path under /opt) we
            // fall back to the absolute form because there's no clean
            // relative form to write.
            let stored_path = relative_from(&root, &absolute_dep_path)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| absolute_dep_path.display().to_string());
            let mut inline = toml_edit::InlineTable::new();
            inline.insert(
                "path",
                toml_edit::value(stored_path.clone()).into_value().unwrap(),
            );
            (
                format!("Added dependency '{name}' (path = \"{stored_path}\")"),
                inline,
            )
        }
        AddSource::Git { url, ref_spec } => {
            // Cheap shape check first — this lets us reject obviously
            // malformed input ("not a url") without paying for an
            // `ls-remote` roundtrip.
            if !looks_like_git_url(&url) {
                return Err(format!(
                    "silt add: --git URL `{url}` doesn't look like a git URL \
                     (expected http(s)://, git://, ssh://, file://, or user@host:path)"
                )
                .into());
            }
            // Shape-validate Rev locally so a malformed SHA fails before
            // any network traffic. `verify_reachable` would catch this
            // eventually but the diagnostic is friendlier here, and we
            // also avoid a wasted roundtrip.
            if let silt::git::GitRef::Rev(sha) = &ref_spec
                && !silt::git::is_valid_sha_shape(sha)
            {
                return Err(format!(
                    "silt add: --rev `{sha}` is not a valid commit SHA shape \
                     (expected 7-40 hexadecimal characters)"
                )
                .into());
            }

            // Reachability ping: catches typos and private-repo-no-auth
            // before we mutate anything. We surface git's stderr in the
            // error path (via Display on GitError::CommandFailed) so
            // users see the real diagnostic, e.g. "Repository not
            // found" or "Permission denied (publickey)".
            silt::git::verify_reachable(&url)
                .map_err(|e| format!("silt add: cannot reach `{url}`: {e}"))?;

            // Ref existence: rejects `--branch nonexistent_xyz` etc.
            // For Rev specs this is a no-op (offline shape check).
            silt::git::resolve_ref(&url, &ref_spec).map_err(|e| {
                format!(
                    "silt add: cannot resolve {} `{}` in `{url}`: {e}",
                    ref_spec.kind(),
                    ref_spec.as_ref_string()
                )
            })?;

            // Render the inline table. Key order is fixed (`git` first,
            // then the ref form) so manifests stay diffable across
            // different runs and machines.
            let mut inline = toml_edit::InlineTable::new();
            inline.insert("git", toml_edit::value(url.clone()).into_value().unwrap());
            let ref_value = ref_spec.as_ref_string().to_string();
            inline.insert(
                ref_spec.kind(),
                toml_edit::value(ref_value.clone()).into_value().unwrap(),
            );
            (
                format!(
                    "Added dependency '{name}' (git = \"{url}\", {} = \"{}\")",
                    ref_spec.kind(),
                    ref_value
                ),
                inline,
            )
        }
    };

    // ── Manifest mutation via toml_edit ────────────────────────────────
    //
    // toml_edit preserves formatting, comments, and key ordering
    // verbatim — required so a user who's hand-formatted their
    // silt.toml doesn't lose that work the first time they run `silt
    // add`. We only insert the new entry; everything else stays as-is.
    let manifest_path = root.join("silt.toml");
    let manifest_text = fs::read_to_string(&manifest_path)
        .map_err(|e| format!("failed to read {}: {e}", manifest_path.display()))?;
    let mut doc: toml_edit::DocumentMut = manifest_text
        .parse()
        .map_err(|e| format!("failed to parse {}: {e}", manifest_path.display()))?;

    // Ensure a `[dependencies]` table exists. If it's missing entirely
    // we create one as an explicit table (so it renders as the
    // header-style `[dependencies]` users expect, not as an inline
    // `dependencies = {}` blob).
    if doc.get("dependencies").is_none() {
        doc["dependencies"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    let deps = doc["dependencies"]
        .as_table_mut()
        .ok_or("silt.toml has a [dependencies] entry that isn't a table")?;

    deps.insert(
        &name,
        toml_edit::Item::Value(toml_edit::Value::InlineTable(inline)),
    );

    fs::write(&manifest_path, doc.to_string())
        .map_err(|e| format!("failed to write {}: {e}", manifest_path.display()))?;

    // ── Lockfile regeneration ──────────────────────────────────────────
    //
    // Re-load the just-written manifest and resolve the lockfile from
    // it. We deliberately don't reuse the `manifest` we loaded earlier
    // — toml_edit just rewrote the file, and any future validation
    // tightening should run against the on-disk form, not a stale
    // in-memory copy.
    let updated = Manifest::load(&manifest_path)
        .map_err(|e| format!("manifest re-validation failed after edit: {e}"))?;

    println!("{success_summary}");

    let lockfile =
        Lockfile::resolve(&updated).map_err(|e| format!("failed to resolve dependencies: {e}"))?;
    let lock_path = root.join("silt.lock");
    lockfile
        .write(&lock_path)
        .map_err(|e| format!("failed to write {}: {e}", lock_path.display()))?;

    Ok(())
}

/// Cheap regex-free shape check for git URLs. We deliberately keep the
/// rule loose — the actual `git ls-remote` will fail with a precise
/// diagnostic for transport-level errors. This is just here to reject
/// obvious non-URLs ("not a url", "/usr/local") and surface a friendlier
/// error than `git`'s "fatal: '/usr/local' does not appear to be a git
/// repository".
///
/// Accepts:
///   - `http://...`, `https://...`
///   - `git://...`
///   - `ssh://...`
///   - `file://...`
///   - `user@host:path` (the SCP-style git URL form: an `@` followed by a
///     `:` somewhere later, with no whitespace anywhere)
fn looks_like_git_url(s: &str) -> bool {
    if s.is_empty() || s.contains(char::is_whitespace) {
        return false;
    }
    if s.starts_with("http://")
        || s.starts_with("https://")
        || s.starts_with("git://")
        || s.starts_with("ssh://")
        // `file://` is the canonical local-bare-repo URL form; git
        // clone accepts it natively. Used by hermetic test fixtures
        // and occasionally by users sharing repos via a local mount.
        || s.starts_with("file://")
    {
        // Must have *something* after the scheme.
        return s.split("://").nth(1).is_some_and(|rest| !rest.is_empty());
    }
    // SCP-style: `user@host:path`. Require both `@` and a `:` *after* the
    // `@` so a stray colon-prefix doesn't pass.
    if let Some(at_pos) = s.find('@')
        && let Some(colon_pos) = s[at_pos..].find(':')
    {
        // user@host:something
        let after_colon = &s[at_pos + colon_pos + 1..];
        if !after_colon.is_empty() {
            return true;
        }
    }
    false
}
