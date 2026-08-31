use crate::cache::CacheRow;

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

/// Maximum width for the raw P-seconds sub-field (`{P}s×{KK}`). 6 digits
/// covers ~11 days of seconds; beyond that we clamp rather than grow the cell.
pub const TTL_P_WIDTH_MAX: usize = 6;
const TTL_P_MAX: u64 = 999_999;

/// Maximum width for the humanized total-budget sub-field (`P × K`, e.g.
/// `99h59m`). Values are clamped before humanizing so the cell can't grow
/// past this.
pub const TTL_TOTAL_WIDTH_MAX: usize = 6;
/// Clamp applied to the total-budget seconds (`poll_interval_secs *
/// keep_alive_polls`) before humanizing, so the formatted string can never
/// exceed `TTL_TOTAL_WIDTH_MAX` chars (`99h59m`).
const TTL_TOTAL_MAX_SECS: u64 = 359_999;

/// Clamp applied to the POLL column's raw-seconds value (next-poll or
/// next-retry) before display. 6 digits covers ~11 days. The POLL column
/// otherwise auto-sizes like any other plain column (via `nat_poll` in
/// `render_table`) — this only bounds the displayed number, not a padded
/// sub-field width.
const TTL_SECONDS_MAX: u64 = 999_999;

/// Per-snapshot field widths for the TTL cell's internal `{P}s×{KK} (total)`
/// sub-fields, auto-sized to the widest value seen across all rendered rows
/// so digits line up between rows. The POLL column is a separate, plain
/// column and doesn't need an entry here — its width is measured the same
/// way AGE/PROVIDER/etc. are, from the rendered cell text.
#[derive(Debug, Clone, Copy)]
pub struct TtlWidths {
    /// Width of the raw poll-interval field (`P` in `{P}s×{KK}`).
    pub p: usize,
    /// Width of the humanized total-budget field (`P × K`, e.g. `12m`, `1h36m`).
    pub total: usize,
}

/// Compute `TtlWidths` for a snapshot: the widest raw `P` and the widest
/// humanized total-budget string across all Lifecycle rows with a poll path,
/// capped at `TTL_P_WIDTH_MAX` / `TTL_TOTAL_WIDTH_MAX`. No lower floor — the
/// width auto-shrinks so narrow snapshots don't over-pad.
///
/// Rows without a poll interval (Once/Virtual/Transient, or pure-Watch
/// Lifecycle rows) are ignored. Empty snapshots return 1 for both fields;
/// the value is unused in that case (every cell renders as `---`), but avoid
/// zero so `format!` can't panic.
pub fn compute_ttl_widths(rows: &[CacheRow]) -> TtlWidths {
    use crate::cache::RowKind;
    let mut p = 1usize;
    let mut total = 1usize;
    for row in rows {
        if !matches!(row.kind, Some(RowKind::Lifecycle { .. })) {
            continue;
        }
        if let (Some(pv), Some(k)) = (row.poll_interval_secs, row.keep_alive_polls) {
            p = p.max(pv.min(TTL_P_MAX).to_string().len());
            let total_secs = pv.saturating_mul(k as u64);
            total = total.max(format_duration_compact(total_secs).chars().count());
        }
    }
    TtlWidths {
        p: p.min(TTL_P_WIDTH_MAX),
        total: total.min(TTL_TOTAL_WIDTH_MAX),
    }
}

/// Humanize a seconds count for the TTL cell's total-budget field, clamping
/// to `TTL_TOTAL_MAX_SECS` first so the result never exceeds
/// `TTL_TOTAL_WIDTH_MAX` chars. Shares `format_age`'s unit logic (s/m/h).
fn format_duration_compact(secs: u64) -> String {
    format_age((secs.min(TTL_TOTAL_MAX_SECS) as u128) * 1000)
}

/// Seconds until a failing source's next retry attempt. When suppression is
/// active (`suppressed_until_unix_ms` is set), it's the time remaining until
/// that deadline. When the source is failing but not yet suppressed (below
/// the failure threshold), there is no backoff in effect — it will simply be
/// retried at its next regularly-scheduled poll, so this falls back to
/// `next_poll_in_secs` (0 if that's unavailable either).
fn failure_retry_secs(
    f: &crate::cache::FailureSnapshot,
    next_poll_in_secs: Option<u64>,
    now_unix_ms: u64,
) -> u64 {
    match f.suppressed_until_unix_ms {
        Some(until) => until.saturating_sub(now_unix_ms) / 1000,
        None => next_poll_in_secs.unwrap_or(0),
    }
}

