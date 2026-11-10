//! Managed defaults (spec §3.3): the shipped providers and templates are
//! configuration files the daemon writes to `<config-dir>/defaults/` and seeds like any
//! other layer, not a table compiled into a client.
//!
//! The files are embedded in the binary so they can be written. Configuration
//! is what [`plan`] says the directory holds once the daemon has written it:
//! the files on disk, except that a file the daemon is about to write is read
//! as the text it will write (so `comb check config`, which writes nothing,
//! loads what a daemon start would), the abstract provider in `00-comb.toml`
//! included. Each
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
    ("apk.toml", include_str!("../../defaults/apk.toml")),
    ("apt.toml", include_str!("../../defaults/apt.toml")),
    ("asdf.toml", include_str!("../../defaults/asdf.toml")),
    ("aws.toml", include_str!("../../defaults/aws.toml")),
    ("battery.toml", include_str!("../../defaults/battery.toml")),
    ("brew.toml", include_str!("../../defaults/brew.toml")),
    ("conda.toml", include_str!("../../defaults/conda.toml")),
    (
        "containers.toml",
        include_str!("../../defaults/containers.toml"),
    ),
    ("direnv.toml", include_str!("../../defaults/direnv.toml")),
    ("dnf.toml", include_str!("../../defaults/dnf.toml")),
    ("docker.toml", include_str!("../../defaults/docker.toml")),
    ("gcloud.toml", include_str!("../../defaults/gcloud.toml")),
    ("git.toml", include_str!("../../defaults/git.toml")),
    ("gpg.toml", include_str!("../../defaults/gpg.toml")),
    (
        "hostname.toml",
        include_str!("../../defaults/hostname.toml"),
    ),
    (
        "kerberos.toml",
        include_str!("../../defaults/kerberos.toml"),
    ),
    (
        "kubecontext.toml",
        include_str!("../../defaults/kubecontext.toml"),
    ),
    ("load.toml", include_str!("../../defaults/load.toml")),
    ("mise.toml", include_str!("../../defaults/mise.toml")),
    ("network.toml", include_str!("../../defaults/network.toml")),
    ("op.toml", include_str!("../../defaults/op.toml")),
    ("pacman.toml", include_str!("../../defaults/pacman.toml")),
    ("podman.toml", include_str!("../../defaults/podman.toml")),
    ("python.toml", include_str!("../../defaults/python.toml")),
    ("ssh.toml", include_str!("../../defaults/ssh.toml")),
    ("sudo.toml", include_str!("../../defaults/sudo.toml")),
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
    let parsed = SHIPPED
        .iter()
        .position(|(n, t)| *n == name && *t == text)
        .map(|i| parsed_shipped()[i].clone())
        .unwrap_or_else(|| {
            toml::from_str(text).unwrap_or_else(|e| panic!("defaults/{name} does not parse: {e}"))
        });
    (PathBuf::from("/defaults").join(name), parsed)
}

/// Every [`SHIPPED`] body parsed, once per process: every load reads them,
/// and a clone costs a fraction of a parse.
fn parsed_shipped() -> &'static [toml::Value] {
    static PARSED: std::sync::OnceLock<Vec<toml::Value>> = std::sync::OnceLock::new();
    PARSED.get_or_init(|| {
        SHIPPED
            .iter()
            .map(|(name, text)| {
                toml::from_str(text)
                    .unwrap_or_else(|e| panic!("defaults/{name} does not parse: {e}"))
            })
            .collect()
    })
}

