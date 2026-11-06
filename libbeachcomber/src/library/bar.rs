//! `bar(width, part, ..., empty=, fill_fg=, text_fg=, label_fg=, bold=)`: one
//! styled row of `width` terminal columns made of labels and tracks.
//!
//! A part is a **label** (a string, or `{'label': text, 'fg': colour, 'bg':
//! colour}`) or a **track** (a mapping with `value`). Labels take their own
//! display width and the tracks share the rest, equally, the earlier tracks
//! taking the floor. A track fills a fraction of its columns from one edge,
//! may carry one marker column, and overlays text. Each column is its own
//! styled run, nested as `c | fg(..) | bg(..) | bold` nests, so a bar costs one
//! call instead of a per-cell template loop.
//!
//! Geometry, for a track `W` columns wide (rounding is half away from zero):
//! the fill is `F = clamp(round(value * W), 0, W)` columns from the left, or
//! from the right with `from_right`; a stacked `value` of `[fraction, colour]`
//! pairs ends segment `i` at `min(round((f0 + .. + fi) * W), W)`; the marker is
//! at column `round(marker * W)` (or `floor`), counted from the right with
//! `from_right`, clamped into the track; the text is cut at `W` columns and
//! padded with spaces to `W`.

use super::style::{attribute, colour_arg, paint};
use super::{UNDEFINED, is_absent, text, whole};
use crate::engine::{EngineCtx, op_error};
use crate::term::{self, Colour, Depth, Ground};
use minijinja::value::{Kwargs, Rest, Value as MjValue, ValueKind};
use minijinja::{Environment, Error};
use std::sync::Arc;

pub(super) fn add_bar(jinja: &mut Environment<'_>, ctx: &Arc<EngineCtx>) {
    let paint = Paint {
        on: ctx.colour(),
        depth: ctx.depth(),
    };
    jinja.add_function(
        "bar",
        move |args: Rest<MjValue>, kwargs: Kwargs| -> Result<MjValue, Error> {
            bar(&args, &kwargs, paint)
        },
    );
}

/// How a render draws: with colour or without, at a depth.
#[derive(Clone, Copy)]
struct Paint {
    on: bool,
    depth: Depth,
}

/// The `bar` defaults a part can leave out.
struct Style {
    empty: Option<Colour>,
    fill_fg: Option<Colour>,
    text_fg: Option<Colour>,
    label_fg: Option<Colour>,
    bold: bool,
}

/// A part of a row.
enum Part {
    Label {
        text: String,
        fg: Option<Colour>,
        bg: Option<Colour>,
    },
    Track(Track),
}

struct Track {
    fill: Fill,
    from_right: bool,
    marker: Option<f64>,
    marker_floor: bool,
    marker_char: String,
    marker_fg: Option<Option<Colour>>,
    text: String,
    text_fg: Option<Colour>,
}

enum Fill {
    /// One colour over a fraction of the track; `None` fills nothing.
    Single(Option<f64>, Option<Colour>),
    /// Segments of `(fraction, colour)` from the start edge.
    Stacked(Vec<(f64, Option<Colour>)>),
}

fn bar(args: &[MjValue], kwargs: &Kwargs, paint: Paint) -> Result<MjValue, Error> {
    let style = style_from(kwargs)?;
    let Some((width, parts)) = args.split_first() else {
        return Err(op_error("bar(): takes a width, then labels and tracks"));
    };
    let width = whole("bar", "the width", width)?;
    let width =
        usize::try_from(width).map_err(|_| op_error("bar(): the width must not be negative"))?;
    let parts = parts
        .iter()
        .map(|p| part(p, &style))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(MjValue::from(row(width, &parts, &style, paint)))
}

/// The keyword defaults. Unknown keywords are an error.
fn style_from(kwargs: &Kwargs) -> Result<Style, Error> {
    let colour = |key: &str| -> Result<Option<Colour>, Error> {
        match kwargs.get::<Option<MjValue>>(key)? {
            Some(v) => colour_arg("bar", &v),
            None => Ok(None),
        }
    };
    let bold = match kwargs.get::<Option<MjValue>>("bold")? {
        None => false,
        Some(v) if is_absent(&v) => false,
        Some(v) if v.kind() == ValueKind::Bool => v.is_true(),
        Some(_) => return Err(op_error("bar(): bold must be true or false")),
    };
    let style = Style {
        empty: colour("empty")?,
        fill_fg: colour("fill_fg")?,
        text_fg: colour("text_fg")?,
        label_fg: colour("label_fg")?,
        bold,
    };
    kwargs.assert_all_used().map_err(|_| {
        op_error("bar(): unknown argument (takes empty, fill_fg, text_fg, label_fg, bold)")
    })?;
    Ok(style)
}

const LABEL_KEYS: [&str; 3] = ["label", "fg", "bg"];
const TRACK_KEYS: [&str; 9] = [
    "value",
    "colour",
    "from_right",
    "marker",
    "marker_round",
    "marker_char",
    "marker_fg",
    "text",
    "text_fg",
];

