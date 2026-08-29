//! One expression syntax: classification and reference discovery.
//!
//! Canon `field_resolution.md` §"Value resolution" (invariant 14): `{{ }}`
//! everywhere. A value expression written as exactly one `{{ expr }}` evaluates
//! to the expression's natural type; one written with literal text or more than
//! one tag evaluates to a string. A bare expression (no tags) is still accepted
//! and is equivalent to the single-tag form.
//!
//! This module answers the two questions a caller has *before* evaluating:
//! which of the three forms is this ([`classify`]), and what does it reference
//! ([`discover_refs`], [`daemon_refs`], [`fetch_daemon_data`]). Evaluation
//! itself lives elsewhere.

use crate::virtual_fields::{
    Ref, VirtualFields, build_expression_env, discover_expression_refs, refs_from_names,
};
use std::collections::{HashMap, HashSet};

// ── Forms ─────────────────────────────────────────────────────────────────────

/// How a value expression is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    /// No tag markers at all — the whole source is the expression (backward
    /// compatible).
    Expression,
    /// Exactly one `{{ }}` tag spanning the whole source — keeps its natural type.
    SingleTag,
    /// Literal text, more than one tag, any non-expression tag, or an
    /// unterminated tag marker — string-valued, and the template compiler owns
    /// any syntax diagnostic.
    Template,
}

// ── Tag scanning ──────────────────────────────────────────────────────────────

/// The three MiniJinja tag delimiters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TagKind {
    /// `{{ ... }}`
    Expression,
    /// `{% ... %}`
    Statement,
    /// `{# ... #}`
    Comment,
}

/// One tag found in a source string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tag<'a> {
    pub kind: TagKind,
    /// Byte offset of the opening delimiter.
    pub start: usize,
    /// Byte offset just past the closing delimiter.
    pub end: usize,
    /// Inner text, borrowed from the source, with whitespace-control markers
    /// (`-` / `+`) and surrounding whitespace trimmed.
    pub body: &'a str,
}

/// Scan `src` for MiniJinja tags, in source order.
///
/// A byte-level state machine — no regex, no MiniJinja parse. It mirrors
/// MiniJinja's lexer on the three points that decide where a tag ends:
///
/// - **String literals.** `"…"` / `'…'` with backslash escapes, so a delimiter
///   inside a string (`{{ "}}" }}`) does not close the tag. Comment bodies are
///   not scanned for strings — a comment ends at the first `#}`.
/// - **Bracket balance.** `}}` closes an expression tag only at bracket depth
///   zero, so `{{ {"a": 1}}}` is one tag, not a tag ending mid-literal.
/// - **Whitespace control.** A leading/trailing `-` or `+` is part of the
///   marker, not of the body.
///
/// An unterminated marker (`{{`, `{%` or `{#` with no closing delimiter) yields
/// no [`Tag`] here, and stops the scan. [`classify`] still calls such a source
/// a [`Form::Template`], so the syntax error the user sees comes from the
/// template compiler — which knows it as an unclosed block — rather than from
/// the expression compiler complaining about a stray `{`.
pub fn scan_tags(src: &str) -> Vec<Tag<'_>> {
    scan(src).tags
}

/// What [`scan_tags`] found, plus whether the scan stopped at an unterminated
/// marker. The flag is what lets [`classify`] route a broken source to the
/// template compiler; it is not part of the public tag list.
struct Scan<'a> {
    tags: Vec<Tag<'a>>,
    unterminated: bool,
}

