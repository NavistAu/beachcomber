use crate::cli::style::Glyphs;
use libbeachcomber::term;

/// `s` padded on the right with spaces to `width` columns.
pub fn pad_cols(s: &str, width: usize) -> String {
    format!("{s}{}", " ".repeat(width.saturating_sub(s.cols())))
}
/// Display columns, as the terminal counts them ([`term::width`]).
pub(super) trait Cols {
    fn cols(&self) -> usize;
}

impl Cols for str {
    fn cols(&self) -> usize {
        term::width(self)
    }
}

/// The longest prefix of `s` that fits `cols` columns.
pub(super) fn take_head(s: &str, cols: usize) -> String {
    term::cut(s, cols, "")
}

/// The longest suffix of `s` that fits `cols` columns.
pub(super) fn take_tail(s: &str, cols: usize) -> String {
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
pub(super) fn truncate_tail(s: &str, max: usize, ell: &str) -> String {
    if s.cols() <= max {
        s.to_string()
    } else {
        let keep = max.saturating_sub(ell.cols());
        format!("{ell}{}", take_tail(s, keep))
    }
}

/// The table's truncation mark: the one-column `…` glyph, or `...` under
/// `--ascii` where the glyph can't be trusted.
pub(super) fn ellipsis(ascii: bool) -> &'static str {
    Glyphs::of(ascii).ellipsis
}

/// Truncate a cache path to at most `max` columns. A `#session` suffix
/// compresses first — the directory locates the row, the session id only
/// discriminates it, so the id keeps its last 4 chars (`~/ws/proj#…abcd`).
/// If the directory alone still overflows, its own tail is kept. A path
/// with no `#` falls back to plain tail truncation.
pub(super) fn truncate_path(s: &str, max: usize, ell: &str) -> String {
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
pub(super) fn truncate_middle(s: &str, max: usize, ell: &str) -> String {
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
pub(super) fn truncate(s: &str, max: usize, ell: &str) -> String {
    if s.cols() <= max {
        s.to_string()
    } else {
        format!("{}{ell}", take_head(s, max.saturating_sub(ell.cols())))
    }
}
