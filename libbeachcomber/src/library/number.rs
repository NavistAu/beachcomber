//! Numbers shown: `ramp`, `pct`, `compact` and `bytes`.

use super::{UNDEFINED, is_absent, number};
use crate::engine::{EngineCtx, op_error};
use minijinja::value::{Value as MjValue, ValueKind};
use minijinja::{Environment, Error};
use std::sync::Arc;

pub(super) fn add_ramp(env: &mut Environment<'_>, _: &Arc<EngineCtx>) {
    env.add_filter("ramp", ramp);
}

pub(super) fn add_pct(env: &mut Environment<'_>, _: &Arc<EngineCtx>) {
    env.add_filter("pct", pct);
}

pub(super) fn add_compact(env: &mut Environment<'_>, _: &Arc<EngineCtx>) {
    env.add_filter("compact", compact);
}

pub(super) fn add_bytes(env: &mut Environment<'_>, _: &Arc<EngineCtx>) {
    env.add_filter("bytes", bytes);
}

/// A list argument's elements, or an error `ramp(): <what> must be a list`.
fn list(what: &str, v: &MjValue) -> Result<Vec<MjValue>, Error> {
    if v.kind() != ValueKind::Seq {
        return Err(op_error(format!("ramp(): {what} must be a list, not {v}")));
    }
    Ok(v.try_iter()?.collect())
}

/// `n | ramp(thresholds, values)`: `values[i]` for the first threshold `t`
/// with `n < t`, else the last value. `thresholds` ascend and `values` is one
/// longer. The arguments are checked first.
fn ramp(n: MjValue, thresholds: MjValue, values: MjValue) -> Result<MjValue, Error> {
    let thresholds = list("thresholds", &thresholds)?;
    let values = list("values", &values)?;
    if values.len() != thresholds.len() + 1 {
        return Err(op_error(format!(
            "ramp(): {} thresholds take {} values, not {}",
            thresholds.len(),
            thresholds.len() + 1,
            values.len()
        )));
    }
    let mut limits = Vec::with_capacity(thresholds.len());
    for t in &thresholds {
        let t = number("ramp", "threshold", t)?;
        if limits.last().is_some_and(|prev| t <= *prev) {
            return Err(op_error(format!(
                "ramp(): thresholds must ascend, but {t} follows {}",
                limits.last().copied().unwrap_or_default()
            )));
        }
        limits.push(t);
    }
    if is_absent(&n) {
        return Ok(UNDEFINED);
    }
    let x = number("ramp", "the subject", &n)?;
    let i = limits.iter().position(|t| x < *t).unwrap_or(limits.len());
    Ok(values[i].clone())
}

/// The most digits `pct` will print.
const MAX_DIGITS: usize = 20;

/// `r | pct(digits=0)`: a ratio as a percentage. It rounds as
/// `'%.Nf' | format` does, since it is that formatting.
fn pct(r: MjValue, digits: Option<MjValue>) -> Result<MjValue, Error> {
    let digits = match digits.filter(|d| !is_absent(d)) {
        None => 0,
        Some(d) => number("pct", "digits", &d)
            .ok()
            .filter(|n| n.fract() == 0.0 && (0.0..=MAX_DIGITS as f64).contains(n))
            .map(|n| n as usize)
            .ok_or_else(|| {
                op_error(format!(
                    "pct(): digits {d} is not an integer from 0 to {MAX_DIGITS}"
                ))
            })?,
    };
    if is_absent(&r) {
        return Ok(UNDEFINED);
    }
    let r = number("pct", "the subject", &r)?;
    Ok(MjValue::from(format!("{:.digits$}%", r * 100.0)))
}

