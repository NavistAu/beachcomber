//! `cargo xtask` — repository automation for beachcomber.
//!
//! Two tasks:
//!
//! - `set-version`, which bumps the project version across every manifest,
//!   lockfile, package recipe, and README download URL in a single command.
//!   Each target file is edited by a count-guarded literal replacement of
//!   the current version string; if any file's occurrence count does not
//!   match the expected count the whole run aborts before writing anything,
//!   so a drifting file format surfaces as a loud error rather than a
//!   silent miss.
//! - `gen-header`, which regenerates `libbeachcomber-ffi/include/beachcomber.h`
//!   from that crate's `extern "C"` surface by shelling out to the `cbindgen`
//!   CLI. `cbindgen` is deliberately not a build-dependency (this project
//!   builds offline from `vendor/`, and vendoring cbindgen's own dependency
//!   tree would slow every build for a header only CI and contributors who
//!   touch the FFI surface need); install it with
//!   `cargo install cbindgen --locked` to run this locally.
//!
//! Usage:
//!   cargo xtask set-version <X.Y.Z> [--dry-run] [--no-verify]
//!   cargo xtask gen-header [--check]
//!
//! `--dry-run` reports the plan without touching the tree. `--no-verify` skips
//! the post-edit `cargo check`. The CHANGELOG is intentionally NOT touched —
//! release notes are written by hand. `gen-header --check` runs cbindgen's
//! own `--verify` mode: it fails without writing anything if regenerating
//! would change the committed header, which is what CI's freshness check
//! runs.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// A file the version bump must touch.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Target {
    /// Path relative to the repository root.
    path: String,
    /// Exact number of occurrences of the old version expected in the file.
    expected: usize,
    /// When true, the file is also renamed (old→new substituted in its path).
    rename: bool,
}

// ---------------------------------------------------------------------------
// Pure logic (unit-tested)
// ---------------------------------------------------------------------------

/// Extract the package version from a `Cargo.toml`'s `[package]` table.
///
/// The package `version = "X"` sits at column 0; dependency lines are
/// `name = { version = "..." }`, so the first line beginning with `version = "`
/// is unambiguously the package version.
fn parse_current_version(cargo_toml: &str) -> Result<String, String> {
    for line in cargo_toml.lines() {
        if let Some(rest) = line.strip_prefix("version = \"")
            && let Some(end) = rest.find('"')
        {
            return Ok(rest[..end].to_string());
        }
    }
    Err("no `version = \"...\"` line found in [package]".into())
}

