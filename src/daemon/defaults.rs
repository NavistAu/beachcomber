//! Managed defaults (spec §3.3): the built-in templates are configuration
//! files the daemon writes to `<config-dir>/defaults/` and seeds like any
//! other layer, not a table compiled into a client.
//!
//! The files are embedded in the binary so they can be written, and for no
//! other reason: nothing is loaded from the embed. Configuration is read from the
//! files on disk, the abstract provider in `00-comb.toml` included. Each
//! written file starts with the marker
//! `# Managed by comb <version>`, which is how a later daemon tells a file it
//! wrote from one the user put there, and a file from this version from one a
//! previous version left behind.
//!
//! Per shipped name in the directory: absent → written; first line is a marker
//! for another version → rewritten; first line is not a marker at all →
//! rewritten (the name is shipped, so the file is managed); first line is this
//! version's marker → untouched, even if the body was edited. Per other
//! `*.toml`: first line a marker → deleted, it is a default this version no
//! longer ships; otherwise → left alone and reported as unmanaged. Files that
//! are not `*.toml` are ignored.

use crate::config::Config;
use std::path::{Path, PathBuf};

/// The first line of every file the daemon writes, followed by its version.
pub const MARKER_PREFIX: &str = "# Managed by comb ";

/// (file name, embedded body) for every shipped default, lexical by name.
pub const SHIPPED: &[(&str, &str)] = &[
    ("00-comb.toml", include_str!("../../defaults/00-comb.toml")),
    ("aws.toml", include_str!("../../defaults/aws.toml")),
    ("conda.toml", include_str!("../../defaults/conda.toml")),
    ("gcloud.toml", include_str!("../../defaults/gcloud.toml")),
    (
        "hostname.toml",
        include_str!("../../defaults/hostname.toml"),
    ),
    (
        "kubecontext.toml",
        include_str!("../../defaults/kubecontext.toml"),
    ),
    ("load.toml", include_str!("../../defaults/load.toml")),
    ("op.toml", include_str!("../../defaults/op.toml")),
    ("python.toml", include_str!("../../defaults/python.toml")),
    ("talos.toml", include_str!("../../defaults/talos.toml")),
    (
        "terraform.toml",
        include_str!("../../defaults/terraform.toml"),
    ),
    ("uname.toml", include_str!("../../defaults/uname.toml")),
    ("uptime.toml", include_str!("../../defaults/uptime.toml")),
    ("user.toml", include_str!("../../defaults/user.toml")),
];

/// Every shipped file as a configuration layer, in the order the daemon
/// composes them, each labelled as the file the daemon writes it to would be
/// (`/defaults/<name>`): the lowest layers of a `Config` built in memory (a
/// test, a bench, an embedder with no `defaults/` directory behind it), so
/// the shipped providers and templates are there as they are in a running
/// daemon. The daemon itself reads the files on disk.
pub fn shipped_layers() -> Vec<(PathBuf, toml::Value)> {
    SHIPPED
        .iter()
        .map(|(name, text)| layer(name, text))
        .collect()
}

/// The first of [`shipped_layers`], `00-comb.toml`, alone: the abstract
/// provider, for what is about it and no shipped provider.
pub fn comb_layer() -> (PathBuf, toml::Value) {
    let (name, text) = SHIPPED[0];
    debug_assert_eq!(name, "00-comb.toml");
    layer(name, text)
}

/// The shipped file `name`, its body `text`, as a layer.
fn layer(name: &str, text: &str) -> (PathBuf, toml::Value) {
    (
        PathBuf::from("/defaults").join(name),
        toml::from_str(text).unwrap_or_else(|e| panic!("defaults/{name} does not parse: {e}")),
    )
}

/// What one run of [`write_managed_defaults`] did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub dir: PathBuf,
    /// Names written or rewritten this run.
    pub written: Vec<String>,
    /// Retired managed files removed.
    pub deleted: Vec<String>,
    /// Files in the directory comb did not write.
    pub unmanaged: Vec<String>,
    /// Shipped files present after the run.
    pub files: usize,
}

/// What [`write_managed_defaults`] would do to `dir`, read without touching
/// it — `comb check config` reports this.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Audit {
    /// Shipped names that are absent or whose first line is not this version's marker.
    pub stale: Vec<String>,
    /// Other `*.toml` files carrying a marker: defaults this version no longer ships.
    pub retired: Vec<String>,
    /// Other `*.toml` files without a marker: not comb's to touch.
    pub unmanaged: Vec<String>,
}

/// `true` when the first line of `body` is [`MARKER_PREFIX`] followed by `version`.
pub fn is_current(body: &str, version: &str) -> bool {
    body.lines().next() == Some(&format!("{MARKER_PREFIX}{version}"))
}

/// Whether `body` starts with a marker line, for any version.
fn is_managed(body: &str) -> bool {
    body.lines()
        .next()
        .is_some_and(|l| l.starts_with(MARKER_PREFIX))
}

/// Read `dir` against the shipped set for `version`. A missing or unreadable
/// directory audits as every shipped name stale and nothing else.
pub fn audit(dir: &Path, version: &str) -> Audit {
    let mut audit = Audit::default();
    for (name, _) in SHIPPED {
        let current = std::fs::read_to_string(dir.join(name))
            .map(|body| is_current(&body, version))
            .unwrap_or(false);
        if !current {
            audit.stale.push((*name).to_string());
        }
    }
    for file in Config::conf_d_files(dir) {
        let Some(name) = file.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if SHIPPED.iter().any(|(shipped, _)| *shipped == name) {
            continue;
        }
        let managed = std::fs::read_to_string(&file)
            .map(|body| is_managed(&body))
            .unwrap_or(false);
        if managed {
            audit.retired.push(name.to_string());
        } else {
            audit.unmanaged.push(name.to_string());
        }
    }
    audit
}

/// Bring `dir` to the shipped set for `version`, creating it if absent. Each
/// file is written to `<name>.tmp` and renamed over `<name>`, so a reader
/// never sees a half-written default. Any io error propagates; the caller
/// decides — the daemon refuses to start on one.
pub fn write_managed_defaults(dir: &Path, version: &str) -> std::io::Result<Report> {
    std::fs::create_dir_all(dir)?;
    let audit = audit(dir, version);
    for name in &audit.stale {
        let (_, body) = SHIPPED
            .iter()
            .find(|(shipped, _)| shipped == name)
            .expect("audit only names shipped files as stale");
        let tmp = dir.join(format!("{name}.tmp"));
        std::fs::write(&tmp, format!("{MARKER_PREFIX}{version}\n{body}"))?;
        std::fs::rename(&tmp, dir.join(name))?;
    }
    for name in &audit.retired {
        std::fs::remove_file(dir.join(name))?;
    }
    Ok(Report {
        dir: dir.to_path_buf(),
        written: audit.stale,
        deleted: audit.retired,
        unmanaged: audit.unmanaged,
        files: SHIPPED.len(),
    })
}
