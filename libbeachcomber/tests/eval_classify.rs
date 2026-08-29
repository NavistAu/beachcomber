//! Classification and reference discovery for value expressions.
//!
//! Pins canon `field_resolution.md` invariant 14: a value expression written as
//! exactly one `{{ expr }}` keeps the expression's natural type; one written
//! with literal text or more than one tag is a string-valued template. A bare
//! expression (no tags) is equivalent to the single-tag form.

use libbeachcomber::eval::{
    Form, TagKind, classify, daemon_refs, discover_refs, fetch_daemon_data, scan_tags,
    single_tag_expression,
};
use libbeachcomber::virtual_fields::{Ref, VirtualFields};
use std::collections::HashSet;

fn refs_set(src: &str) -> HashSet<Ref> {
    discover_refs(src).into_iter().collect()
}

fn vfields(entries: &[(&str, &str, &str)]) -> VirtualFields {
    VirtualFields::with_config_overrides(
        entries
            .iter()
            .map(|(p, f, e)| ((p.to_string(), f.to_string()), e.to_string())),
    )
}

#[test]
fn classify_bare_is_expression() {
    assert_eq!(classify("git.branch"), Form::Expression);
    assert_eq!(classify("env.A or cache.x.y"), Form::Expression);
    assert_eq!(classify("  git.branch  "), Form::Expression);

    assert_eq!(single_tag_expression("git.branch"), None);
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

    // A lone `{` opens no tag and is left to the expression compiler.
    assert_eq!(classify("a { b"), Form::Expression);

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
fn discover_refs_template_matches_expression_form() {
    let expected: HashSet<Ref> = [Ref::Resolved("git".into(), "branch".into())]
        .into_iter()
        .collect();
    assert_eq!(refs_set("git.branch"), expected);
    assert_eq!(refs_set("{{ git.branch }}"), expected);
    // The template form finds the same refs as the two single-expression forms.
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
    assert_eq!(discover_refs("a.b or c.d or e.f"), expected);

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
    assert_eq!(discover_refs("nonsense ???"), vec![]);
}

#[test]
fn discover_refs_deduplicates_across_tags() {
    // A ref names a daemon key of exactly two segments; deeper segments are
    // attribute navigation into the fetched value, so `a.b.c` is the same ref
    // as `a.b` and the two collapse to one.
    assert_eq!(
        discover_refs("{{ a.b }}{{ a.b.c }}"),
        vec![Ref::Resolved("a".into(), "b".into())]
    );
    assert_eq!(
        discover_refs("{{ env.A }}{{ env.A.B }}"),
        vec![Ref::Env("A".into())]
    );
}

#[test]
fn daemon_refs_follows_nested_virtual_fields() {
    let vf = vfields(&[("a", "x", "b.y or cache.c.z"), ("b", "y", "cache.d.w")]);

    let refs: HashSet<Ref> = daemon_refs("{{ a.x }}", &vf).into_iter().collect();
    let expected: HashSet<Ref> = [
        Ref::CacheField("d".into(), "w".into()),
        Ref::CacheField("c".into(), "z".into()),
    ]
    .into_iter()
    .collect();
    assert_eq!(refs, expected);

    // The virtual fields themselves are never fetched from the daemon.
    assert!(!refs.contains(&Ref::Resolved("a".into(), "x".into())));
    assert!(!refs.contains(&Ref::Resolved("b".into(), "y".into())));

    // A non-virtual resolved ref survives the closure as-is, and env refs drop out.
    let refs: HashSet<Ref> = daemon_refs("{{ a.x }} {{ other.field }} {{ env.HOME }}", &vf)
        .into_iter()
        .collect();
    assert!(refs.contains(&Ref::Resolved("other".into(), "field".into())));
    assert!(!refs.iter().any(|r| matches!(r, Ref::Env(_))));
}

#[test]
fn daemon_refs_is_sorted_after_expansion() {
    // Expansion splices a virtual field's own refs into the walk at the point
    // it is reached, so a sorted input does not stay sorted: `cache.z.q` is
    // discovered first, but `cache.a.b` — reached through a.x — sorts ahead.
    let vf = vfields(&[("a", "x", "cache.a.b")]);
    assert_eq!(
        daemon_refs("{{ cache.z.q or a.x }}", &vf),
        vec![
            Ref::CacheField("a".into(), "b".into()),
            Ref::CacheField("z".into(), "q".into()),
        ]
    );
}

#[test]
fn daemon_refs_cycle_terminates() {
    let vf = vfields(&[("a", "x", "b.y"), ("b", "y", "a.x")]);
    assert_eq!(daemon_refs("{{ a.x }}", &vf), vec![]);

    // A self-cycle with a real dependency still yields that dependency.
    let vf = vfields(&[("a", "x", "a.x or cache.c.z")]);
    assert_eq!(
        daemon_refs("a.x", &vf),
        vec![Ref::CacheField("c".into(), "z".into())]
    );
}

#[test]
fn fetch_daemon_data_keys() {
    let refs = vec![
        Ref::CacheField("git".into(), "branch".into()),
        Ref::CacheProvider("aws_profiles".into()),
        Ref::Resolved("other".into(), "field".into()),
        // Same daemon key as the ref above — one query, not two.
        Ref::CacheField("other".into(), "field".into()),
        Ref::Resolved("missing".into(), "field".into()),
        // Same key again, and a miss — a miss is not retried either.
        Ref::CacheField("missing".into(), "field".into()),
        Ref::Env("HOME".into()),
    ];

    let mut asked: Vec<String> = Vec::new();
    let data = fetch_daemon_data(&refs, |key| {
        asked.push(key.to_string());
        Ok::<_, String>(match key {
            "git.branch" => Some(serde_json::json!("main")),
            "aws_profiles" => Some(serde_json::json!({"default": {"region": "ap-southeast-2"}})),
            "other.field" => Some(serde_json::json!(7)),
            _ => None,
        })
    })
    .expect("no transport error");

    // `cache.P.F` and a resolved `P.F` are both keyed "P.F"; `cache.P` is keyed "P".
    assert_eq!(data.get("git.branch"), Some(&serde_json::json!("main")));
    assert_eq!(data.get("other.field"), Some(&serde_json::json!(7)));
    assert!(data.contains_key("aws_profiles"));
    // Misses are simply absent, and env refs are never fetched.
    assert!(!data.contains_key("missing.field"));
    assert!(!data.contains_key("HOME"));
    assert_eq!(data.len(), 3);

    // Called exactly once per distinct key, in ref order.
    assert_eq!(
        asked,
        vec!["git.branch", "aws_profiles", "other.field", "missing.field"]
    );
}

#[test]
fn fetch_daemon_data_short_circuits_on_error() {
    let refs = vec![
        Ref::CacheField("git".into(), "branch".into()),
        Ref::CacheField("boom".into(), "field".into()),
        Ref::CacheField("never".into(), "asked".into()),
    ];

    let mut asked: Vec<String> = Vec::new();
    let err = fetch_daemon_data(&refs, |key| {
        asked.push(key.to_string());
        if key == "boom.field" {
            Err("connection reset")
        } else {
            Ok(Some(serde_json::json!("main")))
        }
    })
    .expect_err("the transport error should surface");

    // The caller owns the policy, so the first Err stops the walk and is
    // handed back rather than being swallowed into a partial map.
    assert_eq!(err, "connection reset");
    assert_eq!(asked, vec!["git.branch", "boom.field"]);
}
