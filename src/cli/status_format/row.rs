use super::cells::format_age;
use crate::cache::CacheRow;
use crate::cli::output_format::{Format, Preset};
use crate::cli::style::Glyphs;

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
/// path, age and lifecycle metadata — they describe the same cache
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
        if let Some(e) = &f.last_error {
            fobj.insert("last_error".into(), serde_json::json!(e));
        }
        if let Some(su) = f.suppressed_until_unix_ms {
            fobj.insert("suppressed_until_unix_ms".into(), serde_json::json!(su));
        }
        ctx.insert("failure".into(), serde_json::Value::Object(fobj));
    }
    serde_json::Value::Object(ctx)
}
/// What a row's VALUE cell says: its value, or, for a failing Source with
/// nothing stored, the latest failure's message. `None` is absent.
pub(super) fn shown_value(row: &CacheRow) -> Option<String> {
    if row.value.is_null() {
        return row.failure.as_ref().and_then(|f| f.last_error.clone());
    }
    Some(libbeachcomber::render::csv_cell(Some(&row.value)))
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
impl ChildMark {
    /// The branch glyph that leads the FIELD cell.
    pub(super) fn glyph(self, ascii: bool) -> &'static str {
        tree_branch(self.last, ascii)
    }

    /// The FIELD cell: the branch glyph, hanging under the field name above,
    /// then what the layer is to the value — `:template` for the template
    /// record that defines it, `shadowed` for a layer the read did not take.
    /// Colour dims a shadowed layer as well; the word is there either way.
    pub(super) fn field(self, ascii: bool) -> String {
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
pub(super) fn is_the_row(row: &CacheRow, layer: &crate::cache::LayerRecord) -> bool {
    layer.facet.is_none()
        && !matches!(row.kind, Some(crate::cache::RowKind::Rendered))
        && layer.path == row.path
        && layer.label == row.source
}
/// A row's PATH as the tables show it: the global entry as `~`, and a path
/// under home written from `~` ([`libbeachcomber::path::Home::compact`]).
pub(super) fn shown_path(path: Option<&str>, home: &libbeachcomber::path::Home) -> String {
    path.map_or_else(|| "~".to_string(), |p| home.compact(p))
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

/// The rows `--tree` draws: each value row followed by one row per layer
/// behind it (`children[i]` for `rows[i]`), except the layer that is the row
/// itself. The marks run parallel: `None` for a value row, a [`ChildMark`]
/// for a layer.
pub(super) fn tree_display_rows(
    rows: &[CacheRow],
    children: &[Vec<crate::cache::LayerRecord>],
) -> (Vec<CacheRow>, Vec<Option<ChildMark>>) {
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
    (display, marks)
}
