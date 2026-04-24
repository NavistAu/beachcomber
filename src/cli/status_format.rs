use crate::cache::CacheRow;
use crate::cli::format::build_env;
use minijinja;

// ---------------------------------------------------------------------------
// ANSI colour helper (basic-16 palette, fg-only, no external crate)
// ---------------------------------------------------------------------------

#[allow(dead_code)]
mod ansi {
    #[derive(Clone, Copy, Debug)]
    pub enum Color {
        BrightGreen,
        Yellow,
        BrightYellow,
        Red,
        Dim,
    }

    impl Color {
        pub const fn code(self) -> &'static str {
            match self {
                Color::BrightGreen => "\x1b[92m",
                Color::Yellow => "\x1b[33m",
                Color::BrightYellow => "\x1b[93m",
                Color::Red => "\x1b[31m",
                Color::Dim => "\x1b[2m",
            }
        }
    }

    pub fn wrap(s: &str, c: Color, enabled: bool) -> String {
        if !enabled {
            return s.to_string();
        }
        format!("{}{}\x1b[0m", c.code(), s)
    }
}

// ---------------------------------------------------------------------------
// TTL cell formatter
// ---------------------------------------------------------------------------

#[allow(dead_code)]
pub struct TtlCell {
    pub text: String,
    pub color: Option<ansi::Color>,
}

#[allow(dead_code)]
pub fn format_ttl_cell(
    kind: Option<&crate::cache::RowKind>,
    poll_interval_secs: Option<u64>,
    keep_alive_polls: Option<u32>,
    fsevents_reinstate: Option<bool>,
    failure: Option<&crate::cache::FailureSnapshot>,
    ascii: bool,
) -> TtlCell {
    use crate::cache::RowKind;

    // Glyph table.
    let star = if ascii { "*" } else { "\u{2605}" }; // ★
    let warn = if ascii { "!" } else { "\u{26a0}" }; // ⚠
    let times = if ascii { "x" } else { "\u{00d7}" }; // ×
    let fisheye = if ascii { "F" } else { "\u{25c9}" }; // ◉

    match kind {
        None | Some(RowKind::Once) | Some(RowKind::Virtual) | Some(RowKind::Transient) => {
            TtlCell {
                text: "---".into(),
                color: None,
            }
        }
        Some(RowKind::Lifecycle {
            decay,
            watches_files,
        }) => {
            // Lead char: failure overrides decay.
            let (lead, color) = if failure.is_some() {
                (warn.to_string(), ansi::Color::Red)
            } else {
                match decay {
                    0 => (star.to_string(), ansi::Color::BrightGreen),
                    1 => ("3".into(), ansi::Color::Dim),
                    2 => ("2".into(), ansi::Color::Yellow),
                    3 => ("1".into(), ansi::Color::BrightYellow),
                    _ => ("0".into(), ansi::Color::Red),
                }
            };

            let p = poll_interval_secs.unwrap_or(0).min(999_999);
            let k = keep_alive_polls.unwrap_or(0).min(99);
            let indicator = if fsevents_reinstate.unwrap_or(false) && *watches_files {
                fisheye
            } else {
                " "
            };
            // " <P:6>s<×><K:02> <indicator>" — fixed width: 1 + 1 + 6 + 1 + 1 + 2 + 1 + 1 = 14.
            let text = format!("{} {:>6}s{}{:02} {}", lead, p, times, k, indicator);
            TtlCell {
                text,
                color: Some(color),
            }
        }
    }
}

/// Color mode for status output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    Auto,
    Always,
    Never,
}

/// Resolve whether to enable color output based on the mode, environment, and TTY state.
///
/// Rules (in priority order):
/// 1. `Never` → always false.
/// 2. `NO_COLOR` env var set → always false.
/// 3. `Always` → always true.
/// 4. `Auto` → true if is_tty or watch_interval_env.
pub fn resolve_color(
    mode: ColorMode,
    no_color_env: bool,
    is_tty: bool,
    watch_interval_env: bool,
) -> bool {
    if mode == ColorMode::Never {
        return false;
    }
    if no_color_env {
        return false;
    }
    if mode == ColorMode::Always {
        return true;
    }
    is_tty || watch_interval_env
}