/// True for a strict `MAJOR.MINOR.PATCH` triple of non-empty numeric components.
fn is_valid_version(v: &str) -> bool {
    let parts: Vec<&str> = v.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// Byte offsets of every occurrence of `old` in `content` that is NOT flanked by
/// an ASCII digit on either side. The flank guard prevents matching a shorter
/// version inside a longer one (e.g. `0.6.1` must not match within `0.6.11` or
/// `10.6.1`). Matches are non-overlapping.
fn flanked_match_offsets(content: &str, old: &str) -> Vec<usize> {
    let bytes = content.as_bytes();
    let olen = old.len();
    let mut offsets = Vec::new();
    let mut start = 0;
    while let Some(rel) = content[start..].find(old) {
        let pos = start + rel;
        let before_ok = pos == 0 || !bytes[pos - 1].is_ascii_digit();
        let after = pos + olen;
        let after_ok = after >= bytes.len() || !bytes[after].is_ascii_digit();
        if before_ok && after_ok {
            offsets.push(pos);
        }
        start = pos + olen;
    }
    offsets
}

/// Replace every (digit-boundary) occurrence of `old` with `new`, but only if
/// exactly `expected` occurrences are present. A mismatch is an error (format
/// drift / wrong count) and leaves the content untouched.
fn replace_exact(content: &str, old: &str, new: &str, expected: usize) -> Result<String, String> {
    let offsets = flanked_match_offsets(content, old);
    if offsets.len() != expected {
        return Err(format!(
            "expected {expected} occurrence(s) of `{old}`, found {}",
            offsets.len()
        ));
    }
    let mut out = String::with_capacity(content.len());
    let mut last = 0;
    for pos in offsets {
        out.push_str(&content[last..pos]);
        out.push_str(new);
        last = pos + old.len();
    }
    out.push_str(&content[last..]);
    Ok(out)
}

/// Substitute the version inside a path (for files whose name embeds the version).
fn rename_path(path: &str, old: &str, new: &str) -> String {
    path.replace(old, new)
}

/// The full manifest of files to edit for a version bump from `old`.
/// The rockspec path embeds `old`, so it is built from the current version.
fn release_targets(old: &str) -> Vec<Target> {
    let t = |path: &str, expected: usize| Target {
        path: path.to_string(),
        expected,
        rename: false,
    };
    vec![
        // Root manifest: [workspace.package] version, [package] version, and
        // the libbeachcomber path-dep pin. The lockfile carries beachcomber,
        // libbeachcomber, and libbeachcomber-ffi (workspace-inherited).
        t("Cargo.toml", 3),
        t("Cargo.lock", 3),
        t("libbeachcomber/Cargo.toml", 1),
        t("sdks/node/package.json", 1),
        t("sdks/node/package-lock.json", 2),
        t("sdks/python/pyproject.toml", 1),
        t("sdks/ruby/libbeachcomber.gemspec", 1),
        t("packaging/aur/beachcomber/PKGBUILD", 1),
        t("packaging/aur/beachcomber-bin/PKGBUILD", 1),
        t("packaging/aur/libbeachcomber/PKGBUILD", 1),
        t("packaging/nix/flake.nix", 1),
        t(".github/workflows/release.yml", 1),
        t("README.md", 8),
        Target {
            path: format!("sdks/lua/rockspec/libbeachcomber-{old}-1.rockspec"),
            expected: 2,
            rename: true,
        },
    ]
}

// ---------------------------------------------------------------------------
// IO shell
// ---------------------------------------------------------------------------

fn repo_root() -> PathBuf {
    // xtask lives at <root>/xtask; its parent is the workspace root, regardless
    // of the caller's working directory.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask crate has a parent directory")
        .to_path_buf()
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<(), String> {
    let mut positional = Vec::new();
    let mut dry_run = false;
    let mut no_verify = false;
    let mut check = false;
    for a in args {
        match a.as_str() {
            "--dry-run" => dry_run = true,
            "--no-verify" => no_verify = true,
            "--check" => check = true,
            s if s.starts_with("--") => return Err(format!("unknown flag: {s}")),
            s => positional.push(s.to_string()),
        }
    }

    match positional.first().map(String::as_str) {
        Some("set-version") => {
            let new = positional
                .get(1)
                .ok_or("set-version requires a <X.Y.Z> argument")?;
            set_version(new, dry_run, no_verify)
        }
        Some("gen-header") => gen_header(check),
        Some(other) => Err(format!(
            "unknown task: {other}\n\nUsage:\n  cargo xtask set-version <X.Y.Z> [--dry-run] [--no-verify]\n  cargo xtask gen-header [--check]"
        )),
        None => Err(
            "no task given\n\nUsage:\n  cargo xtask set-version <X.Y.Z> [--dry-run] [--no-verify]\n  cargo xtask gen-header [--check]"
                .into(),
        ),
    }
}

/// Regenerates `libbeachcomber-ffi/include/beachcomber.h` via the `cbindgen`
/// CLI (must be installed separately — see the module doc). `--check` uses
/// cbindgen's own `--verify` mode: it compares the freshly generated header
/// against the committed one and fails without writing anything if they
/// differ, which is what CI's freshness check runs.
fn gen_header(check: bool) -> Result<(), String> {
    let root = repo_root();
    let crate_dir = root.join("libbeachcomber-ffi");
    let config = crate_dir.join("cbindgen.toml");
    let output = crate_dir.join("include/beachcomber.h");

    let mut cmd = std::process::Command::new("cbindgen");
    cmd.arg("--crate")
        .arg("libbeachcomber-ffi")
        .arg("--config")
        .arg(&config)
        .arg("--output")
        .arg(&output);
    if check {
        cmd.arg("--verify");
    }
    cmd.arg(&crate_dir);

    let status = cmd.status().map_err(|e| {
        format!("running cbindgen (install with `cargo install cbindgen --locked`): {e}")
    })?;
    if !status.success() {
        return Err(if check {
            "generated header does not match the committed libbeachcomber-ffi/include/beachcomber.h -- run `cargo xtask gen-header` and commit the result".into()
        } else {
            "cbindgen failed".into()
        });
    }
    if !check {
        println!("Wrote {}", output.display());
    }
    Ok(())
}

fn set_version(new: &str, dry_run: bool, no_verify: bool) -> Result<(), String> {
    if !is_valid_version(new) {
        return Err(format!("`{new}` is not a MAJOR.MINOR.PATCH version"));
    }
    let root = repo_root();
    let cargo_toml = std::fs::read_to_string(root.join("Cargo.toml"))
        .map_err(|e| format!("reading Cargo.toml: {e}"))?;
    let old = parse_current_version(&cargo_toml)?;
    if old == new {
        return Err(format!("version is already {new}"));
    }

    let targets = release_targets(&old);

    // Pass 1: read + validate every file (count guard) before writing anything.
    let mut planned: Vec<(Target, String)> = Vec::with_capacity(targets.len());
    for t in &targets {
        let abs = root.join(&t.path);
        let content =
            std::fs::read_to_string(&abs).map_err(|e| format!("reading {}: {e}", t.path))?;
        let updated = replace_exact(&content, &old, new, t.expected)
            .map_err(|e| format!("{}: {e}", t.path))?;
        planned.push((t.clone(), updated));
    }

    println!("Bumping {old} -> {new} across {} files:", planned.len());
    for (t, _) in &planned {
        let note = if t.rename { "  (+ rename)" } else { "" };
        println!("  {} ({}×){note}", t.path, t.expected);
    }

    if dry_run {
        println!("\n--dry-run: no files written.");
        return Ok(());
    }

    // Pass 2: write (and rename where required).
    for (t, content) in &planned {
        if t.rename {
            let new_path = rename_path(&t.path, &old, new);
            std::fs::write(root.join(&new_path), content)
                .map_err(|e| format!("writing {new_path}: {e}"))?;
            if new_path != t.path {
                std::fs::remove_file(root.join(&t.path))
                    .map_err(|e| format!("removing old {}: {e}", t.path))?;
            }
        } else {
            std::fs::write(root.join(&t.path), content)
                .map_err(|e| format!("writing {}: {e}", t.path))?;
        }
    }

    if no_verify {
        println!("\nDone (--no-verify: skipped cargo check).");
    } else {
        println!("\nRunning `cargo check` to validate the workspace + refresh Cargo.lock...");
        let status = std::process::Command::new("cargo")
            .arg("check")
            .current_dir(&root)
            .status()
            .map_err(|e| format!("running cargo check: {e}"))?;
        if !status.success() {
            return Err("cargo check failed after version bump".into());
        }
        println!("Done.");
    }
    println!("\nReminder: update CHANGELOG.md release notes by hand.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_current_version_reads_package_version() {
        let toml = "[package]\nname = \"beachcomber\"\nversion = \"0.6.1\"\nedition = \"2024\"\n";
        assert_eq!(parse_current_version(toml).unwrap(), "0.6.1");
    }

    #[test]
    fn parse_current_version_ignores_dependency_versions() {
        // Dependency lines must not be mistaken for the package version.
        let toml = "[package]\nversion = \"0.6.1\"\n\n[dependencies]\ntokio = { version = \"1\" }\nserde = \"1\"\n";
        assert_eq!(parse_current_version(toml).unwrap(), "0.6.1");
    }

    #[test]
    fn parse_current_version_errors_when_absent() {
        let toml = "[package]\nname = \"x\"\n";
        assert!(parse_current_version(toml).is_err());
    }

    #[test]
    fn is_valid_version_accepts_triple() {
        assert!(is_valid_version("0.7.0"));
        assert!(is_valid_version("1.0.0"));
        assert!(is_valid_version("10.20.30"));
    }

    #[test]
    fn is_valid_version_rejects_malformed() {
        assert!(!is_valid_version("0.7"));
        assert!(!is_valid_version("0.7.0.0"));
        assert!(!is_valid_version("x.y.z"));
        assert!(!is_valid_version("0.7.0-1"));
        assert!(!is_valid_version(""));
        assert!(!is_valid_version("0..0"));
    }

    #[test]
    fn replace_exact_replaces_all_when_count_matches() {
        let out = replace_exact("a 0.6.1 b 0.6.1", "0.6.1", "0.7.0", 2).unwrap();
        assert_eq!(out, "a 0.7.0 b 0.7.0");
    }

    #[test]
    fn replace_exact_errors_when_too_few() {
        let err = replace_exact("only 0.6.1 here", "0.6.1", "0.7.0", 2).unwrap_err();
        assert!(err.contains("expected 2"), "got: {err}");
        assert!(err.contains("found 1"), "got: {err}");
    }

    #[test]
    fn replace_exact_errors_when_too_many() {
        assert!(replace_exact("0.6.1 0.6.1 0.6.1", "0.6.1", "0.7.0", 2).is_err());
    }

    #[test]
    fn replace_exact_errors_when_absent() {
        assert!(replace_exact("no version here", "0.6.1", "0.7.0", 1).is_err());
    }

    #[test]
    fn replace_exact_ignores_digit_flanked_matches() {
        // `0.6.1` must not match inside the longer version `0.6.11`.
        let content = "version = \"0.6.1\"\ndep = \"0.6.11\"";
        let out = replace_exact(content, "0.6.1", "0.7.0", 1).unwrap();
        assert_eq!(out, "version = \"0.7.0\"\ndep = \"0.6.11\"");
    }

    #[test]
    fn replace_exact_ignores_match_preceded_by_digit() {
        // A version embedded in a larger number (`10.6.1`) is not the package version.
        let out = replace_exact("x10.6.1y 0.6.1", "0.6.1", "0.7.0", 1).unwrap();
        assert_eq!(out, "x10.6.1y 0.7.0");
    }

    #[test]
    fn replace_exact_preserves_packaging_suffix() {
        // The bare token is a substring of the packaging form; replacing it
        // preserves the `-1` revision and `v` tag prefix.
        let out = replace_exact(
            "url .../beachcomber_0.6.1-1_amd64.deb tag = \"v0.6.1\"",
            "0.6.1",
            "0.7.0",
            2,
        )
        .unwrap();
        assert_eq!(
            out,
            "url .../beachcomber_0.7.0-1_amd64.deb tag = \"v0.7.0\""
        );
    }

    #[test]
    fn rename_path_substitutes_version_in_filename() {
        assert_eq!(
            rename_path(
                "sdks/lua/rockspec/libbeachcomber-0.6.1-1.rockspec",
                "0.6.1",
                "0.7.0"
            ),
            "sdks/lua/rockspec/libbeachcomber-0.7.0-1.rockspec"
        );
    }

    #[test]
    fn release_targets_covers_the_full_manifest() {
        let t = release_targets("0.6.1");
        let paths: Vec<&str> = t.iter().map(|x| x.path.as_str()).collect();
        for expected in [
            "Cargo.toml",
            "Cargo.lock",
            "libbeachcomber/Cargo.toml",
            "sdks/node/package.json",
            "sdks/node/package-lock.json",
            "sdks/python/pyproject.toml",
            "sdks/ruby/libbeachcomber.gemspec",
            "packaging/aur/beachcomber/PKGBUILD",
            "packaging/aur/beachcomber-bin/PKGBUILD",
            "packaging/aur/libbeachcomber/PKGBUILD",
            "packaging/nix/flake.nix",
            ".github/workflows/release.yml",
            "README.md",
            "sdks/lua/rockspec/libbeachcomber-0.6.1-1.rockspec",
        ] {
            assert!(paths.contains(&expected), "missing target: {expected}");
        }
        assert_eq!(t.len(), 14, "manifest should be exactly 14 files");
    }

    #[test]
    fn release_targets_rockspec_is_the_only_rename() {
        let t = release_targets("0.6.1");
        let renames: Vec<&str> = t
            .iter()
            .filter(|x| x.rename)
            .map(|x| x.path.as_str())
            .collect();
        assert_eq!(
            renames,
            vec!["sdks/lua/rockspec/libbeachcomber-0.6.1-1.rockspec"]
        );
    }

    #[test]
    fn release_targets_expected_counts_are_right() {
        let t = release_targets("0.6.1");
        let by_path = |p: &str| t.iter().find(|x| x.path == p).unwrap().expected;
        assert_eq!(by_path("README.md"), 8);
        assert_eq!(by_path("Cargo.lock"), 3);
        assert_eq!(by_path("sdks/node/package-lock.json"), 2);
        assert_eq!(
            by_path("sdks/lua/rockspec/libbeachcomber-0.6.1-1.rockspec"),
            2
        );
        assert_eq!(by_path("Cargo.toml"), 3);
        // Total occurrences the tool will rewrite across the tree.
        let total: usize = t.iter().map(|x| x.expected).sum();
        assert_eq!(total, 27);
    }
}
