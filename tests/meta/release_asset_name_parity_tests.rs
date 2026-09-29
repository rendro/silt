//! Release asset-name parity locks.
//!
//! The release asset naming scheme (`silt-<version>-<target>.<ext>` plus
//! `silt-<version>-SHA256SUMS`) is hand-encoded in THREE surfaces:
//!
//!   1. `.github/workflows/release.yml` — the producer: packages archives,
//!      generates the SHA256SUMS file, uploads both to the GitHub release.
//!   2. `install.sh` — consumer: builds the download URL from
//!      `asset="silt-${version}-${target}.${ext}"`.
//!   3. `src/update.rs` (`silt update` self-update) — consumer:
//!      `format!("silt-{latest}-{target}.{ext}")`.
//!
//! Nothing at runtime ties these together: a rename on any one surface
//! keeps `cargo test` green but 404s the next tagged release for the other
//! two. These tests read all three files as text, extract each surface's
//! template, normalize the placeholder syntax (`${{ github.ref_name }}`,
//! `${version}`, `{latest}` → `<V>`, etc.), and assert they encode the
//! same template. They also assert every target triple in the release
//! build matrix is constructible by both consumers' arch/os mappings.
//!
//! If you intentionally rename a release asset, update ALL THREE surfaces
//! (and these locks' canonical constants) in the same commit.

use std::fs;
use std::path::PathBuf;

/// Canonical archive-name template after placeholder normalization.
const ASSET_TEMPLATE: &str = "silt-<V>-<T>.<E>";
/// Canonical checksum-file-name template after placeholder normalization.
const SUMS_TEMPLATE: &str = "silt-<V>-SHA256SUMS";

fn read_repo_file(rel: &str) -> String {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// release.yml with GitHub Actions expressions collapsed to placeholders,
/// so shell-ish tokenization on whitespace works on the packaging lines.
fn release_yml_normalized() -> String {
    read_repo_file(".github/workflows/release.yml")
        .replace("${{ github.ref_name }}", "<V>")
        .replace("${{ matrix.target }}", "<T>")
}

fn normalize_sh(s: &str) -> String {
    s.replace("${version}", "<V>")
        .replace("$version", "<V>")
        .replace("${target}", "<T>")
        .replace("${ext}", "<E>")
}

fn normalize_rs(s: &str) -> String {
    s.replace("{latest}", "<V>")
        .replace("{target}", "<T>")
        .replace("{ext}", "<E>")
}

/// First line in `body` that contains `needle`.
fn find_line<'a>(body: &'a str, needle: &str) -> &'a str {
    body.lines()
        .find(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("no line containing {needle:?}"))
}

/// The first double-quoted string literal at or after `anchor` in `body`.
/// (None of the anchored literals contain escaped quotes.)
fn quoted_after<'a>(body: &'a str, anchor: &str) -> &'a str {
    let start = body
        .find(anchor)
        .unwrap_or_else(|| panic!("anchor {anchor:?} not found"));
    let rest = &body[start..];
    let open = rest
        .find('"')
        .unwrap_or_else(|| panic!("no string literal after {anchor:?}"));
    let lit = &rest[open + 1..];
    let close = lit
        .find('"')
        .unwrap_or_else(|| panic!("unterminated literal after {anchor:?}"));
    &lit[..close]
}

/// Basename of a `/`-separated path or URL.
fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// From a normalized release.yml line, the whitespace token ending in
/// `suffix` (e.g. the archive argument of `tar czf`), reduced to its
/// basename.
fn token_with_suffix<'a>(line: &'a str, suffix: &str) -> &'a str {
    line.split_whitespace()
        .find(|tok| tok.ends_with(suffix))
        .map(basename)
        .unwrap_or_else(|| panic!("no token ending in {suffix:?} on line {line:?}"))
}

/// The `(target, archive)` rows of release.yml's build matrix.
fn release_matrix() -> Vec<(String, String)> {
    let yml = release_yml_normalized();
    let mut rows: Vec<(String, String)> = Vec::new();
    for line in yml.lines() {
        let t = line.trim();
        if let Some(target) = t.strip_prefix("- target:") {
            rows.push((target.trim().to_string(), String::new()));
        } else if let Some(archive) = t.strip_prefix("archive:") {
            let row = rows
                .last_mut()
                .unwrap_or_else(|| panic!("archive: before any - target: in release.yml"));
            row.1 = archive.trim().to_string();
        }
    }
    assert!(
        !rows.is_empty(),
        "release.yml build matrix parsed empty — matrix syntax changed? update this lock"
    );
    for (target, archive) in &rows {
        assert!(
            archive == "tar.gz" || archive == "zip",
            "matrix target {target} has unknown archive type {archive:?}"
        );
    }
    rows
}

