//! The one check of a line before anything runs it: it compiles, and every
//! name it reads is one its context supplies. What a line's context supplies
//! depends on where the line is written, and [`Line::sees`] is the one table
//! of that; what a finding means (a load error, a warning, a refused `put`)
//! is the caller's to say.

use minijinja::Environment;

/// Where a line is written, which decides the names its context supplies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Line {
    /// A client template: a `[client.providers.<p>] template` leaf, or what a
    /// `put` writes to `<p>:template`. The client renders it, and it reads
    /// every provider by name besides.
    Template,
    /// A Source's `root`.
    Root,
    /// A Source's `data`.
    Data,
    /// A Source's `result`, whole or one field's.
    Result,
    /// A provider's `template` line.
    ProviderTemplate,
    /// A provider's `result` line.
    ProviderResult,
    /// A provider's `return` line.
    ProviderReturn,
}

impl Line {
    /// Every kind, in the order the table lists them.
    pub const ALL: [Line; 7] = [
        Line::Template,
        Line::Root,
        Line::Data,
        Line::Result,
        Line::ProviderTemplate,
        Line::ProviderResult,
        Line::ProviderReturn,
    ];

    /// The names a line of this kind reads that its context supplies: the
    /// one table of them. A template also reads each provider by name, which
    /// is no fixed set: its caller adds the providers it knows.
    pub fn sees(self) -> &'static [&'static str] {
        match self {
            Line::Template => &["env", "comb"],
            Line::Root => &["env", "_"],
            Line::Data => &["env", "comb", "sources", "daemon", "_"],
            Line::Result => &["data", "_"],
            Line::ProviderTemplate => &["client", "_"],
            Line::ProviderResult => &["sources", "daemon", "_"],
            Line::ProviderReturn => &["result", "template", "daemon", "_"],
        }
    }
}

/// What is wrong with a line, as a message that carries no value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Finding {
    /// It does not compile, so nothing it says can run.
    Compile(String),
    /// It reads a name its context does not supply.
    Unknown(String),
}

impl std::fmt::Display for Finding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Finding::Compile(message) | Finding::Unknown(message) => f.write_str(message),
        }
    }
}

/// What is wrong with `src` as a line whose context supplies `sees`, on
/// `env`'s functions and filters: that it does not compile, alone, or each
/// name it reads that is not in `sees`, in name order. Nothing is rendered.
pub fn lint(env: &Environment<'_>, src: &str, sees: &[&str]) -> Vec<Finding> {
    match super::undeclared_names(env, src, false) {
        Err(e) => vec![Finding::Compile(e)],
        Ok(names) => names
            .into_iter()
            .filter(|name| !sees.contains(&name.as_str()))
            .map(|name| {
                Finding::Unknown(format!(
                    "unknown name '{name}' (this line sees {})",
                    sees.join(", ")
                ))
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> Environment<'static> {
        crate::engine::build_engine_env(Default::default())
    }

    #[test]
    fn a_line_that_reads_only_what_it_sees_is_clean() {
        assert_eq!(
            lint(
                &env(),
                "{{ env.HOME ~ comb.git.branch | upper }}",
                Line::Template.sees()
            ),
            vec![]
        );
        assert_eq!(lint(&env(), "plain text", &[]), vec![]);
        assert_eq!(
            lint(&env(), "{% set x = 1 %}{{ x }}", &[]),
            vec![],
            "a name the line declares is not one it reads"
        );
    }

    #[test]
    fn each_name_a_line_does_not_see_is_a_finding_in_name_order() {
        let found = lint(&env(), "{{ b.x ~ a ~ env.X }}", &["env"]);
        let shown: Vec<String> = found.iter().map(ToString::to_string).collect();
        assert_eq!(
            shown,
            [
                "unknown name 'a' (this line sees env)",
                "unknown name 'b' (this line sees env)"
            ]
        );
        assert!(found.iter().all(|f| matches!(f, Finding::Unknown(_))));
    }

    #[test]
    fn a_line_that_does_not_compile_is_that_alone() {
        let found = lint(&env(), "{{ nope ", &[]);
        assert!(
            matches!(found.as_slice(), [Finding::Compile(_)]),
            "{found:?}"
        );
        let found = lint(&env(), "a {{ nope b", &[]);
        assert!(
            matches!(found.as_slice(), [Finding::Compile(_)]),
            "{found:?}"
        );
    }

    #[test]
    fn a_function_or_filter_is_not_a_name() {
        assert_eq!(
            lint(
                &env(),
                "{{ lup('.git') ~ ('{}' | fromjson | length) }}",
                &[]
            ),
            vec![]
        );
    }

    /// The one table of what each kind of line reads.
    #[test]
    fn what_each_line_sees() {
        let table: Vec<(Line, &[&str])> = Line::ALL.iter().map(|l| (*l, l.sees())).collect();
        assert_eq!(
            table,
            [
                (Line::Template, &["env", "comb"][..]),
                (Line::Root, &["env", "_"][..]),
                (Line::Data, &["env", "comb", "sources", "daemon", "_"][..]),
                (Line::Result, &["data", "_"][..]),
                (Line::ProviderTemplate, &["client", "_"][..]),
                (Line::ProviderResult, &["sources", "daemon", "_"][..]),
                (
                    Line::ProviderReturn,
                    &["result", "template", "daemon", "_"][..]
                ),
            ]
        );
    }
}
