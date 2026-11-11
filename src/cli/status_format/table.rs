//! The `human` and `table` presets: rows laid out as aligned columns under a
//! header, fitted to a total width.
//!
//! One render is a pipeline: [`measure`] every column's natural width,
//! [`fit`] the flexible columns to the width cap, build a line of
//! [`Cell`]s per row, then [`layout`] the lines with margins and stripes.

use super::ansi;
use super::cells::{
    MARKER_WIDTH, TtlCell, TtlWidths, colour_value, compute_ttl_widths, format_age_with_polls,
    format_poll_cell, format_ttl_cell, row_is_tainted, sanitize_cell, template_marker,
};
use super::row::{ChildMark, shown_path, shown_source, shown_value, tree_display_rows};
use super::text::{Cols, ellipsis, truncate, truncate_middle, truncate_path};
use super::{FormatOptions, RenderOpts};
use crate::cache::CacheRow;
use crate::cli::output_format::{Format, Preset};
use crate::cli::style::Glyphs;
use libbeachcomber::path::Home;
use libbeachcomber::term;

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
const H_FIELD: usize = 1;
const H_SOURCE: usize = 2;
const H_AGE: usize = 5;
const H_POLL: usize = 6;

/// Which of the columns a render draws, and how.
pub struct Shape {
    /// The columns drawn, by index, in order.
    cols: &'static [usize],
    /// The margin on each side: the left one printed, the right reserved.
    margin: usize,
    /// A read's rows (`comb get -v`) rather than the cache's: each value row
    /// is the answer, marked with the rendered glyph and aged to a tenth of a
    /// second under a minute, and AGE and POLL take no stability floors, as
    /// nothing redraws them, unless POLL is drawn, as `comb status` draws it.
    read: bool,
}

/// `comb status`: every column.
pub const STATUS: Shape = Shape {
    cols: &[0, 1, 2, 3, 4, 5, 6, 7],
    margin: MARGIN,
    read: false,
};

/// `comb get -v`: the key in FIELD, no PROVIDER, and POLL and TTL only for
/// `lifecycle` (`-vvv`).
pub fn read_shape(lifecycle: bool) -> Shape {
    Shape {
        cols: if lifecycle {
            &[1, 2, 3, 4, 5, 6, 7]
        } else {
            &[1, 2, 3, 4, 5]
        },
        margin: 0,
        read: true,
    }
}

/// The least the SOURCE column is cut to when the width cap needs room
/// (less at a narrow cap: a fifth of it), lifted by `--no-trunc`.
///
/// SOURCE takes its natural width while the row fits, as PROVIDER does: a
/// Source name is short (`refs`, `head`, `put`, `rendered`). One kind is not:
/// a `config(<file>)` label is a whole path, and cut to a fixed width it lost
/// its file name. Only when the row overflows with FIELD, PATH and VALUE cut
/// to their floors is it cut, from the middle, so both the directory's head
/// and the file name (`config(~/.con…/defaults/aws.toml)`) stay.
const MAX_SOURCE_WIDTH: usize = 24;

/// What the human and table presets show for an absent value: a template leaf
/// that rendered nothing (shown with `--all` or `--filter value=-`).
const ABSENT_TEXT: &str = "null";

// Under `watch`, AGE and POLL tick every refresh ("9s" → "12s" →
// "1m16s×1"), and a column sized to the snapshot's widest value shifts
// every boundary to its right as that value changes width. Floor both at
// a width covering the common forms and right-align the cells (POLL
// already is), so the digits stay put between refreshes. The floors are
// given back if a tight cap can't afford them ([`fit`]).
const AGE_MIN_WIDTH: usize = 8; // "3m54s×0"
const POLL_MIN_WIDTH: usize = 4;

const MIN_VALUE_WIDTH: usize = 8;
// Wide enough that a tail-truncated session path (`…/project#session-id`)
// or destructured field name (`…incident_updates.0.new_status`) still
// shows the leaf that discriminates it.
const MIN_PATH_WIDTH: usize = 20;
const MIN_FIELD_WIDTH: usize = 20;
const MARGIN: usize = 2; // left margin, printed; right margin, reserved only

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
    let (display, marks) = tree_display_rows(rows, children);
    match format {
        Format::Preset(Preset::Table) => render_table(&display, &marks, false, None, false),
        _ => {
            let trunc = if opts.no_trunc { None } else { opts.max_width };
            render_table(&display, &marks, !opts.no_color, trunc, opts.ascii)
        }
    }
}

