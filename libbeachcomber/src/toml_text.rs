//! TOML values and errors as comb shows them: `fromtoml`, a config parse
//! error, `comb config`'s origins. Kept as toml 0.8 wrote them where toml 1
//! writes less or otherwise.

/// A datetime as TOML writes it, a zero fraction dropped: `07:32:00`, not
/// toml 1's `07:32:00.0`.
pub fn datetime(d: &toml::value::Datetime) -> String {
    let mut d = *d;
    if let Some(t) = d.time.as_mut()
        && t.nanosecond == Some(0)
    {
        t.nanosecond = None;
    }
    d.to_string()
}

/// A parse error's reason, as toml writes it below its snippet. A duplicate
/// key names the key, read from `src` at the error's span (toml 1 says only
/// "duplicate key").
pub fn reason(e: &toml::de::Error, src: &str) -> String {
    match e.span().and_then(|s| src.get(s)) {
        Some(key) if e.message() == "duplicate key" => format!("duplicate key `{key}`"),
        _ => e.message().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(text: &str) -> toml::Value {
        text.parse::<toml::Table>().unwrap().remove("d").unwrap()
    }

    #[test]
    fn a_zero_fraction_is_dropped_and_any_other_kept() {
        for (text, want) in [
            ("d = 07:32:00.000", "07:32:00"),
            ("d = 1979-05-27T07:32:00.0Z", "1979-05-27T07:32:00Z"),
            ("d = 07:32:00.010", "07:32:00.01"),
            ("d = 07:32", "07:32"),
        ] {
            let toml::Value::Datetime(d) = value(text) else {
                panic!("{text}")
            };
            assert_eq!(datetime(&d), want, "{text}");
        }
    }

    #[test]
    fn a_duplicate_key_is_named() {
        for text in ["a = 1\na = 2\n", "[a]\n[a]\n", "[x]\na = 1\na = 2\n"] {
            let e = text.parse::<toml::Table>().unwrap_err();
            assert_eq!(reason(&e, text), "duplicate key `a`", "{text}");
        }
        let text = "a = [1, 2";
        let e = text.parse::<toml::Table>().unwrap_err();
        assert_eq!(reason(&e, text), e.message());
    }
}
