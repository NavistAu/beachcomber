//! Config format migrations. Every config file carries its format's version,
//! `schema.version` (absent is 1), written as dotted keys before its first
//! table header; [`MIGRATIONS`] takes a file from one schema to the next, and
//! the current schema is the one past the last of them. A file behind it is
//! migrated when it is loaded ([`Migrator::load`]) or by `comb migrate`, unless
//! `schema.migrate = false` turns that off for it: set in `config.toml`, the
//! default for every `conf.d/` file, each of which may set its own.
//!
//! A migration rewrites the file in place: under a lock, re-read, a temporary
//! beside the file renamed over it, the original kept as `<file>.bak`. A
//! symlink is written through to its target. The files migrated and the
//! schema each reached are remembered, so a file found behind again (a
//! dotfiles manager restored it) is migrated again with a warning to update
//! its source.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One config format change, from schema N to N+1.
pub struct Migration {
    /// A config file's text at schema N, rewritten to N+1, its formatting and
    /// comments kept; `Err` says why it cannot be. The stamp is not this
    /// function's: [`Migrator::rewrite`] writes it after the last migration.
    pub file: fn(&str) -> Result<String, String>,
    /// One template string at schema N, rewritten to N+1: for the templates
    /// comb keeps outside config files, the persisted `:template` puts
    /// ([`template`]).
    pub template: fn(&str) -> String,
}

/// Every migration, in order: the first takes schema 1 to 2, the next 2 to 3.
/// None is ever removed.
pub static MIGRATIONS: &[Migration] = &[
    // 1 → 2: comb's functions are called through `_` (`lup()` is `_.lup()`).
    Migration {
        file: |text| templates_in_file(text, underscore_calls),
        template: |text| splice(text, &underscore_calls(text)),
    },
];

/// Schema 1 to 2 of one template: where each bare call of a library function
/// starts ([`libbeachcomber::eval::bare_calls`]), for `_.` to go in front.
/// Filters, strings, comments, a name already after a `.` and a name the
/// template declares itself are not calls of comb's.
fn underscore_calls(text: &str) -> Vec<usize> {
    libbeachcomber::eval::bare_calls(&libbeachcomber::engine::engine_env(), text)
        .into_iter()
        .map(|(at, _)| at)
        .collect()
}

/// `text` with `_.` inserted at each of `at`, offsets into `text`.
fn splice(text: &str, at: &[usize]) -> String {
    let mut out = text.to_string();
    let mut at = at.to_vec();
    at.sort_unstable();
    for &i in at.iter().rev() {
        out.insert_str(i, "_.");
    }
    out
}

/// `text`, a config file, with `_.` inserted where `calls` says in each
/// string in it that holds a tag (`{{` or `{%`): every template comb reads
/// from a config file, whatever its key. `calls` reads the string's value;
/// the `_.` goes in at the same place of the string as the file writes it,
/// so its quoting, its escapes and everything around it are kept.
fn templates_in_file(text: &str, calls: fn(&str) -> Vec<usize>) -> Result<String, String> {
    use toml::Spanned;
    use toml::de::{DeTable, DeValue};
    fn walk<'a>(value: &'a Spanned<DeValue<'a>>, out: &mut Vec<&'a Spanned<DeValue<'a>>>) {
        match value.get_ref() {
            DeValue::String(_) => out.push(value),
            DeValue::Array(items) => items.iter().for_each(|v| walk(v, out)),
            DeValue::Table(table) => table.values().for_each(|v| walk(v, out)),
            _ => {}
        }
    }
    let doc = DeTable::parse(text).map_err(|e| e.to_string())?;
    let mut strings = Vec::new();
    doc.get_ref().values().for_each(|v| walk(v, &mut strings));
    let mut at = Vec::new();
    for string in strings {
        let DeValue::String(value) = string.get_ref() else {
            continue;
        };
        if !(value.contains("{{") || value.contains("{%")) {
            continue;
        }
        let span = string.span();
        let offsets = toml_string(&text[span.clone()])
            .filter(|(read, _)| read == value.as_ref())
            .map(|(_, offsets)| offsets)
            .ok_or_else(|| format!("could not read the string at byte {}", span.start))?;
        at.extend(calls(value).into_iter().map(|i| span.start + offsets[i]));
    }
    Ok(splice(text, &at))
}

