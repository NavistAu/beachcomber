//! Every value format, rendered once.
//!
//! `comb get`, `comb watch` and `comb eval` print a value through
//! [`render_value`] (one key), [`render_values`] (several) or
//! [`render_frame`] (a watch frame); `comb status` lays its rows out with
//! the same cell encoders — [`csv_cell`], [`csv_quote`], [`tsv_escape`],
//! [`sh_name`], [`shell_quote`] — so a format means one thing wherever it is
//! named.
//!
//! The rules:
//! - **Absent.** `None`, and a `null` anywhere in a value, is absent: it
//!   prints nothing (no line, no assignment), an empty cell where a column
//!   must hold its place, and `null` only in json.
//! - **Text.** A scalar prints bare; a container as sorted `address=value`
//!   lines, one per leaf at any depth, each address canonical
//!   ([`crate::key`]) so it can be typed back as a key.
//! - **Shell.** Every line is an assignment to a name in
//!   `[A-Za-z_][A-Za-z0-9_]*` ([`sh_name`]): a leaf is named by its address
//!   below the key, a scalar by the key's last segment. Values are
//!   single-quoted ([`shell_quote`]), so the output is safe to `eval`.
//! - **CSV and TSV.** One cell per top-level field (an object) or per key (a
//!   scalar); a string is its raw text, anything else its JSON text
//!   ([`csv_cell`]). CSV quotes per RFC 4180; TSV escapes `\`, tab, CR and
//!   LF. The uppercase format adds a header row of the cells' names.
//! - **Newlines.** A value printed for embedding — text of a scalar — has no
//!   trailing newline. Every document — json, sh, csv, tsv, and text of a
//!   container or of several keys — ends each line in one.
//!
//! A duration shown to a person — `status`'s AGE and POLL, `check`'s
//! uptimes and intervals, a timeout in an error — reads one way too
//! ([`duration`]).

use serde_json::Value;
use std::time::Duration;

/// A duration as comb shows one: `14s`; `3m`, `1m18s`; `2h`, `2h14m` (the
/// seconds dropped once there are hours); and in milliseconds, `250ms`, when
/// under a minute and not whole seconds.
pub fn duration(d: Duration) -> String {
    let ms = d.as_millis();
    if ms < 60_000 && !ms.is_multiple_of(1000) {
        return format!("{ms}ms");
    }
    let secs = d.as_secs();
    let (hours, minutes, secs) = (secs / 3600, secs / 60 % 60, secs % 60);
    match (hours, minutes, secs) {
        (0, 0, s) => format!("{s}s"),
        (0, m, 0) => format!("{m}m"),
        (0, m, s) => format!("{m}m{s}s"),
        (h, 0, _) => format!("{h}h"),
        (h, m, _) => format!("{h}h{m}m"),
    }
}

/// A value format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// A scalar bare; a container as `address=value` lines.
    Text,
    Json,
    /// Shell assignments.
    Sh,
    /// Comma-separated, with no header row.
    Csv,
    /// Comma-separated, with a header row.
    CsvHeader,
    /// Tab-separated, with no header row.
    Tsv,
    /// Tab-separated, with a header row.
    TsvHeader,
}

impl Format {
    /// Every value format.
    pub const ALL: [Format; 7] = [
        Format::Text,
        Format::Json,
        Format::Sh,
        Format::Csv,
        Format::CsvHeader,
        Format::Tsv,
        Format::TsvHeader,
    ];

    /// The format's name, as `-f` takes it.
    pub fn name(self) -> &'static str {
        match self {
            Format::Text => "text",
            Format::Json => "json",
            Format::Sh => "sh",
            Format::Csv => "csv",
            Format::CsvHeader => "CSV",
            Format::Tsv => "tsv",
            Format::TsvHeader => "TSV",
        }
    }

    /// The format a name names.
    pub fn from_name(name: &str) -> Option<Format> {
        Format::ALL.into_iter().find(|f| f.name() == name)
    }

    /// How a delimited format writes a row; `None` for the others.
    pub fn delimited(self) -> Option<Delimited> {
        let (sep, encode, header): (char, fn(&str) -> String, bool) = match self {
            Format::Csv => (',', csv_quote, false),
            Format::CsvHeader => (',', csv_quote, true),
            Format::Tsv => ('\t', tsv_escape, false),
            Format::TsvHeader => ('\t', tsv_escape, true),
            Format::Text | Format::Json | Format::Sh => return None,
        };
        Some(Delimited {
            sep,
            encode,
            header,
        })
    }
}

