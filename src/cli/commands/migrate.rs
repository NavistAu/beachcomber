//! Handler for the `migrate` subcommand: bring config files to this comb's
//! schema ([`crate::config::migrate`]), printing a diff of each.

use crate::cli::exit::Outcome;
use crate::config::Config;
use crate::config::migrate::{Migrator, bak_of};
use std::io::Write;
use std::path::{Path, PathBuf};

pub fn run_migrate(files: &[PathBuf], dry_run: bool) -> Outcome {
    let out = &mut std::io::stdout().lock();
    migrate(
        &Migrator::default(),
        &Config::config_path(),
        files,
        dry_run,
        out,
    )
}

/// Migrate `files`, or, with none, every file beside `config_path` migration
/// is on for, writing a unified diff of each change to `out` and, unless
/// `dry_run`, the change itself. A file that cannot be migrated is an error
/// and the rest go on.
pub fn migrate(
    migrator: &Migrator,
    config_path: &Path,
    files: &[PathBuf],
    dry_run: bool,
    out: &mut impl Write,
) -> Outcome {
    let mut outcome = Outcome::Done;
    let files = if files.is_empty() {
        let (selected, errors) = migrator.selected(config_path);
        for e in errors {
            crate::cli::report::error_named(e);
            outcome = Outcome::Error;
        }
        selected
    } else {
        files.to_vec()
    };
    let mut changed = 0;
    for file in &files {
        let change = match migrator.migrate(file, !dry_run) {
            Ok(Some(change)) => change,
            Ok(None) => continue,
            Err(e) => {
                crate::cli::report::error(&Config::error_label(file), e);
                outcome = Outcome::Error;
                continue;
            }
        };
        changed += 1;
        let shown = file.display();
        let diff = similar::TextDiff::from_lines(&change.before, &change.after);
        let _ = write!(
            out,
            "{}",
            diff.unified_diff().header(
                &format!("{shown} (schema {})", change.from),
                &format!("{shown} (schema {})", change.to),
            )
        );
        if !dry_run {
            let _ = writeln!(
                out,
                "migrated {shown}; the original is kept as {}",
                bak_of(&change.target).display()
            );
        }
    }
    if changed == 0 && outcome == Outcome::Done {
        let _ = writeln!(
            out,
            "nothing to migrate: config is at schema {}",
            migrator.current()
        );
    }
    outcome
}
