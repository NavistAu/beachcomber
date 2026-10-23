//! `_` prototype lookup.
//!
//! A `_` table is a prototype, never a provider or a Source. For every key a
//! table may hold, its own value wins whole; an absent key takes the nearest
//! `_` of the same kind, whole. A chain is the tables to consult, nearest
//! first: `[own, nearest _, ..., farthest _]`.

/// What kind of table a chain is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `providers.<p>`, then `providers._`.
    Provider,
    /// `providers.<p>.sources.<s>`, then `providers.<p>.sources._`, then
    /// `providers._.sources._`.
    Source,
}

/// The tables that apply to `provider` (or its Source `source`) among
/// `providers` (the `[daemon.providers]` table), nearest first. A table that
/// is absent is skipped, so a chain may start at a `_`. `source` is read for
/// [`Kind::Source`] only. `provider` and `source` are real names: `_` is a
/// prototype, never one of them.
pub fn chain<'t>(
    providers: &'t toml::Table,
    kind: Kind,
    provider: &str,
    source: &str,
) -> Vec<&'t toml::Table> {
    debug_assert!(provider != "_" && source != "_", "`_` is not a name");
    let table = |t: Option<&'t toml::Value>| t.and_then(toml::Value::as_table);
    let own_provider = table(providers.get(provider));
    let abstract_provider = table(providers.get("_"));
    let sources = |p: Option<&'t toml::Table>| p.and_then(|p| table(p.get("sources")));
    let tables = match kind {
        Kind::Provider => vec![own_provider, abstract_provider],
        Kind::Source => vec![
            sources(own_provider).and_then(|s| table(s.get(source))),
            sources(own_provider).and_then(|s| table(s.get("_"))),
            sources(abstract_provider).and_then(|s| table(s.get("_"))),
        ],
    };
    tables.into_iter().flatten().collect()
}

/// The effective value of `key` on the table at `chain[0]`: its own, else the
/// nearest `_`'s, whole. What the value must be (a line, a table of lines) is
/// for the caller to check.
pub fn effective<'t>(chain: &[&'t toml::Table], key: &str) -> Option<&'t toml::Value> {
    chain.iter().find_map(|table| table.get(key))
}

/// Every value of `key` along the chain, nearest first, each with the index
/// of its table in `chain`: the effective one, then each farther table's. A
/// line's `| super` reaches the next of these, and finds out at render time
/// whether there is one; a render failure at level `n` is the table at
/// `super_chain(..)[n].0`.
pub fn super_chain<'t>(chain: &[&'t toml::Table], key: &str) -> Vec<(usize, &'t toml::Value)> {
    chain
        .iter()
        .enumerate()
        .filter_map(|(at, table)| Some((at, table.get(key)?)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(s: &str) -> toml::Table {
        s.parse().unwrap()
    }

    fn lines<'t>(values: &[(usize, &'t toml::Value)]) -> Vec<(usize, &'t str)> {
        values
            .iter()
            .map(|(at, v)| (*at, v.as_str().unwrap()))
            .collect()
    }

    #[test]
    fn own_key_wins_whole() {
        let own = table("return = \"{{ result | poll('5s') }}\"");
        let proto = table("return = \"{{ result | poll('60s') | failback(3, '1s') }}\"");
        assert_eq!(
            effective(&[&own, &proto], "return").and_then(toml::Value::as_str),
            Some("{{ result | poll('5s') }}")
        );
    }

    #[test]
    fn absent_key_takes_the_nearest_underscore() {
        let own = table("root = 'x'");
        let proto = table("return = \"{{ result | poll('60s') }}\"");
        assert_eq!(
            effective(&[&own, &proto], "return").and_then(toml::Value::as_str),
            Some("{{ result | poll('60s') }}")
        );
        assert_eq!(effective(&[&own, &proto], "data"), None);
    }

    #[test]
    fn a_provider_underscore_is_nearer_than_the_abstract_one() {
        let providers = table(
            r#"
            [_.sources._]
            return = "far"
            [git.sources._]
            return = "near"
            [git.sources.status]
            root = "x"
            [mise.sources.tools]
            root = "y"
            "#,
        );
        let git = chain(&providers, Kind::Source, "git", "status");
        assert_eq!(git.len(), 3);
        assert_eq!(
            effective(&git, "return").and_then(toml::Value::as_str),
            Some("near")
        );
        let mise = chain(&providers, Kind::Source, "mise", "tools");
        assert_eq!(
            effective(&mise, "return").and_then(toml::Value::as_str),
            Some("far")
        );
    }

    #[test]
    fn the_effective_value_may_be_a_table() {
        let own = table("[return]\nfield = 'x'");
        let proto = table("return = 'line'");
        assert!(effective(&[&own, &proto], "return").unwrap().is_table());
    }

    #[test]
    fn the_super_chain_is_every_value_nearest_first() {
        let own = table("return = \"{{ result | super }}\"");
        let provider = table("root = 'x'");
        let abstract_ = table("return = \"{{ result | poll('60s') }}\"");
        assert_eq!(
            lines(&super_chain(&[&own, &provider, &abstract_], "return")),
            vec![
                (0, "{{ result | super }}"),
                (2, "{{ result | poll('60s') }}")
            ],
            "the index is the table's place in the chain, skipping the one without the key"
        );
        assert!(super_chain(&[&own, &abstract_], "data").is_empty());
    }

    #[test]
    fn the_super_chain_does_not_guess_at_super_in_a_string() {
        let own = table("return = \"{{ result | watch('a|super') }}\"");
        let proto = table("return = \"{{ result | poll('60s') }}\"");
        assert_eq!(super_chain(&[&own, &proto], "return").len(), 2);
    }
}