/// Render rows as the plain aligned table: no colour, no width cap, a header.
/// `opts.ascii` picks ASCII-only glyphs over Unicode ones.
pub fn render_human(rows: &[CacheRow], opts: &FormatOptions) -> String {
    render_table(rows, &[], false, None, opts.ascii)
}

/// Render rows as aligned columns under a PROVIDER / FIELD / SOURCE / VALUE /
/// PATH / AGE / POLL / TTL header.
///
/// - `marks`: parallel to `rows`; `Some` draws that row as a `--tree` layer.
/// - `color_enabled`: apply per-cell ANSI colour (AGE yellow when its last run
///   failed, else green, TTL by decay, failure rows red on non-TTL cells, JSON
///   syntax highlighting in structured VALUE cells).
/// - `trunc`: the **total table width cap** (all columns + padding); the
///   flexible columns shrink to fit it. `None` = no truncation.
/// - `ascii`: use ASCII-only glyphs instead of Unicode.
pub(super) fn render_table(
    rows: &[CacheRow],
    marks: &[Option<ChildMark>],
    color_enabled: bool,
    trunc: Option<usize>,
    ascii: bool,
) -> String {
    draw(rows, marks, color_enabled, trunc, ascii, &STATUS)
}

/// [`render_table`] in `shape`: `comb status`'s, or a read's
/// ([`read_shape`]).
pub fn draw(
    rows: &[CacheRow],
    marks: &[Option<ChildMark>],
    color_enabled: bool,
    trunc: Option<usize>,
    ascii: bool,
    shape: &Shape,
) -> String {
    let env = Env {
        color: color_enabled,
        ascii,
        read: shape.read,
        floors: !shape.read || shape.cols.contains(&H_POLL),
        // Display-only: a path under $HOME is written from `~`, and the
        // global entry is `~` itself. Machine presets never come through
        // here — they use `row.path` directly.
        home: Home::from_env(),
        now_unix_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
        // Auto-size the TTL cell's `{P}s×{KK} (total)` sub-fields to the
        // widest values in this snapshot.
        ttl_widths: compute_ttl_widths(rows),
    };
    let mark_of = |index: usize| marks.get(index).copied().flatten();

    let mut widths = measure(rows, &mark_of, &env);
    widths.hide(shape.cols);
    let caps = match trunc {
        Some(total_cap) => fit(&mut widths, total_cap, shape),
        None => Caps::default(),
    };

    let mut lines = vec![header_line(&widths, color_enabled)];
    for (index, row) in rows.iter().enumerate() {
        lines.push(data_line(row, mark_of(index), &env, &widths, &caps));
    }
    layout(&lines, color_enabled, shape)
}

/// What every row of one render shares.
struct Env {
    color: bool,
    ascii: bool,
    /// [`Shape::read`].
    read: bool,
    /// AGE and POLL take their stability floors.
    floors: bool,
    home: Home,
    now_unix_ms: u64,
    ttl_widths: TtlWidths,
}

/// One table cell: its text, possibly carrying ANSI escapes, and the visible
/// width alignment counts (without them).
struct Cell {
    text: String,
    vis: usize,
    /// A log line's text: it runs on past the columns after it, which are
    /// not drawn, and takes no part in their widths.
    span: bool,
}

impl Cell {
    fn plain(text: String) -> Self {
        let vis = text.cols();
        Cell {
            text,
            vis,
            span: false,
        }
    }

    fn empty() -> Self {
        Cell::plain(String::new())
    }
}

// ---------------------------------------------------------------------------
// Natural widths
// ---------------------------------------------------------------------------

/// Every column's natural width: its widest cell, header included, before any
/// truncation. `age_raw` and `poll_raw` are AGE's and POLL's before the
/// stability floors.
struct Widths {
    provider: usize,
    field: usize,
    source: usize,
    value: usize,
    path: usize,
    age: usize,
    poll: usize,
    ttl: usize,
    age_raw: usize,
    poll_raw: usize,
    /// The widest layer row's FIELD (`└─ :template shadowed`): FIELD's floor,
    /// so a layer's label is whole while the cap allows.
    layer_field: usize,
}

