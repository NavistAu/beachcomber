//! Byte-level tag scanner for the three MiniJinja delimiters.
//!
//! Answers "where are the tags?" without a MiniJinja parse, so
//! [`classify`](super::classify) can decide a source's [`Form`](super::Form)
//! before anything is compiled.

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
/// no [`Tag`] here, and stops the scan. [`classify`](super::classify) still
/// calls such a source a [`Form::Template`](super::Form::Template), so the
/// syntax error the user sees comes from the template compiler — which knows it
/// as an unclosed block — rather than from the expression compiler complaining
/// about a stray `{`.
pub fn scan_tags(src: &str) -> Vec<Tag<'_>> {
    scan(src).tags
}

/// What [`scan_tags`] found, plus whether the scan stopped at an unterminated
/// marker. The flag is what lets [`classify`](super::classify) route a broken
/// source to the template compiler; it is not part of the public tag list.
pub(super) struct Scan<'a> {
    pub(super) tags: Vec<Tag<'a>>,
    pub(super) unterminated: bool,
}

pub(super) fn scan(src: &str) -> Scan<'_> {
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
        // Signed, so a stray closer drives the depth negative and the tag never
        // closes: `{{ x } }}` has one `}` too many for the engine, and a depth
        // that saturated at zero would let the scanner find a close MiniJinja
        // rejects. Negative depth falls through to the unterminated path, so the
        // template compiler owns the diagnostic ("unexpected `}`").
        let mut depth: i32 = 0;
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
                depth = depth.saturating_add(1);
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