fn scan(src: &str) -> Scan<'_> {
    let bytes = src.as_bytes();
    let len = bytes.len();
    let mut tags = Vec::new();
    let mut i = 0;

    while i + 1 < len {
        if bytes[i] != b'{' {
            i += 1;
            continue;
        }
        let (kind, closer) = match bytes[i + 1] {
            b'{' => (TagKind::Expression, b'}'),
            b'%' => (TagKind::Statement, b'%'),
            b'#' => (TagKind::Comment, b'#'),
            // A lone `{` that starts no tag.
            _ => {
                i += 1;
                continue;
            }
        };

        let start = i;
        let body_start = i + 2;
        let mut j = body_start;
        let mut in_string: Option<u8> = None;
        let mut depth: usize = 0;
        let mut close_at: Option<usize> = None;
        let structural = kind != TagKind::Comment;

        while j + 1 < len {
            // The string check precedes the close check: a delimiter inside a
            // string literal is text, not the end of the tag.
            if let Some(delim) = in_string {
                if bytes[j] == b'\\' {
                    j += 2;
                } else {
                    if bytes[j] == delim {
                        in_string = None;
                    }
                    j += 1;
                }
            } else if structural && (bytes[j] == b'"' || bytes[j] == b'\'') {
                in_string = Some(bytes[j]);
                j += 1;
            } else if structural && matches!(bytes[j], b'(' | b'[' | b'{') {
                depth += 1;
                j += 1;
            } else if depth == 0 && bytes[j] == closer && bytes[j + 1] == b'}' {
                close_at = Some(j);
                break;
            } else {
                // A closing bracket at depth > 0 is the literal's, not the
                // tag's — this is what keeps `{{ {"a": 1}}}` one tag.
                if structural && matches!(bytes[j], b')' | b']' | b'}') {
                    depth = depth.saturating_sub(1);
                }
                j += 1;
            }
        }

        let Some(body_end) = close_at else {
            // Unterminated — nothing after it can be a tag.
            return Scan {
                tags,
                unterminated: true,
            };
        };
        tags.push(Tag {
            kind,
            start,
            end: body_end + 2,
            body: trim_tag_body(&src[body_start..body_end]),
        });
        i = body_end + 2;
    }

    Scan {
        tags,
        unterminated: false,
    }
}

/// Strip whitespace-control markers and surrounding whitespace from a tag's
/// inner text. `{{- x -}}`, `{{+ x +}}` and `{{ x }}` all yield `x`.
fn trim_tag_body(inner: &str) -> &str {
    let inner = inner.strip_prefix(['-', '+']).unwrap_or(inner);
    let inner = inner.strip_suffix(['-', '+']).unwrap_or(inner);
    inner.trim_ascii()
}

// ── Classification ────────────────────────────────────────────────────────────

/// Classify a value expression into one of the three [`Form`]s.
///
/// Surrounding whitespace is not literal text: it is trimmed before the tag
/// spans are compared against the source.
pub fn classify(src: &str) -> Form {
    analyze(src).0
}

/// The expression inside the tag, when `src` is the [`Form::SingleTag`] form.
///
/// Whitespace-control markers and surrounding whitespace are trimmed, so
/// `{{- git.branch -}}` yields `git.branch`.
pub fn single_tag_expression(src: &str) -> Option<&str> {
    analyze(src).1
}

/// The form of `src`, and — for the single-tag form only — the expression
/// inside the tag. One scan, one place the single-tag rule is written down, so
/// [`classify`], [`single_tag_expression`] and [`discover_refs`] can never
/// disagree about what the single-tag form is.
fn analyze(src: &str) -> (Form, Option<&str>) {
    let trimmed = src.trim_ascii();
    let scan = scan(trimmed);
    if scan.unterminated {
        return (Form::Template, None);
    }
    match scan.tags.as_slice() {
        [] => (Form::Expression, None),
        [tag] if tag.kind == TagKind::Expression && tag.start == 0 && tag.end == trimmed.len() => {
            (Form::SingleTag, Some(tag.body))
        }
        _ => (Form::Template, None),
    }
}

// ── Reference discovery ───────────────────────────────────────────────────────