impl Widths {
    /// Give the columns `cols` leaves out no width, so the fit spends none
    /// on them.
    fn hide(&mut self, cols: &[usize]) {
        let all: [&mut usize; COLS] = [
            &mut self.provider,
            &mut self.field,
            &mut self.source,
            &mut self.value,
            &mut self.path,
            &mut self.age,
            &mut self.poll,
            &mut self.ttl,
        ];
        for (i, width) in all.into_iter().enumerate() {
            if !cols.contains(&i) {
                *width = 0;
            }
        }
    }
}

/// How much of a failing row's error its VALUE cell shows, before the width
/// cap: the machine formats carry the whole message.
const LAST_ERROR_CHARS: usize = 160;

/// `message`'s first line, cut to [`LAST_ERROR_CHARS`] characters with an
/// ellipsis when longer.
fn first_line(message: &str) -> String {
    let line = message.lines().next().unwrap_or_default().trim_end();
    if line.chars().count() <= LAST_ERROR_CHARS {
        return line.to_string();
    }
    let mut cut: String = line.chars().take(LAST_ERROR_CHARS - 1).collect();
    cut.push('\u{2026}');
    cut
}

/// A row's PATH and VALUE as shown, before sanitizing, and whether any
/// provider-controlled text in it needs sanitizing (and so gets a marker).
fn row_texts(row: &CacheRow, env: &Env) -> (String, String, bool) {
    let path = shown_path(row.path.as_deref(), &env.home);
    let value = match shown_value(row) {
        // Nothing stored: the VALUE cell is the latest error.
        Some(error) if row.value.is_null() => first_line(&error),
        shown => shown.unwrap_or_else(|| ABSENT_TEXT.to_string()),
    };
    let tainted = row_is_tainted(row, &path, &value);
    (path, value, tainted)
}

fn measure(rows: &[CacheRow], mark_of: &dyn Fn(usize) -> Option<ChildMark>, env: &Env) -> Widths {
    let mut w = Widths {
        provider: HEADERS[0].len(),
        field: HEADERS[1].len(),
        source: HEADERS[2].len(),
        value: HEADERS[3].len(),
        path: HEADERS[4].len(),
        age: HEADERS[H_AGE].len(),
        poll: HEADERS[H_POLL].len(),
        ttl: HEADERS[TTL_COL_IDX].len(),
        age_raw: 0,
        poll_raw: 0,
        layer_field: 0,
    };
    for (index, row) in rows.iter().enumerate() {
        let (path, value, tainted) = row_texts(row, env);
        // Sanitizing swaps one char for one char, so it never changes a
        // natural width. Only the taint marker does.
        let taint_width = if tainted { MARKER_WIDTH } else { 0 };
        w.path = w.path.max(path.cols());
        w.source = w.source.max(shown_source(&row.source, &env.home).cols());
        if let Some(mark) = mark_of(index).filter(|m| m.line.is_some()) {
            // A log line's text runs past the columns: only its FIELD counts.
            w.field = w.field.max(mark.field(env.ascii).cols());
            continue;
        }
        if let Some(mark) = mark_of(index) {
            // A layer row has no PROVIDER, POLL or TTL of its own.
            w.layer_field = w.layer_field.max(mark.field(env.ascii).cols());
            w.field = w.field.max(w.layer_field);
            w.value = w.value.max(value.cols() + taint_width);
            w.age = w
                .age
                .max(format_age_with_polls(row.age_ms, None, env.ascii).cols());
            continue;
        }
        let marker_width = taint_width + answer_marker(row, env).cols();
        w.provider = w.provider.max(row.provider.cols());
        w.field = w.field.max(row.field.cols());
        w.value = w.value.max(value.cols() + marker_width);
        w.age = w.age.max(age_text(row, None, env).cols());
        w.ttl = w.ttl.max(ttl_cell_of(row, env).text.cols());
        w.poll = w.poll.max(poll_text_of(row, env).cols());
    }
    w.age_raw = w.age;
    w.poll_raw = w.poll;
    if env.floors {
        w.age = w.age.max(AGE_MIN_WIDTH);
        w.poll = w.poll.max(POLL_MIN_WIDTH);
    }
    w
}