/// Resolve the maximum value-column width.
///
/// - `None` arg → use the default (120).
/// - `Some("auto")` → use `terminal_cols` if available, else the default.
/// - `Some(n)` → parse `n` as a `usize`, falling back to the default on error.
pub fn resolve_max_width(arg: Option<&str>, terminal_cols: Option<usize>) -> usize {
    const DEFAULT: usize = 120;
    match arg {
        None => DEFAULT,
        Some("auto") => terminal_cols.unwrap_or(DEFAULT),
        Some(s) => s.parse().unwrap_or(DEFAULT),
    }
}

/// Options controlling how a status preset is rendered.
#[derive(Debug, Clone)]
pub struct RenderOpts {
    /// Whether the output destination is a TTY (enables color in `human`).
    pub is_tty: bool,
    /// Suppress ANSI color codes even on a TTY.
    pub no_color: bool,
    /// Truncate long values in human-readable presets. `None` means no truncation.
    pub max_width: Option<usize>,
    /// Disable truncation regardless of `max_width`.
    pub no_trunc: bool,
}

/// Options controlling CLI-level formatting choices (flags, not render state).
#[derive(Debug, Clone, Default)]
pub struct FormatOptions {
    /// Use ASCII-only characters instead of Unicode box-drawing / ellipsis glyphs.
    pub ascii: bool,
}

impl Default for RenderOpts {
    fn default() -> Self {
        Self {
            is_tty: false,
            no_color: true,
            max_width: Some(40),
            no_trunc: false,
        }
    }
}

