//! Origins: who wrote a record, and how a reader sees that.

use std::sync::Arc;

/// Who wrote a record: a Source of the namespace's provider, or one of the
/// static writers with no Source instance behind it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Origin {
    /// A Source of the provider, by name (`head`), which the daemon refreshes.
    Source(Arc<str>),
    /// The `put` op.
    Put,
    /// A template leaf's memo: the value `| cache()` stored, in the `<p>:memo`
    /// entry with the address's stamp ([`CacheEntry::memos`]), and the keys
    /// stored for it (`hash`, `watch`, `poll`, `decay`) in `<p>:cache`.
    ///
    /// [`CacheEntry::memos`]: super::CacheEntry::memos
    Memo,
    /// A leaf seeded from a configuration file, by the file as shown
    /// ([`Cache::reseed_static`](super::Cache::reseed_static)).
    Config(Arc<str>),
    /// A leaf a provider's `result` or `return` line computed, which no one
    /// Source holds; never stored.
    Computed(Arc<str>),
}

impl Origin {
    /// The Source named `name`.
    pub fn source(name: &str) -> Self {
        Origin::Source(Arc::from(name))
    }

    /// Whether no Source instance is behind it: `put`, a configuration seed,
    /// a memo and a computed leaf. Everything else names a Source the daemon
    /// refreshes.
    pub fn is_static(&self) -> bool {
        !matches!(self, Origin::Source(_))
    }

    /// The Source's name, for a Source.
    pub fn source_name(&self) -> Option<&str> {
        match self {
            Origin::Source(name) => Some(name),
            _ => None,
        }
    }

    /// The Source's name, to look a Source up by: `""` for a static writer,
    /// which names no Source.
    pub fn name(&self) -> &str {
        self.source_name().unwrap_or_default()
    }

    /// The writer as every reader sees it — a `:cache` answer's `source`, a
    /// `status` row's SOURCE, a `layers` label: `source(<name>)` for a Source,
    /// `config(<file>)` for a configuration seed, `computed(result)` /
    /// `computed(return)` for a leaf a provider line computed, `put` as it
    /// is, and `memo` for a template leaf's memo. The function form keeps a
    /// Source that happens to be named `put` or `template` from reading as
    /// one of those words.
    pub fn presented_label(&self) -> String {
        match self {
            Origin::Source(name) => format!("source({name})"),
            Origin::Put => "put".to_string(),
            Origin::Memo => "memo".to_string(),
            Origin::Config(file) => format!("config({file})"),
            Origin::Computed(line) => format!("computed({line})"),
        }
    }
}

/// The short form logs and lifecycle snapshot ids use: the Source's name,
/// `put`, `memo:`, `config:<file>`, `computed:<line>`. The colons keep any
/// Source from being named a static writer's form
/// ([`libbeachcomber::key::name_error`]), bar `put`.
impl std::fmt::Display for Origin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Origin::Source(name) => f.write_str(name),
            Origin::Put => f.write_str("put"),
            Origin::Memo => f.write_str("memo:"),
            Origin::Config(file) => write!(f, "config:{file}"),
            Origin::Computed(line) => write!(f, "computed:{line}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_origin_but_put_is_presented_in_function_form() {
        assert_eq!(Origin::source("head").presented_label(), "source(head)");
        assert_eq!(Origin::Put.presented_label(), "put");
        assert_eq!(Origin::Memo.presented_label(), "memo");
        assert_eq!(
            Origin::Config("/c/defaults/aws.toml".into()).presented_label(),
            "config(/c/defaults/aws.toml)"
        );
        assert_eq!(
            Origin::Computed("return".into()).presented_label(),
            "computed(return)"
        );
        assert_eq!(
            Origin::Computed("result".into()).presented_label(),
            "computed(result)"
        );
        // A Source named like a layer word stays a Source.
        assert_eq!(
            Origin::source("template").presented_label(),
            "source(template)"
        );
        assert_eq!(Origin::source("put").presented_label(), "source(put)");
    }
}
