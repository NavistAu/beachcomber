//! The one check of a line before anything runs it: it compiles, its
//! statements are ones its place allows, and every name it reads is one its
//! context supplies. What a line may do and read depends on where the line is
//! written, and [`Line`] is the one table of that; what a finding means (a
//! load error, a warning, a refused `put`) is the caller's to say.

use super::scan::{self, TagKind};
use super::{Import, imports, library};
use minijinja::Environment;

/// Where a line is written, which decides the names its context supplies and
/// whether it may import a macro library.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Line {
    /// A client template: a `[client.providers.<p>] template` leaf, or what a
    /// `put` writes to `<p>:template`. The client renders it, and it reads
    /// every provider by name besides.
    Template,
    /// A macro library: a `[client.providers.<p>] macro.<name>` text, or what
    /// a `put` writes to `<p>:macro.<name>`. It reads what a template reads,
    /// and prints nothing outside its macros.
    Macro,
    /// A Source's `root`.
    Root,
    /// A Source's `data`.
    Data,
    /// A Source's `result`, whole or one field's.
    Result,
    /// A Source's `return`: the decorator render that records its schedule.
    Return,
    /// A provider's `template` line.
    ProviderTemplate,
    /// A provider's `result` line.
    ProviderResult,
    /// A provider's `return` line.
    ProviderReturn,
}

impl Line {
    /// Every kind, in the order the table lists them.
    pub const ALL: [Line; 9] = [
        Line::Template,
        Line::Macro,
        Line::Root,
        Line::Data,
        Line::Result,
        Line::Return,
        Line::ProviderTemplate,
        Line::ProviderResult,
        Line::ProviderReturn,
    ];

    /// The names a line of this kind reads that its context supplies: the
    /// one table of them. A template also reads each provider by name, which
    /// is no fixed set: its caller says which providers it knows
    /// ([`Known::providers`]).
    pub fn sees(self) -> &'static [&'static str] {
        match self {
            Line::Template | Line::Macro => &["env", "comb"],
            Line::Root => &["env", "_"],
            Line::Data => &["env", "comb", "sources", "daemon", "_"],
            Line::Result => &["data", "_"],
            Line::Return => &["result", "_"],
            Line::ProviderTemplate => &["client", "_"],
            Line::ProviderResult => &["sources", "daemon", "_"],
            Line::ProviderReturn => &["result", "template", "daemon", "_"],
        }
    }

    /// Whether the client renders a line of this kind: only it may import a
    /// macro library, and only it reads providers by name.
    fn client(self) -> bool {
        matches!(self, Line::Template | Line::Macro)
    }

    /// Whose line it is, as a refusal names it.
    fn whose(self) -> &'static str {
        match self {
            Line::Template => "a client template",
            Line::Macro => "a macro library",
            Line::Root | Line::Data | Line::Result | Line::Return => "a Source's line",
            Line::ProviderTemplate | Line::ProviderResult | Line::ProviderReturn => {
                "a provider's line"
            }
        }
    }
}

/// What a line's caller knows of the world around it, which the line may
/// read beyond what its kind supplies.
#[derive(Clone, Copy, Debug, Default)]
pub struct Known<'a> {
    /// The providers a client line reads by name.
    pub providers: &'a [String],
    /// The macro libraries some layer declares, by address
    /// (`<p>:macro.<name>`): a client line importing any other is told so.
    pub libraries: &'a [String],
}

/// What is wrong with a line, as a message that carries no value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Finding {
    /// It does not compile, or says something its place refuses, so nothing
    /// it says can run.
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

/// What is wrong with `src` as a `line`, on `env`'s functions and filters,
/// with `known` the world around it: that it does not compile, alone; or each
/// statement its place refuses ([`refused`]), then each library it imports
/// that no layer declares, then each name it reads that its context does not
/// supply, in name order. Nothing is rendered.
pub fn lint(env: &Environment<'_>, src: &str, line: Line, known: &Known<'_>) -> Vec<Finding> {
    let names = match super::undeclared_names(env, src, false) {
        Err(e) => return vec![Finding::Compile(e)],
        Ok(names) => names,
    };
    let mut sees: Vec<&str> = line.sees().to_vec();
    if line.client() {
        sees.extend(known.providers.iter().map(String::as_str));
    }
    let mut found = refused(src, line);
    if line.client() {
        found.extend(imports(src).into_iter().filter_map(|import| match import {
            Import::Named(name)
                if library(&name).is_some_and(|address| !known.libraries.contains(&address)) =>
            {
                Some(Finding::Unknown(format!(
                    "unknown macro library '{name}' (no layer declares it)"
                )))
            }
            _ => None,
        }));
    }
    found.extend(
        names
            .into_iter()
            .filter(|name| !sees.contains(&name.as_str()))
            .map(|name| {
                Finding::Unknown(format!(
                    "unknown name '{name}' (this line sees {})",
                    sees.join(", ")
                ))
            }),
    );
    found
}

