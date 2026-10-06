//! Classification, reference discovery and the template render for value
//! expressions — `libbeachcomber::eval`, the primitives `resolve` is built on.
//!
//! Pins canon `field_resolution.md` invariant 14: a value expression written as
//! exactly one `{{ expr }}` keeps the expression's natural type; one written
//! with literal text or more than one tag is a string-valued template. A
//! source with no tags at all is its own literal text — there is no
//! bare-expression form. Evaluation against fetched data is `tests/suite/resolve.rs`.

use libbeachcomber::eval::{
    Form, TagKind, classify, discover_refs, render_template, scan_tags, single_tag_expression,
};
use libbeachcomber::virtual_fields::Ref;
use serde_json::{Value, json};
use std::collections::HashSet;

fn refs_set(src: &str) -> HashSet<Ref> {
    discover_refs(src).into_iter().collect()
}

#[test]
fn classify_no_tags_is_template() {
    assert_eq!(classify("hello"), Form::Template);
    assert_eq!(classify("env.A or 'x'"), Form::Template);
    assert_eq!(classify("  git.branch  "), Form::Template);

    assert_eq!(single_tag_expression("hello"), None);
    assert_eq!(discover_refs("env.A or cache.git.branch"), vec![]);
}

#[test]
fn classify_empty_source_is_template() {
    // An empty source is a template of no tags, rendering to "" — not an
    // expression, which would make `-f ''` a compile error.
    assert_eq!(classify(""), Form::Template);
    assert_eq!(classify("   "), Form::Template);
    assert_eq!(classify("\n\t "), Form::Template);

    assert_eq!(single_tag_expression(""), None);
    assert_eq!(discover_refs(""), vec![]);
}

