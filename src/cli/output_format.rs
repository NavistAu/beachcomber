//! Client-side output format type and shared rendering helpers.
//!
//! `OutputFormat` is richer than the protocol `Format` type — every format is
//! rendered locally from the resolved JSON value: `text`/`sh` through
//! `libbeachcomber::render::render_data`, the rest (json, csv, tsv, fmt) here
//! and in the `get`/`watch` handlers.
//!
//! `parse_output_format` and `suffix_to_format` handle the CLI argument /
//! suffix syntax.  `format_sv` and `value_to_string` are shared rendering
//! helpers used by both the `get` and `watch` handlers.

/// Client-side output format.
pub enum OutputFormat {
    Json,
    Text,
    Sh,
    Csv,
    Tsv,
    CsvHeader,
    TsvHeader,
    Fmt(String),
}

impl OutputFormat {
    /// The `render_data` format name for this output format.
    pub fn server_format(&self) -> &str {
        match self {
            OutputFormat::Text => "text",
            OutputFormat::Sh => "sh",
            _ => "json",
        }
    }
}

/// Parse the `-f` / `--format` argument string into an `OutputFormat`.
pub fn parse_output_format(format_str: &str, fmt_template: Option<&str>) -> OutputFormat {
    match format_str {
        "json" => OutputFormat::Json,
        "sh" => OutputFormat::Sh,
        "csv" => OutputFormat::Csv,
        "tsv" => OutputFormat::Tsv,
        "CSV" => OutputFormat::CsvHeader,
        "TSV" => OutputFormat::TsvHeader,
        "fmt" => OutputFormat::Fmt(fmt_template.unwrap_or("").to_string()),
        // "text" and any unknown value fall through to plain text (the default).
        _ => OutputFormat::Text,
    }
}

/// Map a format suffix (e.g. `"j"`) to the corresponding `-f` flag value.
/// Returns `None` if the suffix is not recognised.
pub fn suffix_to_format(suffix: &str) -> Option<&'static str> {
    match suffix {
        "p" => Some("text"), // plain text — now the default, but an explicit .p is still accepted
        "j" => Some("json"),
        "s" => Some("sh"),
        "c" => Some("csv"),
        "C" => Some("CSV"),
        "t" => Some("tsv"),
        "T" => Some("TSV"),
        "f" => Some("fmt"),
        _ => None,
    }
}

/// Format a JSON value as a single display string.
pub fn value_to_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Format response data as CSV or TSV.
pub fn format_sv(data: &serde_json::Value, sep: &str, with_header: bool) -> String {
    match data {
        serde_json::Value::Object(map) => {
            let mut pairs: Vec<(&String, &serde_json::Value)> = map.iter().collect();
            pairs.sort_by_key(|(k, _)| *k);
            let mut out = String::new();
            if with_header {
                let keys: Vec<&str> = pairs.iter().map(|(k, _)| k.as_str()).collect();
                out.push_str(&keys.join(sep));
                out.push('\n');
            }
            let vals: Vec<String> = pairs.iter().map(|(_, v)| value_to_string(v)).collect();
            out.push_str(&vals.join(sep));
            out
        }
        other => value_to_string(other),
    }
}