/// Each statement of `src` a `line` may not hold, whether or not it compiles:
/// `include`, `extends` and `block` anywhere (text is reused with a macro);
/// an import on a line the daemon renders; on a client's, an import whose
/// name is not a macro library's address written as a string; and in a
/// macro library, anything printed outside its macros.
pub fn refused(src: &str, line: Line) -> Vec<Finding> {
    let mut found: Vec<Finding> = scan::engine_tags(src)
        .into_iter()
        .filter(|tag| tag.kind == TagKind::Statement)
        .filter_map(|tag| match scan::statement(tag.body).0 {
            keyword @ ("include" | "extends" | "block") => Some(Finding::Compile(format!(
                "{{% {keyword} %}} is refused: text is reused with a macro"
            ))),
            _ => None,
        })
        .collect();
    if line == Line::Macro && prints(src) {
        found.push(Finding::Compile(
            "a macro library prints nothing: its text, {{ }} and {% call %} belong inside a \
             {% macro %}"
                .into(),
        ));
    }
    let imports = imports(src);
    if !line.client() {
        if !imports.is_empty() {
            found.push(Finding::Compile(format!(
                "macros are client templates: {} cannot import",
                line.whose()
            )));
        }
        return found;
    }
    for import in imports {
        match import {
            Import::Computed(body) => found.push(Finding::Compile(format!(
                "{{% {body} %}}: an import names its library as a string, \"<p>:macro.<name>\""
            ))),
            Import::Named(name) if library(&name).is_none() => {
                found.push(Finding::Compile(format!(
                    "'{name}' is not a macro library: an import names one \"<p>:macro.<name>\""
                )))
            }
            Import::Named(_) => {}
        }
    }
    found
}