/// Format the TTL cell: lifecycle countdown, `{P}s×{KK} (total)` budget, and
/// the watch indicator. The next-poll/next-retry countdown lives in the
/// separate POLL column (`format_poll_cell`), not here.
///
/// Failure state swaps the cell CONTENT to `#{attempt:02}` — the countdown
/// itself is POLL's job (POLL literally *is* the next attempt when a source
/// is failing), so TTL only needs to say "how many attempts have failed" here.
/// The lead glyph (⚠) and the trailing watch indicator are unaffected.
#[allow(dead_code)]
pub fn format_ttl_cell(
    kind: Option<&crate::cache::RowKind>,
    poll_interval_secs: Option<u64>,
    keep_alive_polls: Option<u32>,
    fsevents_reinstate: Option<bool>,
    failure: Option<&crate::cache::FailureSnapshot>,
    ascii: bool,
    widths: &TtlWidths,
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
            // Indicator keyed on fs-watching capability first; reinstate-armed is
            // the decorator. Poll-only sources render a blank cell trailer.
            let indicator = match (*watches_files, fsevents_reinstate.unwrap_or(false)) {
                (false, _) => " ",
                (true, false) => dot,
                (true, true) => dot_ring,
            };

            if let Some(f) = failure {
                let attempt = f.consecutive_failures.min(99);
                let text = format!("{warn} #{attempt:02} {indicator}");
                return TtlCell {
                    text,
                    color: Some(ansi::Color::Red),
                };
            }

            let (lead, color) = match decay {
                0 => (star.to_string(), ansi::Color::BrightGreen),
                1 => ("3".into(), ansi::Color::Dim),
                2 => ("2".into(), ansi::Color::Yellow),
                3 => ("1".into(), ansi::Color::BrightYellow),
                _ => ("0".into(), ansi::Color::Red),
            };

            // `{P}s×{KK} (total)` only renders for sources with a poll path
            // (Poll and WatchAndPoll). Pure Watch sources have no poll; their
            // cells pad with spaces of the same width so the indicator glyph
            // stays in a consistent column across all rows.
            //
            // Width: p_width + "s×KK" (4) + " (" (2) + total_width + ")" (1).
            let poll_seg = match (poll_interval_secs, keep_alive_polls) {
                (Some(p), Some(k)) => {
                    let total_secs = p.saturating_mul(k as u64);
                    let total_str = format_duration_compact(total_secs);
                    format!(
                        "{:>pw$}s{times}{:02} ({:>tw$})",
                        p.min(TTL_P_MAX),
                        k.min(99),
                        total_str,
                        pw = widths.p,
                        tw = widths.total
                    )
                }
                _ => " ".repeat(widths.p + 4 + 2 + widths.total + 1),
            };
            let text = format!("{} {} {}", lead, poll_seg, indicator);
            TtlCell {
                text,
                color: Some(color),
            }
        }
    }
}

