use super::ansi;
use super::text::Cols;
use crate::cache::CacheRow;
use crate::cli::style::Glyphs;
use libbeachcomber::render;
use std::time::Duration;

// ---------------------------------------------------------------------------
// TTL cell formatter
// ---------------------------------------------------------------------------

pub struct TtlCell {
    pub text: String,
    pub color: Option<ansi::Color>,
}

/// Maximum width for the raw P-seconds sub-field (`{P}s×{KK}`). 6 digits
/// covers ~11 days of seconds; beyond that we clamp rather than grow the cell.
pub const TTL_P_WIDTH_MAX: usize = 6;
const TTL_P_MAX: u64 = 999_999;

/// Maximum width for the humanized total-budget sub-field (`P × K`, e.g.
/// `99d23h`). Values are clamped before humanizing so the cell can't grow
/// past this.
pub const TTL_TOTAL_WIDTH_MAX: usize = 6;
/// Clamp applied to the total-budget seconds (`poll_interval_secs *
/// poll_count`) before humanizing, so the formatted string can never
/// exceed `TTL_TOTAL_WIDTH_MAX` chars (`99d23h`).
const TTL_TOTAL_MAX_SECS: u64 = 8_639_999;

/// Clamp applied to the POLL column's seconds (next-poll or next-retry)
/// before display, ~11 days (`11d13h`). The POLL column otherwise auto-sizes
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
pub(super) fn format_duration_compact(secs: u64) -> String {
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
pub fn format_ttl_cell(
    kind: Option<&crate::cache::RowKind>,
    poll_interval_secs: Option<u64>,
    poll_count: Option<u32>,
    failure: Option<&crate::cache::FailureSnapshot>,
    ascii: bool,
    widths: &TtlWidths,
) -> TtlCell {
    use crate::cache::RowKind;

    // Watches-files indicator: `dot` for an entry that watches its files
    // while it is Active.
    let g = Glyphs::of(ascii);
    let (star, warn, times, dot) = (g.star, g.warn, g.times, g.dot);

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
            // Poll-only sources render a blank cell trailer.
            let indicator = if *watches_files { dot } else { " " };

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
/// The glyph that marks a rendered row's VALUE cell when colour is off.
///
/// A rendered row is computed here, from this shell's environment and
/// directory — it is not what the daemon holds. Colour says so with Cyan;
/// without colour the cell needs a mark of its own, so the two builds carry
/// the same information.
pub(super) fn template_marker(
    kind: Option<&crate::cache::RowKind>,
    color_enabled: bool,
    ascii: bool,
) -> String {
    if color_enabled || !matches!(kind, Some(crate::cache::RowKind::Rendered)) {
        return String::new();
    }
    format!("{} ", Glyphs::of(ascii).rendered)
}
/// Visible width of the taint marker prefixed to a sanitized row's VALUE cell.
pub(super) const MARKER_WIDTH: usize = 2;

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
pub(super) fn highlight_json(s: &str) -> String {
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

/// The VALUE cell with colour on. Truncation has already happened, so this
/// can only add escape sequences, never slice one. A taint marker (`tainted`,
/// the first [`MARKER_WIDTH`] chars) keeps its own yellow so a rewritten row
/// stands out; a structured body gets JSON syntax highlighting.
///
/// Cyan is the colour build's mark for a rendered row (design decision 5). A
/// structured body still gets its own JSON highlighting nested inside; each
/// highlighted token carries a full reset, so on-screen only the cell's
/// opening brace stays cyan — the row is marked by that lead-in plus its
/// SOURCE, not by an unbroken cyan span.
pub(super) fn colour_value(row: &CacheRow, value: String, tainted: bool) -> String {
    let (marker_text, body): (String, String) = if tainted {
        (
            value.chars().take(MARKER_WIDTH).collect(),
            value.chars().skip(MARKER_WIDTH).collect(),
        )
    } else {
        (String::new(), value)
    };
    let structured = |body: String| {
        if body.starts_with(['{', '[']) {
            highlight_json(&body)
        } else {
            body
        }
    };
    let failed = row.failure.as_ref().is_some_and(|f| f.last_error.is_some());
    let body = if row.value.is_null() && failed {
        ansi::wrap(&body, ansi::Color::Red, true)
    } else if row.value.is_null() {
        ansi::wrap(&body, ansi::Color::Dim, true)
    } else if matches!(row.kind, Some(crate::cache::RowKind::Rendered)) {
        ansi::wrap(&structured(body), ansi::Color::Cyan, true)
    } else {
        structured(body)
    };
    if marker_text.is_empty() {
        body
    } else {
        format!(
            "{}{body}",
            ansi::wrap(&marker_text, ansi::Color::Yellow, true)
        )
    }
}

/// Whether any provider-controlled text in this row contains a control
/// character, and so will be rewritten by [`sanitize_cell`].
pub(super) fn row_is_tainted(row: &CacheRow, path: &str, value: &str) -> bool {
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
pub(super) fn format_age_with_polls(
    age_ms: Option<u128>,
    polls_elapsed: Option<u32>,
    ascii: bool,
) -> String {
    let Some(age_ms) = age_ms else {
        return "-".to_string();
    };
    let base = format_age(age_ms);
    match polls_elapsed {
        Some(n) => format!("{base}{}{n}", Glyphs::of(ascii).times),
        None => base,
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
            None,
            false,
            &W6,
        );
        assert_eq!(cell.text, "\u{2605}     60s\u{00d7}12 (   12m) \u{2219}");
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
            None,
            true,
            &W6,
        );
        assert_eq!(cell.text, "*     60sx12 (   12m) -");
    }

    #[test]
    fn indicator_blank_when_no_watch_capability() {
        // Poll-only provider: blank trailer.
        let cell = format_ttl_cell(
            Some(&RowKind::Lifecycle {
                decay: 0,
                depth: 4,
                watches_files: false,
            }),
            Some(60),
            Some(12),
            None,
            false,
            &W6,
        );
        assert!(cell.text.ends_with(")  "), "no indicator: {:?}", cell.text);
    }

    #[test]
    fn indicator_dot_when_watches_files() {
        // Watches files while Active: the dot.
        let cell = format_ttl_cell(
            Some(&RowKind::Lifecycle {
                decay: 0,
                depth: 4,
                watches_files: true,
            }),
            Some(60),
            Some(12),
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
        let cell = format_ttl_cell(Some(&RowKind::Once), None, None, None, false, &W6);
        assert_eq!(cell.text, "---");
        assert!(cell.color.is_none());
    }

    #[test]
    fn static_renders_dashes() {
        let cell = format_ttl_cell(Some(&RowKind::Static), None, None, None, false, &W6);
        assert_eq!(cell.text, "---");
    }

    #[test]
    fn transient_renders_dashes() {
        let cell = format_ttl_cell(Some(&RowKind::Transient), None, None, None, false, &W6);
        assert_eq!(cell.text, "---");
    }

    #[test]
    fn template_renders_dashes() {
        // A rendered row has no lifecycle of its own — it is computed at
        // read time and never cached.
        let cell = format_ttl_cell(Some(&RowKind::Rendered), None, None, None, false, &W6);
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
            last_error: None,
        };
        let cell = format_ttl_cell(
            Some(&RowKind::Lifecycle {
                decay: 1,
                depth: 4,
                watches_files: true,
            }),
            Some(60),
            Some(12),
            Some(&snap),
            false,
            &W6,
        );
        assert_eq!(cell.text, "\u{26a0} #03 \u{2219}");
        assert!(matches!(cell.color, Some(ansi::Color::Red)));
    }

    #[test]
    fn failure_ascii_fallback() {
        let snap = FailureSnapshot {
            consecutive_failures: 3,
            suppressed_until_unix_ms: None,
            last_error: None,
        };
        let cell = format_ttl_cell(
            Some(&RowKind::Lifecycle {
                decay: 1,
                depth: 4,
                watches_files: true,
            }),
            Some(60),
            Some(12),
            Some(&snap),
            true,
            &W6,
        );
        assert!(cell.text.starts_with("!"));
    }

    #[test]
    fn total_clamps_at_max() {
        // P*K clamps to TTL_TOTAL_MAX_SECS (99d23h) before humanizing; the
        // raw P itself clamps to TTL_P_MAX (999999).
        let cell = format_ttl_cell(
            Some(&RowKind::Lifecycle {
                decay: 0,
                depth: 4,
                watches_files: false,
            }),
            Some(9_999_999),
            Some(99),
            None,
            false,
            &W6,
        );
        assert!(cell.text.contains("99d23h"), "got: {:?}", cell.text);
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
            None,
            false,
            &widths,
        );
        assert_eq!(cell.text, "\u{2605}  1s\u{00d7}00 (0s) \u{2219}");
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
            kind: Some(RowKind::Lifecycle {
                decay: 0,
                depth: 4,
                watches_files: false,
            }),
            poll_interval_secs: Some(poll),
            poll_count: Some(keep),
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
            last_error: None,
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
            last_error: None,
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
            "11d13h"
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
