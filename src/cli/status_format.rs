use crate::cache::CacheRow;
use libbeachcomber::filters::build_env;
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

/// Maximum width for the P-seconds sub-field. 6 digits covers ~11 days of
/// seconds; beyond that we clamp rather than grow the cell.
pub const TTL_P_WIDTH_MAX: usize = 6;

/// Compute the P-seconds field width for a snapshot: the widest `poll_interval_secs`
/// across all Lifecycle rows, capped at `TTL_P_WIDTH_MAX`. No lower floor — the
/// width auto-shrinks so single-digit-P snapshots don't over-pad.
///
/// Rows without a poll interval (Once/Virtual/Transient) are ignored. Empty
/// snapshots (no lifecycle rows) return 1; the value is unused in that case
/// because every cell renders as `---`, but avoid zero so format! can't panic.
pub fn compute_ttl_p_width(rows: &[CacheRow]) -> usize {
    use crate::cache::RowKind;
    rows.iter()
        .filter(|r| matches!(r.kind, Some(RowKind::Lifecycle { .. })))
        .filter_map(|r| r.poll_interval_secs)
        .map(|p| p.min(999_999).to_string().len())
        .max()
        .unwrap_or(1)
        .min(TTL_P_WIDTH_MAX)
}

#[allow(dead_code)]
pub fn format_ttl_cell(
    kind: Option<&crate::cache::RowKind>,
    poll_interval_secs: Option<u64>,
    keep_alive_polls: Option<u32>,
    fsevents_reinstate: Option<bool>,
    failure: Option<&crate::cache::FailureSnapshot>,
    ascii: bool,
    p_width: usize,
) -> TtlCell {
    use crate::cache::RowKind;

    // Glyph table.
    let star = if ascii { "*" } else { "\u{2605}" }; // ★
    let warn = if ascii { "!" } else { "\u{26a0}" }; // ⚠
    let times = if ascii { "x" } else { "\u{00d7}" }; // ×
    // Watches-files indicator progression:
    //   watches only       → `∙` / `-`
    //   watches + reinstate → `⊙` / `+`  (bullet with a ring around it)
    // Both glyphs are math operator class (U+22xx / U+2299), so fonts render
    // them at a shared baseline. Earlier pairs mixed General Punctuation and
    // Math Symbols-B blocks and rendered vertically mis-aligned.
    let dot = if ascii { "-" } else { "\u{2219}" }; // ∙
    let dot_ring = if ascii { "+" } else { "\u{2299}" }; // ⊙

    match kind {
        None | Some(RowKind::Once) | Some(RowKind::Virtual) | Some(RowKind::Transient) => TtlCell {
            text: "---".into(),
            color: None,
        },
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

            // Indicator keyed on fs-watching capability first; reinstate-armed is
            // the decorator. Poll-only sources render a blank cell trailer.
            let indicator = match (*watches_files, fsevents_reinstate.unwrap_or(false)) {
                (false, _) => " ",
                (true, false) => dot,
                (true, true) => dot_ring,
            };
            // The poll segment (`{p}s×{k:02}`) only renders for sources with a
            // poll path: Poll and WatchAndPoll. Pure Watch sources have no poll;
            // their cells pad with spaces of the same width so the indicator
            // glyph stays in a consistent column across all rows.
            //
            // Width of the poll segment: p_width digits + "s" + "×" + "kk" = p_width + 4.
            let poll_seg = match (poll_interval_secs, keep_alive_polls) {
                (Some(p), Some(k)) => format!(
                    "{:>width$}s{}{:02}",
                    p.min(999_999),
                    times,
                    k.min(99),
                    width = p_width
                ),
                _ => " ".repeat(p_width + 4),
            };
            let text = format!("{} {} {}", lead, poll_seg, indicator);
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

/// Resolve the maximum **total table** width.
///
/// - `None` arg → use `terminal_cols` if available, else the default (120).
/// - `Some("auto")` → use `terminal_cols` if available, else the default.
/// - `Some(n)` → parse `n` as a `usize`, falling back to the default on error.
///
/// The returned value is the cap for the **entire rendered row** (all columns +
/// padding). The renderer shrinks the VALUE column as needed to stay within this
/// budget, keeping all other columns at their natural widths.
pub fn resolve_max_width(arg: Option<&str>, terminal_cols: Option<usize>) -> usize {
    const DEFAULT: usize = 120;
    match arg {
        None => terminal_cols.unwrap_or(DEFAULT),
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
    /// Use ASCII fallback glyphs in the human preset (e.g. `*` instead of `★`).
    pub ascii: bool,
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
            max_width: Some(120),
            no_trunc: false,
            ascii: false,
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
        "table" => render_table(rows, false, None, true, false),
        "human" => {
            let trunc = if opts.no_trunc { None } else { opts.max_width };
            // `no_color` is the resolved color decision from the caller (already
            // accounts for NO_COLOR, TTY, WATCH_INTERVAL). Don't re-AND with is_tty
            // here — that would undo the WATCH_INTERVAL color promotion.
            let color = !opts.no_color;
            render_table(rows, color, trunc, true, opts.ascii)
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
pub fn row_context(r: &CacheRow) -> minijinja::Value {
    use crate::cache::RowKind;
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
    // kind — snake_case discriminator, empty string when None
    let kind_str = match &r.kind {
        Some(RowKind::Lifecycle { .. }) => "lifecycle",
        Some(RowKind::Once) => "once",
        Some(RowKind::Virtual) => "virtual",
        Some(RowKind::Transient) => "transient",
        None => "",
    };
    ctx.insert("kind".into(), serde_json::Value::String(kind_str.into()));
    // decay — only for lifecycle
    if let Some(RowKind::Lifecycle { decay, .. }) = &r.kind {
        ctx.insert("decay".into(), serde_json::json!(*decay));
    }
    // optional lifecycle fields
    if let Some(p) = r.poll_interval_secs {
        ctx.insert("poll_interval_secs".into(), serde_json::json!(p));
    }
    if let Some(k) = r.keep_alive_polls {
        ctx.insert("keep_alive_polls".into(), serde_json::json!(k));
    }
    if let Some(rv) = r.fsevents_reinstate {
        ctx.insert("fsevents_reinstate".into(), serde_json::Value::Bool(rv));
    }
    if let Some(n) = r.polls_elapsed {
        ctx.insert("polls_elapsed".into(), serde_json::json!(n));
    }
    // failure object
    if let Some(f) = &r.failure {
        let mut fobj = serde_json::Map::new();
        fobj.insert(
            "consecutive_failures".into(),
            serde_json::json!(f.consecutive_failures),
        );
        if let Some(su) = f.suppressed_until_unix_ms {
            fobj.insert("suppressed_until_unix_ms".into(), serde_json::json!(su));
        }
        ctx.insert("failure".into(), serde_json::Value::Object(fobj));
    }
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

pub fn render_tsv(rows: &[CacheRow]) -> String {
    use crate::cache::RowKind;
    let mut out = String::new();
    for row in rows {
        let kind = match &row.kind {
            Some(RowKind::Lifecycle { .. }) => "lifecycle",
            Some(RowKind::Once) => "once",
            Some(RowKind::Virtual) => "virtual",
            Some(RowKind::Transient) => "transient",
            None => "",
        };
        let decay = match &row.kind {
            Some(RowKind::Lifecycle { decay, .. }) => decay.to_string(),
            _ => String::new(),
        };
        let path = row.path.as_deref().unwrap_or("");
        let value = serde_json::to_string(&row.value).unwrap_or_default();
        let p = row
            .poll_interval_secs
            .map(|n| n.to_string())
            .unwrap_or_default();
        let k = row
            .keep_alive_polls
            .map(|n| n.to_string())
            .unwrap_or_default();
        let r = row
            .fsevents_reinstate
            .map(|b| b.to_string())
            .unwrap_or_default();
        let fc = row
            .failure
            .as_ref()
            .map(|f| f.consecutive_failures.to_string())
            .unwrap_or_default();
        let fs = row
            .failure
            .as_ref()
            .and_then(|f| f.suppressed_until_unix_ms.map(|n| n.to_string()))
            .unwrap_or_default();
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            row.provider,
            path,
            row.field,
            value,
            row.age_ms,
            row.stale,
            kind,
            decay,
            p,
            k,
            r,
            fc,
            fs,
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// CSV renderer (RFC 4180)
// ---------------------------------------------------------------------------

pub fn render_csv(rows: &[CacheRow]) -> String {
    use crate::cache::RowKind;
    let mut out = String::new();

    // Header row
    out.push_str("PROVIDER,PATH,FIELD,VALUE,AGE_MS,STALE,KIND,DECAY,POLL_INTERVAL_SECS,KEEP_ALIVE_POLLS,FSEVENTS_REINSTATE,FAILURE_CONSECUTIVE_FAILURES,FAILURE_SUPPRESSED_UNTIL_UNIX_MS\n");

    for row in rows {
        let path = row.path.as_deref().unwrap_or("");
        let value = serde_json::to_string(&row.value).unwrap_or_default();
        let stale = if row.stale { "true" } else { "false" };
        let kind = match &row.kind {
            Some(RowKind::Lifecycle { .. }) => "lifecycle",
            Some(RowKind::Once) => "once",
            Some(RowKind::Virtual) => "virtual",
            Some(RowKind::Transient) => "transient",
            None => "",
        };
        let decay = match &row.kind {
            Some(RowKind::Lifecycle { decay, .. }) => decay.to_string(),
            _ => String::new(),
        };
        let p = row
            .poll_interval_secs
            .map(|n| n.to_string())
            .unwrap_or_default();
        let k = row
            .keep_alive_polls
            .map(|n| n.to_string())
            .unwrap_or_default();
        let r = row
            .fsevents_reinstate
            .map(|b| b.to_string())
            .unwrap_or_default();
        let fc = row
            .failure
            .as_ref()
            .map(|f| f.consecutive_failures.to_string())
            .unwrap_or_default();
        let fs = row
            .failure
            .as_ref()
            .and_then(|f| f.suppressed_until_unix_ms.map(|n| n.to_string()))
            .unwrap_or_default();
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{},{},{},{},{},{}\n",
            csv_quote(&row.provider),
            csv_quote(path),
            csv_quote(&row.field),
            csv_quote(&value),
            row.age_ms,
            stale,
            csv_quote(kind),
            csv_quote(&decay),
            csv_quote(&p),
            csv_quote(&k),
            csv_quote(&r),
            csv_quote(&fc),
            csv_quote(&fs),
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

pub fn render_sh_env(rows: &[CacheRow]) -> String {
    use crate::cache::RowKind;
    let mut out = String::new();
    for row in rows {
        let key = sanitize_sh_key(&row.provider, row.path.as_deref(), &row.field);
        let value = value_to_string(&row.value);
        let quoted = shell_quote(&value);
        out.push_str(&format!("{key}={quoted}\n"));
        // kind
        let kind_str = match &row.kind {
            Some(RowKind::Lifecycle { .. }) => Some("lifecycle"),
            Some(RowKind::Once) => Some("once"),
            Some(RowKind::Virtual) => Some("virtual"),
            Some(RowKind::Transient) => Some("transient"),
            None => None,
        };
        if let Some(kind_val) = kind_str {
            out.push_str(&format!("{key}_KIND={}\n", shell_quote(kind_val)));
        }
        // decay (only for lifecycle)
        if let Some(RowKind::Lifecycle { decay, .. }) = &row.kind {
            out.push_str(&format!(
                "{key}_DECAY={}\n",
                shell_quote(&decay.to_string())
            ));
        }
        // poll_interval_secs
        if let Some(p) = row.poll_interval_secs {
            out.push_str(&format!(
                "{key}_POLL_INTERVAL_SECS={}\n",
                shell_quote(&p.to_string())
            ));
        }
        // keep_alive_polls
        if let Some(k) = row.keep_alive_polls {
            out.push_str(&format!(
                "{key}_KEEP_ALIVE_POLLS={}\n",
                shell_quote(&k.to_string())
            ));
        }
        // fsevents_reinstate
        if let Some(r) = row.fsevents_reinstate {
            out.push_str(&format!(
                "{key}_FSEVENTS_REINSTATE={}\n",
                shell_quote(&r.to_string())
            ));
        }
        // failure fields
        if let Some(f) = &row.failure {
            out.push_str(&format!(
                "{key}_FAILURE_CONSECUTIVE_FAILURES={}\n",
                shell_quote(&f.consecutive_failures.to_string())
            ));
            if let Some(su) = f.suppressed_until_unix_ms {
                out.push_str(&format!(
                    "{key}_FAILURE_SUPPRESSED_UNTIL_UNIX_MS={}\n",
                    shell_quote(&su.to_string())
                ));
            }
        }
    }
    out
}

fn render_sh(rows: &[CacheRow]) -> String {
    render_sh_env(rows)
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

// Column indices for the human/table preset.
const COLS: usize = 6;
// TTL is the last column (index 5); failure-state colouring skips it.
const TTL_COL_IDX: usize = 5;
const HEADERS: [&str; COLS] = ["PROVIDER", "PATH", "FIELD", "VALUE", "AGE", "TTL"];

/// Render rows as aligned columns for the `human` preset.
///
/// - `color_enabled`: apply per-cell ANSI colour (AGE yellow/green by stale, TTL by decay,
///   failure rows red on non-TTL cells).
/// - `trunc`: maximum width for the VALUE column (in characters). `None` = no truncation.
/// - `header`: prepend a PROVIDER / PATH / FIELD / VALUE / AGE / TTL header row.
/// - `ascii`: use ASCII-only glyphs in the TTL cell instead of Unicode.
pub fn render_human(rows: &[CacheRow], opts: &FormatOptions) -> String {
    render_table(rows, false, None, true, opts.ascii)
}

fn render_table(
    rows: &[CacheRow],
    color_enabled: bool,
    trunc: Option<usize>,
    header: bool,
    ascii: bool,
) -> String {
    // `trunc` is the **total table width cap** (all columns + padding).
    // We scan all rows first to measure natural column widths, then compute
    // how much room the VALUE column has once other columns claim their natural
    // widths and inter-column separators are accounted for.

    // -----------------------------------------------------------------------
    // Pass 1: measure natural widths of every column (no truncation).
    // Columns: PROVIDER, PATH, FIELD, VALUE, AGE, TTL
    // -----------------------------------------------------------------------
    let mut nat_provider = HEADERS[0].len();
    let mut nat_path = HEADERS[1].len();
    let mut nat_field = HEADERS[2].len();
    let mut nat_age = HEADERS[4].len();
    let mut nat_ttl = HEADERS[5].len();

    // Auto-size the P field inside each TTL cell to the widest P in this
    // snapshot, clamped to [TTL_P_WIDTH_MIN, TTL_P_WIDTH_MAX].
    let p_width = compute_ttl_p_width(rows);

    for row in rows {
        let path = row.path.as_deref().unwrap_or("-");
        nat_provider = nat_provider.max(row.provider.chars().count());
        nat_path = nat_path.max(path.chars().count());
        nat_field = nat_field.max(row.field.chars().count());
        nat_age = nat_age.max(
            format_age_with_polls(row.age_ms, row.polls_elapsed, ascii)
                .chars()
                .count(),
        );
        nat_ttl = nat_ttl.max(
            format_ttl_cell(
                row.kind.as_ref(),
                row.poll_interval_secs,
                row.keep_alive_polls,
                row.fsevents_reinstate,
                row.failure.as_ref(),
                ascii,
                p_width,
            )
            .text
            .chars()
            .count(),
        );
    }

    // -----------------------------------------------------------------------
    // Compute effective VALUE cap from the total-table-width budget.
    // Layout: MARGIN provider(2sp)path(2sp)field(2sp)value(2sp)age(2sp)ttl MARGIN
    // Separators: 5 × 2 = 10
    // Margins: 2 spaces each side (breathing room so the table doesn't hug
    // the terminal edge).
    // Non-VALUE: nat_provider + nat_path + nat_field + nat_age + nat_ttl + 10 + 4
    // -----------------------------------------------------------------------
    const MIN_VALUE_WIDTH: usize = 8;
    const SEP_TOTAL: usize = 10; // 5 separators × 2 spaces
    const MARGIN: usize = 2; // spaces applied to both the left and right edges
    const MARGIN_TOTAL: usize = MARGIN * 2;

    let value_cap: Option<usize> = trunc.map(|total_cap| {
        let non_value =
            nat_provider + nat_path + nat_field + nat_age + nat_ttl + SEP_TOTAL + MARGIN_TOTAL;
        if total_cap > non_value + MIN_VALUE_WIDTH {
            total_cap - non_value
        } else {
            MIN_VALUE_WIDTH
        }
    });

    // -----------------------------------------------------------------------
    // Pass 2: build cells, applying VALUE cap only.
    // -----------------------------------------------------------------------

    // Build string cells — each row is a fixed-length array of display strings.
    // The array may contain ANSI escape sequences; col_widths tracks the
    // *visible* char count (without escapes) for alignment.
    let mut cells: Vec<[String; COLS]> = Vec::new();
    // Parallel vec of visible widths for each cell (escape-stripped).
    let mut visible: Vec<[usize; COLS]> = Vec::new();

    if header {
        let header_row: [String; COLS] = HEADERS.map(|h| h.to_string());
        let header_vis: [usize; COLS] = HEADERS.map(|h| h.len());
        cells.push(header_row);
        visible.push(header_vis);
    }

    for row in rows {
        let path = row.path.as_deref().unwrap_or("-").to_string();
        let mut value = value_to_string(&row.value);
        if let Some(max) = value_cap {
            value = truncate(&value, max);
        }

        // AGE cell — coloured by stale state.
        let age_text = format_age_with_polls(row.age_ms, row.polls_elapsed, ascii);
        let age_vis = age_text.chars().count();
        let age_cell = if color_enabled {
            if row.stale {
                ansi::wrap(&age_text, ansi::Color::Yellow, true)
            } else {
                ansi::wrap(&age_text, ansi::Color::BrightGreen, true)
            }
        } else {
            age_text
        };

        // TTL cell — built from lifecycle metadata.
        let ttl = format_ttl_cell(
            row.kind.as_ref(),
            row.poll_interval_secs,
            row.keep_alive_polls,
            row.fsevents_reinstate,
            row.failure.as_ref(),
            ascii,
            p_width,
        );
        let ttl_vis = ttl.text.chars().count();
        let ttl_cell = match (color_enabled, ttl.color) {
            (true, Some(c)) => ansi::wrap(&ttl.text, c, true),
            _ => ttl.text,
        };

        let provider_vis = row.provider.chars().count();
        let path_vis = path.chars().count();
        let field_vis = row.field.chars().count();
        let value_vis = value.chars().count();

        let mut row_cells: [String; COLS] = [
            row.provider.clone(),
            path,
            row.field.clone(),
            value,
            age_cell,
            ttl_cell,
        ];

        // Failure-state colouring: red on all cells except TTL (which already
        // has its own ⚠ + red from format_ttl_cell).
        if color_enabled && row.failure.is_some() {
            for (i, cell) in row_cells.iter_mut().enumerate() {
                if i != TTL_COL_IDX {
                    *cell = ansi::wrap(cell, ansi::Color::Red, true);
                }
            }
        }

        let row_vis: [usize; COLS] = [
            provider_vis,
            path_vis,
            field_vis,
            value_vis,
            age_vis,
            ttl_vis,
        ];

        cells.push(row_cells);
        visible.push(row_vis);
    }

    if cells.is_empty() {
        return String::new();
    }

    // Compute max visible width per column.
    let mut col_widths = [0usize; COLS];
    for row_vis in &visible {
        for (i, &w) in row_vis.iter().enumerate() {
            if w > col_widths[i] {
                col_widths[i] = w;
            }
        }
    }

    let mut out = String::new();

    let left_margin: String = " ".repeat(MARGIN);
    let right_margin: String = " ".repeat(MARGIN);
    for (row_cells, row_vis) in cells.iter().zip(visible.iter()) {
        out.push_str(&left_margin);
        for (i, cell) in row_cells.iter().enumerate() {
            if i > 0 {
                out.push_str("  "); // 2-space separator
            }
            out.push_str(cell);
            // Pad to column width on all but the last column.
            if i < COLS - 1 {
                let pad = col_widths[i].saturating_sub(row_vis[i]);
                for _ in 0..pad {
                    out.push(' ');
                }
            }
        }
        out.push_str(&right_margin);
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
    /// Match Lifecycle rows with the given decay level (0=active, 1-4=decayN).
    LifecycleDecay(u8),
    /// Match Once rows.
    LifecycleOnce,
    /// Match Virtual rows.
    LifecycleVirtual,
    /// Match on fsevents_reinstate flag value.
    FseventsReinstate(bool),
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
        "lifecycle" => match v {
            "active" => Ok(Predicate::LifecycleDecay(0)),
            "decay1" => Ok(Predicate::LifecycleDecay(1)),
            "decay2" => Ok(Predicate::LifecycleDecay(2)),
            "decay3" => Ok(Predicate::LifecycleDecay(3)),
            "decay4" => Ok(Predicate::LifecycleDecay(4)),
            "once" => Ok(Predicate::LifecycleOnce),
            "virtual" => Ok(Predicate::LifecycleVirtual),
            other => Err(format!(
                "lifecycle= must be active|decay1|decay2|decay3|decay4|once|virtual, got {other}"
            )),
        },
        "fsevents_reinstate" => match v {
            "true" => Ok(Predicate::FseventsReinstate(true)),
            "false" => Ok(Predicate::FseventsReinstate(false)),
            other => Err(format!(
                "fsevents_reinstate= must be true or false, got {other}"
            )),
        },
        other => Err(format!("unknown filter key: {other}")),
    }
}

impl Predicate {
    fn matches(&self, r: &CacheRow) -> bool {
        use crate::cache::RowKind;
        match self {
            Predicate::ProviderEq(v) => &r.provider == v,
            Predicate::ProviderGlob(pat) => simple_glob_match(pat, &r.provider),
            Predicate::PathGlob(pat) => {
                r.path.as_deref().is_some_and(|p| simple_glob_match(pat, p))
            }
            Predicate::PathDash => r.path.is_none(),
            Predicate::FieldEq(v) => &r.field == v,
            Predicate::Stale(b) => r.stale == *b,
            Predicate::LifecycleDecay(target) => matches!(
                &r.kind,
                Some(RowKind::Lifecycle { decay, .. }) if decay == target
            ),
            Predicate::LifecycleOnce => matches!(&r.kind, Some(RowKind::Once)),
            Predicate::LifecycleVirtual => matches!(&r.kind, Some(RowKind::Virtual)),
            Predicate::FseventsReinstate(b) => r.fsevents_reinstate.unwrap_or(false) == *b,
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
/// Valid columns: `provider`, `path`, `field`, `value`, `age`, `stale`, `lifecycle`,
/// `poll_interval`.
/// Returns `Err` for any other column name.
pub fn apply_sort(mut rows: Vec<CacheRow>, col: &str) -> Result<Vec<CacheRow>, String> {
    use crate::cache::RowKind;
    match col {
        // path first so globals (path=None) bubble to the top as one group,
        // then path-scoped rows are grouped by directory. Inside a path group,
        // rows are ordered by provider then field.
        "default" => rows.sort_by(|a, b| {
            a.path
                .cmp(&b.path)
                .then(a.provider.cmp(&b.provider))
                .then(a.field.cmp(&b.field))
        }),
        "provider" => rows.sort_by(|a, b| a.provider.cmp(&b.provider)),
        "path" => rows.sort_by(|a, b| a.path.cmp(&b.path)),
        "field" => rows.sort_by(|a, b| a.field.cmp(&b.field)),
        "value" => rows.sort_by_key(|r| r.value.to_string()),
        "age" => rows.sort_by_key(|r| r.age_ms),
        "stale" => rows.sort_by_key(|r| r.stale),
        // Most-decayed first: Lifecycle{4} < Lifecycle{3} < ... < Lifecycle{0},
        // then Once, Virtual, Transient, None.
        "lifecycle" => rows.sort_by_key(|r| match &r.kind {
            Some(RowKind::Lifecycle { decay, .. }) => (0u8, 4u8 - *decay),
            Some(RowKind::Once) => (1, 0),
            Some(RowKind::Virtual) => (2, 0),
            Some(RowKind::Transient) => (3, 0),
            None => (4, 0),
        }),
        // Slowest-pollers first (descending poll_interval); None last.
        "poll_interval" => {
            rows.sort_by_key(|r| std::cmp::Reverse(r.poll_interval_secs.unwrap_or(0)))
        }
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

/// Format an age with an optional poll-iteration suffix (`×N` / `xN` in ascii mode).
///
/// Used for the AGE column in `comb status` table rendering. The suffix shows how
/// many polls have fired in the current lifecycle step, giving the user a sense of
/// where they are in the K-poll window without computing it from the TTL column.
fn format_age_with_polls(age_ms: u128, polls_elapsed: Option<u32>, ascii: bool) -> String {
    let base = format_age(age_ms);
    match polls_elapsed {
        Some(n) => {
            let sep = if ascii { "x" } else { "\u{00d7}" };
            format!("{base}{sep}{n}")
        }
        None => base,
    }
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
        assert_eq!(ansi::wrap("hi", ansi::Color::Dim, true), "\x1b[2mhi\x1b[0m");
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
            6,
        );
        assert_eq!(cell.text, "\u{2605}     60s\u{00d7}12 \u{2299}");
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
            6,
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
            6,
        );
        assert_eq!(cell.text, "*     60sx12 +");
    }

    #[test]
    fn indicator_blank_when_no_watch_capability() {
        // Poll-only provider: blank trailer regardless of reinstate flag.
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
            6,
        );
        assert!(cell.text.ends_with("12  "), "no indicator: {:?}", cell.text);
    }

    #[test]
    fn indicator_bare_dot_when_watches_files_without_reinstate() {
        // Watches but reinstate=false: bare dot `●` (decorated glyph progression).
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
            6,
        );
        assert!(
            cell.text.ends_with("12 \u{2219}"),
            "expected bare dot indicator, got {:?}",
            cell.text
        );
    }

    #[test]
    fn indicator_bare_dot_ascii() {
        let cell = format_ttl_cell(
            Some(&RowKind::Lifecycle {
                decay: 0,
                watches_files: true,
            }),
            Some(60),
            Some(12),
            Some(false),
            None,
            true,
            6,
        );
        assert!(
            cell.text.ends_with("12 -"),
            "expected '-' ascii dot, got {:?}",
            cell.text
        );
    }

    #[test]
    fn once_renders_dashes() {
        let cell = format_ttl_cell(Some(&RowKind::Once), None, None, None, None, false, 6);
        assert_eq!(cell.text, "---");
        assert!(cell.color.is_none());
    }

    #[test]
    fn virtual_renders_dashes() {
        let cell = format_ttl_cell(Some(&RowKind::Virtual), None, None, None, None, false, 6);
        assert_eq!(cell.text, "---");
    }

    #[test]
    fn transient_renders_dashes() {
        let cell = format_ttl_cell(Some(&RowKind::Transient), None, None, None, None, false, 6);
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
            6,
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
            6,
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
            6,
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
            6,
        );
        assert!(cell.text.contains("0s"));
        assert!(cell.text.contains("\u{00d7}00"));
    }

    #[test]
    fn p_width_tight_for_two_digit_p() {
        // With p_width=2 and a 2-digit P, zero padding: minimum visual gap
        // between the lead and "60s" is the single literal separator space.
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
            2,
        );
        assert_eq!(cell.text, "\u{2605} 60s\u{00d7}12 \u{2299}");
    }

    #[test]
    fn p_width_auto_sizes_to_widest_lifecycle_row() {
        use crate::cache::CacheRow;
        let row_small = CacheRow {
            provider: "load".into(),
            path: None,
            source: "loadavg".into(),
            field: "one".into(),
            value: serde_json::json!(0.5),
            age_ms: 0,
            stale: false,
            kind: Some(RowKind::Lifecycle {
                decay: 0,
                watches_files: false,
            }),
            poll_interval_secs: Some(30),
            keep_alive_polls: Some(4),
            fsevents_reinstate: Some(false),
            polls_elapsed: None,
            failure: None,
        };
        let mut row_large = row_small.clone();
        row_large.poll_interval_secs = Some(12_345);

        // 2-digit P → width 2 (tight, no floor).
        assert_eq!(compute_ttl_p_width(std::slice::from_ref(&row_small)), 2);
        // Widest P is 5 digits → width grows to 5.
        assert_eq!(
            compute_ttl_p_width(&[row_small.clone(), row_large.clone()]),
            5
        );
    }

    #[test]
    fn p_width_caps_at_six() {
        use crate::cache::CacheRow;
        let row = CacheRow {
            provider: "x".into(),
            path: None,
            source: "default".into(),
            field: "y".into(),
            value: serde_json::Value::Null,
            age_ms: 0,
            stale: false,
            kind: Some(RowKind::Lifecycle {
                decay: 0,
                watches_files: false,
            }),
            // Over 999_999 — clamped to 999_999 (6 digits) inside format_ttl_cell;
            // compute_ttl_p_width applies the same clamp before counting digits.
            poll_interval_secs: Some(9_999_999),
            keep_alive_polls: Some(1),
            fsevents_reinstate: Some(false),
            polls_elapsed: None,
            failure: None,
        };
        assert_eq!(compute_ttl_p_width(&[row]), 6);
    }

    #[test]
    fn p_width_one_when_no_lifecycle_rows() {
        // Empty / Once-only snapshot: value unused (all cells render as `---`),
        // but compute_ttl_p_width returns a non-zero default so format! works.
        assert_eq!(compute_ttl_p_width(&[]), 1);
    }
}

#[cfg(test)]
mod format_age_with_polls_tests {
    use super::*;

    #[test]
    fn no_polls_no_suffix() {
        assert_eq!(format_age_with_polls(14_000, None, false), "14s");
    }

    #[test]
    fn polls_unicode_suffix() {
        assert_eq!(
            format_age_with_polls(14_000, Some(3), false),
            "14s\u{00d7}3"
        );
    }

    #[test]
    fn polls_ascii_suffix() {
        assert_eq!(format_age_with_polls(14_000, Some(3), true), "14sx3");
    }

    #[test]
    fn polls_zero() {
        assert_eq!(
            format_age_with_polls(14_000, Some(0), false),
            "14s\u{00d7}0"
        );
    }

    #[test]
    fn polls_two_digit_n() {
        assert_eq!(
            format_age_with_polls(14_000, Some(12), false),
            "14s\u{00d7}12"
        );
    }

    #[test]
    fn minutes_age_with_polls() {
        assert_eq!(
            format_age_with_polls(185_000, Some(5), false),
            "3m5s\u{00d7}5"
        );
    }
}
