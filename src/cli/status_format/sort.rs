use super::row::{row_base, row_facet};
use crate::cache::CacheRow;

/// Sort `rows` by the given column name (ascending, stable).
///
/// Valid columns: `provider`, `path`, `field`, `value`, `age`, `lifecycle`,
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