/// All `echo "..."` values inside one shell function of install.sh.
fn install_sh_fn_echo_values(fn_name: &str) -> Vec<String> {
    let sh = read_repo_file("install.sh");
    let open = format!("{fn_name}() {{");
    let start = sh
        .find(&open)
        .unwrap_or_else(|| panic!("install.sh: function {fn_name} not found"));
    let body_all = &sh[start..];
    let end = body_all
        .find("\n}")
        .unwrap_or_else(|| panic!("install.sh: function {fn_name} has no closing brace"));
    let body = &body_all[..end];
    let mut values = Vec::new();
    let mut rest = body;
    while let Some(i) = rest.find("echo \"") {
        let lit = &rest[i + "echo \"".len()..];
        let close = lit.find('"').expect("unterminated echo literal");
        values.push(lit[..close].to_string());
        rest = &lit[close..];
    }
    assert!(
        !values.is_empty(),
        "install.sh: no echo values found in {fn_name} — rewrite this extractor"
    );
    values
}

/// All match-arm result literals (`=> "..."`) inside update.rs's
/// `detect_target` — the union of its OS suffixes and arch prefixes.
fn update_rs_detect_target_values() -> Vec<String> {
    let rs = read_repo_file("src/update.rs");
    let start = rs
        .find("fn detect_target")
        .expect("src/update.rs: fn detect_target not found");
    let body_all = &rs[start..];
    let end = body_all
        .find("\n}")
        .expect("src/update.rs: detect_target has no closing brace");
    let body = &body_all[..end];
    let mut values = Vec::new();
    let mut rest = body;
    while let Some(i) = rest.find("=> \"") {
        let lit = &rest[i + "=> \"".len()..];
        let close = lit.find('"').expect("unterminated match-arm literal");
        values.push(lit[..close].to_string());
        rest = &lit[close..];
    }
    assert!(
        !values.is_empty(),
        "src/update.rs: no detect_target match arms found — rewrite this extractor"
    );
    values
}

// ── Lock 1: archive-name template parity ────────────────────────────

#[test]
fn asset_name_template_matches_across_all_three_surfaces() {
    // Producer: the tar and 7z packaging steps in release.yml.
    let yml = release_yml_normalized();
    let tar_asset = token_with_suffix(find_line(&yml, "tar czf"), ".tar.gz");
    assert_eq!(
        tar_asset,
        ASSET_TEMPLATE.replace("<E>", "tar.gz"),
        "release.yml `tar czf` packaging name drifted from the canonical \
         silt-<version>-<target>.tar.gz template consumed by install.sh and silt update"
    );
    let zip_asset = token_with_suffix(find_line(&yml, "7z a"), ".zip");
    assert_eq!(
        zip_asset,
        ASSET_TEMPLATE.replace("<E>", "zip"),
        "release.yml `7z a` packaging name drifted from the canonical template"
    );

    // The upload-artifact glob must cover the same basename.
    let upload = token_with_suffix(find_line(&yml, "path: silt-"), ".*");
    assert_eq!(
        upload,
        format!("{}.*", ASSET_TEMPLATE.trim_end_matches(".<E>")),
        "release.yml upload-artifact path glob drifted from the packaging name"
    );

    // Consumer 1: install.sh's asset= assignment.
    let sh = read_repo_file("install.sh");
    let asset_line = find_line(&sh, "asset=\"").trim();
    let sh_asset = quoted_after(asset_line, "asset=");
    assert_eq!(
        normalize_sh(sh_asset),
        ASSET_TEMPLATE,
        "install.sh asset= template drifted from release.yml's packaging name"
    );

    // Consumer 2: update.rs's asset format string.
    let rs = read_repo_file("src/update.rs");
    let rs_asset = quoted_after(&rs, "let asset");
    assert_eq!(
        normalize_rs(rs_asset),
        ASSET_TEMPLATE,
        "src/update.rs asset format! template drifted from release.yml's packaging name"
    );
}

// ── Lock 2: SHA256SUMS file-name parity ─────────────────────────────