fn part(value: &MjValue, style: &Style) -> Result<Part, Error> {
    match value.kind() {
        ValueKind::Map => {}
        ValueKind::String | ValueKind::Number => {
            return Ok(Part::Label {
                text: text("bar", value)?,
                fg: style.label_fg,
                bg: style.empty,
            });
        }
        _ => {
            return Err(op_error(
                "bar(): a part is a label (text) or a mapping, not a list, a flag or none",
            ));
        }
    }
    let keys: Vec<String> = value
        .try_iter()?
        .map(|k| k.as_str().map_or_else(|| k.to_string(), str::to_string))
        .collect();
    let has = |k: &str| keys.iter().any(|x| x == k);
    let get = |k: &str| value.get_attr(k).ok().filter(|v| !v.is_undefined());
    if has("label") {
        if has("value") {
            return Err(op_error("bar(): a part is a label or a track, not both"));
        }
        if let Some(k) = keys.iter().find(|k| !LABEL_KEYS.contains(&k.as_str())) {
            return Err(op_error(format!(
                "bar(): unknown key {k:?} in a label (takes label, fg, bg)"
            )));
        }
        let shown = get("label").unwrap_or(UNDEFINED);
        let colour = |key: &str, default: Option<Colour>| match get(key) {
            None => Ok(default),
            Some(v) => colour_arg("bar", &v),
        };
        return Ok(Part::Label {
            text: if is_absent(&shown) {
                String::new()
            } else {
                text("bar", &shown)?
            },
            fg: colour("fg", style.label_fg)?,
            bg: colour("bg", style.empty)?,
        });
    }
    if let Some(k) = keys.iter().find(|k| !TRACK_KEYS.contains(&k.as_str())) {
        return Err(op_error(format!(
            "bar(): unknown key {k:?} in a track (takes {})",
            TRACK_KEYS.join(", ")
        )));
    }
    if !has("value") {
        return Err(op_error("bar(): a track needs a value"));
    }
    let value_of = get("value").unwrap_or(UNDEFINED);
    let colour = match get("colour") {
        None => None,
        Some(c) => Some(colour_arg("bar", &c)?),
    };
    let fill = if is_absent(&value_of) {
        Fill::Single(None, None)
    } else if value_of.kind() == ValueKind::Seq {
        if get("colour").is_some() {
            return Err(op_error(
                "bar(): a stacked value carries its colours; drop the track's colour",
            ));
        }
        let mut segments = Vec::new();
        for pair in value_of.try_iter()? {
            let parts: Vec<MjValue> = match pair.kind() {
                ValueKind::Seq => pair.try_iter()?.collect(),
                _ => Vec::new(),
            };
            let [fraction, c] = parts.as_slice() else {
                return Err(op_error(
                    "bar(): a stacked value is a list of [fraction, colour] pairs",
                ));
            };
            segments.push((fraction_of(fraction)?, colour_arg("bar", c)?));
        }
        Fill::Stacked(segments)
    } else {
        let Some(c) = colour else {
            return Err(op_error("bar(): a track with a value needs a colour"));
        };
        Fill::Single(Some(fraction_of(&value_of)?), c)
    };
    let marker = match get("marker").filter(|m| !is_absent(m)) {
        None => None,
        Some(m) => Some(fraction_of(&m)?),
    };
    let marker_floor = match get("marker_round").filter(|m| !is_absent(m)) {
        None => false,
        Some(r) => match r.as_str() {
            Some("round") => false,
            Some("floor") => true,
            _ => return Err(op_error("bar(): marker_round must be 'round' or 'floor'")),
        },
    };
    let marker_char = match get("marker_char").filter(|m| !is_absent(m)) {
        None => "▏".to_string(),
        Some(c) => {
            let c = text("bar", &c)?;
            if term::width(&c) != 1 {
                return Err(op_error("bar(): marker_char must be one column wide"));
            }
            c
        }
    };
    let from_right = match get("from_right").filter(|m| !is_absent(m)) {
        None => false,
        Some(f) if f.kind() == ValueKind::Bool => f.is_true(),
        Some(_) => return Err(op_error("bar(): from_right must be true or false")),
    };
    let marker_fg = match get("marker_fg") {
        None => None,
        Some(c) => Some(colour_arg("bar", &c)?),
    };
    let text_fg = match get("text_fg") {
        None => style.text_fg,
        Some(c) => colour_arg("bar", &c)?,
    };
    let shown = match get("text").filter(|t| !is_absent(t)) {
        None => String::new(),
        Some(t) => text("bar", &t)?,
    };
    Ok(Part::Track(Track {
        fill,
        from_right,
        marker,
        marker_floor,
        marker_char,
        marker_fg,
        text: shown,
        text_fg,
    }))
}

/// A number, finite.
fn fraction_of(v: &MjValue) -> Result<f64, Error> {
    if v.kind() != ValueKind::Number {
        return Err(op_error(format!("bar(): {v} is not a number")));
    }
    f64::try_from(v.clone())
        .ok()
        .filter(|f| f.is_finite())
        .ok_or_else(|| op_error(format!("bar(): {v} is not a finite number")))
}