/// A TOML string literal as a file writes it, read: its value, and for each
/// byte of the value the offset in `raw` it is written at (an escape's bytes
/// are all at its backslash). `None` when `raw` is no string literal.
fn toml_string(raw: &str) -> Option<(String, Vec<usize>)> {
    let (quote, open) = if raw.starts_with("\"\"\"") {
        ('"', 3)
    } else if raw.starts_with("'''") {
        ('\'', 3)
    } else {
        (raw.chars().next().filter(|c| matches!(c, '"' | '\''))?, 1)
    };
    let multi = open == 3;
    let body = raw.get(open..raw.len().checked_sub(open)?)?;
    // A newline right after a multi-line string's opening quotes is not its
    // value's.
    let skip = match body {
        b if multi && b.starts_with("\r\n") => 2,
        b if multi && b.starts_with('\n') => 1,
        _ => 0,
    };
    let (mut value, mut offsets) = (String::new(), Vec::new());
    let mut chars = body
        .char_indices()
        .skip_while(|(i, _)| *i < skip)
        .peekable();
    while let Some((at, c)) = chars.next() {
        let decoded = if c != '\\' || quote == '\'' {
            c
        } else {
            let (_, e) = chars.next()?;
            let mut hex = |n: usize| {
                let digits: String = (0..n)
                    .filter_map(|_| chars.next().map(|(_, c)| c))
                    .collect();
                u32::from_str_radix(&digits, 16)
                    .ok()
                    .and_then(char::from_u32)
            };
            match e {
                'b' => '\u{8}',
                't' => '\t',
                'n' => '\n',
                'f' => '\u{c}',
                'r' => '\r',
                'e' => '\u{1b}',
                '"' => '"',
                '\\' => '\\',
                'x' => hex(2)?,
                'u' => hex(4)?,
                'U' => hex(8)?,
                // A line-ending backslash: the whitespace after it is not
                // the value's.
                c if multi && c.is_whitespace() => {
                    while chars.next_if(|(_, c)| c.is_whitespace()).is_some() {}
                    continue;
                }
                _ => return None,
            }
        };
        offsets.extend(std::iter::repeat_n(open + at, decoded.len_utf8()));
        value.push(decoded);
    }
    Some((value, offsets))
}

/// The schema this comb reads and writes: the one past the last migration.
pub fn current() -> i64 {
    current_of(MIGRATIONS)
}

fn current_of(migrations: &[Migration]) -> i64 {
    migrations.len() as i64 + 1
}

/// `text`, a template written at schema `from`, at the current schema.
pub fn template(text: &str, from: i64) -> String {
    template_with(MIGRATIONS, text, from)
}

fn template_with(migrations: &[Migration], text: &str, from: i64) -> String {
    let start = usize::try_from(from - 1).unwrap_or(0);
    migrations
        .iter()
        .skip(start)
        .fold(text.to_string(), |text, m| (m.template)(&text))
}

/// A file's `schema` table, read: its version, 1 when absent, and its
/// `migrate`, `None` when it does not say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Schema {
    pub version: i64,
    pub migrate: Option<bool>,
}

/// The `schema` table of a parsed file, or what is wrong with it.
pub fn schema(layer: &toml::Value) -> Result<Schema, String> {
    let mut out = Schema {
        version: 1,
        migrate: None,
    };
    let Some(table) = layer.get("schema") else {
        return Ok(out);
    };
    let table = table
        .as_table()
        .ok_or("schema must be a table: schema.version = <n>")?;
    for (key, value) in table {
        match (key.as_str(), value) {
            ("version", toml::Value::Integer(n)) if *n >= 1 => out.version = *n,
            ("version", _) => return Err("schema.version must be an integer of at least 1".into()),
            ("migrate", toml::Value::Boolean(b)) => out.migrate = Some(*b),
            ("migrate", _) => return Err("schema.migrate must be true or false".into()),
            (other, _) => {
                return Err(format!(
                    "unknown key schema.{other}: schema holds version and migrate"
                ));
            }
        }
    }
    Ok(out)
}

/// `text` with `schema.version = <version>`: the dotted key before the first
/// table header updated in place, its comment kept, or, where there is none,
/// inserted before the file's other `schema.` keys, or else at the top.
pub fn stamp(text: &str, version: i64) -> String {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let header = lines
        .iter()
        .position(|l| l.trim_start().starts_with('['))
        .unwrap_or(lines.len());
    let key_of = |line: &str| {
        let line = line.trim_start();
        line.strip_prefix("schema.").map(|rest| {
            let end = rest
                .find(|c: char| c == '=' || c.is_whitespace())
                .unwrap_or(rest.len());
            rest[..end].to_string()
        })
    };
    let mut out = String::with_capacity(text.len() + 24);
    if let Some(at) = (0..header).find(|&i| key_of(lines[i]).as_deref() == Some("version")) {
        for (i, line) in lines.iter().enumerate() {
            if i != at {
                out.push_str(line);
                continue;
            }
            let body = line.trim_end_matches(['\n', '\r']);
            let eol = &line[body.len()..];
            let indent = &body[..body.len() - body.trim_start().len()];
            // What follows the number (` # a comment`) is kept.
            let value = body.split_once('=').map_or("", |(_, v)| v.trim_start());
            let rest =
                value.trim_start_matches(|c: char| c.is_ascii_digit() || c == '_' || c == '+');
            out.push_str(&format!("{indent}schema.version = {version}{rest}{eol}"));
        }
        return out;
    }
    let at = (0..header)
        .find(|&i| key_of(lines[i]).is_some())
        .unwrap_or(0);
    out.extend(lines[..at].iter().copied());
    out.push_str(&format!("schema.version = {version}\n"));
    out.extend(lines[at..].iter().copied());
    out
}