/// Whether `src`, a macro library, prints anything outside its macros: text
/// that is not whitespace, a `{{ }}` or a `{% call %}` where it stands, outside
/// every body that prints nothing there ([`scan::capture`]).
fn prints(src: &str) -> bool {
    let mut depth = 0usize;
    let mut at = 0;
    for tag in scan::engine_tags(src) {
        if depth == 0 && !src[at..tag.start].trim().is_empty() {
            return true;
        }
        at = tag.end;
        match tag.kind {
            TagKind::Expression if depth == 0 => return true,
            TagKind::Statement => {
                if depth == 0 && scan::statement(tag.body).0 == "call" {
                    return true;
                }
                depth = scan::capture(tag.body).depth(depth);
            }
            TagKind::Comment | TagKind::Expression => {}
        }
    }
    depth == 0 && !src[at..].trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> Environment<'static> {
        crate::engine::build_engine_env(Default::default())
    }

    /// `src` linted as a `line` that knows of no provider.
    fn lint_as(src: &str, line: Line) -> Vec<Finding> {
        lint(&env(), src, line, &Known::default())
    }

    fn compile_findings(src: &str, line: Line) -> Vec<String> {
        lint_as(src, line)
            .into_iter()
            .filter_map(|f| match f {
                Finding::Compile(m) => Some(m),
                Finding::Unknown(_) => None,
            })
            .collect()
    }

    #[test]
    fn a_line_that_reads_only_what_it_sees_is_clean() {
        assert_eq!(
            lint_as("{{ env.HOME ~ comb.git.branch | upper }}", Line::Template),
            vec![]
        );
        assert_eq!(lint_as("plain text", Line::Result), vec![]);
        assert_eq!(
            lint_as("{% set x = 1 %}{{ x }}", Line::Result),
            vec![],
            "a name the line declares is not one it reads"
        );
    }

    #[test]
    fn each_name_a_line_does_not_see_is_a_finding_in_name_order() {
        let found = lint_as("{{ b.x ~ a ~ env.X }}", Line::Root);
        let shown: Vec<String> = found.iter().map(ToString::to_string).collect();
        assert_eq!(
            shown,
            [
                "unknown name 'a' (this line sees env, _)",
                "unknown name 'b' (this line sees env, _)"
            ]
        );
        assert!(found.iter().all(|f| matches!(f, Finding::Unknown(_))));
    }

    /// A client template reads the providers its caller knows by name; no
    /// other line does.
    #[test]
    fn a_template_reads_the_providers_its_caller_knows() {
        let providers = ["git".to_string()];
        let known = Known {
            providers: &providers,
            ..Known::default()
        };
        assert_eq!(
            lint(&env(), "{{ git.branch }}", Line::Template, &known),
            vec![]
        );
        assert!(matches!(
            lint(&env(), "{{ git.branch }}", Line::Data, &known).as_slice(),
            [Finding::Unknown(_)]
        ));
    }

    #[test]
    fn a_line_that_does_not_compile_is_that_alone() {
        for src in ["{{ nope ", "a {{ nope b", "{% include 'x' %}{{ nope "] {
            let found = lint_as(src, Line::Template);
            assert!(
                matches!(found.as_slice(), [Finding::Compile(_)]),
                "{src}: {found:?}"
            );
        }
    }

    #[test]
    fn a_function_or_filter_is_not_a_name() {
        assert_eq!(
            lint_as(
                "{{ lup('.git') ~ ('{}' | fromjson | length) }}",
                Line::Result
            ),
            vec![]
        );
        // Every item of the library is a call, whatever its tier: `stat` is no name.
        for line in [Line::Root, Line::Data, Line::Result, Line::Template] {
            assert_eq!(
                lint_as("{{ ('x' | stat).size ~ ('*' | glob | length) }}", line),
                vec![],
                "{line:?}"
            );
        }
    }

    /// The one table of what each kind of line reads.
    #[test]
    fn what_each_line_sees() {
        let table: Vec<(Line, &[&str])> = Line::ALL.iter().map(|l| (*l, l.sees())).collect();
        assert_eq!(
            table,
            [
                (Line::Template, &["env", "comb"][..]),
                (Line::Macro, &["env", "comb"][..]),
                (Line::Root, &["env", "_"][..]),
                (Line::Data, &["env", "comb", "sources", "daemon", "_"][..]),
                (Line::Result, &["data", "_"][..]),
                (Line::Return, &["result", "_"][..]),
                (Line::ProviderTemplate, &["client", "_"][..]),
                (Line::ProviderResult, &["sources", "daemon", "_"][..]),
                (
                    Line::ProviderReturn,
                    &["result", "template", "daemon", "_"][..]
                ),
            ]
        );
    }

    /// There is one way to reuse text, a macro: `include`, `extends` and
    /// `block` are compile findings on every line, though the engine parses
    /// them.
    #[test]
    fn include_extends_and_block_are_refused_everywhere() {
        for (src, keyword) in [
            ("{% include 'p:macro.x' %}", "include"),
            ("{% extends 'p:macro.x' %}", "extends"),
            ("{%- block b -%}{%- endblock -%}", "block"),
        ] {
            assert!(env().template_from_str(src).is_ok(), "{src} parses");
            for line in Line::ALL {
                assert_eq!(
                    compile_findings(src, line),
                    [format!(
                        "{{% {keyword} %}} is refused: text is reused with a macro"
                    )],
                    "{src} as {line:?}"
                );
            }
        }
        for src in [
            "{% raw %}{% include 'x' %}{% endraw %}",
            "{# {% include 'x' %} #}",
            "{{ 'include' }}{% set block = 1 %}{{ block }}",
        ] {
            assert_eq!(
                compile_findings(src, Line::Template),
                Vec::<String>::new(),
                "{src}"
            );
        }
    }

    /// A macro defined and called within one text runs on every surface that
    /// prints (a library prints nothing, below); importing a library is the
    /// client's alone.
    #[test]
    fn an_import_is_refused_in_a_daemon_line() {
        let inline =
            "{% macro m(x) %}<{{ x }}>{% endmacro %}{% call m(1) %}{% endcall %}{{ m(2) }}";
        for line in Line::ALL.into_iter().filter(|l| *l != Line::Macro) {
            assert_eq!(
                compile_findings(inline, line),
                Vec::<String>::new(),
                "{line:?}"
            );
        }
        for src in [
            "{% import 'p:macro.x' as x %}{{ x.m() }}",
            "{% from \"p:macro.x\" import m %}{{ m() }}",
            "{% import name as x %}",
        ] {
            for (line, whose) in [
                (Line::Root, "a Source's line"),
                (Line::Data, "a Source's line"),
                (Line::Result, "a Source's line"),
                (Line::Return, "a Source's line"),
                (Line::ProviderTemplate, "a provider's line"),
                (Line::ProviderResult, "a provider's line"),
                (Line::ProviderReturn, "a provider's line"),
            ] {
                assert_eq!(
                    compile_findings(src, line),
                    [format!(
                        "macros are client templates: {whose} cannot import"
                    )],
                    "{src} as {line:?}"
                );
            }
        }
    }

    /// A client template imports a library by its address, written as a
    /// string: never a computed name, never an address that is not one.
    #[test]
    fn a_template_import_names_a_macro_library_literally() {
        for src in [
            "{% import 'scanline:macro.rate' as rate %}{{ rate.row() }}",
            "{% from \"scanline:macro.usage\" import usage %}{{ usage() }}",
            "{%- from 'p:macro.x' import a as b, c with context -%}{{ b() ~ c() }}",
            "{% import \"p:macro.x\" as x without context %}",
            r#"{% import "p:macro\u002ex" as x %}"#,
        ] {
            assert_eq!(
                compile_findings(src, Line::Template),
                Vec::<String>::new(),
                "{src}"
            );
        }
        for (src, why) in [
            (
                "{% import name as x %}",
                "{% import name as x %}: an import names its library as a string, \
                 \"<p>:macro.<name>\"",
            ),
            (
                "{% from 'p:macro.' ~ n import m %}",
                "{% from 'p:macro.' ~ n import m %}: an import names its library as a string, \
                 \"<p>:macro.<name>\"",
            ),
            (
                "{% import 'p:template.x' as x %}",
                "'p:template.x' is not a macro library: an import names one \"<p>:macro.<name>\"",
            ),
            (
                "{% import 'p:macro' as x %}",
                "'p:macro' is not a macro library: an import names one \"<p>:macro.<name>\"",
            ),
            (
                "{% import 'p:macro.a.b' as x %}",
                "'p:macro.a.b' is not a macro library: an import names one \"<p>:macro.<name>\"",
            ),
            (
                "{% import 'env:macro.a' as x %}",
                "'env:macro.a' is not a macro library: an import names one \"<p>:macro.<name>\"",
            ),
        ] {
            assert_eq!(compile_findings(src, Line::Template), [why], "{src}");
        }
    }

    /// A macro library defines macros: whitespace, statements and what a
    /// macro or a block `{% set %}` holds print nothing; text, a `{{ }}` or a
    /// `{% call %}` outside them would.
    #[test]
    fn a_macro_library_prints_nothing() {
        for src in [
            "",
            "\n{%- macro a(x) -%}\n  <{{ x }}>{{ caller() if caller }}\n{%- endmacro -%}\n",
            "{% set w = env.W | int %}{% set ns = namespace(d={}) %}\
             {% for n in ['a', 'b'] %}{% set ns.d = ns.d | merge({n: w}) %}{% endfor %}\
             {% macro m() %}{% macro inner() %}i{% endmacro %}{{ inner() }}{% endmacro %}",
            "{% set block %}captured {{ 1 }}{% endset %}{% set x | upper %}y{% endset %}",
            "{% set eq = (1 == 1) %}{% set ne = 1 != 2 %}{% set s = 'a=b' %}",
            "{% from 'p:macro.x' import a %}{% import 'p:macro.y' as y %}{# note #}",
        ] {
            assert_eq!(
                compile_findings(src, Line::Macro),
                Vec::<String>::new(),
                "{src}"
            );
        }
        let prints = "a macro library prints nothing: its text, {{ }} and {% call %} belong \
                      inside a {% macro %}";
        for src in [
            "text",
            "{% macro m() %}{% endmacro %} trailing",
            "{{ 1 }}",
            "{% macro m() %}{% endmacro %}{% call m() %}{% endcall %}",
            "{% if true %}x{% endif %}",
            "{% raw %}{{ x }}{% endraw %}",
        ] {
            assert_eq!(compile_findings(src, Line::Macro), [prints], "{src}");
            assert_eq!(
                compile_findings(src, Line::Template),
                Vec::<String>::new(),
                "a template prints: {src}"
            );
        }
    }

    /// A client line importing a library no layer declares is told so, as
    /// an unknown finding: a later `put` may declare it.
    #[test]
    fn an_import_of_a_library_no_layer_declares_is_unknown() {
        let libraries = ["p:macro.x".to_string()];
        let known = Known {
            libraries: &libraries,
            ..Known::default()
        };
        let src = "{% from \"p:macro['x']\" import a %}{% from 'p:macro.y' import b %}\
                   {% macro c() %}{{ a() ~ b() }}{% endmacro %}";
        for line in [Line::Template, Line::Macro] {
            let found = lint(&env(), src, line, &known);
            assert_eq!(
                found,
                [Finding::Unknown(
                    "unknown macro library 'p:macro.y' (no layer declares it)".into()
                )],
                "{line:?}"
            );
        }
    }
}
