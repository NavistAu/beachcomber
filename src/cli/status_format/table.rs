use super::ansi;
use super::cells::*;
use super::row::*;
use super::text::*;
use super::{FormatOptions, RenderOpts};
use crate::cache::CacheRow;
use crate::cli::output_format::{Format, Preset};
use crate::cli::style::Glyphs;
use libbeachcomber::term;

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
                kind: None,
                poll_interval_secs: None,
                poll_count: None,
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
/// - `color_enabled`: apply per-cell ANSI colour (AGE yellow when its last run failed, else green, TTL by decay,
///   failure rows red on non-TTL cells, JSON syntax highlighting in structured VALUE cells).
/// - `trunc`: maximum width for the VALUE column (in characters). `None` = no truncation.
/// - `header`: prepend a PROVIDER / FIELD / SOURCE / VALUE / PATH / AGE / POLL / TTL header row.
/// - `ascii`: use ASCII-only glyphs in the TTL cell instead of Unicode.
pub fn render_human(rows: &[CacheRow], opts: &FormatOptions) -> String {
    render_table(rows, &[], false, None, true, opts.ascii)
}
pub(super) fn render_table(
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
    let home = libbeachcomber::path::Home::from_env();

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
        let value = shown_value(row).unwrap_or_else(|| ABSENT_TEXT.to_string());
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
        let value_raw = shown_value(row).unwrap_or_else(|| ABSENT_TEXT.to_string());
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
        // watch-jitter note above), yellow when its last run failed.
        let age_text = format!(
            "{:>width$}",
            format_age_with_polls(row.age_ms, row.polls_elapsed, ascii),
            width = nat_age
        );
        let age_vis = age_text.cols();
        let age_cell = if color_enabled {
            if row
                .failure
                .as_ref()
                .is_some_and(|f| f.consecutive_failures > 0)
            {
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
            let failed = row.failure.as_ref().is_some_and(|f| f.last_error.is_some());
            let body = if row.value.is_null() && failed {
                ansi::wrap(&body, ansi::Color::Red, true)
            } else if row.value.is_null() {
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
    let stripe = ansi::stripe_bg();
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