/// `n` shown in `base` steps of `units`: the unit is chosen after rounding
/// (`999950` is `1.0M`, not `1000k`), one decimal below 10 and none from 10 up,
/// and a plain count below the first step has none.
fn scaled(n: f64, base: f64, units: &[&str]) -> String {
    let sign = if n < 0.0 { "-" } else { "" };
    let mut value = n.abs();
    let mut unit = 0;
    while value >= base && unit + 1 < units.len() {
        value /= base;
        unit += 1;
    }
    loop {
        // Halves round up: `9.95` is the float just under, and shows as `10`.
        let tenths = (value * 10.0).round();
        let (shown, text) = if unit > 0 && tenths < 100.0 {
            (tenths / 10.0, format!("{:.1}", tenths / 10.0))
        } else {
            (value.round(), format!("{:.0}", value.round()))
        };
        if shown >= base && unit + 1 < units.len() {
            value /= base;
            unit += 1;
            continue;
        }
        return format!("{sign}{text}{}", units[unit]);
    }
}

/// `n | compact`: `1234` is `1.2k`, `56789` is `57k`.
fn compact(n: MjValue) -> Result<MjValue, Error> {
    if is_absent(&n) {
        return Ok(UNDEFINED);
    }
    let x = number("compact", "the subject", &n)?;
    Ok(MjValue::from(scaled(
        x,
        1000.0,
        &["", "k", "M", "G", "T", "P"],
    )))
}

/// `n | bytes`: a size in 1024 steps, `1288490188` is `1.2G`.
fn bytes(n: MjValue) -> Result<MjValue, Error> {
    if is_absent(&n) {
        return Ok(UNDEFINED);
    }
    let x = number("bytes", "the subject", &n)?;
    if x < 0.0 {
        return Err(op_error(format!("bytes(): the subject {n} is negative")));
    }
    Ok(MjValue::from(scaled(
        x,
        1024.0,
        &["B", "K", "M", "G", "T", "P"],
    )))
}

#[cfg(test)]
mod tests {
    use crate::library::testing::eval;

    fn ok(expr: &str) -> String {
        eval(expr).unwrap_or_else(|e| panic!("{expr}: {e}"))
    }

    fn err(expr: &str) -> String {
        eval(expr).unwrap_err()
    }

    #[test]
    fn ramp_picks_the_first_band_the_value_is_under() {
        let r = |n: &str| ok(&format!("{n} | ramp([50, 80], ['green', 'orange', 'red'])"));
        assert_eq!(r("0"), "green");
        assert_eq!(r("49.9"), "green");
        assert_eq!(r("50"), "orange", "n < t picks values[i]; equal moves on");
        assert_eq!(r("79"), "orange");
        assert_eq!(r("80"), "red");
        assert_eq!(r("1000"), "red");
        assert_eq!(r("-5"), "green");
        assert_eq!(ok("7 | ramp([], ['only'])"), "only");
    }

    #[test]
    fn ramp_values_may_be_any_type() {
        assert_eq!(ok("1 | ramp([2], [10, 20]) + 1"), "11");
        assert_eq!(ok("5 | ramp([2], [10, 20]) + 1"), "21");
    }

    #[test]
    fn ramp_of_nothing_is_undefined_after_its_arguments_are_checked() {
        assert_eq!(ok("(none | ramp([1], ['a', 'b'])) is undefined"), "true");
        assert_eq!(ok("(nope | ramp([1], ['a', 'b'])) is undefined"), "true");
        let e = err("none | ramp([2, 1], [1, 2, 3])");
        assert!(e.contains("ramp(): thresholds must ascend"), "{e}");
    }

    #[test]
    fn ramp_rejects_bad_arguments() {
        let e = err("1 | ramp([50, 80], ['a', 'b'])");
        assert!(
            e.contains("ramp(): 2 thresholds take 3 values, not 2"),
            "{e}"
        );
        let e = err("1 | ramp([50, 50], ['a', 'b', 'c'])");
        assert!(e.contains("ramp(): thresholds must ascend"), "{e}");
        let e = err("1 | ramp(5, ['a', 'b'])");
        assert!(e.contains("ramp(): thresholds must be a list"), "{e}");
        let e = err("1 | ramp([5], 'ab')");
        assert!(e.contains("ramp(): values must be a list"), "{e}");
        let e = err("1 | ramp(['x'], ['a', 'b'])");
        assert!(e.contains("ramp(): threshold x is not a number"), "{e}");
        let e = err("'1' | ramp([5], ['a', 'b'])");
        assert!(e.contains("ramp(): the subject 1 is not a number"), "{e}");
    }