/// How a delimited format writes a row: its separator, its cell encoder,
/// and whether a header row comes first.
#[derive(Clone, Copy)]
pub struct Delimited {
    pub sep: char,
    pub encode: fn(&str) -> String,
    pub header: bool,
}

impl Delimited {
    /// One row of cells, encoded and ending in a newline.
    pub fn line(&self, cells: &[String]) -> String {
        let mut line = cells
            .iter()
            .map(|c| (self.encode)(c))
            .collect::<Vec<_>>()
            .join(&self.sep.to_string());
        line.push('\n');
        line
    }

    /// The header row (when the format has one) and then the row.
    fn document(&self, names: &[String], cells: &[String]) -> String {
        let mut out = String::new();
        if self.header {
            out.push_str(&self.line(names));
        }
        out.push_str(&self.line(cells));
        out
    }
}

/// POSIX single-quote a string: wrap in `'...'`, rewriting each internal `'`
/// as `'\''`. The one shell-escaping helper.
pub fn shell_quote(s: &str) -> String {
    let escaped = s.replace('\'', r"'\''");
    format!("'{escaped}'")
}

/// A shell variable name for `address`, a canonical address: its segments
/// joined by `_`, made a name by [`sh_ident`]. `c.d` is `c_d`, `['a.b']` is
/// `a_b`, `e f` is `e_f`, `list.0` is `list_0`.
pub fn sh_name(address: &str) -> String {
    let segments = crate::key::segments(address);
    sh_ident(
        &segments
            .iter()
            .map(|s| s.as_ref())
            .collect::<Vec<_>>()
            .join("_"),
    )
}

/// `text` as a shell variable name: every character outside `[A-Za-z0-9_]`
/// written as `_`, and a leading digit (or nothing at all) given a leading
/// `_`.
pub fn sh_ident(text: &str) -> String {
    let mut name: String = text
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) {
        name.insert(0, '_');
    }
    name
}

/// A cell's text: a string raw, absent empty, anything else its JSON text.
pub fn csv_cell(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

/// RFC 4180: a cell holding `,`, `"`, CR or LF is wrapped in double quotes,
/// its quotes doubled.
pub fn csv_quote(cell: &str) -> String {
    if cell.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", cell.replace('"', "\"\""))
    } else {
        cell.to_string()
    }
}

/// A TSV cell: `\`, tab, CR and LF written as `\\`, `\t`, `\r` and `\n`, so a
/// cell never breaks its row.
pub fn tsv_escape(cell: &str) -> String {
    let mut out = String::with_capacity(cell.len());
    for c in cell.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out
}

/// What `comb get KEY` prints for the value it resolved: nothing for a miss
/// but `null` under json.
pub fn render_value(key: &str, value: Option<&Value>, format: Format) -> String {
    let value = present(value);
    match format {
        Format::Json => format!("{}\n", pretty(value)),
        Format::Text => match value {
            None => String::new(),
            Some(container @ (Value::Object(_) | Value::Array(_))) => {
                text_lines(container).concat()
            }
            Some(scalar) => csv_cell(Some(scalar)),
        },
        Format::Sh => value.map(|v| sh_lines(key, v).concat()).unwrap_or_default(),
        delimited => {
            let Some(value) = value else {
                return String::new();
            };
            let (names, cells) = columns(key, Some(value));
            delimited
                .delimited()
                .expect("a delimited format")
                .document(&names, &cells)
        }
    }
}

/// What `comb get K1 K2 …` prints: one document across the keys, a miss
/// holding its place — `null` in the json array, an empty line under text
/// and sh, an empty cell under csv and tsv.
pub fn render_values(slots: &[(&str, Option<&Value>)], format: Format) -> String {
    match format {
        Format::Json => {
            let values: Vec<Value> = slots
                .iter()
                .map(|(_, v)| present(*v).cloned().unwrap_or(Value::Null))
                .collect();
            format!("{}\n", pretty(Some(&Value::Array(values))))
        }
        Format::Text | Format::Sh => slots
            .iter()
            .map(|(key, value)| {
                let lines = match (present(*value), format) {
                    (None, _) => Vec::new(),
                    (Some(v), Format::Sh) => sh_lines(key, v),
                    (Some(c @ (Value::Object(_) | Value::Array(_))), _) => text_lines(c),
                    (Some(scalar), _) => vec![format!("{}\n", csv_cell(Some(scalar)))],
                };
                if lines.is_empty() {
                    "\n".to_string()
                } else {
                    lines.concat()
                }
            })
            .collect(),
        delimited => {
            let mut names = Vec::new();
            let mut cells = Vec::new();
            for (key, value) in slots {
                let (n, c) = columns(key, present(*value));
                names.extend(n);
                cells.extend(c);
            }
            delimited
                .delimited()
                .expect("a delimited format")
                .document(&names, &cells)
        }
    }
}