/// The row: labels at their own width, the tracks sharing what is left.
fn row(width: usize, parts: &[Part], style: &Style, paint: Paint) -> String {
    let labels: usize = parts
        .iter()
        .map(|p| match p {
            Part::Label { text, .. } => term::width(text),
            Part::Track(_) => 0,
        })
        .sum();
    let tracks = parts.iter().filter(|p| matches!(p, Part::Track(_))).count();
    let avail = width.saturating_sub(labels);
    let share = avail.checked_div(tracks).unwrap_or(0);
    let extra = avail.checked_rem(tracks).unwrap_or(0);
    let mut out = String::new();
    let mut seen = 0;
    for p in parts {
        match p {
            Part::Label { text, fg, bg } => {
                let run = paint_run(text, *fg, *bg, style.bold, paint);
                out.push_str(&run);
            }
            Part::Track(t) => {
                // The later tracks take the remainder.
                let w = share + usize::from(seen >= tracks - extra);
                seen += 1;
                out.push_str(&track(t, w, style, paint));
            }
        }
    }
    out
}

/// `text | fg(fg) | bg(bg) | bold`.
fn paint_run(text: &str, fg: Option<Colour>, bg: Option<Colour>, bold: bool, p: Paint) -> String {
    let run = paint(text, fg, Ground::Fore, p.depth, p.on);
    let run = paint(&run, bg, Ground::Back, p.depth, p.on);
    if bold { attribute(run, 1, p.on) } else { run }
}

/// One cell: a glyph from column `col`, `width` columns wide.
struct Cell {
    col: usize,
    width: usize,
    glyph: String,
    marker: bool,
}

fn track(t: &Track, w: usize, style: &Style, paint: Paint) -> String {
    if w == 0 {
        return String::new();
    }
    let region = regions(&t.fill, w, t.from_right);
    let mut cells = cells(&t.text, w);
    if let Some(m) = t.marker {
        place_marker(
            &mut cells,
            w,
            marker_column(m, t.marker_floor, w, t.from_right),
            &t.marker_char,
        );
    }
    let mut out = String::new();
    for cell in &cells {
        let (fg, bg, bold) = match region[cell.col] {
            Some(fill) => (
                if cell.marker {
                    t.marker_fg.unwrap_or(style.fill_fg)
                } else {
                    style.fill_fg
                },
                fill,
                style.bold,
            ),
            None if cell.marker || cell.glyph != " " => (
                if cell.marker {
                    t.marker_fg.unwrap_or(t.text_fg)
                } else {
                    t.text_fg
                },
                style.empty,
                style.bold,
            ),
            // An unfilled space is background only.
            None => (None, style.empty, false),
        };
        out.push_str(&paint_run(&cell.glyph, fg, bg, bold, paint));
    }
    out
}

/// For each column, the fill colour it is filled with, or `None` if unfilled.
fn regions(fill: &Fill, w: usize, from_right: bool) -> Vec<Option<Option<Colour>>> {
    let end_of = |cumulative: f64| (cumulative * w as f64).round().clamp(0.0, w as f64) as usize;
    let mut col: Vec<Option<Option<Colour>>> = vec![None; w];
    let mut set = |start: usize, end: usize, colour: Option<Colour>| {
        for i in start..end {
            let at = if from_right { w - 1 - i } else { i };
            col[at] = Some(colour);
        }
    };
    match fill {
        Fill::Single(None, _) => {}
        Fill::Single(Some(v), colour) => set(0, end_of(*v), *colour),
        Fill::Stacked(segments) => {
            let (mut cumulative, mut from) = (0.0, 0);
            for (fraction, colour) in segments {
                cumulative += fraction;
                let to = end_of(cumulative).max(from);
                set(from, to, *colour);
                from = to;
            }
        }
    }
    col
}

/// The text cut to `w` columns and padded to them, as cells.
fn cells(text: &str, w: usize) -> Vec<Cell> {
    let plain = term::strip_escapes(text);
    let mut padded = term::cut(&plain, w, "");
    padded.push_str(&" ".repeat(w.saturating_sub(term::width(&padded))));
    let mut cells: Vec<Cell> = Vec::new();
    let mut col = 0;
    for c in padded.chars() {
        let glyph = c.to_string();
        match term::width(&glyph) {
            // A combining mark rides on the character before it.
            0 => {
                if let Some(last) = cells.last_mut() {
                    last.glyph.push(c);
                }
            }
            n => {
                cells.push(Cell {
                    col,
                    width: n.min(2),
                    glyph,
                    marker: false,
                });
                col += n.min(2);
            }
        }
    }
    cells
}

/// The marker's column in a track of `w` columns.
fn marker_column(marker: f64, floor: bool, w: usize, from_right: bool) -> usize {
    let k = marker * w as f64;
    let k = if floor { k.floor() } else { k.round() };
    let col = if from_right { w as f64 - k } else { k };
    col.clamp(0.0, (w - 1) as f64) as usize
}