// ---------------------------------------------------------------------------
// Fitting to the width cap
// ---------------------------------------------------------------------------

/// The widths the flexible columns are cut to; `None` leaves a column at its
/// natural width.
#[derive(Default)]
struct Caps {
    source: Option<usize>,
    field: Option<usize>,
    path: Option<usize>,
    value: Option<usize>,
}

/// Split the total-table-width budget across the flexible columns.
///
/// Layout: MARGIN provider(2sp)field(2sp)source(2sp)value(2sp)path(2sp)
/// age(2sp)poll(2sp)ttl MARGIN. Separators: 7 × 2 = 14. Margins: 2 spaces
/// each side (breathing room so the table doesn't hug the terminal edge).
///
/// VALUE, FIELD and PATH are the three flexible columns; PROVIDER, SOURCE,
/// AGE, POLL and TTL always take their natural widths. Whatever the budget
/// leaves after those five is `flex`.
///
/// All three are unbounded in practice — values are arbitrary JSON, paths
/// carry `#session` suffixes, and destructuring makes field names as deep
/// as the data. Any one of them left uncapped pins the other two at their
/// floors across the whole table: that is what an uncapped PATH did to
/// VALUE before, and what an uncapped FIELD did once destructuring landed.
///
/// Split: FIELD first, natural up to a flex-relative ceiling — losing a
/// few chars of a field name hurts more than losing them from a JSON
/// blob, so a modest name (shallow destructuring) eats from VALUE; the
/// ceiling keeps a deep name (75+ chars at --depth 0) from starving
/// VALUE outright. VALUE then takes what remains above PATH's floor;
/// PATH takes what both decline.
fn fit(w: &mut Widths, total_cap: usize, shape: &Shape) -> Caps {
    // Two spaces between each pair of columns drawn, and both margins.
    let seps = 2 * shape.cols.len().saturating_sub(1) + 2 * shape.margin;
    let fixed = |w: &Widths| w.provider + w.source + w.age + w.poll + w.ttl + seps;
    // A floor never widens a column past what it actually needs, hence the
    // `.min(w.*)` on each. FIELD's is wide enough for every layer label, cut
    // below that only when VALUE, PATH and SOURCE at their floors leave no room.
    let path_floor = MIN_PATH_WIDTH.min(w.path);
    let value_floor = MIN_VALUE_WIDTH.min(w.value);
    let field_least = MIN_FIELD_WIDTH.min(w.field);
    let field_want = field_least.max(w.layer_field);
    let flex_floors = field_want + path_floor + value_floor;
    // SOURCE is cut only by what the row still overflows with FIELD, PATH
    // and VALUE at their floors, down to its own ([`MAX_SOURCE_WIDTH`]): a
    // config file's name outlasts the rest of a long value.
    let least = fixed(w) + flex_floors;
    let source_floor = MAX_SOURCE_WIDTH.min(total_cap / 5).min(w.source);
    let source_w = w
        .source
        .saturating_sub(least.saturating_sub(total_cap))
        .max(source_floor);
    let source = (source_w < w.source).then_some(source_w);
    w.source = source_w;
    let mut flex = total_cap.saturating_sub(fixed(w));
    // A tight cap may not afford the AGE/POLL stability floors: if the
    // flexible columns can't stand on their own floors, give the padding
    // back — a stable column is pointless when the row overflows the cap.
    if flex < flex_floors && (w.age > w.age_raw || w.poll > w.poll_raw) {
        w.age = w.age_raw;
        w.poll = w.poll_raw;
        flex = total_cap.saturating_sub(fixed(w));
    }
    if w.value + w.field + w.path <= flex {
        // All three fit at their natural widths: only SOURCE may be cut.
        return Caps {
            source,
            ..Caps::default()
        };
    }
    let field_floor = field_want
        .min(flex.saturating_sub(path_floor + value_floor))
        .max(field_least);

    let field_w = w.field.min((flex * 2 / 5).max(field_floor));
    let value_w = w
        .value
        .min(flex.saturating_sub(field_w + path_floor))
        .max(value_floor);
    let path_w = w
        .path
        .min(flex.saturating_sub(value_w + field_w))
        .max(path_floor);

    Caps {
        source,
        field: (field_w < w.field).then_some(field_w),
        path: (path_w < w.path).then_some(path_w),
        value: (value_w < w.value).then_some(value_w),
    }
}

