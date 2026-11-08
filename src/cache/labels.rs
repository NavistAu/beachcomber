//! Source labels: which are static, and how a reader sees them.

/// Source label written by the `put` op. A field carrying this label has no
/// Source instance behind it — it is static, stored data (like a leaf seeded
/// from a configuration file, `config:<file>`).
pub const PUT_SOURCE: &str = "put";

/// Prefix on a source label naming a leaf seeded from a configuration file
/// (`config:<file>`, written by [`Cache::reseed_static`]); `Cache::list_rows`
/// checks for this prefix.
pub const CONFIG_SOURCE_PREFIX: &str = "config:";

/// Label of a template leaf's memo: the value `| cache()` stored, in the
/// `<p>:memo` entry with the address's stamp ([`CacheEntry::memos`]), and the
/// keys stored for it (`hash`, `watch`, `poll`, `decay`) in `<p>:cache`. No
/// Source instance is behind it, and the colon keeps any Source from being
/// named it ([`libbeachcomber::key::name_error`]); readers see it as `memo`.
pub const MEMO_SOURCE: &str = "memo:";

/// Whether a source name is a static label — one with no Source instance
/// behind it: `put`, `config:<file>`,
/// `computed:<line>` and a memo's. Everything else names a Source the daemon
/// refreshes.
pub fn is_static_label(source_name: &str) -> bool {
    source_name == PUT_SOURCE
        || source_name == MEMO_SOURCE
        || source_name.starts_with(CONFIG_SOURCE_PREFIX)
        || source_name.starts_with(COMPUTED_PREFIX)
}

/// Prefix on the label of a leaf a provider's `result` or `return` line
/// computed, which no one Source holds: `computed:result`, `computed:return`.
/// Like `put` and `config:<file>` it has no Source behind it.
pub const COMPUTED_PREFIX: &str = "computed:";

/// A record's writer label as every reader sees it — a `:cache` answer's
/// `source`, a `status` row's SOURCE: `source(<name>)` for a Source,
/// `config(<file>)` for a configuration seed, `computed(result)` /
/// `computed(return)` for a leaf a provider line computed, `put` as it is,
/// and `memo` for a template leaf's memo. The function form keeps a Source that happens to be named `put` or
/// `template` from reading as one of those words.
///
/// Storage keeps the short labels (`head`, `config:<file>`,
/// `computed:return`): they key records, lifecycle snapshots and failure
/// state, so only what leaves the daemon is presented.
pub fn presented_label(label: &str) -> String {
    if label == PUT_SOURCE {
        label.to_string()
    } else if label == MEMO_SOURCE {
        "memo".to_string()
    } else if let Some(file) = label.strip_prefix(CONFIG_SOURCE_PREFIX) {
        format!("config({file})")
    } else if let Some(line) = label.strip_prefix(COMPUTED_PREFIX) {
        format!("computed({line})")
    } else {
        format!("source({label})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use crate::cache::test_support::*;

    #[test]
    fn every_label_but_put_is_presented_in_function_form() {
        assert_eq!(presented_label("head"), "source(head)");
        assert_eq!(presented_label("put"), "put");
        assert_eq!(
            presented_label("config:/c/defaults/aws.toml"),
            "config(/c/defaults/aws.toml)"
        );
        assert_eq!(presented_label("computed:return"), "computed(return)");
        assert_eq!(presented_label("computed:result"), "computed(result)");
        // A Source named like a layer word stays a Source.
        assert_eq!(presented_label("template"), "source(template)");
    }
}
