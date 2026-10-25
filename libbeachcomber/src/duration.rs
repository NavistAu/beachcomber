//! Durations: the one grammar every flag, setting, decorator and keyword
//! argument that takes a duration reads. [`crate::render::duration`] is the
//! one way one is shown, and this grammar reads everything it writes.
//!
//! A duration is one or more whole numbers each with a unit, `h`, `m`, `s` or
//! `ms`, largest first and each unit at most once (`"250ms"`, `"30s"`,
//! `"1m30s"`, `"2h14m"`); a bare number is seconds (`"60"`). Zero is zero in
//! every unit; a consumer that cannot take zero, or anything below a whole
//! second, refuses it itself.

use std::time::Duration;

/// The grammar, as every refusal states it.
pub const GRAMMAR: &str = "whole numbers each with a unit, h, m, s or ms, largest first; a bare \
     number is seconds (\"250ms\", \"30s\", \"1m30s\", \"60\")";

/// Parse a duration in the one grammar, or `None` for text that is not one
/// (or overflows).
pub fn parse(s: &str) -> Option<Duration> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(secs) = s.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    // Units largest first, each at most once.
    const UNITS: [(&str, u64); 4] = [("h", 3_600_000), ("m", 60_000), ("s", 1_000), ("ms", 1)];
    let mut rest = s;
    let mut total_ms: u64 = 0;
    let mut next_unit = 0;
    while !rest.is_empty() {
        let digits = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        if digits == 0 {
            return None;
        }
        let n: u64 = rest[..digits].parse().ok()?;
        rest = &rest[digits..];
        // `ms` before `m`, so `250ms` is not read as minutes then `s`.
        let (i, (unit, scale)) = UNITS
            .iter()
            .enumerate()
            .skip(next_unit)
            .filter(|(_, (u, _))| rest.starts_with(u))
            .max_by_key(|(_, (u, _))| u.len())?;
        rest = &rest[unit.len()..];
        total_ms = total_ms.checked_add(n.checked_mul(*scale)?)?;
        next_unit = i + 1;
    }
    Some(Duration::from_millis(total_ms))
}

/// The duration a template value holds: a string in the one grammar, or a
/// whole number of seconds, as a config value is. The error says what was
/// wrong, for the caller to prefix with its own name.
pub fn from_value(value: &minijinja::Value) -> Result<Duration, String> {
    use minijinja::value::ValueKind;
    match value.kind() {
        ValueKind::String => {
            let text = value.as_str().unwrap_or_default();
            parse(text).ok_or_else(|| format!("invalid duration `{text}`: {GRAMMAR}"))
        }
        ValueKind::Number => value
            .as_i64()
            .and_then(|n| u64::try_from(n).ok())
            .map(Duration::from_secs)
            .ok_or_else(|| format!("a number is whole seconds, got {value}")),
        _ => Err(format!("a duration is a string like '30s', got {value}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grammar() {
        assert_eq!(parse("250ms"), Some(Duration::from_millis(250)));
        assert_eq!(parse("30s"), Some(Duration::from_secs(30)));
        assert_eq!(parse("2m"), Some(Duration::from_secs(120)));
        assert_eq!(parse("1h"), Some(Duration::from_secs(3600)));
        assert_eq!(parse("60"), Some(Duration::from_secs(60)));
        assert_eq!(parse("0ms"), Some(Duration::ZERO));
        assert_eq!(parse("1m30s"), Some(Duration::from_secs(90)));
        assert_eq!(parse("2h14m"), Some(Duration::from_secs(8040)));
        assert_eq!(parse("1s500ms"), Some(Duration::from_millis(1500)));
        for bad in [
            "",
            "x",
            "-1s",
            "1.5s",
            "30s1m",
            "1m1m",
            "1m 30s",
            "18446744073709551615m",
        ] {
            assert_eq!(parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_template_value_is_a_string_or_whole_seconds() {
        use minijinja::Value;
        assert_eq!(from_value(&Value::from("2s")), Ok(Duration::from_secs(2)));
        assert_eq!(from_value(&Value::from(3)), Ok(Duration::from_secs(3)));
        assert!(from_value(&Value::from(-1)).is_err());
        assert!(from_value(&Value::from(1.5)).is_err());
        assert!(from_value(&Value::from(true)).is_err());
        assert!(from_value(&Value::from("soon")).is_err());
    }

    #[test]
    fn what_comb_shows_reads_back() {
        for d in [
            Duration::from_secs(5),
            Duration::from_millis(250),
            Duration::from_secs(90),
            Duration::from_secs(3600),
            Duration::from_secs(8040),
        ] {
            assert_eq!(parse(&crate::render::duration(d)), Some(d), "{d:?}");
        }
    }
}