/// Format the POLL column: seconds until this source's next scheduled poll,
/// or — for a failing source — seconds until its next retry attempt (POLL
/// literally *is* the next attempt in that case). `-` for rows with no poll
/// path: Once/Virtual/Transient/no-kind, and pure-Watch Lifecycle rows.
pub fn format_poll_cell(
    kind: Option<&crate::cache::RowKind>,
    next_poll_in_secs: Option<u64>,
    failure: Option<&crate::cache::FailureSnapshot>,
    now_unix_ms: u64,
) -> String {
    use crate::cache::RowKind;
    if !matches!(kind, Some(RowKind::Lifecycle { .. })) {
        return "-".to_string();
    }
    if let Some(f) = failure {
        let retry = failure_retry_secs(f, next_poll_in_secs, now_unix_ms).min(TTL_SECONDS_MAX);
        return format!("{retry}s");
    }
    match next_poll_in_secs {
        Some(n) => format!("{}s", n.min(TTL_SECONDS_MAX)),
        None => "-".to_string(),
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
        "table" => render_table(rows, false, None, true, false, false),
        "human" => {
            let trunc = if opts.no_trunc { None } else { opts.max_width };
            // `no_color` is the resolved color decision from the caller (already
            // accounts for NO_COLOR, TTY, WATCH_INTERVAL). Don't re-AND with is_tty
            // here — that would undo the WATCH_INTERVAL color promotion.
            let color = !opts.no_color;
            render_table(rows, color, trunc, true, opts.ascii, true)
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
    let rendered: Vec<String> = rows
        .iter()
        .map(|r| {
            let ctx = row_context(r);
            crate::cli::format::render_fmt_template_json(template, &ctx)
                .unwrap_or_else(|e| format!("<{e}>"))
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
    // 1. Render each row.
    let rendered: Vec<String> = rows
        .iter()
        .map(|r| {
            let ctx = row_context(r);
            crate::cli::format::render_fmt_template_json(body, &ctx)
                .unwrap_or_else(|e| format!("<{e}>"))
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

/// Build the template render context for a `CacheRow`.
///
/// A `serde_json::Value` object — the shape
/// [`crate::cli::format::render_fmt_template_json`] takes.
pub fn row_context(r: &CacheRow) -> serde_json::Value {
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
    if let Some(n) = r.next_poll_in_secs {
        ctx.insert("next_poll_in_secs".into(), serde_json::json!(n));
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
    serde_json::Value::Object(ctx)
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
const COLS: usize = 7;
// TTL is the last column (index 6); failure-state colouring skips only this
// one (its own warn-glyph + red already come from format_ttl_cell). POLL
// (index 5) is a plain column and gets red-wrapped like the others.
const TTL_COL_IDX: usize = 6;
const HEADERS: [&str; COLS] = ["PROVIDER", "PATH", "FIELD", "VALUE", "AGE", "POLL", "TTL"];

/// Render rows as aligned columns for the `human` preset.
///
/// - `color_enabled`: apply per-cell ANSI colour (AGE yellow/green by stale, TTL by decay,
///   failure rows red on non-TTL cells).
/// - `trunc`: maximum width for the VALUE column (in characters). `None` = no truncation.
/// - `header`: prepend a PROVIDER / PATH / FIELD / VALUE / AGE / POLL / TTL header row.
/// - `ascii`: use ASCII-only glyphs in the TTL cell instead of Unicode.
pub fn render_human(rows: &[CacheRow], opts: &FormatOptions) -> String {
    render_table(rows, false, None, true, opts.ascii, true)
}

/// Compact a path under `$HOME` to a `~`-prefixed display form. Exact-prefix
/// match only — no symlink resolution, no partial-segment matches (e.g. a
/// home of `/Users/joe` does not compact `/Users/joel/x`). Paths outside
/// `$HOME`, and the pathless-global marker `-`, pass through unchanged.
fn compact_home_path(path: &str, home: Option<&str>) -> String {
    match home {
        Some(h) if !h.is_empty() && path == h => "~".to_string(),
        Some(h) if !h.is_empty() && path.starts_with(&format!("{h}/")) => {
            format!("~{}", &path[h.len()..])
        }
        _ => path.to_string(),
    }
}

fn render_table(
    rows: &[CacheRow],
    color_enabled: bool,
    trunc: Option<usize>,
    header: bool,
    ascii: bool,
    compact_home: bool,
) -> String {
    // `trunc` is the **total table width cap** (all columns + padding).
    // We scan all rows first to measure natural column widths, then compute
    // how much room the VALUE column has once other columns claim their natural
    // widths and inter-column separators are accounted for.

    // Display-only: `~`-compact paths under $HOME so the freed width benefits
    // the VALUE column. Machine presets (json/csv/tsv/sh) never call through
    // this path — they use `row.path` directly. Resolved once per render.
    let home = if compact_home {
        std::env::var("HOME").ok()
    } else {
        None
    };

    let now_unix_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    // -----------------------------------------------------------------------
    // Pass 1: measure natural widths of every column (no truncation).
    // Columns: PROVIDER, PATH, FIELD, VALUE, AGE, POLL, TTL
    // -----------------------------------------------------------------------
    let mut nat_provider = HEADERS[0].len();
    let mut nat_path = HEADERS[1].len();
    let mut nat_field = HEADERS[2].len();
    let mut nat_age = HEADERS[4].len();
    let mut nat_poll = HEADERS[5].len();
    let mut nat_ttl = HEADERS[6].len();

    // Auto-size the TTL cell's `{P}s×{KK} (total)` sub-fields to the widest
    // values in this snapshot, capped at TTL_P_WIDTH_MAX / TTL_TOTAL_WIDTH_MAX.
    let ttl_widths = compute_ttl_widths(rows);

    for row in rows {
        let path = compact_home_path(row.path.as_deref().unwrap_or("-"), home.as_deref());
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
                &ttl_widths,
            )
            .text
            .chars()
            .count(),
        );
        nat_poll = nat_poll.max(
            format_poll_cell(
                row.kind.as_ref(),
                row.next_poll_in_secs,
                row.failure.as_ref(),
                now_unix_ms,
            )
            .chars()
            .count(),
        );
    }

    // -----------------------------------------------------------------------
    // Compute effective VALUE cap from the total-table-width budget.
    // Layout: MARGIN provider(2sp)path(2sp)field(2sp)value(2sp)age(2sp)poll(2sp)ttl MARGIN
    // Separators: 6 × 2 = 12
    // Margins: 2 spaces each side (breathing room so the table doesn't hug
    // the terminal edge).
    // Non-VALUE: nat_provider + nat_path + nat_field + nat_age + nat_poll + nat_ttl + 12 + 4
    // -----------------------------------------------------------------------
    const MIN_VALUE_WIDTH: usize = 8;
    const SEP_TOTAL: usize = 12; // 6 separators × 2 spaces
    const MARGIN: usize = 2; // spaces applied to both the left and right edges
    const MARGIN_TOTAL: usize = MARGIN * 2;

    let value_cap: Option<usize> = trunc.map(|total_cap| {
        let non_value = nat_provider
            + nat_path
            + nat_field
            + nat_age
            + nat_poll
            + nat_ttl
            + SEP_TOTAL
            + MARGIN_TOTAL;
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
        let path = compact_home_path(row.path.as_deref().unwrap_or("-"), home.as_deref());
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

        // TTL cell — built from lifecycle metadata. TTL is the last column,
        // so the general per-row padding loop below (which only pads
        // non-last columns) never touches it; it has its own internal
        // fixed-width discipline via `ttl_widths` instead.
        let ttl = format_ttl_cell(
            row.kind.as_ref(),
            row.poll_interval_secs,
            row.keep_alive_polls,
            row.fsevents_reinstate,
            row.failure.as_ref(),
            ascii,
            &ttl_widths,
        );
        let ttl_vis = ttl.text.chars().count();
        let ttl_cell = match (color_enabled, ttl.color) {
            (true, Some(c)) => ansi::wrap(&ttl.text, c, true),
            _ => ttl.text,
        };

        // POLL cell — seconds until next poll / next retry attempt. Unlike
        // PROVIDER/PATH/FIELD/etc., POLL is right-aligned (it's a number),
        // so it's pre-padded here against the natural width from Pass 1
        // rather than relying on the general (left-justifying) padding loop.
        let poll_text = format!(
            "{:>width$}",
            format_poll_cell(
                row.kind.as_ref(),
                row.next_poll_in_secs,
                row.failure.as_ref(),
                now_unix_ms,
            ),
            width = nat_poll
        );
        let poll_vis = poll_text.chars().count();

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
            poll_text,
            ttl_cell,
        ];

        // Failure-state colouring: red on all cells except TTL (which already
        // has its own ⚠ + red from format_ttl_cell). POLL is a plain cell —
        // it gets red-wrapped like PROVIDER/PATH/FIELD/VALUE/AGE.
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
            poll_vis,
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
mod compact_home_path_tests {
    use super::compact_home_path;

    #[test]
    fn compacts_exact_prefix_match() {
        assert_eq!(
            compact_home_path("/Users/x/ws/beachcomber", Some("/Users/x")),
            "~/ws/beachcomber"
        );
    }

    #[test]
    fn compacts_path_equal_to_home() {
        assert_eq!(compact_home_path("/Users/x", Some("/Users/x")), "~");
    }

    #[test]
    fn leaves_non_home_path_untouched() {
        assert_eq!(
            compact_home_path("/var/log/beachcomber", Some("/Users/x")),
            "/var/log/beachcomber"
        );
    }

    #[test]
    fn does_not_compact_on_partial_segment_match() {
        // "/Users/joe" is a string-prefix of "/Users/joel/x" but not a path
        // component prefix — must not compact.
        assert_eq!(
            compact_home_path("/Users/joel/x", Some("/Users/joe")),
            "/Users/joel/x"
        );
    }

    #[test]
    fn leaves_dash_marker_untouched() {
        assert_eq!(compact_home_path("-", Some("/Users/x")), "-");
    }

    #[test]
    fn passes_through_when_home_unknown() {
        assert_eq!(
            compact_home_path("/Users/x/ws/beachcomber", None),
            "/Users/x/ws/beachcomber"
        );
    }

    #[test]
    fn passes_through_when_home_empty() {
        assert_eq!(
            compact_home_path("/Users/x/ws/beachcomber", Some("")),
            "/Users/x/ws/beachcomber"
        );
    }
}

#[cfg(test)]
mod ttl_cell_tests {
    use super::*;
    use crate::cache::{CacheRow, FailureSnapshot, RowKind};

    /// Widths used by most pinned tests below: wide enough (6/6) that the
    /// digits in these examples never get clamped, matching the module's
    /// `TTL_P_WIDTH_MAX` / `TTL_TOTAL_WIDTH_MAX` caps.
    const W6: TtlWidths = TtlWidths { p: 6, total: 6 };

    #[test]
    fn active_lifecycle_unicode() {
        // P=60, K=12 → total budget 720s = 12m, shown in parens.
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
            &W6,
        );
        assert_eq!(cell.text, "\u{2605}     60s\u{00d7}12 (   12m) \u{2299}");
        assert!(matches!(cell.color, Some(ansi::Color::BrightGreen)));
    }

    #[test]
    fn decay4_lifecycle_unicode() {
        // P=480 (already-decayed effective interval), K=12 → total 5760s = 1h36m.
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
            &W6,
        );
        assert_eq!(cell.text, "0    480s\u{00d7}12 ( 1h36m)  ");
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
            &W6,
        );
        assert_eq!(cell.text, "*     60sx12 (   12m) +");
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
            &W6,
        );
        assert!(cell.text.ends_with(")  "), "no indicator: {:?}", cell.text);
    }

    #[test]
    fn indicator_bare_dot_when_watches_files_without_reinstate() {
        // Watches but reinstate=false: bare dot (decorated glyph progression).
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
            &W6,
        );
        assert!(
            cell.text.ends_with(") \u{2219}"),
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
            &W6,
        );
        assert!(
            cell.text.ends_with(") -"),
            "expected '-' ascii dot, got {:?}",
            cell.text
        );
    }

    #[test]
    fn once_renders_dashes() {
        let cell = format_ttl_cell(Some(&RowKind::Once), None, None, None, None, false, &W6);
        assert_eq!(cell.text, "---");
        assert!(cell.color.is_none());
    }

    #[test]
    fn virtual_renders_dashes() {
        let cell = format_ttl_cell(Some(&RowKind::Virtual), None, None, None, None, false, &W6);
        assert_eq!(cell.text, "---");
    }

    #[test]
    fn transient_renders_dashes() {
        let cell = format_ttl_cell(
            Some(&RowKind::Transient),
            None,
            None,
            None,
            None,
            false,
            &W6,
        );
        assert_eq!(cell.text, "---");
    }

    #[test]
    fn failure_shows_attempt_count_not_timing() {
        // TTL's failure content is just "how many attempts have failed" —
        // the retry countdown lives in the POLL column now (POLL literally
        // *is* the next attempt when a source is failing).
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
            &W6,
        );
        assert_eq!(cell.text, "\u{26a0} #03 \u{2299}");
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
            &W6,
        );
        assert!(cell.text.starts_with("!"));
    }

    #[test]
    fn total_clamps_at_max() {
        // P*K clamps to TTL_TOTAL_MAX_SECS (99h59m) before humanizing; the
        // raw P itself clamps to TTL_P_MAX (999999).
        let cell = format_ttl_cell(
            Some(&RowKind::Lifecycle {
                decay: 0,
                watches_files: false,
            }),
            Some(9_999_999),
            Some(99),
            Some(false),
            None,
            false,
            &W6,
        );
        assert!(cell.text.contains("99h59m"), "got: {:?}", cell.text);
        assert!(cell.text.contains("999999s"), "got: {:?}", cell.text);
    }

    #[test]
    fn zero_does_not_panic() {
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
            &W6,
        );
        assert!(cell.text.contains("0s"), "got: {:?}", cell.text);
    }

    #[test]
    fn widths_tight_no_floor_padding() {
        // P=1 in a width-2 field pads one space; total=1*0=0 → "0s" exactly
        // fills a width-2 field. No extra floor padding beyond that.
        let widths = TtlWidths { p: 2, total: 2 };
        let cell = format_ttl_cell(
            Some(&RowKind::Lifecycle {
                decay: 0,
                watches_files: true,
            }),
            Some(1),
            Some(0),
            Some(true),
            None,
            false,
            &widths,
        );
        assert_eq!(cell.text, "\u{2605}  1s\u{00d7}00 (0s) \u{2299}");
    }

    fn lifecycle_row(poll: u64, keep: u32, next: u64) -> CacheRow {
        CacheRow {
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
            poll_interval_secs: Some(poll),
            keep_alive_polls: Some(keep),
            fsevents_reinstate: Some(false),
            polls_elapsed: None,
            next_poll_in_secs: Some(next),
            failure: None,
        }
    }

    #[test]
    fn widths_auto_size_to_widest_row() {
        // P=30 (2 digits); total=30*4=120s="2m" (2 chars).
        let row_small = lifecycle_row(30, 4, 5);
        // P=12345 (5 digits); total=12345*4=49380s="13h43m" (6 chars).
        let row_large = lifecycle_row(12_345, 4, 12_345);

        let w = compute_ttl_widths(std::slice::from_ref(&row_small));
        assert_eq!((w.p, w.total), (2, 2));

        let w = compute_ttl_widths(&[row_small, row_large]);
        assert_eq!((w.p, w.total), (5, 6));
    }

    #[test]
    fn widths_cap_at_max() {
        let row = lifecycle_row(9_999_999, 99, 9_999_999);
        let w = compute_ttl_widths(&[row]);
        assert_eq!((w.p, w.total), (TTL_P_WIDTH_MAX, TTL_TOTAL_WIDTH_MAX));
    }

    #[test]
    fn widths_default_when_no_lifecycle_rows() {
        // Empty / Once-only snapshot: values unused (every cell renders as
        // `---`), but a non-zero default keeps format! from panicking.
        let w = compute_ttl_widths(&[]);
        assert_eq!((w.p, w.total), (1, 1));
    }
}

#[cfg(test)]
mod poll_cell_tests {
    use super::*;
    use crate::cache::{FailureSnapshot, RowKind};

    #[test]
    fn active_row_shows_next_poll_seconds() {
        assert_eq!(
            format_poll_cell(
                Some(&RowKind::Lifecycle {
                    decay: 0,
                    watches_files: true,
                }),
                Some(37),
                None,
                0,
            ),
            "37s"
        );
    }

    #[test]
    fn watch_only_row_shows_dash() {
        // Lifecycle but no poll path (pure Watch): next_poll_in_secs is None.
        assert_eq!(
            format_poll_cell(
                Some(&RowKind::Lifecycle {
                    decay: 0,
                    watches_files: true,
                }),
                None,
                None,
                0,
            ),
            "-"
        );
    }

    #[test]
    fn once_virtual_transient_and_no_kind_show_dash() {
        for kind in [
            None,
            Some(RowKind::Once),
            Some(RowKind::Virtual),
            Some(RowKind::Transient),
        ] {
            assert_eq!(
                format_poll_cell(kind.as_ref(), Some(37), None, 0),
                "-",
                "kind={kind:?}"
            );
        }
    }

    #[test]
    fn failure_not_suppressed_falls_back_to_next_poll() {
        // Failing but below the suppression threshold: no backoff in effect,
        // so POLL mirrors the source's regular next-poll time.
        let snap = FailureSnapshot {
            consecutive_failures: 3,
            suppressed_until_unix_ms: None,
        };
        assert_eq!(
            format_poll_cell(
                Some(&RowKind::Lifecycle {
                    decay: 1,
                    watches_files: true,
                }),
                Some(45),
                Some(&snap),
                0,
            ),
            "45s"
        );
    }

    #[test]
    fn failure_suppressed_computes_retry_from_now() {
        let snap = FailureSnapshot {
            consecutive_failures: 5,
            suppressed_until_unix_ms: Some(10_000),
        };
        // (10_000 - 3_000) / 1000 = 7s remaining until suppression lifts.
        assert_eq!(
            format_poll_cell(
                Some(&RowKind::Lifecycle {
                    decay: 1,
                    watches_files: false,
                }),
                Some(999),
                Some(&snap),
                3_000,
            ),
            "7s"
        );
    }

    #[test]
    fn clamps_at_max() {
        assert_eq!(
            format_poll_cell(
                Some(&RowKind::Lifecycle {
                    decay: 0,
                    watches_files: false,
                }),
                Some(9_999_999),
                None,
                0,
            ),
            "999999s"
        );
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