// ---------------------------------------------------------------------------
// Cells
// ---------------------------------------------------------------------------

/// The header line. AGE and POLL cells are right-aligned; their headers sit
/// over the digits by pre-padding them to the same width.
fn header_line(w: &Widths, color: bool) -> [Cell; COLS] {
    let mut texts: [String; COLS] = HEADERS.map(str::to_string);
    texts[H_AGE] = format!("{:>w$}", HEADERS[H_AGE], w = w.age);
    texts[H_POLL] = format!("{:>w$}", HEADERS[H_POLL], w = w.poll);
    texts.map(|h| Cell {
        vis: h.len(),
        text: ansi::wrap(&h, ansi::Color::Bold, color),
        span: false,
    })
}

/// `text` cut to `cap` columns by `cut`, when there is a cap.
fn capped(
    text: String,
    cap: Option<usize>,
    cut: fn(&str, usize, &str) -> String,
    ell: &str,
) -> String {
    match cap {
        Some(cap) => cut(&text, cap, ell),
        None => text,
    }
}

/// One data row's cells. Provider-controlled text is sanitized first, then
/// truncated — never the other way round, so a truncation can't leave half an
/// escape sequence intact.
fn data_line(
    row: &CacheRow,
    mark: Option<ChildMark>,
    env: &Env,
    w: &Widths,
    caps: &Caps,
) -> [Cell; COLS] {
    let ell = ellipsis(env.ascii);
    if let Some(mark) = mark
        && let Some(level) = mark.line
    {
        return line_cells(row, mark, level, env);
    }
    let (path_raw, value_raw, tainted) = row_texts(row, env);

    let provider = match mark {
        Some(_) => String::new(),
        None => sanitize_cell(&row.provider, env.ascii),
    };
    let field = match mark {
        Some(mark) => mark.field(env.ascii),
        None => sanitize_cell(&row.field, env.ascii),
    };
    let source = capped(
        sanitize_cell(&shown_source(&row.source, &env.home), env.ascii),
        caps.source,
        truncate_middle,
        ell,
    );
    let field = capped(field, caps.field, truncate_middle, ell);
    let path = capped(
        sanitize_cell(&path_raw, env.ascii),
        caps.path,
        truncate_path,
        ell,
    );

    let mut cells = [
        Cell::plain(provider),
        Cell::plain(field),
        Cell::plain(source),
        value_cell(row, mark, &value_raw, tainted, env, caps.value),
        Cell::plain(path),
        age_cell(row, mark, env, w.age),
        poll_cell(row, mark, env, w.poll),
        ttl_cell(row, mark, env),
    ];

    // Failure-state colouring: red on all cells except TTL (which already
    // has its own ⚠ + red from format_ttl_cell). POLL is a plain cell — it
    // gets red-wrapped like PROVIDER/PATH/FIELD/VALUE/AGE.
    if env.color && row.failure.is_some() {
        for (i, cell) in cells.iter_mut().enumerate() {
            if i != TTL_COL_IDX {
                cell.text = ansi::wrap(&cell.text, ansi::Color::Red, true);
            }
        }
    }
    // A layer the read did not take is dimmed, beside the `shadowed` its
    // FIELD cell says ([`ChildMark::field`]).
    if env.color && mark.is_some_and(|m| !m.answered) {
        for cell in cells.iter_mut() {
            cell.text = ansi::wrap(&term::strip_escapes(&cell.text), ansi::Color::Dim, true);
        }
    }
    cells
}

/// A log line's cells: its mark in FIELD, then `[level] text` from SOURCE
/// on, uncut.
fn line_cells(
    row: &CacheRow,
    mark: ChildMark,
    level: libbeachcomber::protocol::Level,
    env: &Env,
) -> [Cell; COLS] {
    let text = row.value.as_str().unwrap_or_default();
    let mut cells: [Cell; COLS] = std::array::from_fn(|_| Cell::empty());
    cells[H_FIELD] = Cell::plain(mark.field(env.ascii));
    cells[H_SOURCE] = Cell {
        text: format!(
            "[{}] {}",
            ansi::level(level, level.name(), env.color),
            sanitize_cell(text, env.ascii)
        ),
        vis: 0,
        span: true,
    };
    cells
}

