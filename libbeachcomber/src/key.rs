//! Keys and addresses: the one grammar `comb get`, the wire, the daemon and
//! the templates share.
//!
//! A key is a **head** and an **address** below it. The head is a provider
//! name, optionally `name:facet`; it runs to the first `.` or `[`. The address
//! is a run of segments, each written one of three ways:
//!
//! - `.seg` — any characters but `.`, `[` and `]`: `git.branch`,
//!   `mise.cargo:cargo-nextest`, `mise.npm:@scope/pkg`;
//! - `[n]` — an array index, the same segment as `.n`: `p.list[0]` is
//!   `p.list.0`;
//! - `['…']` or `["…"]` — a quoted key, where a backslash escapes only the
//!   quote and the backslash: `mise['go:golang.org/x/tools/gopls']`.
//!
//! Those are jinja's own spellings, so an address written in a template
//! (`comb.mise['cargo:x']`) is written the same way to `comb get`. Brackets
//! are the one quoting form.
//!
//! Inside the daemon and the client an address is carried as a string in
//! **canonical form** ([`format_address`]): bare segments joined by `.`, and a
//! segment that needs quoting — empty, or holding `.`, `[` or `]` — as
//! `['…']`. A plain dotted address is its own canonical form, so everything
//! that never meets such a segment reads exactly as before, and every address
//! comb prints can be typed back as a key.

use std::borrow::Cow;

/// The characters a bare segment cannot hold.
fn ends_bare(c: char) -> bool {
    matches!(c, '.' | '[' | ']')
}

/// The head of `key`: everything before the first `.` or `[`.
pub fn head_of(key: &str) -> &str {
    key.find(['.', '[']).map_or(key, |i| &key[..i])
}

/// Split a key into its head and the canonical address below it (`""` for a
/// bare head).
///
/// Refused, as `invalid key: <key>`: an empty head, a head starting with `/`
/// or `~` (a directory, never a key), a head holding `]`, an empty segment
/// (`a..b`, a trailing `.`), a stray `]`, an unterminated bracket or quote,
/// and an index that is not a canonical decimal. The facet syntax of the head
/// is [`parse_facet`]'s to judge.
pub fn split_key(key: &str) -> Result<(&str, String), String> {
    let invalid = || format!("invalid key: {key}");
    let head = head_of(key);
    if head.is_empty() || head.starts_with(['/', '~']) || head.contains(']') {
        return Err(invalid());
    }
    let segments = parse_segments(&key[head.len()..], true).ok_or_else(invalid)?;
    Ok((head, format_address(&segments)))
}

/// The segments of a canonical address (or of one a caller wrote below a
/// head with its leading `.` dropped): `a.b['c.d'][0]` is `a`, `b`, `c.d`,
/// `0`. `None` for text that is not an address.
pub fn parse_address(address: &str) -> Option<Vec<String>> {
    parse_segments(address, false)
}

/// The segments of a canonical `address`, or the empty list for `""`. An
/// address comb built itself is canonical by construction, so a parse failure
/// is the empty list rather than a panic. A plain dotted address borrows its
/// segments; only a quoted one is parsed.
pub fn segments(address: &str) -> Vec<Cow<'_, str>> {
    if address.is_empty() {
        return Vec::new();
    }
    if !address.contains(['[', ']']) {
        return address.split('.').map(Cow::Borrowed).collect();
    }
    parse_address(address)
        .unwrap_or_default()
        .into_iter()
        .map(Cow::Owned)
        .collect()
}

/// `after_head` is what follows a head: there, a bare segment must be
/// introduced by `.`. In a bare address the first bare segment is not.
fn parse_segments(text: &str, after_head: bool) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut chars = text.char_indices().peekable();
    let mut first = true;
    while let Some(&(_, c)) = chars.peek() {
        match c {
            '[' => {
                chars.next();
                out.push(parse_bracket(text, &mut chars)?);
            }
            '.' => {
                chars.next();
                out.push(parse_bare(text, &mut chars)?);
            }
            ']' => return None,
            _ if first && !after_head => out.push(parse_bare(text, &mut chars)?),
            _ => return None,
        }
        first = false;
    }
    Some(out)
}

