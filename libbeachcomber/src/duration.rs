//! Durations: the one grammar every flag, setting, decorator and keyword
//! argument that takes a duration reads, and the one way one is shown.
//!
//! A duration is a whole number with an optional unit, `ms`, `s`, `m` or `h`;
//! a bare number is seconds (`"250ms"`, `"30s"`, `"2m"`, `"60"`). Zero is
//! zero in every unit; a consumer that cannot take zero, or anything below a
//! whole second, refuses it itself.

use std::time::Duration;

/// The grammar, as every refusal states it.
pub const GRAMMAR: &str = "a whole number with an optional unit, ms, s, m or h; a bare number \
     is seconds (\"250ms\", \"30s\", \"2m\", \"60\")";

/// Parse a duration in the one grammar, or `None` for text that is not one
/// (or overflows).
pub fn parse(s: &str) -> Option<Duration> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(stripped) = s.strip_suffix("ms") {
        return stripped
            .trim()
            .parse::<u64>()
            .ok()
            .map(Duration::from_millis);
    }
    let (num_str, multiplier) = if let Some(stripped) = s.strip_suffix('s') {
        (stripped, 1u64)
    } else if let Some(stripped) = s.strip_suffix('m') {
        (stripped, 60)
    } else if let Some(stripped) = s.strip_suffix('h') {
        (stripped, 3600)
    } else {
        (s, 1)
    };
    num_str
        .trim()
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(multiplier))
        .map(Duration::from_secs)
}

/// A duration as a person writes one: whole seconds as `"5s"`, anything
/// finer as `"250ms"`.
pub fn display(d: Duration) -> String {
    if d.subsec_nanos() == 0 {
        format!("{}s", d.as_secs())
    } else {
        format!("{}ms", d.as_millis())
    }
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
        for bad in ["", "x", "1m30s", "-1s", "1.5s", "18446744073709551615m"] {
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
    fn display_writes_what_parse_reads() {
        for d in [Duration::from_secs(5), Duration::from_millis(250)] {
            assert_eq!(parse(&display(d)), Some(d));
        }
    }
}