/// `content`, the text of the defaults file `name`, parsed, when it is the
/// shipped file exactly as this version writes it: the parse kept from the
/// first load. `None` for any other text, which the caller parses.
pub fn parsed_as_written(name: &str, content: &str) -> Option<toml::Value> {
    let i = SHIPPED.iter().position(|(n, _)| *n == name)?;
    let body = content
        .strip_prefix(MARKER_PREFIX)?
        .strip_prefix(VERSION)?
        .strip_prefix('\n')?;
    (body == SHIPPED[i].1).then(|| parsed_shipped()[i].clone())
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

/// This build's version: the one the daemon writes into its managed files'
/// markers and `comb check config` judges them against.
pub const VERSION: &str = env!("BEACHCOMBER_VERSION");

/// One `*.toml` the daemon will load from the defaults directory.
#[derive(Debug, PartialEq, Eq)]
pub struct Planned {
    pub name: String,
    /// Whether this version ships the name (else it is a file comb did not write).
    pub shipped: bool,
    /// `Some(text)`: the daemon writes this over the file (or creates it) on
    /// start and then loads it. `None`: the file on disk is loaded as it is.
    pub write: Option<String>,
}

/// The daemon's start-up decision for a defaults directory, read without
/// touching it: which files it writes, which it removes, and the layers it
/// then loads. [`write_managed_defaults`] carries it out and
/// [`crate::config::Config`] composes from [`Plan::loaded`], so the daemon and
/// `comb check config` share this one decision.
#[derive(Debug, PartialEq, Eq)]
pub struct Plan {
    pub dir: PathBuf,
    /// Every file loaded, lexical by name: the shipped names and any other `*.toml` left alone.
    pub files: Vec<Planned>,
    /// Other `*.toml` files carrying a marker: defaults this version no longer ships. Removed, not loaded.
    pub retired: Vec<String>,
}

impl Plan {
    /// Shipped names the daemon writes: absent or not this version's marker on the first line.
    pub fn stale(&self) -> Vec<String> {
        self.files
            .iter()
            .filter(|f| f.shipped && f.write.is_some())
            .map(|f| f.name.clone())
            .collect()
    }

    /// Other `*.toml` files without a marker: not comb's to touch, loaded as they are.
    pub fn unmanaged(&self) -> Vec<String> {
        self.files
            .iter()
            .filter(|f| !f.shipped)
            .map(|f| f.name.clone())
            .collect()
    }

    /// The files in load order, each with the text standing in for the file
    /// when the daemon is about to write it (`None`: read it from disk).
    pub fn loaded(&self) -> Vec<(PathBuf, Option<&str>)> {
        self.files
            .iter()
            .map(|f| (self.dir.join(&f.name), f.write.as_deref()))
            .collect()
    }
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

/// Decide what a daemon of `version` does with `dir` and loads from it. A
/// missing or unreadable directory plans every shipped name written and
/// nothing else.
pub fn plan(dir: &Path, version: &str) -> Plan {
    let mut files: Vec<Planned> = SHIPPED
        .iter()
        .map(|(name, body)| {
            let current = std::fs::read_to_string(dir.join(name))
                .map(|text| is_current(&text, version))
                .unwrap_or(false);
            Planned {
                name: (*name).to_string(),
                shipped: true,
                write: (!current).then(|| format!("{MARKER_PREFIX}{version}\n{body}")),
            }
        })
        .collect();
    let mut retired = Vec::new();
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
            retired.push(name.to_string());
        } else {
            files.push(Planned {
                name: name.to_string(),
                shipped: false,
                write: None,
            });
        }
    }
    files.sort_by(|a, b| a.name.cmp(&b.name));
    Plan {
        dir: dir.to_path_buf(),
        files,
        retired,
    }
}

/// Bring `dir` to the shipped set for `version`, creating it if absent: carry
/// out [`plan`]. Each file is written to `<name>.tmp` and renamed over
/// `<name>`, so a reader never sees a half-written default; a directory not
/// there yet is written whole beside it and renamed into place. Any io error
/// propagates; the caller decides — the daemon refuses to start on one.
pub fn write_managed_defaults(dir: &Path, version: &str) -> std::io::Result<Report> {
    let plan = plan(dir, version);
    if !dir.exists() && write_fresh(dir, &plan).is_ok() {
        return Ok(report(plan));
    }
    std::fs::create_dir_all(dir)?;
    for file in &plan.files {
        let Some(text) = &file.write else { continue };
        let tmp = dir.join(format!("{}.tmp", file.name));
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, dir.join(&file.name))?;
    }
    for name in &plan.retired {
        std::fs::remove_file(dir.join(name))?;
    }
    Ok(report(plan))
}

/// A first start: every file written into a directory beside `dir` and that
/// renamed into place, one rename for the set rather than one per file. An
/// error (another process made `dir` first, say) leaves `dir` to the
/// per-file path.
fn write_fresh(dir: &Path, plan: &Plan) -> std::io::Result<()> {
    let parent = dir.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(".defaults.{}.tmp", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir(&tmp)?;
    let written = plan.files.iter().try_for_each(|file| match &file.write {
        Some(text) => std::fs::write(tmp.join(&file.name), text),
        None => Ok(()),
    });
    let renamed = written.and_then(|()| std::fs::rename(&tmp, dir));
    if renamed.is_err() {
        let _ = std::fs::remove_dir_all(&tmp);
    }
    renamed
}

fn report(plan: Plan) -> Report {
    Report {
        dir: plan.dir.clone(),
        written: plan.stale(),
        unmanaged: plan.unmanaged(),
        deleted: plan.retired,
        files: SHIPPED.len(),
    }
}