/// Put `glyph` at column `col`. On a wide character the row keeps its width:
/// the marker replaces the character on its first column and a space keeps
/// the second, and a marker on the second column replaces the character with
/// a space before it.
fn place_marker(cells: &mut Vec<Cell>, w: usize, col: usize, glyph: &str) {
    let Some(i) = cells
        .iter()
        .position(|c| c.col <= col && col < c.col + c.width)
    else {
        return;
    };
    let wide = cells[i].width == 2;
    let start = cells[i].col;
    let marker = |col: usize| Cell {
        col,
        width: 1,
        glyph: glyph.to_string(),
        marker: true,
    };
    let space = |col: usize| Cell {
        col,
        width: 1,
        glyph: " ".to_string(),
        marker: false,
    };
    if !wide {
        cells[i] = marker(col);
    } else if col == start {
        cells.splice(i..=i, [marker(start), space(start + 1)]);
    } else {
        cells.splice(i..=i, [space(start), marker(start + 1)]);
    }
    debug_assert!(cells.iter().map(|c| c.width).sum::<usize>() == w);
}

#[cfg(test)]
mod tests {
    use crate::engine::{EngineCtx, build_pure_engine_env};
    use crate::term::strip_escapes;
    use minijinja::Value as MjValue;
    use serde_json::{Value, json};
    use std::sync::Arc;

    const EMPTY: &str = "#3e3d32";
    const DARK: &str = "#1a1a14";
    const WHITE: &str = "#ffffff";