/// What leads a value row's VALUE cell: under a read the rendered glyph on
/// every value row that has a value, the answer; otherwise
/// [`template_marker`]'s.
fn answer_marker(row: &CacheRow, env: &Env) -> String {
    match env.read {
        true if row.value.is_null() => String::new(),
        true => format!("{} ", Glyphs::of(env.ascii).rendered),
        false => template_marker(row.kind.as_ref(), env.color, env.ascii),
    }
}

/// The AGE text: a layer's polls are not its own; under a read a value row
/// is aged to a tenth of a second under a minute (`2.1s`).
fn age_text(row: &CacheRow, mark: Option<ChildMark>, env: &Env) -> String {
    let polls = if mark.is_some() {
        None
    } else {
        row.polls_elapsed
    };
    match row.age_ms {
        Some(ms) if env.read && mark.is_none() && ms < 60_000 => {
            let base = format!("{:.1}s", ms as f64 / 1000.0);
            match polls {
                Some(n) => format!("{base}{}{n}", Glyphs::of(env.ascii).times),
                None => base,
            }
        }
        _ => format_age_with_polls(row.age_ms, polls, env.ascii),
    }
}

/// The VALUE cell. A sanitized row is flagged in it, so the eye can find the
/// rows whose text was rewritten without diffing against the source. The
/// markers are part of the cell, so they take part in the width budget and
/// truncate with everything else.
fn value_cell(
    row: &CacheRow,
    mark: Option<ChildMark>,
    value_raw: &str,
    tainted: bool,
    env: &Env,
    cap: Option<usize>,
) -> Cell {
    let taint = if tainted {
        format!("{} ", Glyphs::of(env.ascii).tainted)
    } else {
        String::new()
    };
    // A rendered row is marked ahead of the taint marker: colour-off builds
    // need to see "this was computed here" before anything else in the cell
    // (design decision 5).
    let rendered = match mark {
        Some(_) => String::new(),
        None => answer_marker(row, env),
    };
    let value = format!("{rendered}{taint}{}", sanitize_cell(value_raw, env.ascii));
    let value = capped(value, cap, truncate, ellipsis(env.ascii));
    let vis = value.cols();
    let text = if env.color {
        colour_value(row, value, tainted)
    } else {
        value
    };
    Cell {
        text,
        vis,
        span: false,
    }
}

/// The AGE cell, right-aligned against the floored column width (see
/// [`AGE_MIN_WIDTH`]), yellow when its last run failed.
fn age_cell(row: &CacheRow, mark: Option<ChildMark>, env: &Env, width: usize) -> Cell {
    let text = format!("{:>width$}", age_text(row, mark, env));
    let vis = text.cols();
    let text = if !env.color {
        text
    } else if row
        .failure
        .as_ref()
        .is_some_and(|f| f.consecutive_failures > 0)
    {
        ansi::wrap(&text, ansi::Color::Yellow, true)
    } else {
        ansi::wrap(&text, ansi::Color::BrightGreen, true)
    };
    Cell {
        text,
        vis,
        span: false,
    }
}

/// A row's TTL cell, from its lifecycle metadata.
fn ttl_cell_of(row: &CacheRow, env: &Env) -> TtlCell {
    format_ttl_cell(
        row.kind.as_ref(),
        row.poll_interval_secs,
        row.poll_count,
        row.failure.as_ref(),
        env.ascii,
        &env.ttl_widths,
    )
}

/// The TTL cell. TTL is the last column, so the padding in [`layout`] (which
/// only pads non-last columns) never touches it; it has its own internal
/// fixed-width discipline via `ttl_widths` instead. A layer row has no
/// lifecycle of its own to show.
fn ttl_cell(row: &CacheRow, mark: Option<ChildMark>, env: &Env) -> Cell {
    let ttl = if mark.is_some() {
        TtlCell {
            text: String::new(),
            color: None,
        }
    } else {
        ttl_cell_of(row, env)
    };
    let vis = ttl.text.cols();
    let text = match (env.color, ttl.color) {
        (true, Some(c)) => ansi::wrap(&ttl.text, c, true),
        _ => ttl.text,
    };
    Cell {
        text,
        vis,
        span: false,
    }
}

