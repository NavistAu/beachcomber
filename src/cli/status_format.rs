use crate::cache::CacheRow;
use crate::cli::output_format::{Format, Preset};
use crate::cli::style::Glyphs;
use libbeachcomber::render;
use libbeachcomber::render::shell_quote;
use libbeachcomber::term;
use std::time::Duration;

// ---------------------------------------------------------------------------
// ANSI colour helper (basic-16 palette, fg-only), on libbeachcomber::term
// ---------------------------------------------------------------------------

#[allow(dead_code)]
mod ansi {
    use libbeachcomber::term;

    #[derive(Clone, Copy, Debug)]
    pub enum Color {
        BrightGreen,
        Yellow,
        BrightYellow,
        Red,
        Cyan,
        Magenta,
        Dim,
        Bold,
    }

    impl Color {
        /// The SGR parameters: a colour name through [`term::Colour`], or an
        /// attribute.
        pub fn params(self) -> String {
            let named = |name: &str| {
                term::Colour::parse(name)
                    .expect("a colour name")
                    .params(term::Ground::Fore, term::Depth::Basic)
            };
            match self {
                Color::BrightGreen => named("bright_green"),
                Color::Yellow => named("yellow"),
                Color::BrightYellow => named("bright_yellow"),
                Color::Red => named("red"),
                Color::Cyan => named("cyan"),
                Color::Magenta => named("magenta"),
                Color::Dim => "2".to_string(),
                Color::Bold => "1".to_string(),
            }
        }
    }

    pub fn wrap(s: &str, c: Color, enabled: bool) -> String {
        if !enabled {
            return s.to_string();
        }
        term::sgr(&c.params(), s)
    }

    pub const RESET: &str = "\x1b[0m";
}

/// Subtle background for alternating data rows: one step off the terminal's
/// own background on the 256-colour grayscale ramp. The terminal's polarity
/// comes from `COLORFGBG` ("fg;bg", sometimes "fg;default;bg" — some
/// terminals export it, none guarantee it): a background index of 0–6 or 8
/// is dark. Unset or unparsable defaults to dark, the common case. Killed
/// entirely by --color=never / NO_COLOR / non-TTY.
fn stripe_bg() -> &'static str {
    const DARK: &str = "\x1b[48;5;234m"; // faint lift off black
    const LIGHT: &str = "\x1b[48;5;254m"; // faint dip off white
    match std::env::var("COLORFGBG") {
        Ok(v) => match v.rsplit(';').next().and_then(|bg| bg.parse::<u8>().ok()) {
            Some(bg) if bg <= 6 || bg == 8 => DARK,
            Some(_) => LIGHT,
            None => DARK,
        },
        Err(_) => DARK,
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
/// poll_count`) before humanizing, so the formatted string can never
/// exceed `TTL_TOTAL_WIDTH_MAX` chars (`99h59m`).
const TTL_TOTAL_MAX_SECS: u64 = 359_999;

/// Clamp applied to the POLL column's seconds (next-poll or next-retry)
/// before display, ~11 days (`277h46m`). The POLL column otherwise auto-sizes
/// like any other plain column (via `nat_poll` in `render_table`) — this only
/// bounds the displayed duration, not a padded sub-field width.
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
/// Rows without a poll interval (Once/Static/Transient, or pure-Watch
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
        if let (Some(pv), Some(k)) = (row.poll_interval_secs, row.poll_count) {
            p = p.max(pv.min(TTL_P_MAX).to_string().len());
            let total_secs = pv.saturating_mul(k as u64);
            total = total.max(format_duration_compact(total_secs).cols());
        }
    }
    TtlWidths {
        p: p.min(TTL_P_WIDTH_MAX),
        total: total.min(TTL_TOTAL_WIDTH_MAX),
    }
}