type Chars<'a> = std::iter::Peekable<std::str::CharIndices<'a>>;

/// A bare segment, up to the next `.`, `[`, `]` or the end. Never empty.
fn parse_bare(text: &str, chars: &mut Chars<'_>) -> Option<String> {
    let start = chars.peek().map_or(text.len(), |&(i, _)| i);
    let mut end = text.len();
    while let Some(&(i, c)) = chars.peek() {
        if ends_bare(c) {
            end = i;
            break;
        }
        chars.next();
    }
    let segment = &text[start..end];
    (!segment.is_empty()).then(|| segment.to_string())
}

/// What follows a `[`: a quoted key or a decimal index, then `]`.
fn parse_bracket(text: &str, chars: &mut Chars<'_>) -> Option<String> {
    let (_, open) = *chars.peek()?;
    let segment = if open == '\'' || open == '"' {
        chars.next();
        let mut s = String::new();
        loop {
            let (_, c) = chars.next()?;
            match c {
                '\\' => {
                    let (_, escaped) = chars.next()?;
                    if escaped != open && escaped != '\\' {
                        return None;
                    }
                    s.push(escaped);
                }
                c if c == open => break,
                c => s.push(c),
            }
        }
        s
    } else {
        let start = chars.peek().map(|&(i, _)| i)?;
        let mut end = start;
        while let Some(&(i, c)) = chars.peek() {
            if !c.is_ascii_digit() {
                break;
            }
            end = i + c.len_utf8();
            chars.next();
        }
        let digits = &text[start..end];
        let canonical = digits == "0" || (!digits.is_empty() && !digits.starts_with('0'));
        if !canonical {
            return None;
        }
        digits.to_string()
    };
    match chars.next()? {
        (_, ']') => Some(segment),
        _ => None,
    }
}

/// Whether `segment` must be written `['…']`.
fn needs_quote(segment: &str) -> bool {
    segment.is_empty() || segment.contains(ends_bare)
}

/// One segment as it is written after a `.`, or as `['…']` when it must be
/// quoted.
pub fn quote(segment: &str) -> Cow<'_, str> {
    if !needs_quote(segment) {
        return Cow::Borrowed(segment);
    }
    let escaped = segment.replace('\\', r"\\").replace('\'', r"\'");
    Cow::Owned(format!("['{escaped}']"))
}

/// Append `segment` to the canonical `address`.
pub fn push(address: &str, segment: &str) -> String {
    let quoted = quote(segment);
    if address.is_empty() || quoted.starts_with('[') {
        format!("{address}{quoted}")
    } else {
        format!("{address}.{quoted}")
    }
}

/// The canonical form of `segments`.
pub fn format_address<S: AsRef<str>>(segments: &[S]) -> String {
    let mut out = String::new();
    for segment in segments {
        out = push(&out, segment.as_ref());
    }
    out
}

/// The whole key for `address` under `head`: `git` and `branch` are
/// `git.branch`, `mise` and `['a.b']` are `mise['a.b']`, and an empty address
/// is the head alone.
pub fn join(head: &str, address: &str) -> String {
    if address.is_empty() {
        head.to_string()
    } else if address.starts_with('[') {
        format!("{head}{address}")
    } else {
        format!("{head}.{address}")
    }
}

/// The canonical address one segment up from `address`, or `None` for the
/// root (`""`).
pub fn parent(address: &str) -> Option<String> {
    let mut segments = segments(address);
    segments.pop()?;
    Some(format_address(&segments))
}

/// The last segment of a canonical address, or `None` for the root.
pub fn last(address: &str) -> Option<String> {
    segments(address).pop().map(Cow::into_owned)
}