/// One file migrated, or that would be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// The file as named.
    pub file: PathBuf,
    /// What was written: the file, its symlinks resolved.
    pub target: PathBuf,
    pub from: i64,
    pub to: i64,
    pub before: String,
    pub after: String,
    /// The file had been migrated past `from` before: something put it back.
    pub restored: bool,
}

/// What migrates: the migrations, and the directory the lock and the record
/// of migrated files live in.
pub struct Migrator {
    migrations: &'static [Migration],
    state_dir: PathBuf,
}

impl Default for Migrator {
    /// [`MIGRATIONS`], with the lock and record in
    /// `$XDG_STATE_HOME/beachcomber/`, beside the daemon's log.
    fn default() -> Self {
        let log = super::Config::log_path();
        let state_dir = log
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        Self::new(MIGRATIONS, state_dir)
    }
}

impl Migrator {
    pub fn new(migrations: &'static [Migration], state_dir: PathBuf) -> Self {
        Self {
            migrations,
            state_dir,
        }
    }

    /// The schema this migrator brings a file to.
    pub fn current(&self) -> i64 {
        current_of(self.migrations)
    }

    /// `text`, at schema `from`, through every migration since and stamped
    /// with the current schema. The result must parse and say it is current.
    pub fn rewrite(&self, text: &str, from: i64) -> Result<String, String> {
        let start = usize::try_from(from - 1).unwrap_or(0);
        let mut text = text.to_string();
        for (i, m) in self.migrations.iter().enumerate().skip(start) {
            text = (m.file)(&text)
                .map_err(|e| format!("migrating schema {} to {}: {e}", i + 1, i + 2))?;
        }
        let text = stamp(&text, self.current());
        let layer: toml::Value =
            toml::from_str(&text).map_err(|e| format!("the migrated file does not parse: {e}"))?;
        match schema(&layer) {
            Ok(s) if s.version == self.current() => Ok(text),
            _ => Err(
                "the migrated file's schema.version could not be stamped: write it as \
                 a dotted key before the first table"
                    .into(),
            ),
        }
    }

    /// A loaded file's layer, at the current schema: as it is, when it is;
    /// migrated and re-read when it is behind and migration is on for it
    /// (its own `schema.migrate`, else `inherited`). `Err` is the load error:
    /// a malformed `schema`, a file written for a newer comb, or one behind
    /// that is not migrated.
    pub fn load(
        &self,
        file: &Path,
        layer: toml::Value,
        inherited: bool,
    ) -> Result<(toml::Value, Option<Change>), String> {
        let label = super::Config::error_label(file);
        let s = schema(&layer).map_err(|e| format!("{label}: {e}"))?;
        let current = self.current();
        if s.version > current {
            return Err(newer(&label, s.version, current));
        }
        if s.version == current {
            return Ok((layer, None));
        }
        if !s.migrate.unwrap_or(inherited) {
            return Err(behind(
                &label,
                s.version,
                current,
                "migration is off for it (schema.migrate = false)",
            ));
        }
        match self.migrate(file, true) {
            // Migrated meanwhile by another process: re-read below.
            Ok(None) => {}
            Ok(Some(change)) => {
                let layer = toml::from_str(&change.after).map_err(|e| format!("{label}: {e}"))?;
                return Ok((layer, Some(change)));
            }
            Err(e) => return Err(behind(&label, s.version, current, &e)),
        }
        let text = std::fs::read_to_string(file).map_err(|e| format!("{label}: {e}"))?;
        let layer: toml::Value = toml::from_str(&text).map_err(|e| format!("{label}: {e}"))?;
        match schema(&layer) {
            Ok(s) if s.version == current => Ok((layer, None)),
            _ => Err(behind(
                &label,
                s.version,
                current,
                "it changed while migrating",
            )),
        }
    }