/// Humanize a seconds count for the TTL cell's total-budget field, clamping
/// to `TTL_TOTAL_MAX_SECS` first so the result never exceeds
/// `TTL_TOTAL_WIDTH_MAX` chars.
fn format_duration_compact(secs: u64) -> String {
    render::duration(Duration::from_secs(secs.min(TTL_TOTAL_MAX_SECS)))
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
    poll_count: Option<u32>,
    fsevents_reinstate: Option<bool>,
    failure: Option<&crate::cache::FailureSnapshot>,
    ascii: bool,
    widths: &TtlWidths,
) -> TtlCell {
    use crate::cache::RowKind;

    // Watches-files indicator progression: `dot` for watches only, `ring`
    // (a bullet with a ring around it) for watches + reinstate. Both glyphs
    // are math operator class (U+22xx / U+2299), so fonts render them at a
    // shared baseline. Earlier pairs mixed General Punctuation and Math
    // Symbols-B blocks and rendered vertically mis-aligned.
    let g = Glyphs::of(ascii);
    let (star, warn, times, dot, dot_ring) = (g.star, g.warn, g.times, g.dot, g.ring);

    match kind {
        None
        | Some(RowKind::Once)
        | Some(RowKind::Static)
        | Some(RowKind::Transient)
        | Some(RowKind::Rendered) => TtlCell {
            text: "---".into(),
            color: None,
        },
        Some(RowKind::Lifecycle {
            decay,
            depth,
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

            // The digit counts decay steps left before eviction, from the
            // entry's own depth (`put --ttl` entries have depth 1).
            let left = depth.saturating_sub(*decay);
            let (lead, color) = match (decay, left) {
                (0, _) => (star.to_string(), ansi::Color::BrightGreen),
                (_, 0) => ("0".into(), ansi::Color::Red),
                (_, 1) => ("1".into(), ansi::Color::BrightYellow),
                (_, 2) => ("2".into(), ansi::Color::Yellow),
                (_, n) => (n.to_string(), ansi::Color::Dim),
            };

            // `{P}s×{KK} (total)` only renders for sources with a poll path
            // (Poll and WatchAndPoll). Pure Watch sources have no poll; their
            // cells pad with spaces of the same width so the indicator glyph
            // stays in a consistent column across all rows.
            //
            // Width: p_width + "s×KK" (4) + " (" (2) + total_width + ")" (1).
            let poll_seg = match (poll_interval_secs, poll_count) {
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

/// Format the POLL column: the time until this source's next scheduled poll,
/// or — for a failing source — until its next retry attempt (POLL
/// literally *is* the next attempt in that case). `-` for rows with no poll
/// path: Once/Static/Transient/no-kind, and pure-Watch Lifecycle rows.
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
        return render::duration(Duration::from_secs(retry));
    }
    match next_poll_in_secs {
        Some(n) => render::duration(Duration::from_secs(n.min(TTL_SECONDS_MAX))),
        None => "-".to_string(),
    }
}

/// Resolve the maximum **total table** width.
///
/// - `None` arg → use `terminal_cols` if available, else the default (120).
/// - `Some("auto")` → use `terminal_cols` if available, else the default.
/// - `Some(n)` → parse `n` as a `usize`, falling back to the default on error.
///
/// The returned value is the cap for the **entire rendered row** (all columns +
/// padding). The renderer shrinks the VALUE and PATH columns as needed to stay
/// within this budget — VALUE first in line, PATH taking what is left — and
/// keeps the other five columns at their natural widths.
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
    /// Object-destructuring depth requested on the command line. `None` means
    /// "use the preset's default" — see [`default_depth`].
    pub depth: Option<usize>,
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
            depth: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Object destructuring (`--depth`)
// ---------------------------------------------------------------------------

/// Destructuring depth to use when `--depth` was not given.
///
/// The human-facing presets go one level down: a nested object crammed into
/// one cell is not readable, but unlimited recursion is worse — a provider
/// holding a deep API blob expanded to thirteen thousand rows and pushed the
/// FIELD column to 75 characters of mostly punctuation. Two levels is the
/// depth at which structure becomes visible without the table becoming a
/// dump; `--depth 0` still asks for everything.
///
/// The machine presets stay at 1 so existing consumers keep getting
/// byte-identical output unless they ask for more. Custom minijinja templates
/// count as machine output for the same reason.
pub fn default_depth(format: &Format) -> usize {
    match format {
        Format::Preset(Preset::Human | Preset::Table) => 2,
        _ => 1,
    }
}

/// Expand object-valued rows into one child row per key, recursively.
///
/// A row `(git, /repo, session, {"agent":"claude","color":"blue"})` becomes
/// rows `session.agent` and `session.color`, each inheriting the parent's
/// path, age, staleness and lifecycle metadata — they describe the same cache
/// entry, just a deeper slice of it.
///
/// `depth` counts field-name segments: 1 is the un-destructured original, 2
/// goes one level down, and 0 means unlimited. Arrays are never expanded —
/// their indices are not field names and a list reads better whole — and an
/// empty object stays as its own row rather than vanishing.
pub fn flatten_rows(rows: Vec<CacheRow>, depth: usize) -> Vec<CacheRow> {
    if depth == 1 {
        return rows;
    }
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        push_flattened(&mut out, row, depth, 1);
    }
    out
}

fn push_flattened(out: &mut Vec<CacheRow>, row: CacheRow, depth: usize, level: usize) {
    let expandable = matches!(&row.value, serde_json::Value::Object(m) if !m.is_empty());
    if !expandable || (depth != 0 && level >= depth) {
        out.push(row);
        return;
    }
    let serde_json::Value::Object(map) = &row.value else {
        unreachable!("guarded by `expandable` above")
    };
    for (key, child_value) in map {
        let child = CacheRow {
            field: libbeachcomber::key::push(&row.field, key),
            value: child_value.clone(),
            ..row.clone()
        };
        push_flattened(out, child, depth, level + 1);
    }
}

/// Dispatch to the appropriate renderer for the given preset name.
///
/// Supported presets: `human`, `json`, `tsv`, `csv`, `table`, `sh`.
/// Any other value is treated as a minijinja template rendered per row.
/// If the value starts with `table ` (with a space), the remainder is rendered
/// as a tab-separated template with aligned columns and a derived header row.
pub fn render_preset(format: &Format, rows: &[CacheRow], opts: &RenderOpts) -> String {
    match format {
        Format::Preset(Preset::Json) => render_json(rows),
        Format::Preset(p @ (Preset::Csv | Preset::CsvHeader | Preset::Tsv | Preset::TsvHeader)) => {
            render_delimited(rows, p.value_format().expect("a value format"))
        }
        Format::Preset(Preset::Sh) => render_sh(rows),
        Format::Preset(Preset::Table) => render_table(rows, &[], false, None, true, false),
        Format::Preset(Preset::Human) => {
            let trunc = if opts.no_trunc { None } else { opts.max_width };
            // `no_color` is the resolved color decision from the caller (already
            // accounts for NO_COLOR, TTY and --color). Don't re-AND with is_tty
            // here — that would override an explicit `--color=always`.
            let color = !opts.no_color;
            render_table(rows, &[], color, trunc, true, opts.ascii)
        }
        Format::Preset(other) => unreachable!("status's formats hold no {}", other.name()),
        Format::Template(_) => {
            unreachable!("a status template renders through render_template_format")
        }
    }
}

// ---------------------------------------------------------------------------
// Custom minijinja template renderers
// ---------------------------------------------------------------------------

/// How a `-f` template renders against one row: the template, and the row's
/// fields as locals ([`row_context`]). `comb status` renders it the way
/// `comb eval` renders any template ([`libbeachcomber::resolve::eval_with`]).
pub type RowRender<'a> = dyn FnMut(
        &str,
        &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Option<serde_json::Value>, libbeachcomber::resolve::ResolveError>
    + 'a;

/// `status -f TEMPLATE`: the template rendered once per row, one row per
/// line; `table TEMPLATE` splits each on tabs and aligns the columns under a
/// header of the names the template reads. A template that does not compile
/// fails the command once (`Err`); a row that fails while rendering shows its
/// error in its place.
pub fn render_template_format(
    template: &str,
    rows: &[CacheRow],
    render: &mut RowRender<'_>,
) -> Result<String, String> {
    use libbeachcomber::render::{Format as ValueFormat, render_value};
    use libbeachcomber::resolve::ResolveError;
    let (body, table) = match template.strip_prefix("table ") {
        Some(body) => (body, true),
        None => (template, false),
    };
    let mut rendered = Vec::with_capacity(rows.len());
    for row in rows {
        let serde_json::Value::Object(ctx) = row_context(row) else {
            unreachable!("a row's context is an object")
        };
        match render(body, &ctx) {
            Ok(value) => rendered.push(
                render_value("", value.as_ref(), ValueFormat::Text)
                    .trim_end_matches('\n')
                    .to_string(),
            ),
            Err(ResolveError::Config(message)) => return Err(message),
            Err(e) => rendered.push(format!("<{e}>")),
        }
    }
    if table {
        return Ok(align_template_table(body, &rendered));
    }
    let mut out = rendered.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    Ok(out)
}

/// `table` layout: each rendered row split on tabs, columns aligned across
/// every row and the header derived from the names the template reads.
fn align_template_table(body: &str, rendered: &[String]) -> String {
    let matrix: Vec<Vec<&str>> = rendered.iter().map(|s| s.split('\t').collect()).collect();
    let header: Vec<String> = extract_template_header(body);

    let n_cols = header
        .len()
        .max(matrix.iter().map(|r| r.len()).max().unwrap_or(0));
    let mut widths = vec![0usize; n_cols];
    for (i, h) in header.iter().enumerate() {
        widths[i] = widths[i].max(h.cols());
    }
    for row in &matrix {
        for (i, cell) in row.iter().enumerate() {
            if i < n_cols {
                widths[i] = widths[i].max(cell.cols());
            }
        }
    }

    let mut out = String::new();
    for (i, h) in header.iter().enumerate() {
        if i > 0 {
            out.push_str("  ");
        }
        if i + 1 < header.len() {
            out.push_str(&pad_cols(h, widths[i]));
        } else {
            out.push_str(h);
        }
    }
    out.push('\n');
    for row in &matrix {
        for (i, cell) in row.iter().enumerate() {
            if i > 0 {
                out.push_str("  ");
            }
            if i + 1 < row.len() {
                let w = if i < n_cols { widths[i] } else { 0 };
                out.push_str(&pad_cols(cell, w));
            } else {
                out.push_str(cell);
            }
        }
        out.push('\n');
    }
    out
}

/// `s` padded on the right with spaces to `width` columns.
pub fn pad_cols(s: &str, width: usize) -> String {
    format!("{s}{}", " ".repeat(width.saturating_sub(s.cols())))
}

/// A row's fields, as the locals of a `status -f` template.
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
    ctx.insert("source".into(), serde_json::Value::String(r.source.clone()));
    ctx.insert("value".into(), r.value.clone());
    ctx.insert("age_ms".into(), serde_json::json!(r.age_ms));
    ctx.insert(
        "age_human".into(),
        serde_json::Value::String(r.age_ms.map_or_else(|| "-".to_string(), format_age)),
    );
    ctx.insert("stale".into(), serde_json::Value::Bool(r.stale));
    // kind — empty string when None
    let kind_str = r.kind.as_ref().map_or("", RowKind::name);
    ctx.insert("kind".into(), serde_json::Value::String(kind_str.into()));
    // decay — only for lifecycle
    if let Some(RowKind::Lifecycle { decay, .. }) = &r.kind {
        ctx.insert("decay".into(), serde_json::json!(*decay));
    }
    // optional lifecycle fields
    if let Some(p) = r.poll_interval_secs {
        ctx.insert("poll_interval_secs".into(), serde_json::json!(p));
    }
    if let Some(k) = r.poll_count {
        ctx.insert("poll_count".into(), serde_json::json!(k));
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
// CSV and TSV
// ---------------------------------------------------------------------------

/// The columns `status -f csv|tsv` lays a row out in, as the uppercase
/// formats name them in their header.
const DELIMITED_COLUMNS: [&str; 14] = [
    "PROVIDER",
    "PATH",
    "FIELD",
    "SOURCE",
    "VALUE",
    "AGE_MS",
    "STALE",
    "KIND",
    "DECAY",
    "POLL_INTERVAL_SECS",
    "POLL_COUNT",
    "FSEVENTS_REINSTATE",
    "FAILURE_CONSECUTIVE_FAILURES",
    "FAILURE_SUPPRESSED_UNTIL_UNIX_MS",
];

/// One row's cells, in [`DELIMITED_COLUMNS`] order, before encoding: a
/// VALUE is its raw text, as `comb get -f csv` prints one
/// ([`libbeachcomber::render::csv_cell`]), and an absent field an empty cell.
fn delimited_cells(row: &CacheRow) -> Vec<String> {
    use crate::cache::RowKind;
    use libbeachcomber::render::csv_cell;
    let opt = |v: Option<String>| v.unwrap_or_default();
    let kind = row.kind.as_ref().map_or("", RowKind::name);
    let decay = match &row.kind {
        Some(RowKind::Lifecycle { decay, .. }) => decay.to_string(),
        _ => String::new(),
    };
    vec![
        row.provider.clone(),
        row.path.clone().unwrap_or_default(),
        row.field.clone(),
        row.source.clone(),
        csv_cell(Some(&row.value)),
        opt(row.age_ms.map(|a| a.to_string())),
        row.stale.to_string(),
        kind.to_string(),
        decay,
        opt(row.poll_interval_secs.map(|n| n.to_string())),
        opt(row.poll_count.map(|n| n.to_string())),
        opt(row.fsevents_reinstate.map(|b| b.to_string())),
        opt(row
            .failure
            .as_ref()
            .map(|f| f.consecutive_failures.to_string())),
        opt(row
            .failure
            .as_ref()
            .and_then(|f| f.suppressed_until_unix_ms.map(|n| n.to_string()))),
    ]
}

/// `status -f csv|CSV|tsv|TSV`: one line per row through the shared cell
/// encoders, and the uppercase formats a header row first.
pub fn render_delimited(rows: &[CacheRow], format: libbeachcomber::render::Format) -> String {
    let delimited = format
        .delimited()
        .unwrap_or_else(|| unreachable!("{} is not a delimited format", format.name()));
    let mut out = String::new();
    if delimited.header {
        let names: Vec<String> = DELIMITED_COLUMNS.iter().map(|c| c.to_string()).collect();
        out.push_str(&delimited.line(&names));
    }
    for row in rows {
        out.push_str(&delimited.line(&delimited_cells(row)));
    }
    out
}

// ---------------------------------------------------------------------------
// Shell-sourceable renderer
// ---------------------------------------------------------------------------

pub fn render_sh_env(rows: &[CacheRow]) -> String {
    use crate::cache::RowKind;
    let mut out = String::new();
    for row in rows {
        let key = sanitize_sh_key(&row.provider, row.path.as_deref(), &row.field);
        let value = libbeachcomber::render::csv_cell(Some(&row.value));
        let quoted = shell_quote(&value);
        out.push_str(&format!("{key}={quoted}\n"));
        // source — which Source produced the value, `rendered` when the CLI
        // rendered it here rather than the daemon caching it.
        out.push_str(&format!("{key}_SOURCE={}\n", shell_quote(&row.source)));
        // kind
        if let Some(kind_val) = row.kind.as_ref().map(RowKind::name) {
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
        // poll_count
        if let Some(k) = row.poll_count {
            out.push_str(&format!(
                "{key}_POLL_COUNT={}\n",
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

/// A row's shell variable name, `provider_path_field`: the provider, each
/// component of the path and the field's segments, joined by `_` and made a
/// name by the shared encoder ([`libbeachcomber::render::sh_ident`]), as
/// `comb get -f sh` names its lines.
fn sanitize_sh_key(provider: &str, path: Option<&str>, field: &str) -> String {
    let mut parts: Vec<String> = vec![provider.to_string()];
    parts.extend(
        path.unwrap_or_default()
            .split('/')
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    );
    parts.extend(
        libbeachcomber::key::segments(field)
            .into_iter()
            .map(|s| s.into_owned()),
    );
    libbeachcomber::render::sh_ident(&parts.join("_"))
}

// ---------------------------------------------------------------------------
// Table / human renderer (shared implementation)
// ---------------------------------------------------------------------------

// Column indices for the human/table preset.
const COLS: usize = 8;
// TTL is the last column (index 7); failure-state colouring skips only this
// one (its own warn-glyph + red already come from format_ttl_cell). POLL
// (index 6) is a plain column and gets red-wrapped like the others.
const TTL_COL_IDX: usize = 7;
// PROVIDER and FIELD sit adjacent because together they are the cache key the
// user typed (`git.branch`); SOURCE follows, naming what produced the VALUE
// beside it (a Source, `put`, `config(<file>)`, or `rendered` for a row this
// CLI rendered); PATH follows VALUE as the scope qualifier rather than
// splitting the key. Human/table presets only — the machine presets
// (json/tsv/csv/sh) keep their own established column order.
const HEADERS: [&str; COLS] = [
    "PROVIDER", "FIELD", "SOURCE", "VALUE", "PATH", "AGE", "POLL", "TTL",
];
const H_PROVIDER: usize = 0;
const H_FIELD: usize = 1;
const H_SOURCE: usize = 2;
const H_VALUE: usize = 3;
const H_PATH: usize = 4;
const H_AGE: usize = 5;
const H_POLL: usize = 6;

/// Ceiling on the SOURCE column in the human preset, lifted by
/// `--no-trunc`.
///
/// SOURCE would otherwise take its natural width like PROVIDER, because a
/// Source name is short (`refs`, `head`, `put`, `rendered`). One kind is not:
/// a `config:<file>` label is a whole path, and uncapped it pinned VALUE and
/// PATH at their floors across every row of the table. Its tail
/// (`…/defaults/aws.toml`) is what tells one config file from another, so the
/// cell truncates from the head, like PATH does.
const MAX_SOURCE_WIDTH: usize = 24;

/// What the human and table presets show for an absent value: a template leaf
/// that rendered nothing (shown with `--all` or `--filter value=-`).
const ABSENT_TEXT: &str = "null";

/// The glyph that marks a rendered row's VALUE cell when colour is off.
///
/// A rendered row is computed here, from this shell's environment and
/// directory — it is not what the daemon holds. Colour says so with Cyan;
/// without colour the cell needs a mark of its own, so the two builds carry
/// the same information.
fn template_marker(
    kind: Option<&crate::cache::RowKind>,
    color_enabled: bool,
    ascii: bool,
) -> String {
    if color_enabled || !matches!(kind, Some(crate::cache::RowKind::Rendered)) {
        return String::new();
    }
    format!("{} ", Glyphs::of(ascii).rendered)
}

/// How a `--tree` layer row is drawn: one of the layers behind the value row
/// above it.
#[derive(Debug, Clone, Copy)]
pub struct ChildMark {
    /// The last layer under its value row.
    pub last: bool,
    /// The layer the read takes.
    pub answered: bool,
    /// A template record (`<p>:template`), not data.
    pub template: bool,
}

/// The branch glyph of a row hanging under the one above: `├─`, or `└─` for
/// the last, and `|-` / `` `- `` under `--ascii`. Every tree comb draws
/// (`status --tree`, `check config --origins`) uses it.
pub fn tree_branch(last: bool, ascii: bool) -> &'static str {
    let g = Glyphs::of(ascii);
    if last { g.last_branch } else { g.branch }
}

/// `s` dimmed when colour is on, as a shadowed tree row is.
pub fn dim(s: &str, color_enabled: bool) -> String {
    ansi::wrap(s, ansi::Color::Dim, color_enabled)
}

/// `s` in bold when colour is on, as a table's header is.
pub fn bold(s: &str, color_enabled: bool) -> String {
    ansi::wrap(s, ansi::Color::Bold, color_enabled)
}

impl ChildMark {
    /// The branch glyph that leads the FIELD cell.
    fn glyph(self, ascii: bool) -> &'static str {
        tree_branch(self.last, ascii)
    }

    /// The FIELD cell: the branch glyph, hanging under the field name above,
    /// then what the layer is to the value — `:template` for the template
    /// record that defines it, `shadowed` for a layer the read did not take.
    /// Colour dims a shadowed layer as well; the word is there either way.
    fn field(self, ascii: bool) -> String {
        let mut cell = vec![self.glyph(ascii)];
        if self.template {
            cell.push(":template");
        }
        if !self.answered {
            cell.push("shadowed");
        }
        cell.join(" ")
    }
}

/// Whether `layer` is `row` itself, which the row already shows: the record
/// the read took, or under `-a` a shadowed record shown as a row of its own,
/// whose children then show what shadows it. A rendered row is never its own
/// layer — its template record is what defines it.
fn is_the_row(row: &CacheRow, layer: &crate::cache::LayerRecord) -> bool {
    layer.facet.is_none()
        && !matches!(row.kind, Some(crate::cache::RowKind::Rendered))
        && layer.path == row.path
        && layer.label == row.source
}

/// `comb status --tree`: each value row followed by the layers behind it
/// (`children[i]` for `rows[i]`), in the `human`, `table` or `json` preset.
/// `json` gives each row its whole `layers` array; the tables draw each layer
/// as a row of its own under its value, except the one that is the row
/// itself, so a value with nothing behind it is drawn alone.
pub fn render_tree(
    format: &Format,
    rows: &[CacheRow],
    children: &[Vec<crate::cache::LayerRecord>],
    opts: &RenderOpts,
) -> String {
    if format == &Format::Preset(Preset::Json) {
        let mut out = String::new();
        for (row, layers) in rows.iter().zip(children) {
            if let Ok(mut v) = serde_json::to_value(row) {
                v["layers"] = serde_json::to_value(layers).unwrap_or_default();
                out.push_str(&v.to_string());
                out.push('\n');
            }
        }
        return out;
    }
    let mut display = Vec::new();
    let mut marks = Vec::new();
    for (row, layers) in rows.iter().zip(children) {
        display.push(row.clone());
        marks.push(None);
        let layers: Vec<_> = layers.iter().filter(|l| !is_the_row(row, l)).collect();
        for (i, layer) in layers.iter().enumerate() {
            display.push(CacheRow {
                shadowed: false,
                provider: row.provider.clone(),
                path: layer.path.clone(),
                source: layer.label.clone(),
                field: row.field.clone(),
                value: layer.value.clone(),
                age_ms: Some(layer.age_ms),
                stale: layer.stale,
                kind: None,
                poll_interval_secs: None,
                poll_count: None,
                fsevents_reinstate: None,
                polls_elapsed: None,
                next_poll_in_secs: None,
                failure: None,
            });
            marks.push(Some(ChildMark {
                last: i + 1 == layers.len(),
                answered: layer.answered,
                template: layer.facet.as_deref() == Some("template"),
            }));
        }
    }
    match format {
        Format::Preset(Preset::Table) => render_table(&display, &marks, false, None, true, false),
        _ => {
            let trunc = if opts.no_trunc { None } else { opts.max_width };
            render_table(&display, &marks, !opts.no_color, trunc, true, opts.ascii)
        }
    }
}

/// Render rows as aligned columns for the `human` preset.
///
/// - `color_enabled`: apply per-cell ANSI colour (AGE yellow/green by stale, TTL by decay,
///   failure rows red on non-TTL cells, JSON syntax highlighting in structured VALUE cells).
/// - `trunc`: maximum width for the VALUE column (in characters). `None` = no truncation.
/// - `header`: prepend a PROVIDER / FIELD / SOURCE / VALUE / PATH / AGE / POLL / TTL header row.
/// - `ascii`: use ASCII-only glyphs in the TTL cell instead of Unicode.
pub fn render_human(rows: &[CacheRow], opts: &FormatOptions) -> String {
    render_table(rows, &[], false, None, true, opts.ascii)
}

/// Compact a path under `$HOME` to a `~`-prefixed display form. Exact-prefix
/// match only — no symlink resolution, no partial-segment matches (e.g. a
/// home of `/Users/joe` does not compact `/Users/joel/x`). Paths outside
/// `$HOME`, and the pathless-global marker `-`, pass through unchanged.
/// Visible width of the taint marker prefixed to a sanitized row's VALUE cell.
const MARKER_WIDTH: usize = 2;

/// Whether `chars[i..]` begins with `lit`.
fn starts_with_at(chars: &[char], i: usize, lit: &str) -> bool {
    lit.chars()
        .enumerate()
        .all(|(off, want)| chars.get(i + off) == Some(&want))
}

/// Colourize a compact-JSON string for the human preset's VALUE cell.
///
/// Object keys dim, string values green, numbers cyan, `true`/`false`/`null`
/// magenta, punctuation left alone. Only ever *adds* escape sequences, so it
/// must run **after** truncation — highlighting a cut string is safe, cutting
/// a highlighted one would slice an escape sequence in half.
///
/// The input is a display string, so it may be truncated mid-token and may
/// carry a trailing `...`. Every branch therefore tolerates running off the
/// end: an unterminated string or number simply colours to the end of what is
/// there.
fn highlight_json(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len() + 32);
    // Container stack, so a `,` knows whether the next string is an object key
    // or an array element.
    let mut stack: Vec<char> = Vec::new();
    let mut expect_key = false;
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];
        match c {
            '"' => {
                let start = i;
                i += 1;
                while i < chars.len() {
                    match chars[i] {
                        '\\' => i += 2, // escaped char — never ends the string
                        '"' => {
                            i += 1;
                            break;
                        }
                        _ => i += 1,
                    }
                }
                let end = i.min(chars.len());
                let text: String = chars[start..end].iter().collect();
                let color = if expect_key {
                    ansi::Color::Dim
                } else {
                    ansi::Color::BrightGreen
                };
                out.push_str(&ansi::wrap(&text, color, true));
            }
            '{' | '[' => {
                stack.push(c);
                expect_key = c == '{';
                out.push(c);
                i += 1;
            }
            '}' | ']' => {
                stack.pop();
                expect_key = false;
                out.push(c);
                i += 1;
            }
            ',' => {
                expect_key = stack.last() == Some(&'{');
                out.push(c);
                i += 1;
            }
            ':' => {
                expect_key = false;
                out.push(c);
                i += 1;
            }
            '-' | '0'..='9' => {
                let start = i;
                i += 1;
                while i < chars.len() && matches!(chars[i], '0'..='9' | '.' | 'e' | 'E' | '+' | '-')
                {
                    i += 1;
                }
                let text: String = chars[start..i].iter().collect();
                out.push_str(&ansi::wrap(&text, ansi::Color::Cyan, true));
            }
            't' | 'f' | 'n' => {
                match ["true", "false", "null"]
                    .into_iter()
                    .find(|lit| starts_with_at(&chars, i, lit))
                {
                    Some(lit) => {
                        out.push_str(&ansi::wrap(lit, ansi::Color::Magenta, true));
                        i += lit.len();
                    }
                    None => {
                        out.push(c);
                        i += 1;
                    }
                }
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// Whether any provider-controlled text in this row contains a control
/// character, and so will be rewritten by [`sanitize_cell`].
fn row_is_tainted(row: &CacheRow, path: &str, value: &str) -> bool {
    [row.provider.as_str(), row.field.as_str(), path, value]
        .iter()
        .any(|s| s.chars().any(char::is_control))
}

/// Replace control characters in provider-controlled cell text with visible
/// stand-ins.
///
/// Cell text comes from providers, from `comb put`, and from paths — none of
/// it is ours. Emitted verbatim, an escape sequence in a value is executed by
/// the terminal: at best it breaks column alignment, at worst it repositions
/// the cursor or recolours the rest of the screen from a string an unrelated
/// process wrote into the cache.
///
/// C0 controls map to their Unicode Control Pictures (`ESC` → `␛`, `TAB` →
/// `␉`), DEL to `␡`, and anything else `char::is_control` reports — the C1
/// range — to the replacement character. Every stand-in is exactly one char
/// wide, so sanitizing never shifts a column. `--ascii` collapses the lot to
/// `?`, the pictures being non-ASCII themselves.
pub fn sanitize_cell(s: &str, ascii: bool) -> String {
    if !s.chars().any(char::is_control) {
        return s.to_string();
    }
    s.chars()
        .map(|c| {
            if !c.is_control() {
                return c;
            }
            if ascii {
                return '?';
            }
            match c as u32 {
                // The Control Pictures block mirrors C0 one-for-one at +0x2400.
                cp @ 0x00..=0x1F => char::from_u32(0x2400 + cp).unwrap_or('\u{FFFD}'),
                0x7F => '\u{2421}', // ␡
                _ => '\u{FFFD}',
            }
        })
        .collect()
}

/// A row's PATH as the tables show it: the global entry as `~`, and a path
/// under home written from `~` ([`crate::home::Home::compact`]).
fn shown_path(path: Option<&str>, home: &crate::home::Home) -> String {
    path.map_or_else(|| "~".to_string(), |p| home.compact(p))
}

fn render_table(
    rows: &[CacheRow],
    marks: &[Option<ChildMark>],
    color_enabled: bool,
    trunc: Option<usize>,
    header: bool,
    ascii: bool,
) -> String {
    // `trunc` is the **total table width cap** (all columns + padding).
    // We scan all rows first to measure natural column widths, then compute
    // how much room the VALUE column has once other columns claim their natural
    // widths and inter-column separators are accounted for.

    // Display-only: a path under $HOME is written from `~`, and the global
    // entry is `~` itself, in both tables. Machine presets (json/csv/tsv/sh)
    // never call through this path — they use `row.path` directly.
    let home = crate::home::Home::from_env();

    let now_unix_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    // -----------------------------------------------------------------------
    // Pass 1: measure natural widths of every column (no truncation).
    // Columns: PROVIDER, FIELD, SOURCE, VALUE, PATH, AGE, POLL, TTL
    // -----------------------------------------------------------------------
    let mut nat_provider = HEADERS[H_PROVIDER].len();
    let mut nat_field = HEADERS[H_FIELD].len();
    let mut nat_source = HEADERS[H_SOURCE].len();
    let mut nat_value = HEADERS[H_VALUE].len();
    let mut nat_path = HEADERS[H_PATH].len();
    let mut nat_age = HEADERS[H_AGE].len();
    let mut nat_poll = HEADERS[H_POLL].len();
    let mut nat_ttl = HEADERS[TTL_COL_IDX].len();

    // Auto-size the TTL cell's `{P}s×{KK} (total)` sub-fields to the widest
    // values in this snapshot, capped at TTL_P_WIDTH_MAX / TTL_TOTAL_WIDTH_MAX.
    let ttl_widths = compute_ttl_widths(rows);

    // Budget-aware: SOURCE competes with VALUE/PATH for the same `trunc`
    // total, so a 24-char config label alone must not starve those columns
    // at a narrow cap.
    let source_cap = trunc.map(|c| MAX_SOURCE_WIDTH.min(c / 5));

    for (index, row) in rows.iter().enumerate() {
        let mark = marks.get(index).copied().flatten();
        let path = shown_path(row.path.as_deref(), &home);
        let value = if row.value.is_null() {
            ABSENT_TEXT.to_string()
        } else {
            libbeachcomber::render::csv_cell(Some(&row.value))
        };
        // Sanitizing swaps one char for one char, so it never changes a
        // natural width. Only the taint marker does.
        let taint_width = if row_is_tainted(row, &path, &value) {
            MARKER_WIDTH
        } else {
            0
        };
        let marker_width =
            taint_width + template_marker(row.kind.as_ref(), color_enabled, ascii).cols();
        if let Some(mark) = mark {
            nat_field = nat_field.max(mark.field(ascii).cols());
            nat_path = nat_path.max(path.cols());
            nat_source = nat_source.max(match source_cap {
                Some(cap) => row.source.cols().min(cap),
                None => row.source.cols(),
            });
            nat_value = nat_value.max(value.cols() + taint_width);
            nat_age = nat_age.max(format_age_with_polls(row.age_ms, None, ascii).cols());
            continue;
        }
        nat_provider = nat_provider.max(row.provider.cols());
        nat_path = nat_path.max(path.cols());
        nat_field = nat_field.max(row.field.cols());
        nat_source = nat_source.max(match source_cap {
            Some(cap) => row.source.cols().min(cap),
            None => row.source.cols(),
        });
        nat_value = nat_value.max(value.cols() + marker_width);
        nat_age = nat_age.max(format_age_with_polls(row.age_ms, row.polls_elapsed, ascii).cols());
        nat_ttl = nat_ttl.max(
            format_ttl_cell(
                row.kind.as_ref(),
                row.poll_interval_secs,
                row.poll_count,
                row.fsevents_reinstate,
                row.failure.as_ref(),
                ascii,
                &ttl_widths,
            )
            .text
            .cols(),
        );
        nat_poll = nat_poll.max(
            format_poll_cell(
                row.kind.as_ref(),
                row.next_poll_in_secs,
                row.failure.as_ref(),
                now_unix_ms,
            )
            .cols(),
        );
    }

    // Under `watch`, AGE and POLL tick every refresh ("9s" → "12s" →
    // "1m16s×1"), and a column sized to the snapshot's widest value shifts
    // every boundary to its right as that value changes width. Floor both at
    // a width covering the common forms and right-align the cells (POLL
    // already is), so the digits stay put between refreshes. The floors are
    // given back below if a tight cap can't afford them.
    const AGE_MIN_WIDTH: usize = 8; // "3m54s×0"
    const POLL_MIN_WIDTH: usize = 4;
    let (nat_age_raw, nat_poll_raw) = (nat_age, nat_poll);
    nat_age = nat_age.max(AGE_MIN_WIDTH);
    nat_poll = nat_poll.max(POLL_MIN_WIDTH);

    // -----------------------------------------------------------------------
    // Split the total-table-width budget across the flexible columns.
    // Layout: MARGIN provider(2sp)field(2sp)source(2sp)value(2sp)path(2sp)age(2sp)poll(2sp)ttl MARGIN
    // Separators: 7 × 2 = 14
    // Margins: 2 spaces each side (breathing room so the table doesn't hug
    // the terminal edge).
    // -----------------------------------------------------------------------
    const MIN_VALUE_WIDTH: usize = 8;
    // Wide enough that a tail-truncated session path (`…/project#session-id`)
    // or destructured field name (`…incident_updates.0.new_status`) still
    // shows the leaf that discriminates it.
    const MIN_PATH_WIDTH: usize = 20;
    const MIN_FIELD_WIDTH: usize = 20;
    const SEP_TOTAL: usize = 14; // 7 separators × 2 spaces
    const MARGIN: usize = 2; // left margin, printed; right margin, reserved only
    const MARGIN_TOTAL: usize = MARGIN * 2;

    // VALUE, FIELD and PATH are the three flexible columns; PROVIDER, SOURCE,
    // AGE, POLL and TTL always take their natural widths. Whatever the budget
    // leaves after those five is `flex`.
    //
    // All three are unbounded in practice — values are arbitrary JSON, paths
    // carry `#session` suffixes, and destructuring makes field names as deep
    // as the data. Any one of them left uncapped pins the other two at their
    // floors across the whole table: that is what an uncapped PATH did to
    // VALUE before, and what an uncapped FIELD did once destructuring landed.
    //
    // Split: FIELD first, natural up to a flex-relative ceiling — losing a
    // few chars of a field name hurts more than losing them from a JSON
    // blob, so a modest name (shallow destructuring) eats from VALUE; the
    // ceiling keeps a deep name (75+ chars at --depth 0) from starving
    // VALUE outright. VALUE then takes what remains above PATH's floor;
    // PATH takes what both decline.
    let (field_cap, path_cap, value_cap) = match trunc {
        None => (None, None, None),
        Some(total_cap) => {
            let mut fixed =
                nat_provider + nat_source + nat_age + nat_poll + nat_ttl + SEP_TOTAL + MARGIN_TOTAL;
            let mut flex = total_cap.saturating_sub(fixed);
            // A tight cap may not afford the AGE/POLL stability floors: if
            // the flexible columns can't stand on their own floors, give the
            // padding back — a stable column is pointless when the row
            // overflows the cap.
            let flex_floors = MIN_FIELD_WIDTH.min(nat_field)
                + MIN_PATH_WIDTH.min(nat_path)
                + MIN_VALUE_WIDTH.min(nat_value);
            if flex < flex_floors && (nat_age > nat_age_raw || nat_poll > nat_poll_raw) {
                nat_age = nat_age_raw;
                nat_poll = nat_poll_raw;
                fixed = nat_provider
                    + nat_source
                    + nat_age
                    + nat_poll
                    + nat_ttl
                    + SEP_TOTAL
                    + MARGIN_TOTAL;
                flex = total_cap.saturating_sub(fixed);
            }
            if nat_value + nat_field + nat_path <= flex {
                // All three fit at their natural widths: nothing truncates.
                (None, None, None)
            } else {
                // A floor never widens a column past what it actually needs,
                // hence the `.min(nat_*)` on each.
                let field_floor = MIN_FIELD_WIDTH.min(nat_field);
                let path_floor = MIN_PATH_WIDTH.min(nat_path);

                let field_w = nat_field.min((flex * 2 / 5).max(field_floor));
                let value_w = nat_value
                    .min(flex.saturating_sub(field_w + path_floor))
                    .max(MIN_VALUE_WIDTH.min(nat_value));
                let path_w = nat_path
                    .min(flex.saturating_sub(value_w + field_w))
                    .max(path_floor);

                (
                    (field_w < nat_field).then_some(field_w),
                    (path_w < nat_path).then_some(path_w),
                    (value_w < nat_value).then_some(value_w),
                )
            }
        }
    };

    // -----------------------------------------------------------------------
    // Pass 2: build cells, applying the caps computed above.
    // -----------------------------------------------------------------------

    // Build string cells — each row is a fixed-length array of display strings.
    // The array may contain ANSI escape sequences; col_widths tracks the
    // *visible* char count (without escapes) for alignment.
    let mut cells: Vec<[String; COLS]> = Vec::new();
    // Parallel vec of visible widths for each cell (escape-stripped).
    let mut visible: Vec<[usize; COLS]> = Vec::new();

    if header {
        // AGE and POLL cells are right-aligned; sit their headers over the
        // digits by pre-padding them to the same width.
        let mut header_texts: [String; COLS] = HEADERS.map(str::to_string);
        header_texts[H_AGE] = format!("{:>nat_age$}", HEADERS[H_AGE]);
        header_texts[H_POLL] = format!("{:>nat_poll$}", HEADERS[H_POLL]);
        let header_vis: [usize; COLS] = [
            header_texts[0].len(),
            header_texts[1].len(),
            header_texts[2].len(),
            header_texts[3].len(),
            header_texts[4].len(),
            header_texts[5].len(),
            header_texts[6].len(),
            header_texts[7].len(),
        ];
        let header_row: [String; COLS] =
            header_texts.map(|h| ansi::wrap(&h, ansi::Color::Bold, color_enabled));
        cells.push(header_row);
        visible.push(header_vis);
    }

    for (index, row) in rows.iter().enumerate() {
        let mark = marks.get(index).copied().flatten();
        // Provider-controlled text is sanitized first, then truncated — never
        // the other way round, so a truncation can't leave half an escape
        // sequence intact.
        let path_raw = shown_path(row.path.as_deref(), &home);
        let value_raw = if row.value.is_null() {
            ABSENT_TEXT.to_string()
        } else {
            libbeachcomber::render::csv_cell(Some(&row.value))
        };
        let tainted = row_is_tainted(row, &path_raw, &value_raw);

        let ell = ellipsis(ascii);
        let provider_text = match mark {
            Some(_) => String::new(),
            None => sanitize_cell(&row.provider, ascii),
        };
        let mut source_text = sanitize_cell(&row.source, ascii);
        if let Some(cap) = source_cap {
            source_text = truncate_path(&source_text, cap, ell);
        }
        let mut field_text = match mark {
            Some(mark) => mark.field(ascii),
            None => sanitize_cell(&row.field, ascii),
        };
        if let Some(cap) = field_cap {
            field_text = truncate_middle(&field_text, cap, ell);
        }
        let mut path = sanitize_cell(&path_raw, ascii);
        if let Some(cap) = path_cap {
            path = truncate_path(&path, cap, ell);
        }

        // A sanitized row is flagged in its VALUE cell, so the eye can find
        // the rows whose text was rewritten without diffing against the
        // source. The marker is part of the cell, so it takes part in the
        // width budget and truncates with everything else.
        let marker = if tainted {
            format!("{} ", Glyphs::of(ascii).tainted)
        } else {
            String::new()
        };
        // A rendered row is marked ahead of the taint marker: colour-off
        // builds need to see "this was computed here" before anything else in
        // the cell (design decision 5).
        let tmpl_marker = template_marker(row.kind.as_ref(), color_enabled, ascii);
        let mut value = format!("{tmpl_marker}{marker}{}", sanitize_cell(&value_raw, ascii));
        if let Some(max) = value_cap {
            value = truncate(&value, max, ell);
        }

        // AGE cell — right-aligned against the floored column width (see the
        // watch-jitter note above), coloured by stale state.
        let age_text = format!(
            "{:>width$}",
            format_age_with_polls(row.age_ms, row.polls_elapsed, ascii),
            width = nat_age
        );
        let age_vis = age_text.cols();
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
            row.poll_count,
            row.fsevents_reinstate,
            row.failure.as_ref(),
            ascii,
            &ttl_widths,
        );
        // A layer row has no lifecycle of its own to show.
        let ttl = if mark.is_some() {
            TtlCell {
                text: String::new(),
                color: None,
            }
        } else {
            ttl
        };
        let ttl_vis = ttl.text.cols();
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
            if mark.is_some() {
                String::new()
            } else {
                format_poll_cell(
                    row.kind.as_ref(),
                    row.next_poll_in_secs,
                    row.failure.as_ref(),
                    now_unix_ms,
                )
            },
            width = nat_poll
        );
        let poll_vis = poll_text.cols();

        let provider_vis = provider_text.cols();
        let path_vis = path.cols();
        let field_vis = field_text.cols();
        let source_vis = source_text.cols();
        let value_vis = value.cols();

        // VALUE cell. Truncation has already happened above, so the colouring
        // here can only ever add escape sequences — it never slices one.
        // The taint marker keeps its own yellow so a rewritten row stands out;
        // a structured body gets JSON syntax highlighting.
        let value_cell = if !color_enabled {
            value
        } else {
            let (marker_text, body): (String, String) = if marker.is_empty() {
                (String::new(), value)
            } else {
                (
                    value.chars().take(MARKER_WIDTH).collect(),
                    value.chars().skip(MARKER_WIDTH).collect(),
                )
            };
            // Cyan is the colour build's mark for a rendered row (design
            // decision 5). A structured body still gets its own JSON
            // highlighting nested inside; each highlighted token carries a
            // full reset, so on-screen only the cell's opening brace stays
            // cyan — the row is marked by that lead-in plus its SOURCE, not
            // by an unbroken cyan span.
            let body = if row.value.is_null() {
                ansi::wrap(&body, ansi::Color::Dim, true)
            } else if matches!(row.kind, Some(crate::cache::RowKind::Rendered)) {
                let body = if body.starts_with(['{', '[']) {
                    highlight_json(&body)
                } else {
                    body
                };
                ansi::wrap(&body, ansi::Color::Cyan, true)
            } else if body.starts_with(['{', '[']) {
                highlight_json(&body)
            } else {
                body
            };
            if marker_text.is_empty() {
                body
            } else {
                format!(
                    "{}{body}",
                    ansi::wrap(&marker_text, ansi::Color::Yellow, true)
                )
            }
        };

        let mut row_cells: [String; COLS] = [
            provider_text,
            field_text,
            source_text,
            value_cell,
            path,
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
        // A layer the read did not take is dimmed, beside the `shadowed` its
        // FIELD cell says ([`ChildMark::field`]).
        if color_enabled && mark.is_some_and(|m| !m.answered) {
            for cell in row_cells.iter_mut() {
                *cell = ansi::wrap(&term::strip_escapes(cell), ansi::Color::Dim, true);
            }
        }

        let row_vis: [usize; COLS] = [
            provider_vis,
            field_vis,
            source_vis,
            value_vis,
            path_vis,
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

    // The right margin is reserved in the width budget above but never
    // painted. Emitting it padded every row out to exactly `max_width` — i.e.
    // exactly the terminal width — which viewers that reserve a column wrap:
    // viddy paints a scrollbar in the last column, so a full-width row spilled
    // its trailing space onto the next screen line and the whole table rendered
    // double-spaced. Reserving without printing keeps the two columns of
    // breathing room and leaves the rows free of trailing whitespace.
    let left_margin: String = " ".repeat(MARGIN);
    let data_start = usize::from(header);
    let stripe = stripe_bg();
    for (idx, (row_cells, row_vis)) in cells.iter().zip(visible.iter()).enumerate() {
        // Alternate a subtle background on every other data row so the eye
        // can track across long lines. Colour-on only, so watch/viddy
        // without --color=always (and every machine consumer) see plain
        // rows. Header excluded from the count.
        let striped = color_enabled && idx >= data_start && (idx - data_start) % 2 == 1;

        let mut line = String::new();
        for (i, cell) in row_cells.iter().enumerate() {
            if i > 0 {
                line.push_str("  "); // 2-space separator
            }
            line.push_str(cell);
            // Pad to column width on all but the last column — and on the
            // last too when striping, so the stripe is a rectangle rather
            // than a ragged bar.
            if i < COLS - 1 || striped {
                let pad = col_widths[i].saturating_sub(row_vis[i]);
                for _ in 0..pad {
                    line.push(' ');
                }
            }
        }

        out.push_str(&left_margin);
        if striped {
            // Every cell's own colour wrap ends in a full reset, which would
            // clear the background mid-stripe — re-open it after each one.
            // Cell text is sanitized, so the only resets are our own.
            out.push_str(stripe);
            out.push_str(&line.replace(ansi::RESET, &format!("{}{stripe}", ansi::RESET)));
            out.push_str(ansi::RESET);
        } else {
            out.push_str(&line);
        }
        out.push('\n');
    }

    out
}

// ---------------------------------------------------------------------------
// Filter and sort helpers (T30 / T31)
// ---------------------------------------------------------------------------

/// The filters `comb status` applies on a key the user gave none on: facet
/// rows (`aws:template`, `*:cache`), absent values and shadowed records are
/// hidden, so the default table is the values a read would answer. `--all`
/// drops them.
pub const DEFAULT_FILTERS: [&str; 3] = ["facet=-", "value!=-", "shadowed=false"];

/// The filters to apply: `user`'s, plus each [`DEFAULT_FILTERS`] entry whose
/// key `user` does not name (with either operator), unless `all`.
pub fn effective_filters(user: &[String], all: bool) -> Vec<String> {
    let mut out = user.to_vec();
    if all {
        return out;
    }
    for default in DEFAULT_FILTERS {
        let key = split_filter(default).map(|(k, _, _)| k);
        let named = user
            .iter()
            .any(|f| split_filter(f).map(|(k, _, _)| k) == key);
        if !named {
            out.push(default.to_string());
        }
    }
    out
}

/// A row's provider without its facet: `aws` for `aws:template`.
pub fn row_base(provider: &str) -> &str {
    provider.split_once(':').map_or(provider, |(base, _)| base)
}

/// A row's facet: `template` for `aws:template`, none for `aws`.
pub fn row_facet(provider: &str) -> Option<&str> {
    provider.split_once(':').map(|(_, facet)| facet)
}

/// The SOURCE label a row displays, which `--filter source=` matches.
pub fn display_source(row: &CacheRow) -> String {
    row.source.clone()
}

/// Apply a list of `key=value` / `key!=value` filter strings to `rows`,
/// returning only the rows that match ALL of them (AND semantics).
///
/// Keys: `provider` (the base name, exact or `*` glob), `facet` (a name or
/// glob, `-` for none), `path` (glob, `-` for none), `field`, `source` (glob
/// over the displayed label), `value` (glob over the displayed value, `-` for
/// absent), `stale`, `lifecycle`, `fsevents_reinstate`. `!=` negates.
/// Returns `Err` if any filter string uses an unknown key or invalid value.
pub fn apply_filters(rows: Vec<CacheRow>, filters: &[String]) -> Result<Vec<CacheRow>, String> {
    let filters: Vec<Filter> = filters
        .iter()
        .map(|s| parse_filter(s))
        .collect::<Result<_, _>>()?;
    Ok(rows
        .into_iter()
        .filter(|r| filters.iter().all(|f| f.matches(r)))
        .collect())
}

/// Whether `provider` (a base name) could survive every `provider` filter
/// in `filters`. Used to skip resolving (and `exec()`ing) a template row's
/// leaves before rendering, when `--filter provider` has already ruled the
/// provider out — `apply_filters` runs the real, authoritative filter
/// afterward, over the rendered rows, so a filter string this can't parse or
/// a predicate it doesn't recognise is not this function's error to raise:
/// it answers "could still pass" and lets `apply_filters` give the answer,
/// and the syntax error, its usual way.
pub fn provider_could_pass(filters: &[String], provider: &str) -> bool {
    filters.iter().all(|f| match parse_filter(f) {
        Ok(
            filter @ Filter {
                predicate: Predicate::Provider(_),
                ..
            },
        ) => filter.matches_base(provider),
        _ => true,
    })
}

/// `key=value` or `key!=value`, split into its key, whether it is negated,
/// and its value.
fn split_filter(s: &str) -> Option<(&str, bool, &str)> {
    let eq = s.find('=')?;
    let (key, negate) = match s[..eq].strip_suffix('!') {
        Some(key) => (key, true),
        None => (&s[..eq], false),
    };
    Some((key, negate, &s[eq + 1..]))
}

struct Filter {
    predicate: Predicate,
    negate: bool,
}

enum Predicate {
    /// The provider's base name, exact or glob.
    Provider(String),
    /// The facet: a name or glob, or none (`-`).
    Facet(Option<String>),
    /// The path as shown or as stored: a glob, the global entry being `~`.
    Path(String),
    Field(String),
    /// The displayed SOURCE label, as a glob.
    Source(String),
    /// The displayed value as a glob, or absent (`-`).
    Value(Option<String>),
    Stale(bool),
    /// A record the read from the caller's directory does not take.
    Shadowed(bool),
    /// Match Lifecycle rows with the given decay level (0=active, 1-4=decayN).
    LifecycleDecay(u8),
    /// Match Once rows.
    LifecycleOnce,
    /// Match Static rows.
    LifecycleStatic,
    /// Match rows rendered here.
    LifecycleRendered,
    /// Match on fsevents_reinstate flag value.
    FseventsReinstate(bool),
}

fn parse_filter(s: &str) -> Result<Filter, String> {
    let (k, negate, v) =
        split_filter(s).ok_or_else(|| format!("filter must be key=value or key!=value: {s}"))?;
    let dash = |v: &str| (v != "-").then(|| v.to_string());
    let predicate = match k {
        "provider" => Predicate::Provider(v.to_string()),
        "facet" => Predicate::Facet(dash(v)),
        "path" if v == "-" => {
            return Err("path=-: the global entry is written ~ (path=~)".to_string());
        }
        "path" => Predicate::Path(v.to_string()),
        "field" => Predicate::Field(v.to_string()),
        "source" => Predicate::Source(v.to_string()),
        "value" => Predicate::Value(dash(v)),
        "stale" => match v {
            "true" => Predicate::Stale(true),
            "false" => Predicate::Stale(false),
            other => return Err(format!("stale= must be true or false, got {other}")),
        },
        "shadowed" => match v {
            "true" => Predicate::Shadowed(true),
            "false" => Predicate::Shadowed(false),
            other => return Err(format!("shadowed= must be true or false, got {other}")),
        },
        "lifecycle" => match v {
            "active" => Predicate::LifecycleDecay(0),
            "once" => Predicate::LifecycleOnce,
            "static" => Predicate::LifecycleStatic,
            "rendered" => Predicate::LifecycleRendered,
            other => match other
                .strip_prefix("decay")
                .and_then(|n| n.parse::<u8>().ok())
            {
                Some(n) if n >= 1 => Predicate::LifecycleDecay(n),
                _ => {
                    return Err(format!(
                        "lifecycle= must be active|decay<N>|once|static|rendered, got {other}"
                    ));
                }
            },
        },
        "fsevents_reinstate" => match v {
            "true" => Predicate::FseventsReinstate(true),
            "false" => Predicate::FseventsReinstate(false),
            other => {
                return Err(format!(
                    "fsevents_reinstate= must be true or false, got {other}"
                ));
            }
        },
        other => return Err(format!("unknown filter key: {other}")),
    };
    Ok(Filter { predicate, negate })
}

impl Filter {
    fn matches(&self, r: &CacheRow) -> bool {
        self.predicate.matches(r) != self.negate
    }

    /// A `provider` filter against a bare base name.
    fn matches_base(&self, base: &str) -> bool {
        match &self.predicate {
            Predicate::Provider(pat) => simple_glob_match(pat, base) != self.negate,
            _ => true,
        }
    }
}

impl Predicate {
    fn matches(&self, r: &CacheRow) -> bool {
        use crate::cache::RowKind;
        match self {
            Predicate::Provider(pat) => simple_glob_match(pat, row_base(&r.provider)),
            Predicate::Facet(None) => row_facet(&r.provider).is_none(),
            Predicate::Facet(Some(pat)) => {
                row_facet(&r.provider).is_some_and(|f| simple_glob_match(pat, f))
            }
            Predicate::Path(pat) => {
                let home = crate::home::Home::from_env();
                simple_glob_match(pat, &shown_path(r.path.as_deref(), &home))
                    || r.path.as_deref().is_some_and(|p| simple_glob_match(pat, p))
            }
            Predicate::Field(v) => &r.field == v,
            Predicate::Source(pat) => simple_glob_match(pat, &display_source(r)),
            Predicate::Value(None) => r.value.is_null(),
            // A glob matches a present value only: `value=*` is "not absent".
            Predicate::Value(Some(pat)) => {
                !r.value.is_null()
                    && simple_glob_match(pat, &libbeachcomber::render::csv_cell(Some(&r.value)))
            }
            Predicate::Stale(b) => r.stale == *b,
            Predicate::Shadowed(b) => r.shadowed == *b,
            Predicate::LifecycleDecay(target) => matches!(
                &r.kind,
                Some(RowKind::Lifecycle { decay, .. }) if decay == target
            ),
            Predicate::LifecycleOnce => matches!(&r.kind, Some(RowKind::Once)),
            Predicate::LifecycleStatic => matches!(&r.kind, Some(RowKind::Static)),
            Predicate::LifecycleRendered => matches!(&r.kind, Some(RowKind::Rendered)),
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
        // A provider's rows sit together: its plain rows, then each facet's
        // (an absent facet sorts first), by field, then by path with the
        // global entry (path=None) first.
        "default" => rows.sort_by(|a, b| {
            row_base(&a.provider)
                .cmp(row_base(&b.provider))
                .then(row_facet(&a.provider).cmp(&row_facet(&b.provider)))
                .then(a.field.cmp(&b.field))
                .then(a.path.cmp(&b.path))
        }),
        "provider" => rows.sort_by(|a, b| a.provider.cmp(&b.provider)),
        "path" => rows.sort_by(|a, b| a.path.cmp(&b.path)),
        "field" => rows.sort_by(|a, b| a.field.cmp(&b.field)),
        "value" => rows.sort_by_key(|r| r.value.to_string()),
        "age" => rows.sort_by_key(|r| r.age_ms),
        "stale" => rows.sort_by_key(|r| r.stale),
        // Closest to eviction first (fewest steps left), Active last,
        // then Once, Static, Rendered, Transient, None.
        "lifecycle" => rows.sort_by_key(|r| match &r.kind {
            Some(RowKind::Lifecycle { decay, depth, .. }) => (0u8, depth.saturating_sub(*decay)),
            Some(RowKind::Once) => (1, 0),
            Some(RowKind::Static) => (2, 0),
            Some(RowKind::Rendered) => (3, 0),
            Some(RowKind::Transient) => (4, 0),
            None => (5, 0),
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

/// An age in milliseconds, in whole seconds: `0s`, `14s`, `3m`, `2h14m`.
pub fn format_age(age_ms: u128) -> String {
    let secs = u64::try_from(age_ms / 1000).unwrap_or(u64::MAX);
    render::duration(Duration::from_secs(secs))
}

/// Format an age with an optional poll-iteration suffix (`×N` / `xN` in ascii mode).
///
/// Used for the AGE column in `comb status` table rendering. The suffix shows how
/// many polls have fired in the current lifecycle step, giving the user a sense of
/// where they are in the K-poll window without computing it from the TTL column.
/// A row with no age (a rendered value that read no cached input) is `-`.
fn format_age_with_polls(age_ms: Option<u128>, polls_elapsed: Option<u32>, ascii: bool) -> String {
    let Some(age_ms) = age_ms else {
        return "-".to_string();
    };
    let base = format_age(age_ms);
    match polls_elapsed {
        Some(n) => format!("{base}{}{n}", Glyphs::of(ascii).times),
        None => base,
    }
}

/// Display columns, as the terminal counts them ([`term::width`]).
trait Cols {
    fn cols(&self) -> usize;
}

impl Cols for str {
    fn cols(&self) -> usize {
        term::width(self)
    }
}

/// The longest prefix of `s` that fits `cols` columns.
fn take_head(s: &str, cols: usize) -> String {
    term::cut(s, cols, "")
}

/// The longest suffix of `s` that fits `cols` columns.
fn take_tail(s: &str, cols: usize) -> String {
    let start = s
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(s.len()))
        .find(|&i| s[i..].cols() <= cols)
        .unwrap_or(s.len());
    s[start..].to_string()
}

/// Truncate to at most `max` columns, keeping the **tail**.
///
/// Used for the directory part of PATH, whose discriminating part is at the
/// end — the project directory — while its head is uniform `~/ws/…` noise
/// that is safe to drop. FIELD uses [`truncate_middle`] instead.
fn truncate_tail(s: &str, max: usize, ell: &str) -> String {
    if s.cols() <= max {
        s.to_string()
    } else {
        let keep = max.saturating_sub(ell.cols());
        format!("{ell}{}", take_tail(s, keep))
    }
}

/// The table's truncation mark: the one-column `…` glyph, or `...` under
/// `--ascii` where the glyph can't be trusted.
fn ellipsis(ascii: bool) -> &'static str {
    Glyphs::of(ascii).ellipsis
}

/// Truncate a cache path to at most `max` columns. A `#session` suffix
/// compresses first — the directory locates the row, the session id only
/// discriminates it, so the id keeps its last 4 chars (`~/ws/proj#…abcd`).
/// If the directory alone still overflows, its own tail is kept. A path
/// with no `#` falls back to plain tail truncation.
fn truncate_path(s: &str, max: usize, ell: &str) -> String {
    if s.cols() <= max {
        return s.to_string();
    }
    if let Some((dir, sess)) = s.rsplit_once('#') {
        let compressed = if sess.cols() > 4 {
            format!("#{ell}{}", take_tail(s, 4))
        } else {
            format!("#{sess}")
        };
        let dir_budget = max.saturating_sub(compressed.cols());
        let out = format!("{}{compressed}", truncate_tail(dir, dir_budget, ell));
        if out.cols() <= max {
            return out;
        }
    }
    truncate_tail(s, max, ell)
}

/// Truncate to at most `max` columns keeping both edges. Used for FIELD:
/// the head names the owning field (`api_status.…`), the tail is the
/// discriminating leaf key (`….new_status`) — a destructured name needs
/// both, unlike a path, whose head is uniform `~/ws/…` noise.
fn truncate_middle(s: &str, max: usize, ell: &str) -> String {
    if s.cols() <= max {
        s.to_string()
    } else {
        let keep = max.saturating_sub(ell.cols());
        format!(
            "{}{ell}{}",
            take_head(s, keep - keep / 2),
            take_tail(s, keep / 2)
        )
    }
}

/// Truncate a string to at most `max` columns, appending `ell` if trimmed.
fn truncate(s: &str, max: usize, ell: &str) -> String {
    if s.cols() <= max {
        s.to_string()
    } else {
        format!("{}{ell}", take_head(s, max.saturating_sub(ell.cols())))
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
                depth: 4,
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
                depth: 4,
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
    fn lead_digit_counts_steps_to_eviction_from_the_entrys_own_depth() {
        // A `put --ttl` entry has depth 1: Active is 1 step from eviction,
        // Decay1 is 0 (the next tick evicts).
        let cell = |decay, depth| {
            format_ttl_cell(
                Some(&RowKind::Lifecycle {
                    decay,
                    depth,
                    watches_files: false,
                }),
                Some(60),
                Some(1),
                Some(false),
                None,
                false,
                &W6,
            )
        };
        let c = cell(1, 1);
        assert!(c.text.starts_with("0 "), "{}", c.text);
        assert!(matches!(c.color, Some(ansi::Color::Red)));
        // Depth 4 is unchanged: Decay1 reads 3.
        let c = cell(1, 4);
        assert!(c.text.starts_with("3 "), "{}", c.text);
        assert!(matches!(c.color, Some(ansi::Color::Dim)));
        // A deeper entry keeps counting.
        assert!(cell(1, 6).text.starts_with("5 "));
    }

    #[test]
    fn ascii_fallback() {
        let cell = format_ttl_cell(
            Some(&RowKind::Lifecycle {
                decay: 0,
                depth: 4,
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
                depth: 4,
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
                depth: 4,
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
                depth: 4,
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
    fn static_renders_dashes() {
        let cell = format_ttl_cell(Some(&RowKind::Static), None, None, None, None, false, &W6);
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
    fn template_renders_dashes() {
        // A rendered row has no lifecycle of its own — it is computed at
        // read time and never cached.
        let cell = format_ttl_cell(Some(&RowKind::Rendered), None, None, None, None, false, &W6);
        assert_eq!(cell.text, "---");
        assert!(cell.color.is_none());
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
                depth: 4,
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
                depth: 4,
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
                depth: 4,
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
                depth: 4,
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
                depth: 4,
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
            shadowed: false,
            provider: "load".into(),
            path: None,
            source: "loadavg".into(),
            field: "one".into(),
            value: serde_json::json!(0.5),
            age_ms: Some(0),
            stale: false,
            kind: Some(RowKind::Lifecycle {
                decay: 0,
                depth: 4,
                watches_files: false,
            }),
            poll_interval_secs: Some(poll),
            poll_count: Some(keep),
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
                    depth: 4,
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
    fn a_long_wait_reads_as_every_duration_does() {
        let lifecycle = RowKind::Lifecycle {
            decay: 0,
            depth: 4,
            watches_files: true,
        };
        assert_eq!(
            format_poll_cell(Some(&lifecycle), Some(3600), None, 0),
            "1h"
        );
        assert_eq!(
            format_poll_cell(Some(&lifecycle), Some(90), None, 0),
            "1m30s"
        );
    }

    #[test]
    fn watch_only_row_shows_dash() {
        // Lifecycle but no poll path (pure Watch): next_poll_in_secs is None.
        assert_eq!(
            format_poll_cell(
                Some(&RowKind::Lifecycle {
                    decay: 0,
                    depth: 4,
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
    fn once_static_transient_template_and_no_kind_show_dash() {
        for kind in [
            None,
            Some(RowKind::Once),
            Some(RowKind::Static),
            Some(RowKind::Transient),
            Some(RowKind::Rendered),
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
                    depth: 4,
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
                    depth: 4,
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
                    depth: 4,
                    watches_files: false,
                }),
                Some(9_999_999),
                None,
                0,
            ),
            "277h46m"
        );
    }
}

#[cfg(test)]
mod format_age_with_polls_tests {
    use super::*;

    #[test]
    fn no_polls_no_suffix() {
        assert_eq!(format_age_with_polls(Some(14_000), None, false), "14s");
    }

    #[test]
    fn polls_unicode_suffix() {
        assert_eq!(
            format_age_with_polls(Some(14_000), Some(3), false),
            "14s\u{00d7}3"
        );
    }

    #[test]
    fn polls_ascii_suffix() {
        assert_eq!(format_age_with_polls(Some(14_000), Some(3), true), "14sx3");
    }

    #[test]
    fn polls_zero() {
        assert_eq!(
            format_age_with_polls(Some(14_000), Some(0), false),
            "14s\u{00d7}0"
        );
    }

    #[test]
    fn polls_two_digit_n() {
        assert_eq!(
            format_age_with_polls(Some(14_000), Some(12), false),
            "14s\u{00d7}12"
        );
    }

    #[test]
    fn minutes_age_with_polls() {
        assert_eq!(
            format_age_with_polls(Some(185_000), Some(5), false),
            "3m5s\u{00d7}5"
        );
    }
}
