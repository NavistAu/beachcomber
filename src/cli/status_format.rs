use crate::cache::CacheRow;

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
/// Unknown presets fall back to `tsv` (non-interactive safe default).
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
        // Unknown or custom-template presets (T29 handles templates) fall through to tsv.
        _ => render_tsv(rows),
    }
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
fn render_table(
    rows: &[CacheRow],
    color: bool,
    trunc: Option<usize>,
    header: bool,
) -> String {
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