#[test]
fn sha256sums_name_template_matches_across_all_three_surfaces() {
    // Producer: the redirect target of the Generate SHA256SUMS step.
    let yml = release_yml_normalized();
    let redirect = find_line(&yml, "> silt");
    let yml_sums = token_with_suffix(redirect, "SHA256SUMS");
    assert_eq!(
        yml_sums, SUMS_TEMPLATE,
        "release.yml SHA256SUMS output name drifted from the canonical \
         silt-<version>-SHA256SUMS template consumed by install.sh and silt update"
    );

    // The sha256sum invocation must glob both archive extensions under the
    // same silt-<version>- prefix, or some assets ship unverifiable.
    let sum_cmd = find_line(&yml, "sha256sum silt");
    for glob in ["silt-<V>-*.tar.gz", "silt-<V>-*.zip"] {
        assert!(
            sum_cmd.contains(glob),
            "release.yml sha256sum invocation no longer covers {glob}: {sum_cmd:?}"
        );
    }

    // Consumer 1: install.sh's sums_url last path segment.
    let sh = read_repo_file("install.sh");
    let sums_line = find_line(&sh, "sums_url=\"").trim();
    let sh_sums_url = quoted_after(sums_line, "sums_url=");
    assert_eq!(
        normalize_sh(basename(sh_sums_url)),
        SUMS_TEMPLATE,
        "install.sh sums_url file name drifted from release.yml's SHA256SUMS name"
    );

    // Consumer 2: update.rs's sums_url format string last path segment.
    let rs = read_repo_file("src/update.rs");
    let rs_sums_url = quoted_after(&rs, "let sums_url");
    assert_eq!(
        normalize_rs(basename(rs_sums_url)),
        SUMS_TEMPLATE,
        "src/update.rs sums_url file name drifted from release.yml's SHA256SUMS name"
    );
}

// ── Lock 3: build-matrix targets constructible by both consumers ────

#[test]
fn every_matrix_target_is_constructible_by_install_sh_and_update_rs() {
    let arch_set = install_sh_fn_echo_values("detect_arch");
    let os_set = install_sh_fn_echo_values("detect_os");
    let update_set = update_rs_detect_target_values();

    // Both consumers compose the triple as <arch>-<os>.
    let sh = read_repo_file("install.sh");
    assert!(
        sh.contains("target=\"${arch}-${os}\""),
        "install.sh no longer composes target as ${{arch}}-${{os}} — \
         re-derive this lock's composition rule"
    );
    let rs = read_repo_file("src/update.rs");
    assert!(
        rs.contains("format!(\"{arch}-{os}\")"),
        "src/update.rs detect_target no longer composes target as {{arch}}-{{os}} — \
         re-derive this lock's composition rule"
    );

    for (target, archive) in release_matrix() {
        let (arch, os) = target
            .split_once('-')
            .unwrap_or_else(|| panic!("matrix target {target:?} has no arch-os separator"));
        assert!(
            arch_set.iter().any(|a| a == arch),
            "release.yml builds {target} but install.sh detect_arch cannot \
             produce arch {arch:?} (knows {arch_set:?}) — installer would 404"
        );
        assert!(
            os_set.iter().any(|o| o == os),
            "release.yml builds {target} but install.sh detect_os cannot \
             produce os {os:?} (knows {os_set:?}) — installer would 404"
        );
        assert!(
            update_set.iter().any(|v| v == arch) && update_set.iter().any(|v| v == os),
            "release.yml builds {target} but src/update.rs detect_target cannot \
             produce it (knows {update_set:?}) — silt update would 404"
        );
        // Extension choice parity: both consumers pick zip exactly for
        // windows targets (install.sh: os = pc-windows-msvc; update.rs:
        // target.contains("windows")). The matrix must agree.
        assert_eq!(
            archive == "zip",
            target.contains("windows"),
            "matrix target {target} packages as {archive:?}, but both consumers \
             request {} for it",
            if target.contains("windows") {
                "zip"
            } else {
                "tar.gz"
            }
        );
    }
}

// ── Lock 4: consumer mappings do not point at unshipped assets ──────

#[test]
fn consumer_arch_os_cross_product_matches_release_matrix() {
    // The reverse direction of lock 3: every triple the consumers CAN
    // construct must exist in the release matrix, or real machines get a
    // 404 from install.sh / silt update. Triples the consumers can name
    // but that we deliberately do not ship are listed here — extend this
    // list consciously, in the same commit that documents why.
    const KNOWN_UNSHIPPED: &[&str] = &["aarch64-pc-windows-msvc"];

    let matrix: Vec<String> = release_matrix().into_iter().map(|(t, _)| t).collect();
    for arch in install_sh_fn_echo_values("detect_arch") {
        for os in install_sh_fn_echo_values("detect_os") {
            let triple = format!("{arch}-{os}");
            if KNOWN_UNSHIPPED.contains(&triple.as_str()) {
                continue;
            }
            assert!(
                matrix.contains(&triple),
                "install.sh can construct {triple} on a real machine, but \
                 release.yml's build matrix does not ship it (ships {matrix:?}); \
                 add the matrix target or add it to KNOWN_UNSHIPPED with a rationale"
            );
        }
    }
}