    /// Migrate `file` to the current schema, whatever its `schema.migrate`
    /// says: `None` when it is already there. With `write`, under the lock,
    /// re-read, the rewrite renamed over the file (its symlinks resolved) and
    /// the original kept as `<file>.bak`; without, nothing is written.
    pub fn migrate(&self, file: &Path, write: bool) -> Result<Option<Change>, String> {
        let target = std::fs::canonicalize(file).map_err(|e| e.to_string())?;
        let _lock = if write { Some(self.lock()?) } else { None };
        let before = std::fs::read_to_string(&target).map_err(|e| e.to_string())?;
        let layer: toml::Value = toml::from_str(&before).map_err(|e| e.to_string())?;
        let from = schema(&layer)?.version;
        let to = self.current();
        if from > to {
            return Err(newer(&super::Config::error_label(file), from, to));
        }
        if from == to {
            return Ok(None);
        }
        let after = self.rewrite(&before, from)?;
        let mut change = Change {
            file: file.to_path_buf(),
            target: target.clone(),
            from,
            to,
            before,
            after,
            restored: false,
        };
        if !write {
            return Ok(Some(change));
        }
        let key = target.display().to_string();
        let mut record = self.record();
        change.restored = record.get(&key).is_some_and(|&reached| reached > from);
        write_through(&target, &change.before, &change.after)?;
        record.insert(key, to);
        if let Err(e) = self.write_record(&record) {
            tracing::warn!("could not record migrated config files: {e}");
        }
        log(&change);
        Ok(Some(change))
    }

    /// An exclusive lock on `migrate.lock` in the state directory, held until
    /// the file is dropped: one process migrates at a time.
    fn lock(&self) -> Result<std::fs::File, String> {
        use std::os::fd::AsRawFd;
        std::fs::create_dir_all(&self.state_dir).map_err(|e| e.to_string())?;
        let path = self.state_dir.join("migrate.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        // SAFETY: flock on an fd this function owns.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            let e = std::io::Error::last_os_error();
            return Err(format!("{}: {e}", path.display()));
        }
        Ok(file)
    }

    fn record_path(&self) -> PathBuf {
        self.state_dir.join("migrated.json")
    }

    /// The files migrated, by canonical path, and the schema each reached. An
    /// unreadable record is empty: it only sharpens a warning.
    fn record(&self) -> BTreeMap<String, i64> {
        std::fs::read(self.record_path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn write_record(&self, record: &BTreeMap<String, i64>) -> std::io::Result<()> {
        let path = self.record_path();
        let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        std::fs::write(&tmp, serde_json::to_vec_pretty(record)?)?;
        std::fs::rename(&tmp, &path)
    }

    /// The files `comb migrate` with no names migrates: `config.toml` and
    /// every `conf.d/*.toml` that exist and migration is on for, each read for
    /// its `schema.migrate` (`config.toml`'s the default for `conf.d/`). A
    /// file that cannot be read is an error beside them.
    pub fn selected(&self, config_path: &Path) -> (Vec<PathBuf>, Vec<String>) {
        let (mut files, mut errors) = (Vec::new(), Vec::new());
        let mut inherited = true;
        let candidates = std::iter::once(config_path.to_path_buf()).chain(
            super::Config::conf_d_files(&super::Config::conf_d_dir_for(config_path)),
        );
        for (i, file) in candidates.enumerate() {
            let text = match std::fs::read_to_string(&file) {
                Ok(text) => text,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    errors.push(format!("{}: {e}", super::Config::error_label(&file)));
                    continue;
                }
            };
            let migrate = toml::from_str::<toml::Value>(&text)
                .map_err(|e| e.to_string())
                .and_then(|layer| schema(&layer));
            match migrate {
                Ok(s) => {
                    let on = s.migrate.unwrap_or(inherited);
                    if i == 0 {
                        inherited = on;
                    }
                    if on {
                        files.push(file);
                    }
                }
                Err(e) => errors.push(format!("{}: {e}", super::Config::error_label(&file))),
            }
        }
        (files, errors)
    }
}

/// Log `change`, made: a warning first when the file had been put back.
pub fn log(change: &Change) {
    let shown = super::Config::error_label(&change.file);
    let (from, to) = (change.from, change.to);
    if change.restored {
        tracing::warn!(
            "{shown} was back at schema {from} after comb had migrated it: migrated again; \
             update its source (a dotfiles manager restored it) to keep it at schema {to}"
        );
    }
    tracing::info!(
        "{shown} migrated from schema {from} to {to}; the original is kept as {}",
        bak_of(&change.target).display()
    );
}

/// `<file>.bak`, beside it.
pub fn bak_of(target: &Path) -> PathBuf {
    let mut name = target.file_name().unwrap_or_default().to_os_string();
    name.push(".bak");
    target.with_file_name(name)
}

/// Replace `target`'s text `before` with `after`: refused unless the file can
/// be written; the original kept as `<target>.bak`; the new text written to a
/// temporary beside it, with its permissions, and renamed over it.
fn write_through(target: &Path, before: &str, after: &str) -> Result<(), String> {
    // A rename would replace a read-only file in a writable directory: the
    // file's own permission is asked first.
    std::fs::OpenOptions::new()
        .write(true)
        .open(target)
        .map_err(|e| format!("it cannot be written: {e}"))?;
    let bak = bak_of(target);
    std::fs::write(&bak, before).map_err(|e| format!("{}: {e}", bak.display()))?;
    let name = target.file_name().unwrap_or_default().to_string_lossy();
    let tmp = target.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    let written = (|| {
        std::fs::write(&tmp, after)?;
        std::fs::set_permissions(&tmp, std::fs::metadata(target)?.permissions())?;
        std::fs::rename(&tmp, target)
    })();
    written.map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{}: {e}", target.display())
    })
}