#[test]
fn classify_single_tag_variants() {
    // Plain.
    assert_eq!(classify("{{ git.branch }}"), Form::SingleTag);
    assert_eq!(
        single_tag_expression("{{ git.branch }}"),
        Some("git.branch")
    );

    // Whitespace-control markers are trimmed off the expression — both spellings.
    assert_eq!(classify("{{- git.branch -}}"), Form::SingleTag);
    assert_eq!(
        single_tag_expression("{{- git.branch -}}"),
        Some("git.branch")
    );
    assert_eq!(classify("{{+ git.branch +}}"), Form::SingleTag);
    assert_eq!(
        single_tag_expression("{{+ git.branch +}}"),
        Some("git.branch")
    );

    // Surrounding whitespace does not make it a template.
    assert_eq!(classify("  {{ git.branch }}\n"), Form::SingleTag);
    assert_eq!(
        single_tag_expression("  {{ git.branch }}\n"),
        Some("git.branch")
    );

    // The scanner respects string literals: the `}}` inside the string is not
    // the closing delimiter, so this is one tag spanning the whole source.
    assert_eq!(classify(r#"{{ "}}" }}"#), Form::SingleTag);
    assert_eq!(single_tag_expression(r#"{{ "}}" }}"#), Some(r#""}}""#));

    // Single-quoted strings too.
    assert_eq!(classify("{{ '}}' }}"), Form::SingleTag);
    assert_eq!(single_tag_expression("{{ '}}' }}"), Some("'}}'"));

    // A backslash-escaped quote does not end the string, so the `}}` after it
    // is still inside the literal.
    assert_eq!(classify(r#"{{ "a\"}}" }}"#), Form::SingleTag);
    assert_eq!(
        single_tag_expression(r#"{{ "a\"}}" }}"#),
        Some(r#""a\"}}""#)
    );

    // `}}` closes an expression tag only at bracket depth zero, so a dict or
    // list literal butted up against the closer is still one tag.
    assert_eq!(classify(r#"{{ {"a": 1}}}"#), Form::SingleTag);
    assert_eq!(
        single_tag_expression(r#"{{ {"a": 1}}}"#),
        Some(r#"{"a": 1}"#)
    );
    assert_eq!(classify("{{ [1, 2] }}"), Form::SingleTag);
    assert_eq!(single_tag_expression("{{ [1, 2] }}"), Some("[1, 2]"));

    let tags = scan_tags(r#"{{ "}}" }}"#);
    assert_eq!(tags.len(), 1);
    assert_eq!(tags[0].kind, TagKind::Expression);
    assert_eq!((tags[0].start, tags[0].end), (0, 10));
    assert_eq!(tags[0].body, r#""}}""#);
}

#[test]
fn classify_unterminated_marker_is_template() {
    // The scanner stops at an unterminated marker and hands the source to the
    // template compiler, whose diagnostic names the unclosed block — rather
    // than to the expression compiler, which would complain about a stray `{`.
    assert_eq!(classify("on {{ git.branch"), Form::Template);
    assert_eq!(classify("{{ x"), Form::Template);
    assert_eq!(classify("{% if x"), Form::Template);
    assert_eq!(classify("{# note"), Form::Template);

    assert_eq!(single_tag_expression("{{ x"), None);
    assert!(scan_tags("on {{ git.branch").is_empty());

    // A lone `{` opens no tag, and with no tags at all the source is literal
    // text.
    assert_eq!(classify("a { b"), Form::Template);

    // A stray closer drives the scanner's bracket depth negative, and a negative
    // depth never closes: the `}}` two bytes on is a close MiniJinja does not
    // accept, so treating it as one would hand a broken source to the
    // expression compiler. Unterminated instead — the template compiler reports
    // "unexpected `}`".
    assert_eq!(classify("{{ x } }}"), Form::Template);
    assert_eq!(single_tag_expression("{{ x } }}"), None);
    assert!(scan_tags("{{ x } }}").is_empty());
}

#[test]
fn scan_tags_respects_strings_in_statement_tags() {
    // The `%}` inside the string literal does not close the statement tag.
    let tags = scan_tags(r#"{% if x == "%}" %}y{% endif %}"#);
    assert_eq!(tags.len(), 2);
    assert_eq!(tags[0].kind, TagKind::Statement);
    assert_eq!(tags[0].body, r#"if x == "%}""#);
    assert_eq!(tags[1].kind, TagKind::Statement);
    assert_eq!(tags[1].body, "endif");
}

#[test]
fn classify_template_variants() {
    // A comment tag alongside an expression tag.
    assert_eq!(classify("{# note #}{{ git.branch }}"), Form::Template);
    // More than one expression tag.
    assert_eq!(classify("{{ git.branch }}{{ git.dirty }}"), Form::Template);
    // Literal text around the tag.
    assert_eq!(classify("on {{ git.branch }}"), Form::Template);
    assert_eq!(classify("{{ git.branch }} tail"), Form::Template);
    // A statement tag is never the single-tag form.
    assert_eq!(classify("{% if git.dirty %}*{% endif %}"), Form::Template);

    for src in [
        "{# note #}{{ git.branch }}",
        "{{ git.branch }}{{ git.dirty }}",
        "on {{ git.branch }}",
    ] {
        assert_eq!(single_tag_expression(src), None, "src: {src}");
    }

    let tags = scan_tags("{# note #}{{ git.branch }}");
    assert_eq!(tags.len(), 2);
    assert_eq!(tags[0].kind, TagKind::Comment);
    assert_eq!(tags[0].body, "note");
    assert_eq!(tags[1].kind, TagKind::Expression);
    assert_eq!(tags[1].body, "git.branch");
}

#[test]
fn discover_refs_template_matches_single_tag_form() {
    let expected: HashSet<Ref> = [Ref::Resolved("git".into(), "branch".into())]
        .into_iter()
        .collect();
    assert_eq!(refs_set("{{ git.branch }}"), expected);
    // The template form finds the same refs as the single-tag form.
    assert_eq!(refs_set("on {{ git.branch }}"), expected);
}

#[test]
fn discover_refs_template_with_statement_tag() {
    let refs = refs_set("{% if git.dirty %}*{% endif %}");
    assert!(
        refs.contains(&Ref::Resolved("git".into(), "dirty".into())),
        "expected git.dirty in {refs:?}"
    );

    // All four ref kinds are classified the same way in a template as in an
    // expression, and `cwd` / bare names are ignored.
    let refs = refs_set("{% if env.A %}{{ cache.c.z }}{{ cache.p }}{{ cwd }}{{ bare }}{% endif %}");
    let expected: HashSet<Ref> = [
        Ref::Env("A".into()),
        Ref::CacheField("c".into(), "z".into()),
        Ref::CacheProvider("p".into()),
    ]
    .into_iter()
    .collect();
    assert_eq!(refs, expected);
}

#[test]
fn discover_refs_order_is_deterministic() {
    // The underlying analyses return a HashSet, so without the sort in
    // discover_refs five runs give five orders.
    let expected = vec![
        Ref::Resolved("a".into(), "b".into()),
        Ref::Resolved("c".into(), "d".into()),
        Ref::Resolved("e".into(), "f".into()),
    ];
    assert_eq!(discover_refs("{{ a.b or c.d or e.f }}"), expected);

    // Variants sort in declaration order: Env < CacheField < CacheProvider < Resolved.
    assert_eq!(
        discover_refs("{{ z.f or cache.p or cache.c.z or env.A }}"),
        vec![
            Ref::Env("A".into()),
            Ref::CacheField("c".into(), "z".into()),
            Ref::CacheProvider("p".into()),
            Ref::Resolved("z".into(), "f".into()),
        ]
    );
}

#[test]
fn discover_refs_uncompilable_template_yields_no_refs() {
    // The compile error surfaces at evaluation time, not from discovery.
    assert_eq!(discover_refs("on {{ nonsense ??? }}"), vec![]);
}

#[test]
fn discover_refs_keeps_every_segment_of_a_resolved_ref() {
    // A ref names a node at any depth, never the group above it: `a.b` and
    // `a.b.c` are two addresses, so they are two refs. Only `env.*` and
    // `cache.*` cut at a fixed depth — `env.A.B` is the variable `A` with
    // attribute navigation into it, and `cache.P.F.G` is the cached field
    // `P.F` — so those collapse.
    assert_eq!(
        discover_refs("{{ a.b }}{{ a.b.c }}"),
        vec![
            Ref::Resolved("a".into(), "b".into()),
            Ref::Resolved("a".into(), "b.c".into()),
        ]
    );
    assert_eq!(
        discover_refs("{{ env.A }}{{ env.A.B }}"),
        vec![Ref::Env("A".into())]
    );
    assert_eq!(
        discover_refs("{{ cache.p.f }}{{ cache.p.f.g }}"),
        vec![Ref::CacheField("p".into(), "f".into())]
    );
    // And the same ref twice is one ref.
    assert_eq!(
        discover_refs("{{ a.b.c }}{{ a.b.c }}"),
        vec![Ref::Resolved("a".into(), "b.c".into())]
    );
}

// ── render_template over an already-assembled context ─────────────────────────

#[test]
fn render_template_over_assembled_context() {
    let ctx = json!({"git": {"branch": "main", "dirty": true, "tag": Value::Null}});

    assert_eq!(
        render_template("{{ git.branch }}{% if git.dirty %}*{% endif %}", &ctx).unwrap(),
        "main*"
    );
    // A null in the assembled context renders as nothing — not the literal
    // word `none`. The same "missing is falsy" reading canon gives `env.*`
    // misses and `render::render_data` gives an explicit null.
    let rendered = render_template("[{{ git.tag }}]", &ctx).unwrap();
    assert_eq!(rendered, "[]");
    assert!(!rendered.contains("none"), "got: {rendered}");

    // An unbound name is undefined, and chaining through it stays undefined —
    // not a render error (see `build_expression_env`'s `Chainable`).
    assert_eq!(
        render_template("[{{ nope.field }}]", &json!({})).unwrap(),
        "[]"
    );
    assert_eq!(
        render_template("[{{ nope.field.deeper }}]", &json!({})).unwrap(),
        "[]"
    );

    // Errors are prefixed by phase.
    let err = render_template("{{ x } }}", &ctx).unwrap_err();
    assert!(err.starts_with("template compile error:"), "got: {err}");
    let err = render_template("{% for x in 5 %}{% endfor %}", &json!({})).unwrap_err();
    assert!(err.starts_with("template render error:"), "got: {err}");
}

#[test]
fn render_template_has_no_exec() {
    // The formatting surfaces' render registers `exec()` in its disabled
    // form, so a call fails at the call site with the on-topic message —
    // not minijinja's "unknown function", indistinguishable from a typo.
    let err = render_template("{{ exec('printf', 'x') }}", &json!({})).unwrap_err();
    assert!(err.starts_with("template render error:"), "got: {err}");
    assert!(
        err.contains("exec() is disabled in this context"),
        "got: {err}"
    );
}