/// One frame of `comb watch`: the value as `comb get` prints it, ending in a
/// newline, with json on one line so each frame is one line.
pub fn render_frame(key: &str, value: &Value, format: Format) -> String {
    let mut frame = match format {
        Format::Json => format!(
            "{}\n",
            serde_json::to_string(present(Some(value)).unwrap_or(&Value::Null)).unwrap_or_default()
        ),
        other => render_value(key, Some(value), other),
    };
    if !frame.ends_with('\n') {
        frame.push('\n');
    }
    frame
}

/// `value`, unless it is absent.
fn present(value: Option<&Value>) -> Option<&Value> {
    value.filter(|v| !v.is_null())
}

fn pretty(value: Option<&Value>) -> String {
    serde_json::to_string_pretty(value.unwrap_or(&Value::Null)).unwrap_or_default()
}

/// `address=value` lines for every present leaf of a container, sorted.
fn text_lines(container: &Value) -> Vec<String> {
    let mut leaves = Vec::new();
    collect_leaves("", container, &mut leaves);
    let mut lines: Vec<String> = leaves
        .into_iter()
        .map(|(address, leaf)| format!("{address}={}\n", csv_cell(Some(leaf))))
        .collect();
    lines.sort();
    lines
}

/// Assignments for `value` read under `key`: every present leaf of a
/// container named by its address, a scalar by the key's last segment.
fn sh_lines(key: &str, value: &Value) -> Vec<String> {
    let assign = |name: &str, leaf: &Value| {
        format!("{}={}\n", sh_name(name), shell_quote(&csv_cell(Some(leaf))))
    };
    match value {
        Value::Object(_) | Value::Array(_) => {
            let mut leaves = Vec::new();
            collect_leaves("", value, &mut leaves);
            let mut lines: Vec<String> = leaves
                .into_iter()
                .map(|(address, leaf)| assign(&address, leaf))
                .collect();
            lines.sort();
            lines
        }
        scalar => {
            let address = crate::key::split_key(key)
                .map(|(_, address)| address)
                .unwrap_or_default();
            let last =
                crate::key::last(&address).unwrap_or_else(|| crate::key::head_of(key).to_string());
            vec![assign(&crate::key::quote(&last), scalar)]
        }
    }
}

/// The names and cells a value contributes to a delimited row: an object
/// one per top-level field, sorted, anything else one named by its key.
fn columns(key: &str, value: Option<&Value>) -> (Vec<String>, Vec<String>) {
    match value {
        Some(Value::Object(map)) => {
            let mut fields: Vec<(&String, &Value)> = map.iter().collect();
            fields.sort_by_key(|(name, _)| *name);
            fields
                .into_iter()
                .map(|(name, v)| (crate::key::quote(name).into_owned(), csv_cell(Some(v))))
                .unzip()
        }
        other => (vec![key.to_string()], vec![csv_cell(other)]),
    }
}

