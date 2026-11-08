//! The `comb status` renderers, as a pipeline: [`row`] builds display rows
//! from cache rows, [`filter`] and [`sort`] select and order them, [`cells`]
//! formats each column, [`table`] lays the human and table presets out, and
//! [`machine`] renders the machine formats (json, csv, tsv, sh, templates).

mod ansi;
mod cells;
mod filter;
mod machine;
mod row;
mod sort;
mod table;
mod text;

use crate::cache::CacheRow;
use crate::cli::output_format::{Format, Preset};

pub use ansi::{bold, dim};
pub use cells::{
    TTL_P_WIDTH_MAX, TTL_TOTAL_WIDTH_MAX, TtlCell, TtlWidths, compute_ttl_widths, format_age,
    format_poll_cell, format_ttl_cell, sanitize_cell,
};
pub use filter::{DEFAULT_FILTERS, apply_filters, effective_filters, provider_could_pass};
pub use machine::{RowRender, render_delimited, render_sh_env, render_template_format};
pub use row::{
    ChildMark, default_depth, display_source, flatten_rows, row_base, row_context, row_facet,
    tree_branch,
};
pub use sort::apply_sort;
pub use table::{render_human, render_tree};
pub use text::pad_cols;

use machine::render_json;
use table::render_table;

/// Resolve the maximum **total table** width.
///
/// - `None` arg → use `terminal_cols` if available, else the default (120).
/// - `Some("auto")` → use `terminal_cols` if available, else the default.
/// - `Some(n)` → parse `n` as a `usize`; anything else is an error.
///
/// The returned value is the cap for the **entire rendered row** (all columns +
/// padding). The renderer shrinks the VALUE and PATH columns as needed to stay
/// within this budget — VALUE first in line, PATH taking what is left — and
/// keeps the other five columns at their natural widths.
pub fn resolve_max_width(arg: Option<&str>, terminal_cols: Option<usize>) -> Result<usize, String> {
    const DEFAULT: usize = 120;
    match arg {
        None | Some("auto") => Ok(terminal_cols.unwrap_or(DEFAULT)),
        Some(s) => s
            .parse()
            .map_err(|_| format!("{s:?} is not a width: use a whole number or 'auto'")),
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
        Format::Preset(Preset::Sh) => render_sh_env(rows),
        Format::Preset(Preset::Table) => render_table(rows, &[], false, None, false),
        Format::Preset(Preset::Human) => {
            let trunc = if opts.no_trunc { None } else { opts.max_width };
            // `no_color` is the resolved color decision from the caller (already
            // accounts for NO_COLOR, TTY and --color). Don't re-AND with is_tty
            // here — that would override an explicit `--color=always`.
            let color = !opts.no_color;
            render_table(rows, &[], color, trunc, opts.ascii)
        }
        Format::Preset(other) => unreachable!("status's formats hold no {}", other.name()),
        Format::Template(_) => {
            unreachable!("a status template renders through render_template_format")
        }
    }
}