/// Whether `address` is `ancestor` or lies below it. Everything lies below
/// the root `""`.
pub fn within(address: &str, ancestor: &str) -> bool {
    ancestor.is_empty()
        || address == ancestor
        || address
            .strip_prefix(ancestor)
            .is_some_and(|tail| tail.starts_with(['.', '[']))
}

/// The first segment of a canonical address and the canonical address below
/// it (`None` when nothing follows), or `None` for the root.
pub fn split_first(address: &str) -> Option<(String, Option<String>)> {
    let mut segments = parse_address(address)?.into_iter();
    let first = segments.next()?;
    let rest: Vec<String> = segments.collect();
    Some((first, (!rest.is_empty()).then(|| format_address(&rest))))
}

/// Parse the facet syntax of a head: `base:facet`. `Ok(None)` for a plain
/// provider name, `Ok(Some(facet))` for a valid facet key, and an
/// `invalid facet key` error for more than one colon, an empty base or facet,
/// a facet holding anything but ASCII letters, digits, `_` and `-`, or a facet
/// named `put` or `source` — the layer facets a request key splits off before
/// this check and a config table cannot be.
pub fn parse_facet(head: &str) -> Result<Option<&str>, String> {
    let Some((base, facet)) = head.split_once(':') else {
        return Ok(None);
    };
    let valid = !base.is_empty()
        && !facet.is_empty()
        && facet
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        && !matches!(facet, "put" | "source");
    if valid {
        Ok(Some(facet))
    } else {
        Err(format!("invalid facet key: {head}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(key: &str) -> (String, Vec<String>) {
        let (head, address) = split_key(key).unwrap_or_else(|e| panic!("{key}: {e}"));
        (head.to_string(), owned(&address))
    }

    fn owned(address: &str) -> Vec<String> {
        segments(address).into_iter().map(String::from).collect()
    }

    fn segs(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_dotted_key_is_a_head_and_its_segments() {
        assert_eq!(split("git"), ("git".into(), vec![]));
        assert_eq!(split("git.branch"), ("git".into(), segs(&["branch"])));
        assert_eq!(split("p.a.b.c"), ("p".into(), segs(&["a", "b", "c"])));
        assert_eq!(
            split("git:cache.branch"),
            ("git:cache".into(), segs(&["branch"]))
        );
    }

    #[test]
    fn a_bare_segment_holds_colons_slashes_at_and_dashes() {
        assert_eq!(
            split("mise.cargo:cargo-nextest"),
            ("mise".into(), segs(&["cargo:cargo-nextest"]))
        );
        assert_eq!(
            split("mise.npm:@scope/pkg"),
            ("mise".into(), segs(&["npm:@scope/pkg"]))
        );
        assert_eq!(
            split("git.branch:age"),
            ("git".into(), segs(&["branch:age"]))
        );
    }

    #[test]
    fn brackets_quote_a_key_and_index_an_array() {
        assert_eq!(
            split("mise['go:golang.org/x/tools/gopls']"),
            ("mise".into(), segs(&["go:golang.org/x/tools/gopls"]))
        );
        assert_eq!(split(r#"p["a.b"].c"#), ("p".into(), segs(&["a.b", "c"])));
        assert_eq!(split("p.list[0]"), ("p".into(), segs(&["list", "0"])));
        assert_eq!(split("p.list.0"), ("p".into(), segs(&["list", "0"])));
        assert_eq!(
            split("p['x'][2]['y.z']"),
            ("p".into(), segs(&["x", "2", "y.z"]))
        );
    }

    #[test]
    fn a_backslash_escapes_only_the_quote_and_itself() {
        assert_eq!(split(r"p['it\'s']"), ("p".into(), segs(&["it's"])));
        assert_eq!(
            split(r#"p["say \"hi\""]"#),
            ("p".into(), segs(&[r#"say "hi""#]))
        );
        assert_eq!(split(r"p['a\\b']"), ("p".into(), segs(&[r"a\b"])));
        // The other quote needs no escape inside a quoted key.
        assert_eq!(split(r#"p["it's"]"#), ("p".into(), segs(&["it's"])));
        assert!(split_key(r"p['a\nb']").is_err(), "\\n is not an escape");
    }

    #[test]
    fn malformed_keys_are_invalid() {
        for key in [
            "", ".x", "[0]", "p..a", "p.", "p.a.", "p]", "p.a]b", "p[", "p['a", "p['a'", "p['a']x",
            "p[01]", "p[-1]", "p[x]", "/tmp/x", "~/ws", "~",
        ] {
            assert_eq!(
                split_key(key),
                Err(format!("invalid key: {key}")),
                "{key:?} should be refused"
            );
        }
    }

    #[test]
    fn canonical_form_quotes_only_what_must_be() {
        assert_eq!(format_address(&["branch"]), "branch");
        assert_eq!(format_address(&["a", "b"]), "a.b");
        assert_eq!(format_address(&["cargo:x"]), "cargo:x");
        assert_eq!(
            format_address(&["go:golang.org/x", "v"]),
            "['go:golang.org/x'].v"
        );
        assert_eq!(format_address(&["a", "b.c"]), "a['b.c']");
        assert_eq!(format_address(&["a[0]"]), r"['a[0]']");
        assert_eq!(format_address(&["it's.x"]), r"['it\'s.x']");
        assert_eq!(format_address(&[""]), "['']");
        assert_eq!(format_address::<&str>(&[]), "");
    }

    #[test]
    fn canonical_form_reads_back_to_the_same_segments() {
        for list in [
            vec!["a"],
            vec!["a", "b"],
            vec!["go:golang.org/x/tools/gopls"],
            vec!["x", "a.b", "0", r"q'\u"],
            vec!["[", "]", ""],
        ] {
            let address = format_address(&list);
            assert_eq!(owned(&address), segs(&list), "{address}");
            let key = join("p", &address);
            assert_eq!(split(&key), ("p".into(), segs(&list)), "{key}");
        }
    }

    #[test]
    fn a_key_is_canonicalised() {
        assert_eq!(split_key("p.list[0]").unwrap().1, "list.0");
        assert_eq!(split_key(r#"p["a"].b"#).unwrap().1, "a.b");
        assert_eq!(split_key("p['a.b']").unwrap().1, "['a.b']");
    }

    #[test]
    fn join_parent_within_and_split_first_respect_quoted_segments() {
        assert_eq!(join("p", ""), "p");
        assert_eq!(join("p", "a.b"), "p.a.b");
        assert_eq!(join("p", "['a.b']"), "p['a.b']");
        assert_eq!(parent("a['b.c']").as_deref(), Some("a"));
        assert_eq!(parent("['b.c'].d").as_deref(), Some("['b.c']"));
        assert_eq!(parent("a").as_deref(), Some(""));
        assert_eq!(parent(""), None);
        assert!(within("a.b", "a"));
        assert!(within("a['b.c']", "a"));
        assert!(within("a", "a"));
        assert!(within("anything", ""));
        assert!(!within("ab", "a"));
        assert_eq!(
            split_first("['a.b'].c.d"),
            Some(("a.b".to_string(), Some("c.d".to_string())))
        );
        assert_eq!(split_first("a"), Some(("a".to_string(), None)));
        assert_eq!(split_first(""), None);
    }

    #[test]
    fn head_of_stops_at_a_dot_or_a_bracket() {
        assert_eq!(head_of("git.branch"), "git");
        assert_eq!(head_of("mise['x']"), "mise");
        assert_eq!(head_of("git:cache"), "git:cache");
    }

    #[test]
    fn parse_facet_keeps_its_rules() {
        assert_eq!(parse_facet("myapp"), Ok(None));
        assert_eq!(parse_facet("scanline:template"), Ok(Some("template")));
        assert_eq!(parse_facet("a:my_facet-2"), Ok(Some("my_facet-2")));
        for bad in ["a:b:c", ":facet", "base:", "a:b!", "a:put", "a:source"] {
            assert!(parse_facet(bad).is_err(), "{bad}");
        }
    }
}