/// Every reference in `src`, for all three forms, deduplicated and sorted.
///
/// `Expression` and `SingleTag` sources go through MiniJinja's expression
/// meta-analysis; `Template` sources through the template's. Both run on the
/// same environment and classify the dotted names they find identically. A
/// source that fails to compile yields no refs — the compile error surfaces at
/// evaluation time.
///
/// Both analyses hand back a `HashSet`, whose iteration order varies run to
/// run. Sorting here — the one choke point [`daemon_refs`] and
/// [`fetch_daemon_data`] flow through — is what makes ref order reproducible.
pub fn discover_refs(src: &str) -> Vec<Ref> {
    let mut refs = match analyze(src) {
        (Form::Expression, _) => discover_expression_refs(src),
        (Form::SingleTag, Some(expr)) => discover_expression_refs(expr),
        // Template — and, unreachably, a single tag with no body.
        _ => {
            let env = build_expression_env();
            let Ok(template) = env.template_from_str(src) else {
                return Vec::new();
            };
            refs_from_names(template.undeclared_variables(true))
        }
    };
    refs.sort();
    refs
}

/// The refs a caller must fetch from the daemon before evaluating `src`.
///
/// The transitive closure of [`discover_refs`] over virtual fields: every
/// `Resolved(p, f)` that is itself virtual is replaced by the refs of its own
/// expression, recursively. The result holds only `CacheField`, `CacheProvider`
/// and non-virtual `Resolved` refs — deduplicated and sorted. `Env` refs come
/// from the caller's shell, never the daemon, and are dropped.
///
/// A reference cycle terminates: each virtual field is expanded at most once.
pub fn daemon_refs(src: &str, vf: &VirtualFields) -> Vec<Ref> {
    let mut out = Vec::new();
    let mut seen: HashSet<Ref> = HashSet::new();
    let mut expanded: HashSet<(String, String)> = HashSet::new();
    close_over_virtuals(&discover_refs(src), vf, &mut out, &mut seen, &mut expanded);
    // Expansion interleaves each virtual field's own refs into the walk, so a
    // sorted input does not stay sorted through the closure.
    out.sort();
    out
}

fn close_over_virtuals(
    refs: &[Ref],
    vf: &VirtualFields,
    out: &mut Vec<Ref>,
    seen: &mut HashSet<Ref>,
    expanded: &mut HashSet<(String, String)>,
) {
    for r in refs {
        match r {
            // env.* is read from the calling shell, not fetched.
            Ref::Env(_) => {}
            Ref::Resolved(p, f) if vf.is_virtual(p, f) => {
                // `expanded` doubles as the cycle guard: a field already
                // expanded (on this path or another) is never expanded again.
                if expanded.insert((p.clone(), f.clone()))
                    && let Some(expr) = vf.expression(p, f)
                {
                    close_over_virtuals(&discover_refs(expr), vf, out, seen, expanded);
                }
            }
            other => {
                if seen.insert(other.clone()) {
                    out.push(other.clone());
                }
            }
        }
    }
}

/// Fetch each ref's value through `fetch`, keyed the way
/// [`EvalContext::daemon_data`](crate::virtual_fields::EvalContext) expects.
///
/// `CacheField(p, f)` and `Resolved(p, f)` are asked for as `"p.f"`;
/// `CacheProvider(p)` as `"p"`. `Env` refs are skipped. A `Ok(None)` is a miss
/// and is simply absent from the map.
///
/// `fetch` is called at most once per distinct key: `cache.p.f` and a resolved
/// `p.f` are different refs but the same daemon query, and a miss is not worth
/// a second round trip either.
///
/// The first `Err` short-circuits, so the caller decides what a transport
/// failure means — abort the render, or fall back to what did arrive. The
/// closure shape lets a `Client`, a `Session`, or a foreign source supply the
/// values.
pub fn fetch_daemon_data<E>(
    refs: &[Ref],
    mut fetch: impl FnMut(&str) -> Result<Option<serde_json::Value>, E>,
) -> Result<HashMap<String, serde_json::Value>, E> {
    let mut data = HashMap::new();
    let mut asked: HashSet<String> = HashSet::new();
    for r in refs {
        let key = match r {
            Ref::Env(_) => continue,
            Ref::CacheField(p, f) | Ref::Resolved(p, f) => format!("{p}.{f}"),
            Ref::CacheProvider(p) => p.clone(),
        };
        if !asked.insert(key.clone()) {
            continue;
        }
        if let Some(v) = fetch(&key)? {
            data.insert(key, v);
        }
    }
    Ok(data)
}