/// Dispatch to the appropriate renderer for the given preset name.
///
/// Supported presets: `human`, `json`, `tsv`, `csv`, `table`, `sh`.
/// Any other value is treated as a minijinja template rendered per row.
/// If the value starts with `table ` (with a space), the remainder is rendered
/// as a tab-separated template with aligned columns and a derived header row.
pub fn render_preset(preset: &str, rows: &[CacheRow], opts: &RenderOpts) -> String {
    match preset {
        "json" => render_json(rows),
        "csv" => render_csv(rows),
        "tsv" => render_tsv(rows),
        "sh" => render_sh(rows),
        "table" => render_table(rows, false, None, true),
        "human" => {
            let trunc = if opts.no_trunc { None } else { opts.max_width };
            let color = !opts.no_color && opts.is_tty;
            render_table(rows, color, trunc, true)
        }
        custom => {
            if let Some(body) = custom.strip_prefix("table ") {
                render_minijinja_table(body, rows)
            } else {
                render_minijinja(custom, rows)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Custom minijinja template renderers
// ---------------------------------------------------------------------------

/// Render each row using a minijinja template. One output line per row.
pub fn render_minijinja(template: &str, rows: &[CacheRow]) -> String {
    let env = build_env();
    let rendered: Vec<String> = rows
        .iter()
        .map(|r| {
            let ctx = row_context(r);
            env.render_str(template, ctx)
                .unwrap_or_else(|e| format!("<template error: {e}>"))
        })
        .collect();
    let mut out = rendered.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

/// Render rows using a tab-separated minijinja template, then align columns.
///
/// The template body (after stripping the `table ` prefix) is rendered per row.
/// Column widths are computed across all rendered rows plus the derived header,
/// and the output is printed as a left-aligned block.
pub fn render_minijinja_table(body: &str, rows: &[CacheRow]) -> String {
    let env = build_env();

    // 1. Render each row.
    let rendered: Vec<String> = rows
        .iter()
        .map(|r| {
            let ctx = row_context(r);
            env.render_str(body, ctx)
                .unwrap_or_else(|e| format!("<template error: {e}>"))
        })
        .collect();

    // 2. Split each rendered string on '\t' → matrix.
    let matrix: Vec<Vec<&str>> = rendered.iter().map(|s| s.split('\t').collect()).collect();

    // 3. Derive header from variable names in `body`.
    let header: Vec<String> = extract_template_header(body);

    // 4. Compute per-column widths.
    let n_cols = header
        .len()
        .max(matrix.iter().map(|r| r.len()).max().unwrap_or(0));
    let mut widths = vec![0usize; n_cols];
    for (i, h) in header.iter().enumerate() {
        widths[i] = widths[i].max(h.len());
    }
    for row in &matrix {
        for (i, cell) in row.iter().enumerate() {
            if i < n_cols {
                widths[i] = widths[i].max(cell.len());
            }
        }
    }

    // 5. Render aligned.
    let mut out = String::new();

    // Header row.
    for (i, h) in header.iter().enumerate() {
        if i > 0 {
            out.push_str("  ");
        }
        if i + 1 < header.len() {
            out.push_str(&format!("{:<width$}", h, width = widths[i]));
        } else {
            out.push_str(h);
        }
    }
    out.push('\n');

    // Data rows.
    for row in &matrix {
        for (i, cell) in row.iter().enumerate() {
            if i > 0 {
                out.push_str("  ");
            }
            if i + 1 < row.len() {
                let w = if i < n_cols { widths[i] } else { 0 };
                out.push_str(&format!("{:<width$}", cell, width = w));
            } else {
                out.push_str(cell);
            }
        }
        out.push('\n');
    }

    out
}

/// Build a minijinja context `Value` from a `CacheRow`.
fn row_context(r: &CacheRow) -> minijinja::Value {
    let mut ctx = serde_json::Map::new();
    ctx.insert(
        "provider".into(),
        serde_json::Value::String(r.provider.clone()),
    );
    ctx.insert(
        "path".into(),
        match &r.path {
            Some(p) => serde_json::Value::String(p.clone()),
            None => serde_json::Value::Null,
        },
    );
    ctx.insert("field".into(), serde_json::Value::String(r.field.clone()));
    ctx.insert("value".into(), r.value.clone());
    ctx.insert("age_ms".into(), serde_json::json!(r.age_ms));
    ctx.insert(
        "age_human".into(),
        serde_json::Value::String(format_age(r.age_ms)),
    );
    ctx.insert("stale".into(), serde_json::Value::Bool(r.stale));
    minijinja::Value::from_serialize(serde_json::Value::Object(ctx))
}

/// Scan `template` for `{{ varname }}` patterns and return uppercased names
/// in order of first appearance. Only simple top-level variable references
/// (identifiers immediately after `{{`) are extracted; dotted paths and filter
/// expressions are ignored.
fn extract_template_header(template: &str) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    let bytes = template.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i + 1 < len {
        if bytes[i] == b'{' && bytes[i + 1] == b'{' {
            i += 2;
            // Skip whitespace.
            while i < len && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            // Read identifier.
            let start = i;
            while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let name = &template[start..i];
            if !name.is_empty() {
                let upper = name.to_uppercase();
                if !seen.contains(&upper) {
                    seen.push(upper);
                }
            }
        } else {
            i += 1;
        }
    }

    seen
}

// ---------------------------------------------------------------------------
// JSON (NDJSON) renderer
// ---------------------------------------------------------------------------

fn render_json(rows: &[CacheRow]) -> String {
    let mut out = String::new();
    for row in rows {
        if let Ok(s) = serde_json::to_string(row) {
            out.push_str(&s);
            out.push('\n');
        }
    }
    out
}

// ---------------------------------------------------------------------------
// TSV renderer
// ---------------------------------------------------------------------------

fn render_tsv(rows: &[CacheRow]) -> String {
    let mut out = String::new();
    for row in rows {
        let path = row.path.as_deref().unwrap_or("-");
        let value = value_to_string(&row.value);
        let stale = if row.stale { "true" } else { "false" };
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\n",
            row.provider, path, row.field, value, row.age_ms, stale
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// CSV renderer (RFC 4180)
// ---------------------------------------------------------------------------

fn render_csv(rows: &[CacheRow]) -> String {
    let mut out = String::new();

    // Header row
    out.push_str("PROVIDER,PATH,FIELD,VALUE,AGE,STALE\n");

    for row in rows {
        let path = row.path.as_deref().unwrap_or("-");
        let value = value_to_string(&row.value);
        let stale = if row.stale { "true" } else { "false" };
        out.push_str(&format!(
            "{},{},{},{},{},{}\n",
            csv_quote(&row.provider),
            csv_quote(path),
            csv_quote(&row.field),
            csv_quote(&value),
            row.age_ms,
            stale
        ));
    }
    out
}

/// RFC 4180 quoting: wrap in double-quotes if value contains `,`, `"`, or newline.
/// Internal double-quotes are doubled.
fn csv_quote(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        let escaped = s.replace('"', "\"\"");
        format!("\"{escaped}\"")
    } else {
        s.to_string()
    }
}

// ---------------------------------------------------------------------------
// Shell-sourceable renderer
// ---------------------------------------------------------------------------

fn render_sh(rows: &[CacheRow]) -> String {
    let mut out = String::new();
    for row in rows {
        let key = sanitize_sh_key(&row.provider, row.path.as_deref(), &row.field);
        let value = value_to_string(&row.value);
        let quoted = shell_quote(&value);
        out.push_str(&format!("{key}={quoted}\n"));
    }
    out
}

/// Build a shell-safe variable name: `provider_path_field`.
/// Path segments replace `/` with `_`; leading/trailing underscores from empty parts removed.
fn sanitize_sh_key(provider: &str, path: Option<&str>, field: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.push(sanitize_ident(provider));
    if let Some(p) = path {
        // Replace slashes with underscores, then split on underscores to flatten.
        let normalized = p.replace('/', "_");
        // Strip leading underscores that result from leading slashes.
        let normalized = normalized.trim_matches('_');
        if !normalized.is_empty() {
            for segment in normalized.split('_').filter(|s| !s.is_empty()) {
                parts.push(sanitize_ident(segment));
            }
        }
    }
    parts.push(sanitize_ident(field));
    parts.join("_")
}

/// Replace any non-alphanumeric character with `_`.
fn sanitize_ident(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// Shell single-quote a string. Wrap in `'...'`; internal single quotes become `'\''`.
fn shell_quote(s: &str) -> String {
    let escaped = s.replace('\'', r"'\''");
    format!("'{escaped}'")
}

// ---------------------------------------------------------------------------
// Table / human renderer (shared implementation)
// ---------------------------------------------------------------------------

const ANSI_DIM: &str = "\x1b[2m";
const ANSI_RESET: &str = "\x1b[0m";

/// Render rows as aligned columns.
///
/// - `color`: apply ANSI dim to stale rows.
/// - `trunc`: maximum width for the VALUE column (in characters). `None` = no truncation.
/// - `header`: prepend a PROVIDER / PATH / FIELD / VALUE / AGE / STALE header row.
fn render_table(rows: &[CacheRow], color: bool, trunc: Option<usize>, header: bool) -> String {
    // Column indices: PROVIDER, PATH, FIELD, VALUE, AGE, STALE
    const COLS: usize = 6;

    // Build string cells.
    let mut cells: Vec<[String; COLS]> = Vec::new();

    if header {
        cells.push([
            "PROVIDER".to_string(),
            "PATH".to_string(),
            "FIELD".to_string(),
            "VALUE".to_string(),
            "AGE".to_string(),
            "STALE".to_string(),
        ]);
    }

    for row in rows {
        let path = row.path.as_deref().unwrap_or("-").to_string();
        let mut value = value_to_string(&row.value);
        if let Some(max) = trunc {
            value = truncate(&value, max);
        }
        let age = format_age(row.age_ms);
        let stale = if row.stale { "true" } else { "false" }.to_string();
        cells.push([
            row.provider.clone(),
            path,
            row.field.clone(),
            value,
            age,
            stale,
        ]);
    }

    if cells.is_empty() {
        return String::new();
    }

    // Compute max width per column.
    let mut col_widths = [0usize; COLS];
    for row in &cells {
        for (i, cell) in row.iter().enumerate() {
            let w = cell.chars().count();
            if w > col_widths[i] {
                col_widths[i] = w;
            }
        }
    }

    let mut out = String::new();

    // Iterate original rows alongside cells to determine stale flag per row.
    let header_offset = if header { 1 } else { 0 };

    for (cell_idx, row_cells) in cells.iter().enumerate() {
        let is_stale = if cell_idx < header_offset {
            // Header row is never stale.
            false
        } else {
            rows[cell_idx - header_offset].stale
        };

        if color && is_stale {
            out.push_str(ANSI_DIM);
        }

        // All columns except the last get right-padded.
        for (i, cell) in row_cells.iter().enumerate() {
            if i > 0 {
                out.push_str("  "); // 2-space separator
            }
            if i < COLS - 1 {
                let pad = col_widths[i].saturating_sub(cell.chars().count());
                out.push_str(cell);
                for _ in 0..pad {
                    out.push(' ');
                }
            } else {
                // Last column: no trailing padding.
                out.push_str(cell);
            }
        }

        if color && is_stale {
            out.push_str(ANSI_RESET);
        }
        out.push('\n');
    }

    out
}

// ---------------------------------------------------------------------------
// Filter and sort helpers (T30 / T31)
// ---------------------------------------------------------------------------

/// Apply a list of `key=value` filter strings to `rows`, returning only the rows
/// that match ALL predicates (AND semantics).
///
/// Supported keys: `provider`, `path`, `field`, `stale`.
/// Returns `Err` if any filter string uses an unknown key or invalid value.
pub fn apply_filters(rows: Vec<CacheRow>, filters: &[String]) -> Result<Vec<CacheRow>, String> {
    let preds: Vec<Predicate> = filters
        .iter()
        .map(|s| parse_filter(s))
        .collect::<Result<_, _>>()?;
    Ok(rows
        .into_iter()
        .filter(|r| preds.iter().all(|p| p.matches(r)))
        .collect())
}

enum Predicate {
    ProviderEq(String),
    ProviderGlob(String),
    PathGlob(String),
    PathDash,
    FieldEq(String),
    Stale(bool),
}

fn parse_filter(s: &str) -> Result<Predicate, String> {
    let (k, v) = s
        .split_once('=')
        .ok_or_else(|| format!("filter must be key=value: {s}"))?;
    match k {
        "provider" => {
            if v.contains('*') {
                Ok(Predicate::ProviderGlob(v.to_string()))
            } else {
                Ok(Predicate::ProviderEq(v.to_string()))
            }
        }
        "path" => {
            if v == "-" {
                Ok(Predicate::PathDash)
            } else {
                Ok(Predicate::PathGlob(v.to_string()))
            }
        }
        "field" => Ok(Predicate::FieldEq(v.to_string())),
        "stale" => match v {
            "true" => Ok(Predicate::Stale(true)),
            "false" => Ok(Predicate::Stale(false)),
            other => Err(format!("stale= must be true or false, got {other}")),
        },
        other => Err(format!("unknown filter key: {other}")),
    }
}

impl Predicate {
    fn matches(&self, r: &CacheRow) -> bool {
        match self {
            Predicate::ProviderEq(v) => &r.provider == v,
            Predicate::ProviderGlob(pat) => simple_glob_match(pat, &r.provider),
            Predicate::PathGlob(pat) => {
                r.path.as_deref().is_some_and(|p| simple_glob_match(pat, p))
            }
            Predicate::PathDash => r.path.is_none(),
            Predicate::FieldEq(v) => &r.field == v,
            Predicate::Stale(b) => r.stale == *b,
        }
    }
}

/// Simple glob matcher supporting only `*` as a wildcard. Anchored start and end.
fn simple_glob_match(pattern: &str, text: &str) -> bool {
    fn helper(p: &[char], t: &[char]) -> bool {
        match (p.first(), t.first()) {
            (None, None) => true,
            (None, _) => false,
            (Some('*'), _) => {
                for i in 0..=t.len() {
                    if helper(&p[1..], &t[i..]) {
                        return true;
                    }
                }
                false
            }
            (Some(_), None) => false,
            (Some(pc), Some(tc)) if pc == tc => helper(&p[1..], &t[1..]),
            _ => false,
        }
    }
    helper(
        &pattern.chars().collect::<Vec<_>>(),
        &text.chars().collect::<Vec<_>>(),
    )
}

/// Sort `rows` by the given column name (ascending, stable).
///
/// Valid columns: `provider`, `path`, `field`, `value`, `age`, `stale`.
/// Returns `Err` for any other column name.
pub fn apply_sort(mut rows: Vec<CacheRow>, col: &str) -> Result<Vec<CacheRow>, String> {
    match col {
        "default" => rows.sort_by(|a, b| {
            a.provider
                .cmp(&b.provider)
                .then(a.path.cmp(&b.path))
                .then(a.field.cmp(&b.field))
        }),
        "provider" => rows.sort_by(|a, b| a.provider.cmp(&b.provider)),
        "path" => rows.sort_by(|a, b| a.path.cmp(&b.path)),
        "field" => rows.sort_by(|a, b| a.field.cmp(&b.field)),
        "value" => rows.sort_by(|a, b| a.value.to_string().cmp(&b.value.to_string())),
        "age" => rows.sort_by_key(|r| r.age_ms),
        "stale" => rows.sort_by_key(|r| r.stale),
        other => return Err(format!("invalid sort column: {other}")),
    }
    Ok(rows)
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Convert a `serde_json::Value` to a display string.
/// Strings are returned unquoted; other types use their JSON representation.
pub fn value_to_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Format an age in milliseconds as a human-readable duration.
///
/// Examples: `0s`, `14s`, `3m`, `2h14m`.
pub fn format_age(age_ms: u128) -> String {
    let secs = age_ms / 1000;
    if secs < 60 {
        return format!("{secs}s");
    }
    let minutes = secs / 60;
    if minutes < 60 {
        let s = secs % 60;
        if s == 0 {
            return format!("{minutes}m");
        }
        return format!("{minutes}m{s}s");
    }
    let hours = minutes / 60;
    let m = minutes % 60;
    if m == 0 {
        return format!("{hours}h");
    }
    format!("{hours}h{m}m")
}

/// Truncate a string to at most `max` characters, appending `…` if trimmed.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        // Reserve 3 chars for "...".
        let take = max.saturating_sub(3);
        let truncated: String = s.chars().take(take).collect();
        format!("{truncated}...")
    }
}

#[cfg(test)]
mod ansi_tests {
    use super::ansi;

    #[test]
    fn colour_codes_match_basic_16() {
        assert_eq!(
            ansi::wrap("hi", ansi::Color::BrightGreen, true),
            "\x1b[92mhi\x1b[0m"
        );
        assert_eq!(
            ansi::wrap("hi", ansi::Color::Yellow, true),
            "\x1b[33mhi\x1b[0m"
        );
        assert_eq!(
            ansi::wrap("hi", ansi::Color::BrightYellow, true),
            "\x1b[93mhi\x1b[0m"
        );
        assert_eq!(
            ansi::wrap("hi", ansi::Color::Red, true),
            "\x1b[31mhi\x1b[0m"
        );
        assert_eq!(
            ansi::wrap("hi", ansi::Color::Dim, true),
            "\x1b[2mhi\x1b[0m"
        );
    }

    #[test]
    fn colour_disabled_returns_plain() {
        assert_eq!(ansi::wrap("hi", ansi::Color::Red, false), "hi");
    }
}

#[cfg(test)]
mod ttl_cell_tests {
    use super::*;
    use crate::cache::{FailureSnapshot, RowKind};

    #[test]
    fn active_lifecycle_unicode() {
        let cell = format_ttl_cell(
            Some(&RowKind::Lifecycle {
                decay: 0,
                watches_files: true,
            }),
            Some(60),
            Some(12),
            Some(true),
            None,
            false,
        );
        assert_eq!(
            cell.text,
            "\u{2605}     60s\u{00d7}12 \u{25c9}"
        );
        assert!(matches!(cell.color, Some(ansi::Color::BrightGreen)));
    }

    #[test]
    fn decay4_lifecycle_unicode() {
        let cell = format_ttl_cell(
            Some(&RowKind::Lifecycle {
                decay: 4,
                watches_files: false,
            }),
            Some(480),
            Some(12),
            Some(false),
            None,
            false,
        );
        assert_eq!(cell.text, "0    480s\u{00d7}12  ");
        assert!(matches!(cell.color, Some(ansi::Color::Red)));
    }

    #[test]
    fn ascii_fallback() {
        let cell = format_ttl_cell(
            Some(&RowKind::Lifecycle {
                decay: 0,
                watches_files: true,
            }),
            Some(60),
            Some(12),
            Some(true),
            None,
            true,
        );
        assert_eq!(cell.text, "*     60sx12 F");
    }

    #[test]
    fn indicator_suppressed_when_no_watch_capability() {
        let cell = format_ttl_cell(
            Some(&RowKind::Lifecycle {
                decay: 0,
                watches_files: false,
            }),
            Some(60),
            Some(12),
            Some(true),
            None,
            false,
        );
        assert!(cell.text.ends_with("12  "), "no indicator: {:?}", cell.text);
    }

    #[test]
    fn indicator_suppressed_when_flag_false() {
        let cell = format_ttl_cell(
            Some(&RowKind::Lifecycle {
                decay: 0,
                watches_files: true,
            }),
            Some(60),
            Some(12),
            Some(false),
            None,
            false,
        );
        assert!(cell.text.ends_with("12  "));
    }

    #[test]
    fn once_renders_dashes() {
        let cell = format_ttl_cell(Some(&RowKind::Once), None, None, None, None, false);
        assert_eq!(cell.text, "---");
        assert!(cell.color.is_none());
    }

    #[test]
    fn virtual_renders_dashes() {
        let cell = format_ttl_cell(Some(&RowKind::Virtual), None, None, None, None, false);
        assert_eq!(cell.text, "---");
    }

    #[test]
    fn transient_renders_dashes() {
        let cell = format_ttl_cell(Some(&RowKind::Transient), None, None, None, None, false);
        assert_eq!(cell.text, "---");
    }

    #[test]
    fn failure_swaps_lead_to_warn() {
        let snap = FailureSnapshot {
            consecutive_failures: 3,
            suppressed_until_unix_ms: None,
        };
        let cell = format_ttl_cell(
            Some(&RowKind::Lifecycle {
                decay: 1,
                watches_files: true,
            }),
            Some(60),
            Some(12),
            Some(true),
            Some(&snap),
            false,
        );
        assert!(cell.text.starts_with("\u{26a0}"));
        assert!(matches!(cell.color, Some(ansi::Color::Red)));
    }

    #[test]
    fn failure_ascii_fallback() {
        let snap = FailureSnapshot {
            consecutive_failures: 3,
            suppressed_until_unix_ms: None,
        };
        let cell = format_ttl_cell(
            Some(&RowKind::Lifecycle {
                decay: 1,
                watches_files: true,
            }),
            Some(60),
            Some(12),
            Some(true),
            Some(&snap),
            true,
        );
        assert!(cell.text.starts_with("!"));
    }

    #[test]
    fn p_cap_at_six_chars() {
        let cell = format_ttl_cell(
            Some(&RowKind::Lifecycle {
                decay: 0,
                watches_files: false,
            }),
            Some(9_999_999),
            Some(12),
            Some(false),
            None,
            false,
        );
        // P clamped to 999999.
        assert!(cell.text.contains("999999s"));
    }

    #[test]
    fn p_zero_does_not_panic() {
        let cell = format_ttl_cell(
            Some(&RowKind::Lifecycle {
                decay: 0,
                watches_files: false,
            }),
            Some(0),
            Some(0),
            Some(false),
            None,
            false,
        );
        assert!(cell.text.contains("0s"));
        assert!(cell.text.contains("\u{00d7}00"));
    }
}