/// Every present leaf below `value`, with its canonical address under
/// `prefix`. An empty container contributes nothing, and nor does a `null`.
fn collect_leaves<'v>(prefix: &str, value: &'v Value, out: &mut Vec<(String, &'v Value)>) {
    let at = |segment: &str| crate::key::push(prefix, segment);
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                collect_leaves(&at(k), v, out);
            }
        }
        Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                collect_leaves(&at(&i.to_string()), v, out);
            }
        }
        Value::Null => {}
        leaf => out.push((prefix.to_string(), leaf)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_duration_reads_as_comb_writes_one_everywhere() {
        let ms = std::time::Duration::from_millis;
        let s = std::time::Duration::from_secs;
        for (d, text) in [
            (s(0), "0s"),
            (ms(250), "250ms"),
            (ms(1500), "1500ms"),
            (s(14), "14s"),
            (s(60), "1m"),
            (s(78), "1m18s"),
            (ms(78_400), "1m18s"),
            (s(3600), "1h"),
            (s(8040), "2h14m"),
            (s(8070), "2h14m"),
            (s(90_061), "25h1m"),
        ] {
            assert_eq!(duration(d), text, "{d:?}");
        }
    }

    #[test]
    fn a_scalar_prints_bare_with_no_newline() {
        assert_eq!(
            render_value("g.b", Some(&json!("main")), Format::Text),
            "main"
        );
        assert_eq!(render_value("n.x", Some(&json!(42)), Format::Text), "42");
        assert_eq!(
            render_value("n.x", Some(&json!(true)), Format::Text),
            "true"
        );
    }

    #[test]
    fn absent_prints_nothing_but_null_under_json() {
        for format in Format::ALL {
            let want = if format == Format::Json { "null\n" } else { "" };
            assert_eq!(render_value("k", None, format), want, "{format:?}");
            assert_eq!(
                render_value("k", Some(&Value::Null), format),
                want,
                "{format:?}"
            );
        }
    }

    #[test]
    fn a_container_is_sorted_lines_of_canonical_addresses() {
        let data = json!({
            "z": "last",
            "a": {"b": [1, {"c": 2}], "n": null},
            "d.e": 3,
            "empty": {},
        });
        assert_eq!(
            render_value("p", Some(&data), Format::Text),
            "['d.e']=3\na.b.0=1\na.b.1.c=2\nz=last\n"
        );
    }

    #[test]
    fn sh_names_every_line_as_a_shell_variable() {
        let data = json!({"a.b": 1, "c": {"d": "it's"}, "e f": 3, "0x": 4});
        assert_eq!(
            render_value("dk", Some(&data), Format::Sh),
            "_0x='4'\na_b='1'\nc_d='it'\\''s'\ne_f='3'\n"
        );
        // A scalar is named by its key's last segment.
        assert_eq!(
            render_value("git.branch", Some(&json!("main")), Format::Sh),
            "branch='main'\n"
        );
        assert_eq!(
            render_value("mise['go:x.y/z']", Some(&json!("1")), Format::Sh),
            "go_x_y_z='1'\n"
        );
    }

    #[test]
    fn csv_quotes_its_cells_and_tsv_escapes_them() {
        let data = json!({"a": "x,y", "b": "say \"hi\"", "c": {"d": 1}});
        assert_eq!(
            render_value("dq", Some(&data), Format::CsvHeader),
            "a,b,c\n\"x,y\",\"say \"\"hi\"\"\",\"{\"\"d\"\":1}\"\n"
        );
        assert_eq!(
            render_value("dq", Some(&data), Format::Csv),
            "\"x,y\",\"say \"\"hi\"\"\",\"{\"\"d\"\":1}\"\n"
        );
        let tabbed = json!({"a": "x\ty", "b": "1\\2\n"});
        assert_eq!(
            render_value("t", Some(&tabbed), Format::TsvHeader),
            "a\tb\nx\\ty\t1\\\\2\\n\n"
        );
    }

    #[test]
    fn several_keys_hold_their_places() {
        let x = json!(1);
        let y = json!("two");
        let slots = [("demo.x", Some(&x)), ("nope.z", None), ("demo.y", Some(&y))];
        assert_eq!(render_values(&slots, Format::Text), "1\n\ntwo\n");
        assert_eq!(render_values(&slots, Format::Sh), "x='1'\n\ny='two'\n");
        assert_eq!(render_values(&slots, Format::Csv), "1,,two\n");
        assert_eq!(
            render_values(&slots, Format::CsvHeader),
            "demo.x,nope.z,demo.y\n1,,two\n"
        );
        assert_eq!(
            render_values(&slots, Format::Json),
            "[\n  1,\n  null,\n  \"two\"\n]\n"
        );
    }

    #[test]
    fn a_frame_is_get_rendering_on_its_own_line() {
        assert_eq!(render_frame("g.b", &json!("main"), Format::Text), "main\n");
        assert_eq!(
            render_frame("p", &json!({"a": 1}), Format::Json),
            "{\"a\":1}\n"
        );
        assert_eq!(render_frame("p", &json!({"a": 1}), Format::Text), "a=1\n");
    }

    #[test]
    fn shell_quote_wraps_and_escapes_inner_quotes() {
        assert_eq!(shell_quote("plain"), "'plain'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn sh_name_is_always_an_identifier() {
        assert_eq!(sh_name("c.d"), "c_d");
        assert_eq!(sh_name("['a.b']"), "a_b");
        assert_eq!(sh_name("list.0"), "list_0");
        assert_eq!(sh_name("0"), "_0");
        assert_eq!(sh_name(""), "_");
    }
}