fn behind(label: &str, version: i64, current: i64, why: &str) -> String {
    format!(
        "{label}: schema {version} is behind this comb's schema {current} and was not migrated: \
         {why}; run `comb migrate` with the file named to migrate it"
    )
}

fn newer(label: &str, version: i64, current: i64) -> String {
    format!(
        "{label}: schema {version} was written for a newer comb: this one reads schema {current}"
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The test migration, schema 1 to 2: `old_fn(` becomes `new_fn(`.
    pub(crate) static TEST_MIGRATIONS: &[Migration] = &[Migration {
        file: |text| Ok(text.replace("old_fn(", "new_fn(")),
        template: |text| text.replace("old_fn(", "new_fn("),
    }];

    pub(crate) fn migrator(state: &Path) -> Migrator {
        Migrator::new(TEST_MIGRATIONS, state.to_path_buf())
    }

    #[test]
    fn the_current_schema_is_2() {
        assert_eq!(current(), 2);
        assert_eq!(template("{{ lup('x') }}", 2), "{{ lup('x') }}");
    }

    /// Schema 1 to 2: each function call is written through `_`.
    #[test]
    fn every_function_call_is_written_through_underscore() {
        for (old, new) in [
            ("{{ spark([1, 2]) }}", "{{ _.spark([1, 2]) }}"),
            ("{{ hyperlink('u', 't') }}", "{{ _.hyperlink('u', 't') }}"),
            ("{{ icon('dev-git') }}", "{{ _.icon('dev-git') }}"),
            ("{{ file_icon('a.rs') }}", "{{ _.file_icon('a.rs') }}"),
            ("{{ bar(0.5) }}", "{{ _.bar(0.5) }}"),
            ("{{ merge(sources) }}", "{{ _.merge(sources) }}"),
            ("{{ now() }}", "{{ _.now() }}"),
            ("{{ spinner('ab') }}", "{{ _.spinner('ab') }}"),
            ("{{ lup('.git') }}", "{{ _.lup('.git') }}"),
            ("{{ exec('ls') }}", "{{ _.exec('ls') }}"),
            (
                "{{ execa('ls').exit_code }}",
                "{{ _.execa('ls').exit_code }}",
            ),
            ("{{ url('http://x/') }}", "{{ _.url('http://x/') }}"),
            (
                "{{ plugin('sys', 'uname') }}",
                "{{ _.plugin('sys', 'uname') }}",
            ),
            ("{{ log.warn('w') }}", "{{ _.log.warn('w') }}"),
            (
                "{% if a %}{{ fail('no') }}{% endif %}",
                "{% if a %}{{ _.fail('no') }}{% endif %}",
            ),
            (
                "{% set r = lup ('.git', root=true) %}{{ [r, now()] }}",
                "{% set r = _.lup ('.git', root=true) %}{{ [r, _.now()] }}",
            ),
        ] {
            assert_eq!(template(old, 1), new, "{old}");
            assert_eq!(template(new, 1), new, "idempotent: {new}");
        }
    }

    /// Only calls by the bare name move: a filter, a string, a comment, a
    /// name after a `.`, a name read not called, and a macro or variable the
    /// template declares are left.
    #[test]
    fn what_is_no_bare_call_is_left() {
        for text in [
            "{{ 'ls' | exec() }}",
            "{{ 'ls' | exec }}",
            "{{ a | merge(b) | execa(ok=[0]) }}",
            "{{ 'lup(x)' ~ \"exec('y')\" }}",
            "{# now() #}{{ 1 }}",
            "{% raw %}{{ now() }}{% endraw %}",
            "{{ x.lup('a') ~ _.exec('b') ~ comb.git.merge }}",
            "{{ now.x ~ log.y ~ url }}",
            "{% macro bar(x) %}[{{ x }}]{% endmacro %}{{ bar(1) }}",
            "{% from 'p:macro.m' import icon %}{{ icon('x') }}",
            "{% set now = 1 %}{{ now }}",
            "now() outside a tag",
        ] {
            assert_eq!(template(text, 1), text, "{text}");
        }
    }

    /// A file's templates are rewritten wherever they are, and nothing else in
    /// it: comments, quoting, escapes and layout are kept.
    #[test]
    fn a_file_is_rewritten_in_its_templates_and_nowhere_else() {
        let old = r#"# lup() in a comment stays
[client.providers.k]
template.a = "{{ now() }}"   # now() here stays too
template.b = '{{ exec("ls") | trim }}'
macro.m = '''
{% macro m() %}{{ icon('x') }}{% endmacro %}'''
[daemon.providers.p.sources.s]
root = "{{ lup(\".git\", root=true) | dirname }}"
data = """
{{ plugin('sys', \
  'uname') }}"""
result = { a = "{{ data.a | merge({'b': spinner('ab')}) }}" }
other = "plain lup() text"
list = ["{{ bar(1) }}", 'x']
"#;
        let new = r#"# lup() in a comment stays
[client.providers.k]
template.a = "{{ _.now() }}"   # now() here stays too
template.b = '{{ _.exec("ls") | trim }}'
macro.m = '''
{% macro m() %}{{ _.icon('x') }}{% endmacro %}'''
[daemon.providers.p.sources.s]
root = "{{ _.lup(\".git\", root=true) | dirname }}"
data = """
{{ _.plugin('sys', \
  'uname') }}"""
result = { a = "{{ data.a | merge({'b': _.spinner('ab')}) }}" }
other = "plain lup() text"
list = ["{{ _.bar(1) }}", 'x']
"#;
        let migrate = MIGRATIONS[0].file;
        assert_eq!(migrate(old).unwrap(), new);
        assert_eq!(migrate(new).unwrap(), new, "idempotent");
        // Through the migrator, the stamp goes on top.
        let migrator = Migrator::new(MIGRATIONS, PathBuf::from("/nonexistent"));
        assert_eq!(
            migrator.rewrite(old, 1).unwrap(),
            format!("schema.version = 2\n{new}")
        );
    }

    #[test]
    fn a_toml_string_is_read_with_where_each_byte_is_written() {
        let read = |raw: &str| toml_string(raw).unwrap();
        assert_eq!(read(r#""a\"b""#), ("a\"b".into(), vec![1, 2, 4]));
        assert_eq!(read("'a\\b'"), ("a\\b".into(), vec![1, 2, 3]));
        assert_eq!(read("\"\"\"\nab\"\"\""), ("ab".into(), vec![4, 5]));
        assert_eq!(read(r#""\u00e9x""#).0, "éx");
        assert_eq!(read(r#""\u00e9x""#).1, vec![1, 1, 7]);
        assert!(toml_string("x").is_none());
    }

    #[test]
    fn schema_reads_the_table_and_refuses_what_it_does_not_hold() {
        let s = |text: &str| schema(&toml::from_str(text).unwrap());
        assert_eq!(
            s("").unwrap(),
            Schema {
                version: 1,
                migrate: None
            }
        );
        assert_eq!(
            s("schema.version = 3\nschema.migrate = false\n").unwrap(),
            Schema {
                version: 3,
                migrate: Some(false)
            }
        );
        assert!(s("schema.version = 0\n").is_err());
        assert!(s("schema.version = \"2\"\n").is_err());
        assert!(s("schema.other = 1\n").is_err());
        assert!(s("schema = 1\n").is_err());
    }

    #[test]
    fn stamp_inserts_before_the_first_header_or_updates_in_place() {
        assert_eq!(
            stamp("# top\n[daemon]\nx = 1\n", 2),
            "schema.version = 2\n# top\n[daemon]\nx = 1\n"
        );
        assert_eq!(
            stamp("# top\nschema.migrate = true\n[daemon]\n", 2),
            "# top\nschema.version = 2\nschema.migrate = true\n[daemon]\n"
        );
        assert_eq!(
            stamp("schema.version = 1 # was\n[daemon]\n", 3),
            "schema.version = 3 # was\n[daemon]\n"
        );
        assert_eq!(stamp("schema.version=1", 2), "schema.version = 2");
        assert_eq!(stamp("", 2), "schema.version = 2\n");
        // A `schema.version` inside a table is not the stamp.
        assert_eq!(
            stamp("[t]\nschema.version = 1\n", 2),
            "schema.version = 2\n[t]\nschema.version = 1\n"
        );
    }

    #[test]
    fn templates_migrate_from_their_schema() {
        assert_eq!(
            template_with(TEST_MIGRATIONS, "{{ old_fn() }}", 1),
            "{{ new_fn() }}"
        );
        assert_eq!(
            template_with(TEST_MIGRATIONS, "{{ old_fn() }}", 2),
            "{{ old_fn() }}"
        );
    }

    #[test]
    fn two_migrators_racing_rewrite_the_file_once() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        std::fs::write(&file, "[daemon]\nlog_level = \"info\" # old_fn(\n").unwrap();
        let state = dir.path().join("state");
        let changes: Vec<_> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..4)
                .map(|_| s.spawn(|| migrator(&state).migrate(&file, true).unwrap()))
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert_eq!(changes.iter().flatten().count(), 1, "{changes:?}");
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "schema.version = 2\n[daemon]\nlog_level = \"info\" # new_fn(\n"
        );
    }

    // ── Loading migrates ────────────────────────────────────────────────

    struct Dir {
        _tmp: tempfile::TempDir,
        root: PathBuf,
        main: PathBuf,
        conf_d: PathBuf,
    }

    impl Dir {
        fn new() -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path().to_path_buf();
            let conf_d = root.join("conf.d");
            std::fs::create_dir(&conf_d).unwrap();
            Self {
                main: root.join("config.toml"),
                conf_d,
                root,
                _tmp: tmp,
            }
        }

        fn load(&self) -> super::super::Config {
            super::super::Config::load_with(&self.main, &migrator(&self.root.join("state")))
        }
    }

    const OLD: &str =
        "# my config\n[client.providers.git]\n# keep me\ntemplate.x = \"{{ old_fn() }}\"\n";

    #[test]
    fn a_stamp_is_read_and_does_not_compose() {
        let d = Dir::new();
        std::fs::write(
            &d.main,
            "schema.version = 2\nschema.migrate = false\n[daemon]\nlog_level = \"debug\"\n",
        )
        .unwrap();
        let config = d.load();
        assert_eq!(config.errors(), Vec::<String>::new());
        assert!(config.composed().get("schema").is_none());
        assert!(
            config.composed_layers()[..]
                .iter()
                .all(|(_, l)| l.get("schema").is_none())
        );
        assert!(config.migrated().is_empty());
    }

    #[test]
    fn a_file_behind_is_migrated_on_load() {
        let d = Dir::new();
        std::fs::write(&d.main, OLD).unwrap();
        let config = d.load();
        assert_eq!(config.errors(), Vec::<String>::new());
        let after = std::fs::read_to_string(&d.main).unwrap();
        assert_eq!(
            after,
            "schema.version = 2\n# my config\n[client.providers.git]\n# keep me\ntemplate.x = \"{{ new_fn() }}\"\n"
        );
        assert_eq!(
            std::fs::read_to_string(d.root.join("config.toml.bak")).unwrap(),
            OLD
        );
        assert_eq!(config.migrated().len(), 1);
        assert!(!config.migrated()[0].restored);
        // Loaded at the current schema, it is left alone.
        assert!(d.load().migrated().is_empty());
        assert_eq!(std::fs::read_to_string(&d.main).unwrap(), after);
    }

    #[test]
    fn migrate_off_in_config_toml_cascades_and_a_drop_in_turns_it_back_on() {
        let d = Dir::new();
        std::fs::write(&d.main, format!("schema.migrate = false\n{OLD}")).unwrap();
        std::fs::write(
            d.conf_d.join("a.toml"),
            format!("schema.migrate = true\n{OLD}"),
        )
        .unwrap();
        std::fs::write(d.conf_d.join("b.toml"), OLD).unwrap();
        let errors = d.load().errors();
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert!(
            errors.iter().any(|e| e.contains("config.toml)")),
            "{errors:?}"
        );
        assert!(errors.iter().any(|e| e.contains("b.toml)")), "{errors:?}");
        assert!(
            errors[0].contains("schema 1")
                && errors[0].contains("schema 2")
                && errors[0].contains("comb migrate"),
            "{errors:?}"
        );
        assert!(
            std::fs::read_to_string(d.conf_d.join("a.toml"))
                .unwrap()
                .contains("new_fn(")
        );
        assert!(
            std::fs::read_to_string(d.conf_d.join("b.toml"))
                .unwrap()
                .contains("old_fn(")
        );
        assert!(
            std::fs::read_to_string(&d.main)
                .unwrap()
                .contains("old_fn(")
        );
    }

    #[test]
    fn migrate_on_in_config_toml_and_a_drop_in_turns_it_off() {
        let d = Dir::new();
        std::fs::write(&d.main, format!("schema.migrate = true\n{OLD}")).unwrap();
        std::fs::write(
            d.conf_d.join("a.toml"),
            format!("schema.migrate = false\n{OLD}"),
        )
        .unwrap();
        std::fs::write(d.conf_d.join("b.toml"), OLD).unwrap();
        let errors = d.load().errors();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("a.toml)"), "{errors:?}");
        assert!(
            std::fs::read_to_string(&d.main)
                .unwrap()
                .contains("new_fn(")
        );
        assert!(
            std::fs::read_to_string(d.conf_d.join("b.toml"))
                .unwrap()
                .contains("new_fn(")
        );
    }

    #[test]
    fn a_symlink_is_written_through_and_kept() {
        let d = Dir::new();
        let target = d.root.join("dotfiles.toml");
        std::fs::write(&target, OLD).unwrap();
        std::os::unix::fs::symlink(&target, d.conf_d.join("10-linked.toml")).unwrap();
        assert_eq!(d.load().errors(), Vec::<String>::new());
        let link = d.conf_d.join("10-linked.toml");
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(
            std::fs::read_to_string(&target)
                .unwrap()
                .contains("new_fn(")
        );
        assert_eq!(
            std::fs::read_to_string(d.root.join("dotfiles.toml.bak")).unwrap(),
            OLD
        );
    }

    #[test]
    fn a_read_only_file_is_left_and_fails_the_load() {
        use std::os::unix::fs::PermissionsExt;
        let d = Dir::new();
        std::fs::write(&d.main, OLD).unwrap();
        std::fs::set_permissions(&d.main, std::fs::Permissions::from_mode(0o444)).unwrap();
        let errors = d.load().errors();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0].contains("config.toml)") && errors[0].contains("comb migrate"),
            "{errors:?}"
        );
        assert_eq!(std::fs::read_to_string(&d.main).unwrap(), OLD);
        assert!(!d.root.join("config.toml.bak").exists());
    }

    #[test]
    fn a_file_from_a_newer_comb_fails_the_load() {
        let d = Dir::new();
        std::fs::write(&d.main, "schema.version = 3\n").unwrap();
        let errors = d.load().errors();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("newer comb"), "{errors:?}");
    }

    #[test]
    fn a_file_restored_behind_is_migrated_again_and_flagged() {
        let d = Dir::new();
        std::fs::write(&d.main, OLD).unwrap();
        assert!(!d.load().migrated()[0].restored);
        std::fs::write(&d.main, OLD).unwrap();
        let config = d.load();
        assert!(config.migrated()[0].restored);
        assert!(
            std::fs::read_to_string(&d.main)
                .unwrap()
                .contains("new_fn(")
        );
    }

    // ── comb migrate ────────────────────────────────────────────────────

    fn run(d: &Dir, files: &[PathBuf], dry_run: bool) -> (crate::cli::exit::Outcome, String) {
        let mut out = Vec::new();
        let outcome = crate::cli::commands::migrate::migrate(
            &migrator(&d.root.join("state")),
            &d.main,
            files,
            dry_run,
            &mut out,
        );
        (outcome, String::from_utf8(out).unwrap())
    }

    #[test]
    fn dry_run_prints_the_diff_and_writes_nothing() {
        let d = Dir::new();
        std::fs::write(&d.main, OLD).unwrap();
        let (outcome, out) = run(&d, &[], true);
        assert_eq!(outcome, crate::cli::exit::Outcome::Done);
        assert!(out.contains("-template.x = \"{{ old_fn() }}\""), "{out}");
        assert!(out.contains("+template.x = \"{{ new_fn() }}\""), "{out}");
        assert!(out.contains("+schema.version = 2"), "{out}");
        assert_eq!(std::fs::read_to_string(&d.main).unwrap(), OLD);
        assert!(!d.root.join("config.toml.bak").exists());
        assert!(!d.root.join("state").exists());
    }

    #[test]
    fn a_named_file_is_migrated_though_migration_is_off_for_it() {
        let d = Dir::new();
        let off = format!("schema.migrate = false\n{OLD}");
        std::fs::write(&d.main, &off).unwrap();
        // Unnamed, it is not selected: nothing to do.
        let (outcome, out) = run(&d, &[], false);
        assert_eq!(outcome, crate::cli::exit::Outcome::Done);
        assert!(out.starts_with("nothing to migrate"), "{out}");
        let (outcome, out) = run(&d, std::slice::from_ref(&d.main), false);
        assert_eq!(outcome, crate::cli::exit::Outcome::Done);
        assert!(out.contains("migrated "), "{out}");
        assert_eq!(
            std::fs::read_to_string(&d.main).unwrap(),
            off.replace("schema.migrate", "schema.version = 2\nschema.migrate")
                .replace("old_fn(", "new_fn(")
        );
    }
}
