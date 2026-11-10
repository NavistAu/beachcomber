//! `--filter` and the default filters: which rows `comb status` shows.

use super::row::{display_source, row_base, row_facet, shown_path, shown_value};
use crate::cache::CacheRow;

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
/// Apply a list of `key=value` / `key!=value` filter strings to `rows`,
/// returning only the rows that match ALL of them (AND semantics).
///
/// Keys: `provider` (the base name, exact or `*` glob), `facet` (a name or
/// glob, `-` for none), `path` (glob, `-` for none), `field` (glob), `source` (glob
/// over the displayed label), `value` (glob over the displayed value, `-` for
/// absent), `shadowed`, `lifecycle`. `!=` negates.
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
/// in `filters`. Used to skip resolving (and `_.exec()`ing) a template row's
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
    /// The field name, exact or glob.
    Field(String),
    /// The displayed SOURCE label, as a glob.
    Source(String),
    /// The displayed value as a glob, or absent (`-`): a failing Source's
    /// row with nothing stored displays its latest error.
    Value(Option<String>),
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
                let home = libbeachcomber::path::Home::from_env();
                simple_glob_match(pat, &shown_path(r.path.as_deref(), &home))
                    || r.path.as_deref().is_some_and(|p| simple_glob_match(pat, p))
            }
            Predicate::Field(pat) => simple_glob_match(pat, &r.field),
            Predicate::Source(pat) => simple_glob_match(pat, &display_source(r)),
            // Over the VALUE cell: a failing Source's error counts as shown,
            // so the default `value!=-` keeps its row.
            Predicate::Value(None) => shown_value(r).is_none(),
            // A glob matches a present value only: `value=*` is "not absent".
            Predicate::Value(Some(pat)) => {
                shown_value(r).is_some_and(|v| simple_glob_match(pat, &v))
            }
            Predicate::Shadowed(b) => r.shadowed == *b,
            Predicate::LifecycleDecay(target) => matches!(
                &r.kind,
                Some(RowKind::Lifecycle { decay, .. }) if decay == target
            ),
            Predicate::LifecycleOnce => matches!(&r.kind, Some(RowKind::Once)),
            Predicate::LifecycleStatic => matches!(&r.kind, Some(RowKind::Static)),
            Predicate::LifecycleRendered => matches!(&r.kind, Some(RowKind::Rendered)),
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
