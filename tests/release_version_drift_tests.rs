//! Release-time version-drift guard.
//!
//! Every release ships some doc text that references version numbers.
//! Some references are legitimate history ("added in v0.9") and some
//! are stale futures ("coming in v0.7" when v0.7 has already shipped).
//! This test fails when the stale-future pattern appears for any
//! version <= the current Cargo.toml version.
//!
//! Round 26 caught three instances of this drift manually
//! (docs/getting-started.md "coming in v0.7", docs/stdlib/tcp.md
//! "return Err in v0.9", docs/stdlib/bytes.md "v0.9 module surface").
//! This walker prevents recurrence.

use std::fs;
use std::path::{Path, PathBuf};

/// Parse the current version from Cargo.toml into (major, minor, patch).
fn current_version() -> (u32, u32, u32) {
    let cargo_toml = fs::read_to_string("Cargo.toml").expect("tests run from silt repo root");
    for line in cargo_toml.lines() {
        if let Some(rest) = line.strip_prefix("version = \"")
            && let Some(ver) = rest.strip_suffix("\"")
        {
            let parts: Vec<&str> = ver.split('.').collect();
            if parts.len() == 3 {
                return (
                    parts[0].parse().expect("major"),
                    parts[1].parse().expect("minor"),
                    parts[2].parse().expect("patch"),
                );
            }
        }
    }
    panic!("could not parse version from Cargo.toml");
}

/// Recursively collect every .md file under `dir`.
fn collect_md_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_md_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "md") {
            out.push(p);
        }
    }
}

/// Parse every `v0.X` or `v0.X.Y` reference out of `text`, returning
/// (major, minor) pairs it contains.
fn find_versions(text: &str) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i + 3 < bytes.len() {
        if bytes[i] == b'v' && bytes[i + 1].is_ascii_digit() {
            let mut j = i + 1;
            let start = j;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j < bytes.len()
                && bytes[j] == b'.'
                && j + 1 < bytes.len()
                && bytes[j + 1].is_ascii_digit()
            {
                let major: u32 = std::str::from_utf8(&bytes[start..j])
                    .unwrap()
                    .parse()
                    .unwrap();
                j += 1;
                let minor_start = j;
                while j < bytes.len() && bytes[j].is_ascii_digit() {
                    j += 1;
                }
                let minor: u32 = std::str::from_utf8(&bytes[minor_start..j])
                    .unwrap()
                    .parse()
                    .unwrap();
                out.push((major, minor));
                i = j;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn is_future(v: (u32, u32), current: (u32, u32, u32)) -> bool {
    (v.0, v.1) > (current.0, current.1)
}

/// The stale-future pattern we ban: any phrasing that frames an already-shipped
/// version as pending future work.
const STALE_FUTURE_PHRASES: &[&str] = &[
    "coming in v",
    "will ship in v",
    "planned for v",
    "arriving in v",
    "scheduled for v",
];

#[test]
fn no_doc_markets_released_version_as_future_work() {
    let current = current_version();
    let mut md_files = Vec::new();
    collect_md_files(Path::new("docs"), &mut md_files);
    collect_md_files(Path::new("."), &mut md_files); // README + any root .md

    let mut offenders: Vec<String> = Vec::new();
    for path in md_files {
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        for (line_idx, line) in text.lines().enumerate() {
            let lower = line.to_lowercase();
            for phrase in STALE_FUTURE_PHRASES {
                if let Some(pos) = lower.find(phrase) {
                    let tail = &line[pos + phrase.len() - 1..];
                    let versions = find_versions(tail);
                    for v in versions {
                        if !is_future(v, current) {
                            offenders.push(format!(
                                "{}:{}: \"{}\" references v{}.{} but current is v{}.{}.{}",
                                path.display(),
                                line_idx + 1,
                                line.trim(),
                                v.0,
                                v.1,
                                current.0,
                                current.1,
                                current.2,
                            ));
                        }
                    }
                }
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "stale 'coming in vX.Y' phrasing found for released versions:\n  {}",
        offenders.join("\n  ")
    );
}

#[test]
fn current_version_parses_correctly() {
    let (major, minor, patch) = current_version();
    // sanity: we shipped at least 0.10.0
    assert!(
        major > 0 || minor >= 10,
        "expected version >= 0.10.0, got {major}.{minor}.{patch}"
    );
}

#[test]
fn find_versions_extracts_all_vnums() {
    let text = "Added in v0.9. The v0.10 surface will stabilize v0.11";
    let found = find_versions(text);
    assert_eq!(found, vec![(0, 9), (0, 10), (0, 11)]);
}

#[test]
fn is_future_distinguishes_correctly() {
    let current = (0, 10, 0);
    assert!(is_future((0, 11), current));
    assert!(is_future((1, 0), current));
    assert!(!is_future((0, 10), current));
    assert!(!is_future((0, 9), current));
    assert!(!is_future((0, 7), current));
}

/// Stricter guard for stdlib and language reference docs: any bare
/// `v0.X` or `vX.Y` reference in these reference-style docs must name
/// the current minor or a newer one. Stale pins like "Existing v0.10
/// silt programs will continue to…" get silently ossified otherwise
/// — the future-phrase guard above misses them because the phrasing
/// is past/present tense.
///
/// Legitimate historical references (changelogs, migration notes) can
/// opt out by placing `<!-- drift-ok -->` on the same line.
#[test]
fn no_stale_bare_version_pins_in_reference_docs() {
    let current = current_version();
    let current_mm = (current.0, current.1);

    let mut md_files = Vec::new();
    collect_md_files(Path::new("docs/stdlib"), &mut md_files);
    collect_md_files(Path::new("docs/language"), &mut md_files);

    let mut offenders: Vec<String> = Vec::new();
    for path in md_files {
        // Skip anything that looks like a changelog — those are
        // expected to reference every shipped version.
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_lowercase();
        if name.contains("changelog") || name.contains("history") {
            continue;
        }

        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        for (line_idx, line) in text.lines().enumerate() {
            if line.contains("<!-- drift-ok -->") {
                continue;
            }
            for v in find_versions(line) {
                if (v.0, v.1) < current_mm {
                    offenders.push(format!(
                        "{}:{}: \"{}\" references v{}.{} but current is v{}.{}.{} \
                         (add `<!-- drift-ok -->` on the line to opt out)",
                        path.display(),
                        line_idx + 1,
                        line.trim(),
                        v.0,
                        v.1,
                        current.0,
                        current.1,
                        current.2,
                    ));
                }
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "stale bare version pins found in reference docs:\n  {}",
        offenders.join("\n  ")
    );
}