    #[test]
    fn pct_is_a_ratio_as_a_percentage() {
        assert_eq!(ok("0.423 | pct"), "42%");
        assert_eq!(ok("0.4236 | pct(1)"), "42.4%");
        assert_eq!(ok("1 | pct"), "100%");
        assert_eq!(ok("0 | pct(2)"), "0.00%");
        assert_eq!(ok("1.5 | pct"), "150%");
        assert_eq!(ok("0.423 | pct(none)"), "42%", "none digits is not given");
    }

    #[test]
    fn pct_rounds_as_format_does() {
        for r in [
            0.005,
            0.015,
            0.025,
            0.125,
            0.425,
            0.4235,
            0.995,
            0.0049,
            0.5,
            0.505,
            0.285,
            0.1 + 0.2,
        ] {
            for digits in 0..=3 {
                let want = ok(&format!("('%.{digits}f' | format({r} * 100)) ~ '%'"));
                assert_eq!(ok(&format!("{r} | pct({digits})")), want, "{r} at {digits}");
            }
        }
    }

    #[test]
    fn pct_rejects_bad_input() {
        let e = err("'x' | pct");
        assert!(e.contains("pct(): the subject x is not a number"), "{e}");
        for bad in ["-1", "1.5", "'a'", "21"] {
            let e = err(&format!("0.5 | pct({bad})"));
            assert!(e.contains("pct(): digits"), "{bad}: {e}");
        }
        assert_eq!(ok("(none | pct) is undefined"), "true");
        let e = err("none | pct(-1)");
        assert!(e.contains("pct(): digits"), "arguments first: {e}");
    }

    #[test]
    fn compact_abbreviates_by_thousands() {
        for (n, want) in [
            ("0", "0"),
            ("7", "7"),
            ("999", "999"),
            ("1000", "1.0k"),
            ("1234", "1.2k"),
            ("9949", "9.9k"),
            ("9950", "10k"),
            ("56789", "57k"),
            ("999499", "999k"),
            ("999950", "1.0M"),
            ("1500000", "1.5M"),
            ("2500000000", "2.5G"),
            ("-1234", "-1.2k"),
            ("1234.5", "1.2k"),
            ("12.4", "12"),
        ] {
            assert_eq!(ok(&format!("{n} | compact")), want, "{n}");
        }
    }

    #[test]
    fn compact_rejects_bad_input() {
        let e = err("'x' | compact");
        assert!(
            e.contains("compact(): the subject x is not a number"),
            "{e}"
        );
        assert_eq!(ok("(none | compact) is undefined"), "true");
    }

    #[test]
    fn bytes_is_a_size_in_1024_steps() {
        for (n, want) in [
            ("0", "0B"),
            ("512", "512B"),
            ("1023", "1023B"),
            ("1024", "1.0K"),
            ("1536", "1.5K"),
            ("1288490188", "1.2G"),
            ("10485760", "10M"),
            ("1048524", "1.0M"),
            ("1099511627776", "1.0T"),
            ("1536.0", "1.5K"),
        ] {
            assert_eq!(ok(&format!("{n} | bytes")), want, "{n}");
        }
    }

    #[test]
    fn bytes_rejects_negative_and_non_numbers() {
        let e = err("-1 | bytes");
        assert!(e.contains("bytes(): the subject -1 is negative"), "{e}");
        let e = err("'x' | bytes");
        assert!(e.contains("bytes(): the subject x is not a number"), "{e}");
        assert_eq!(ok("(none | bytes) is undefined"), "true");
    }
}