/// Seconds until next poll / next retry attempt, as a bare string.
fn poll_text_of(row: &CacheRow, env: &Env) -> String {
    format_poll_cell(
        row.kind.as_ref(),
        row.next_poll_in_secs,
        row.failure.as_ref(),
        env.now_unix_ms,
    )
}

/// The POLL cell. Unlike PROVIDER/PATH/FIELD/etc., POLL is right-aligned
/// (it's a number), so it is pre-padded here against its column width rather
/// than relying on the (left-justifying) padding in [`layout`].
fn poll_cell(row: &CacheRow, mark: Option<ChildMark>, env: &Env, width: usize) -> Cell {
    let text = if mark.is_some() {
        String::new()
    } else {
        poll_text_of(row, env)
    };
    Cell::plain(format!("{text:>width$}"))
}

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

/// Join lines (the header first) into the table: margin, two-space
/// separators, columns padded to their widest visible cell.
fn layout(lines: &[[Cell; COLS]], color: bool, shape: &Shape) -> String {
    let mut col_widths = [0usize; COLS];
    for line in lines {
        for (width, cell) in col_widths.iter_mut().zip(line) {
            *width = (*width).max(cell.vis);
        }
    }

    // The right margin is reserved in the width budget but never painted.
    // Emitting it padded every row out to exactly `max_width` — i.e. exactly
    // the terminal width — which viewers that reserve a column wrap: viddy
    // paints a scrollbar in the last column, so a full-width row spilled its
    // trailing space onto the next screen line and the whole table rendered
    // double-spaced. Reserving without printing keeps the two columns of
    // breathing room and leaves the rows free of trailing whitespace.
    let left_margin = " ".repeat(shape.margin);
    let last = shape.cols.last().copied().unwrap_or(0);
    let stripe = ansi::stripe_bg();
    let mut out = String::new();
    for (idx, line) in lines.iter().enumerate() {
        // Alternate a subtle background on every other data row so the eye
        // can track across long lines. Colour-on only, so watch/viddy
        // without --color=always (and every machine consumer) see plain
        // rows. The header (line 0) is excluded from the count.
        let striped = color && idx >= 1 && (idx - 1) % 2 == 1;

        let mut text = String::new();
        for (n, &i) in shape.cols.iter().enumerate() {
            let cell = &line[i];
            if n > 0 {
                text.push_str("  ");
            }
            text.push_str(&cell.text);
            if cell.span {
                break;
            }
            // Pad to column width on all but the last column — and on the
            // last too when striping, so the stripe is a rectangle rather
            // than a ragged bar.
            if i != last || striped {
                text.push_str(&" ".repeat(col_widths[i].saturating_sub(cell.vis)));
            }
        }

        out.push_str(&left_margin);
        if striped {
            // Every cell's own colour wrap ends in a full reset, which would
            // clear the background mid-stripe — re-open it after each one.
            // Cell text is sanitized, so the only resets are our own.
            out.push_str(stripe);
            out.push_str(&text.replace(ansi::RESET, &format!("{}{stripe}", ansi::RESET)));
            out.push_str(ansi::RESET);
        } else {
            out.push_str(&text);
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::FailureSnapshot;

    fn failing_row(error: &str) -> CacheRow {
        CacheRow {
            warn: None,
            shadowed: false,
            provider: "p".into(),
            path: None,
            source: "s".into(),
            field: "f".into(),
            value: serde_json::Value::Null,
            age_ms: None,
            kind: None,
            poll_interval_secs: None,
            poll_count: None,
            polls_elapsed: None,
            next_poll_in_secs: None,
            failure: Some(FailureSnapshot {
                consecutive_failures: 1,
                suppressed_until_unix_ms: None,
                stopped: false,
                last_error: Some(error.into()),
            }),
        }
    }

    #[test]
    fn a_failing_rows_error_shows_its_first_line_cut_to_length() {
        let error = format!("{}\nsecond line", "x".repeat(500));
        let out = render_table(&[failing_row(&error)], &[], false, None, false);
        let cut = format!("{}\u{2026}", "x".repeat(LAST_ERROR_CHARS - 1));
        assert!(out.contains(&cut), "{out}");
        assert!(!out.contains(&"x".repeat(LAST_ERROR_CHARS)), "{out}");
        assert!(!out.contains("second line"), "{out}");
    }

    #[test]
    fn a_failing_rows_error_fits_the_max_width() {
        let error = format!("{}\nsecond line", "x".repeat(500));
        let out = render_table(&[failing_row(&error)], &[], false, Some(80), false);
        for line in out.lines() {
            assert!(line.cols() <= 80, "{} cols: {line}", line.cols());
        }
        assert!(!out.contains("second line"), "{out}");
    }

    #[test]
    fn a_memo_hit_is_its_own_row_and_its_template_hangs_under_it() {
        use crate::cache::LayerRecord;
        let row = CacheRow {
            source: "memo".into(),
            kind: Some(crate::cache::RowKind::Rendered),
            value: "v".into(),
            failure: None,
            ..failing_row("")
        };
        let layer = |facet: &str, answered: bool| LayerRecord {
            label: "config(/c.toml)".into(),
            facet: Some(facet.into()),
            path: None,
            value: "v".into(),
            age_ms: 0,
            answered,
        };
        let (display, marks) = tree_display_rows(
            &[row],
            &[vec![layer("memo", true), layer("template", false)]],
        );
        assert_eq!(display.len(), 2, "the memo is the row: {display:?}");
        assert!(marks[1].is_some_and(|m| m.template && m.last));
    }

    /// A layer's label (`└─ :template shadowed`) is wider than FIELD's
    /// floor: it stays whole while VALUE and SOURCE can be cut instead.
    #[test]
    fn a_layer_label_is_whole_before_value_or_source_is_cut() {
        use crate::cache::LayerRecord;
        let row = CacheRow {
            source: "memo".into(),
            kind: Some(crate::cache::RowKind::Rendered),
            field: "m.v".into(),
            value: "v".into(),
            failure: None,
            ..failing_row("")
        };
        let layer = |facet: &str, answered: bool| LayerRecord {
            label: "config(/home/someone/.config/beachcomber/config.toml)".into(),
            facet: Some(facet.into()),
            path: None,
            value: "{{ 'v' | cache() | default(_.log.warn('computed')) }}".into(),
            age_ms: 0,
            answered,
        };
        let (display, marks) = tree_display_rows(
            &[row],
            &[vec![layer("memo", true), layer("template", false)]],
        );
        let draw_at = |width: usize| {
            draw(
                &display,
                &marks,
                false,
                Some(width),
                false,
                &read_shape(false),
            )
        };
        let out = draw_at(90);
        assert!(out.contains("└─ :template shadowed  "), "{out}");
        for line in out.lines() {
            assert!(line.cols() <= 90, "{} cols: {line}", line.cols());
        }
        // Too narrow even with the rest at their floors, FIELD is cut too.
        assert!(!draw_at(50).contains(":template shadowed"));
    }

    #[test]
    fn a_config_label_is_whole_until_the_width_needs_it_cut_from_the_middle() {
        let home = Home::literal(Some("/h"));
        let label = "config(/h/.config/beachcomber/defaults/aws.toml)";
        assert_eq!(
            shown_source(label, &home),
            "config(~/.config/beachcomber/defaults/aws.toml)"
        );
        assert_eq!(shown_source("source(head)", &home), "source(head)");
        let row = CacheRow {
            source: label.into(),
            failure: None,
            value: "x".into(),
            ..failing_row("")
        };
        let wide = render_table(std::slice::from_ref(&row), &[], false, None, true);
        assert!(wide.contains("defaults/aws.toml)"), "{wide}");
        let narrow = render_table(&[row], &[], false, Some(80), true);
        let cut = narrow.lines().nth(1).unwrap();
        assert!(cut.contains("config("), "{narrow}");
        assert!(cut.contains("aws.toml)"), "{narrow}");
        assert!(cut.contains("..."), "{narrow}");
    }
}