    fn jinja(vars: &[(&str, &str)]) -> minijinja::Environment<'static> {
        let ctx = EngineCtx {
            env: vars
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ..Default::default()
        };
        build_pure_engine_env(Arc::new(ctx))
    }

    /// `bar(width, parts..., <styles>)` over `parts`, at 24-bit colour.
    fn call(width: Value, parts: &[Value], styles: &str) -> Result<String, String> {
        call_with(&[("COLORTERM", "truecolor")], width, parts, styles)
    }

    fn call_with(
        vars: &[(&str, &str)],
        width: Value,
        parts: &[Value],
        styles: &str,
    ) -> Result<String, String> {
        let mut context = serde_json::Map::new();
        context.insert("w".into(), width);
        let mut names = String::from("w");
        for (i, p) in parts.iter().enumerate() {
            context.insert(format!("p{i}"), p.clone());
            names.push_str(&format!(", p{i}"));
        }
        let expr = format!(
            "bar({names}{}{styles})",
            if styles.is_empty() { "" } else { ", " }
        );
        jinja(vars)
            .compile_expression(&expr)
            .and_then(|e| e.eval(MjValue::from_serialize(Value::Object(context))))
            .map(|v| v.to_string())
            .map_err(|e| e.to_string())
    }

    const STYLES: &str =
        "empty='#3e3d32', fill_fg='#1a1a14', text_fg='#ffffff', label_fg='#ffffff', bold=true";

    /// A column's run, as `c | fg(fg) | bg(bg) | bold` nests: `None` is no
    /// such style.
    fn run(c: &str, fg: Option<&str>, bg: Option<&str>, bold: bool) -> String {
        let mut s = c.to_string();
        for (hex, ground) in [(fg, 38), (bg, 48)] {
            if let Some(hex) = hex {
                let n = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).unwrap();
                s = format!("\x1b[{ground};2;{};{};{}m{s}\x1b[0m", n(1), n(3), n(5));
            }
        }
        if bold {
            s = format!("\x1b[1m{s}\x1b[0m");
        }
        s
    }

    /// The characters of a drawn row.
    fn chars(row: &str) -> String {
        strip_escapes(row)
    }

    fn track(value: Value, extra: Value) -> Value {
        let mut m = json!({ "value": value });
        for (k, v) in extra.as_object().unwrap() {
            m[k] = v.clone();
        }
        m
    }

    #[test]
    fn the_context_bar_worked_example() {
        // w = 20, p = 42: the label right-aligned, the marker at floor(0.65 * 20).
        let text = format!("{}42% ", " ".repeat(16));
        let part = json!({
            "value": 0.42, "colour": "#a6e22e", "marker": 0.65, "marker_round": "floor",
            "marker_char": "!", "marker_fg": "#f92672", "text": text, "text_fg": "#a6e22e",
        });
        let row = call(json!(20), &[part], STYLES).unwrap();
        assert_eq!(chars(&row), format!("{}!  42% ", " ".repeat(13)));
        let fill = Some("#a6e22e");
        let mut want = String::new();
        for _ in 0..8 {
            want += &run(" ", Some(DARK), fill, true);
        }
        for _ in 0..5 {
            want += &run(" ", None, Some(EMPTY), false);
        }
        want += &run("!", Some("#f92672"), Some(EMPTY), true);
        want += &run(" ", None, Some(EMPTY), false).repeat(2);
        for c in ['4', '2', '%'] {
            want += &run(&c.to_string(), Some("#a6e22e"), Some(EMPTY), true);
        }
        want += &run(" ", None, Some(EMPTY), false);
        assert_eq!(row, want);
    }

    #[test]
    fn the_dual_rate_row_worked_example() {
        // w = 26, 30% used with 16% of the window elapsed: two tracks of 10
        // around a 6-column label.
        let left = json!({
            "value": 0.30, "colour": "#fd971f", "from_right": true, "marker": 0.16,
            "text": " 30%      ",
        });
        let right = json!({ "value": 0.16, "colour": "#a6e22e", "text": " 4h12m" });
        let row = call(json!(26), &[left, json!("  5H  "), right], STYLES).unwrap();
        assert_eq!(chars(&row), " 30%    ▏   5H   4h12m    ");
        let (fill, label_bg) = (Some("#fd971f"), Some(EMPTY));
        let mut want = run(" ", None, label_bg, false);
        for c in ['3', '0', '%'] {
            want += &run(&c.to_string(), Some(WHITE), label_bg, true);
        }
        want += &run(" ", None, label_bg, false).repeat(3);
        want += &run(" ", Some(DARK), fill, true);
        // The marker is inside the fill: the fill colour behind it.
        want += &run("▏", Some(DARK), fill, true);
        want += &run(" ", Some(DARK), fill, true);
        want += &run("  5H  ", Some(WHITE), label_bg, true);
        let green = Some("#a6e22e");
        want += &run(" ", Some(DARK), green, true);
        want += &run("4", Some(DARK), green, true);
        for c in ['h', '1', '2', 'm'] {
            want += &run(&c.to_string(), Some(WHITE), label_bg, true);
        }
        want += &run(" ", None, label_bg, false).repeat(4);
        assert_eq!(row, want);
    }

    #[test]
    fn the_overage_row_worked_example() {
        let lbl = " OVER ";
        let time = format!("{}4h12m{}", " ".repeat(7), " ".repeat(8));
        let row = call(
            json!(26),
            &[
                json!({ "label": lbl, "fg": "#f92672", "bg": null }),
                json!({ "value": 0.16, "colour": "#a6e22e", "text": time }),
            ],
            "empty='#3e3d32', fill_fg='#1a1a14', text_fg='#ffffff', bold=true",
        )
        .unwrap();
        assert_eq!(
            chars(&row),
            format!(" OVER {}4h12m{}", " ".repeat(7), " ".repeat(8))
        );
        // One run, red foreground, no background, bold.
        assert!(
            row.starts_with(&run(lbl, Some("#f92672"), None, true)),
            "{row:?}"
        );
        let rest = &row[run(lbl, Some("#f92672"), None, true).len()..];
        let green = Some("#a6e22e");
        let mut want = run(" ", Some(DARK), green, true).repeat(3);
        want += &run(" ", None, Some(EMPTY), false).repeat(4);
        for c in ['4', 'h', '1', '2', 'm'] {
            want += &run(&c.to_string(), Some(WHITE), Some(EMPTY), true);
        }
        want += &run(" ", None, Some(EMPTY), false).repeat(8);
        assert_eq!(rest, want);
    }

    #[test]
    fn stacked_values_fill_in_order() {
        let row = call(
            json!(10),
            &[json!({ "value": [[0.3, "#66d9ef"], [0.2, "#a6e22e"]] })],
            "empty='#3e3d32'",
        )
        .unwrap();
        let mut want = run(" ", None, Some("#66d9ef"), false).repeat(3);
        want += &run(" ", None, Some("#a6e22e"), false).repeat(2);
        want += &run(" ", None, Some(EMPTY), false).repeat(5);
        assert_eq!(row, want);
    }

    #[test]
    fn a_stacked_value_from_the_right_starts_at_the_right_edge() {
        let row = call(
            json!(4),
            &[json!({ "value": [[0.5, "red"], [0.25, "blue"]], "from_right": true })],
            "",
        )
        .unwrap();
        // 16 colours: red is SGR 41, blue 44; columns 3,2 red then 1 blue.
        let want = " \x1b[44m \x1b[0m\x1b[41m \x1b[0m\x1b[41m \x1b[0m";
        assert_eq!(row, want);
    }

    #[test]
    fn a_marker_inside_and_outside_the_fill_on_each_side() {
        let styles = "empty='#3e3d32', fill_fg='#1a1a14', text_fg='#ffffff', bold=true";
        for (from_right, marker, col, inside) in [
            (false, 0.3, 3, true),
            (false, 0.8, 8, false),
            (true, 0.3, 7, true),
            (true, 0.8, 2, false),
        ] {
            let part = track(
                json!(0.5),
                json!({ "colour": "#a6e22e", "from_right": from_right, "marker": marker, "marker_char": "|" }),
            );
            let row = call(json!(10), &[part], styles).unwrap();
            let plain = chars(&row);
            assert_eq!(
                plain.find('|'),
                Some(col),
                "{from_right} {marker}: {plain:?}"
            );
            let want = if inside {
                run("|", Some(DARK), Some("#a6e22e"), true)
            } else {
                run("|", Some(WHITE), Some(EMPTY), true)
            };
            assert!(row.contains(&want), "{from_right} {marker}: {row:?}");
        }
    }

    #[test]
    fn the_marker_is_clamped_into_the_track() {
        let at = |marker: f64, from_right: bool| {
            let part = track(
                json!(0.0),
                json!({ "colour": "red", "marker": marker, "marker_char": "|", "from_right": from_right }),
            );
            chars(&call(json!(5), &[part], "").unwrap())
        };
        assert_eq!(at(1.0, false), "    |");
        assert_eq!(at(7.0, false), "    |");
        assert_eq!(at(-3.0, false), "|    ");
        assert_eq!(at(0.0, true), "    |");
        assert_eq!(at(1.0, true), "|    ");
        assert_eq!(at(0.5, false), "   | ");
        let part = track(json!(0.0), json!({ "colour": "red", "marker": null }));
        assert_eq!(chars(&call(json!(5), &[part], "").unwrap()), "     ");
        // No track width, no marker.
        let part = track(json!(0.0), json!({ "colour": "red", "marker": 0.5 }));
        assert_eq!(call(json!(0), &[part], "").unwrap(), "");
    }

    #[test]
    fn marker_rounding_is_round_or_floor() {
        let at = |round: &str| {
            let part = track(
                json!(0.0),
                json!({ "colour": "red", "marker": 0.65, "marker_round": round, "marker_char": "|" }),
            );
            chars(&call(json!(20), &[part], "").unwrap())
                .find('|')
                .unwrap()
        };
        assert_eq!(at("floor"), 13);
        assert_eq!(at("round"), 13);
        let part = track(
            json!(0.0),
            json!({ "colour": "red", "marker": 0.68, "marker_round": "round", "marker_char": "|" }),
        );
        assert_eq!(
            chars(&call(json!(20), &[part], "").unwrap()).find('|'),
            Some(14)
        );
        let part = track(
            json!(0.0),
            json!({ "colour": "red", "marker": 0.68, "marker_round": "floor", "marker_char": "|" }),
        );
        assert_eq!(
            chars(&call(json!(20), &[part], "").unwrap()).find('|'),
            Some(13)
        );
    }

    #[test]
    fn the_fill_rounds_half_away_from_zero_and_clamps() {
        let filled = |w: u64, value: f64| {
            let part = track(json!(value), json!({ "colour": "#a6e22e" }));
            let row = call(json!(w), &[part], "").unwrap();
            row.matches("48;2;166;226;46").count()
        };
        assert_eq!(filled(10, 0.25), 3);
        assert_eq!(filled(10, 0.24), 2);
        assert_eq!(filled(10, 1.0), 10);
        assert_eq!(filled(10, 7.0), 10);
        assert_eq!(filled(10, -1.0), 0);
        assert_eq!(filled(10, 0.0), 0);
        // A value of none fills nothing.
        let part = json!({ "value": null, "colour": "red" });
        assert!(!call(json!(4), &[part], "").unwrap().contains("41m"));
    }

    #[test]
    fn labels_take_their_width_and_the_tracks_share_the_rest() {
        let t = |text: &str| json!({ "value": 0.0, "colour": "red", "text": text });
        // 11 columns, a 3-column label: 8 left, the tracks 4 and 4.
        let row = call(json!(11), &[t("aaaaaa"), json!("[x]"), t("bbbbbb")], "").unwrap();
        assert_eq!(chars(&row), "aaaa[x]bbbb");
        // An odd rest: the earlier track takes the floor.
        let row = call(json!(10), &[t("aaaaaa"), json!("[x]"), t("bbbbbb")], "").unwrap();
        assert_eq!(chars(&row), "aaa[x]bbbb");
        // More labels than width: the row is its labels.
        let row = call(json!(3), &[json!("[abc]"), t("zzzz")], "").unwrap();
        assert_eq!(chars(&row), "[abc]");
        let row = call(json!(0), &[json!("ab")], "").unwrap();
        assert_eq!(chars(&row), "ab");
        // A wide label counts its columns.
        let row = call(json!(6), &[json!("日本"), t("zzzzzz")], "").unwrap();
        assert_eq!(chars(&row), "日本zz");
    }

    #[test]
    fn a_label_is_one_run_with_its_own_or_the_default_colours() {
        let styles = "empty='#3e3d32', label_fg='#ffffff', bold=true";
        let row = call(json!(4), &[json!(" ab ")], styles).unwrap();
        assert_eq!(row, run(" ab ", Some(WHITE), Some(EMPTY), true));
        let part = json!({ "label": "x", "fg": "red" });
        let row = call(json!(1), &[part], styles).unwrap();
        assert_eq!(
            row,
            "\x1b[1m\x1b[48;2;62;61;50m\x1b[31mx\x1b[0m\x1b[0m\x1b[0m"
        );
        // A key given as none applies no colour of that kind.
        let part = json!({ "label": "x", "fg": null, "bg": null });
        assert_eq!(call(json!(1), &[part], styles).unwrap(), "\x1b[1mx\x1b[0m");
        // One left out takes the default.
        let part = json!({ "label": "x", "bg": null });
        assert_eq!(
            call(json!(1), &[part], styles).unwrap(),
            run("x", Some(WHITE), None, true)
        );
    }

    #[test]
    fn text_is_cut_and_padded_to_the_track() {
        let t = |text: &str| json!({ "value": 0.0, "colour": "red", "text": text });
        assert_eq!(chars(&call(json!(4), &[t("abcdefg")], "").unwrap()), "abcd");
        assert_eq!(chars(&call(json!(4), &[t("ab")], "").unwrap()), "ab  ");
        assert_eq!(chars(&call(json!(4), &[t("")], "").unwrap()), "    ");
    }

    #[test]
    fn a_wide_character_in_the_text_takes_two_columns() {
        // The model row: a name, then a fast and a thinking glyph.
        let part = json!({ "value": 0.5, "colour": "#a6e22e", "text": " Fable ⚡💭 37% " });
        let row = call(json!(16), &[part], STYLES).unwrap();
        assert_eq!(chars(&row), " Fable ⚡💭 37% ");
        assert_eq!(crate::term::width(&row), 16);
        // The wide glyph is one run, styled by the column it starts on.
        assert!(
            row.contains(&run("⚡", Some(DARK), Some("#a6e22e"), true)),
            "{row:?}"
        );
        assert!(
            row.contains(&run("💭", Some(WHITE), Some(EMPTY), true)),
            "{row:?}"
        );
        // A text that would straddle the cut drops the wide character whole.
        let part = json!({ "value": 0.0, "colour": "red", "text": "ab⚡" });
        assert_eq!(chars(&call(json!(3), &[part], "").unwrap()), "ab ");
    }

    #[test]
    fn a_marker_on_a_wide_character_keeps_the_row_its_width() {
        let at = |col_marker: f64| {
            let part = json!({
                "value": 0.0, "colour": "red", "text": "a日b", "marker": col_marker, "marker_char": "|",
            });
            chars(&call(json!(4), &[part], "").unwrap())
        };
        // Column 1 is the first column of 日; column 2 its second.
        assert_eq!(at(0.25), "a| b");
        assert_eq!(at(0.5), "a |b");
        assert_eq!(at(0.75), "a日|");
        assert_eq!(crate::term::width(&at(0.25)), 4);
    }

    #[test]
    fn colour_off_is_the_characters_only() {
        let part = json!({
            "value": 0.5, "colour": "red", "marker": 0.1, "text": "ab",
        });
        let off = [("COLORTERM", "truecolor"), ("NO_COLOR", "1")];
        let row = call_with(&off, json!(8), &[json!("[L]"), part], STYLES).unwrap();
        assert_eq!(row, "[L]a▏   ");
        assert!(!row.contains('\x1b'));
        // Everything still checks.
        assert!(call_with(&off, json!(4), &[json!({ "value": 0.5 })], "").is_err());
    }

    #[test]
    fn colours_follow_the_terminal_depth() {
        let part = json!({ "value": 1.0, "colour": "#ff0000" });
        let row = call_with(&[], json!(1), std::slice::from_ref(&part), "").unwrap();
        assert_eq!(row, "\x1b[101m \x1b[0m");
        let row = call_with(&[("TERM", "xterm-256color")], json!(1), &[part], "").unwrap();
        assert_eq!(row, "\x1b[48;5;196m \x1b[0m");
    }

    #[test]
    fn a_style_mapping_splats() {
        let jinja = jinja(&[("COLORTERM", "truecolor")]);
        let value = jinja
            .compile_expression(
                "bar(4, {'value': 0.5, 'colour': 'red'}, **{'empty': 'blue', 'bold': true})",
            )
            .unwrap()
            .eval(())
            .unwrap();
        let direct = jinja
            .compile_expression("bar(4, {'value': 0.5, 'colour': 'red'}, empty='blue', bold=true)")
            .unwrap()
            .eval(())
            .unwrap();
        assert_eq!(value.to_string(), direct.to_string());
    }

    #[test]
    fn a_bad_call_is_an_error_naming_bar() {
        let t = json!({ "value": 0.5, "colour": "red" });
        for (width, parts, styles) in [
            (json!("8"), vec![], ""),
            (json!(1.5), vec![], ""),
            (json!(-1), vec![], ""),
            (json!(null), vec![], ""),
            (json!(4), vec![json!([1])], ""),
            (json!(4), vec![json!(true)], ""),
            (json!(4), vec![json!(null)], ""),
            (json!(4), vec![json!({ "label": "x", "value": 0.5 })], ""),
            (json!(4), vec![json!({ "label": "x", "nope": 1 })], ""),
            (
                json!(4),
                vec![json!({ "value": 0.5, "colour": "red", "nope": 1 })],
                "",
            ),
            (json!(4), vec![json!({ "colour": "red" })], ""),
            (json!(4), vec![json!({ "value": 0.5 })], ""),
            (
                json!(4),
                vec![json!({ "value": "half", "colour": "red" })],
                "",
            ),
            (
                json!(4),
                vec![json!({ "value": [[0.5, "red"]], "colour": "red" })],
                "",
            ),
            (json!(4), vec![json!({ "value": [0.5] })], ""),
            (json!(4), vec![json!({ "value": [[0.5]] })], ""),
            (
                json!(4),
                vec![json!({ "value": 0.5, "colour": "nope" })],
                "",
            ),
            (
                json!(4),
                vec![json!({ "value": 0.5, "colour": "red", "marker": "x" })],
                "",
            ),
            (
                json!(4),
                vec![
                    json!({ "value": 0.5, "colour": "red", "marker": 0.5, "marker_round": "ceil" }),
                ],
                "",
            ),
            (
                json!(4),
                vec![json!({ "value": 0.5, "colour": "red", "marker_char": "ab" })],
                "",
            ),
            (
                json!(4),
                vec![json!({ "value": 0.5, "colour": "red", "from_right": "yes" })],
                "",
            ),
            (json!(4), vec![json!({ "label": "x", "fg": "nope" })], ""),
            (json!(4), vec![t.clone()], "empty='nope'"),
            (json!(4), vec![t.clone()], "bold=1"),
            (json!(4), vec![t.clone()], "colour='red'"),
        ] {
            let err = call(width.clone(), &parts, styles)
                .err()
                .unwrap_or_else(|| panic!("{width} {parts:?} {styles} should fail"));
            assert!(err.contains("bar():"), "{width} {parts:?} {styles}: {err}");
        }
        let err = jinja(&[])
            .compile_expression("bar()")
            .unwrap()
            .eval(())
            .unwrap_err();
        assert!(err.to_string().contains("bar(): takes a width"), "{err}");
    }

    /// The painter's rows, recorded from scanline's pre-phase-6 painter on
    /// one-tree (`tests/fixtures/scanline/README.md`): each row's segments
    /// and the exact bytes it painted. The same segments as one `bar()` call
    /// paint the same bytes.
    #[test]
    fn every_recorded_painter_row_is_painted_byte_for_byte() {
        let fixture: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/scanline/bars.json")).unwrap();
        let theme = &fixture["theme"];
        let rows = fixture["rows"].as_array().unwrap();
        let mut differing = 0;
        assert!(rows.len() > 50, "{} recorded rows", rows.len());
        for row in rows {
            let name = row["name"].as_str().unwrap();
            let parts: Vec<Value> = row["segs"]
                .as_array()
                .unwrap()
                .iter()
                .map(segment_as_part)
                .collect();
            let styles = format!(
                "empty='{}', fill_fg='{}', text_fg=none, label_fg='{}', bold=true",
                theme["bar_empty"].as_str().unwrap(),
                theme["bar_dark_fg"].as_str().unwrap(),
                theme["bar_label_fg"].as_str().unwrap(),
            );
            let got = call(row["width"].clone(), &parts, &styles)
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            match row.get("bar_bytes") {
                // The one deliberate difference: a marker on a wide
                // character keeps the row its width, where the painter
                // dropped a column.
                Some(pinned) => {
                    assert_eq!(got, pinned.as_str().unwrap(), "{name}: {parts:?}");
                    assert_ne!(got, row["bytes"].as_str().unwrap(), "{name}");
                    differing += 1;
                }
                None => assert_eq!(got, row["bytes"].as_str().unwrap(), "{name}: {parts:?}"),
            }
        }
        assert_eq!(
            differing, 1,
            "the one row whose marker lands on a wide glyph"
        );
    }

    /// One painter segment as the part `bar()` takes.
    fn segment_as_part(seg: &Value) -> Value {
        if let Some(label) = seg.get("label") {
            return json!({ "label": label, "fg": seg["fg"], "bg": seg["bg"] });
        }
        let w = seg["w"].as_u64().unwrap();
        let mut part = json!({
            "value": if w == 0 { 0.0 } else { seg["filled"].as_f64().unwrap() / w as f64 },
            "colour": seg["fill"],
            "from_right": seg["from_right"],
            "marker_char": seg["mc"],
            "text": seg["text"],
            "text_fg": seg["lfg"],
        });
        let mk = seg["mk"].as_u64().unwrap();
        // The painter's marker is a column from the left; `mk >= w` is none.
        if mk < w {
            let k = if seg["from_right"].as_bool().unwrap() {
                w - mk
            } else {
                mk
            };
            part["marker"] = json!(k as f64 / w as f64);
        }
        if let Some(fg) = seg["mfg"].as_str().filter(|f| !f.is_empty()) {
            part["marker_fg"] = json!(fg);
        }
        part
    }
}
