use super::row::row_context;
use super::text::{Cols, pad_cols};
use crate::cache::CacheRow;
use libbeachcomber::render::shell_quote;

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

pub(super) fn render_json(rows: &[CacheRow]) -> String {
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
const DELIMITED_COLUMNS: [&str; 13] = [
    "PROVIDER",
    "PATH",
    "FIELD",
    "SOURCE",
    "VALUE",
    "AGE_MS",
    "KIND",
    "DECAY",
    "POLL_INTERVAL_SECS",
    "POLL_COUNT",
    "FAILURE_CONSECUTIVE_FAILURES",
    "FAILURE_SUPPRESSED_UNTIL_UNIX_MS",
    "FAILURE_LAST_ERROR",
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
        kind.to_string(),
        decay,
        opt(row.poll_interval_secs.map(|n| n.to_string())),
        opt(row.poll_count.map(|n| n.to_string())),
        opt(row
            .failure
            .as_ref()
            .map(|f| f.consecutive_failures.to_string())),
        opt(row
            .failure
            .as_ref()
            .and_then(|f| f.suppressed_until_unix_ms.map(|n| n.to_string()))),
        opt(row.failure.as_ref().and_then(|f| f.last_error.clone())),
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
            if let Some(e) = &f.last_error {
                out.push_str(&format!("{key}_FAILURE_LAST_ERROR={}\n", shell_quote(e)));
            }
        }
    }
    out
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
