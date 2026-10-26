//! Time and durations: `seconds` and `duration`.
//!
//! A duration is a number of seconds or text in the one grammar
//! ([`crate::duration`]): units `d`, `h`, `m`, `s` and `ms`. A time is a number
//! of seconds since the Unix epoch.

use super::{UNDEFINED, arguments, is_absent, whole};
use crate::duration::{self, Unit};
use crate::engine::{EngineCtx, op_error};
use crate::render;
use minijinja::value::{Kwargs, Rest, Value as MjValue, ValueKind};
use minijinja::{Environment, Error};
use std::sync::Arc;
use std::time::Duration;

pub(super) fn add_seconds(jinja: &mut Environment<'_>, _: &Arc<EngineCtx>) {
    jinja.add_filter("seconds", seconds);
}

pub(super) fn add_duration(jinja: &mut Environment<'_>, _: &Arc<EngineCtx>) {
    jinja.add_filter(
        "duration",
        |value: MjValue, args: Rest<MjValue>, kwargs: Kwargs| -> Result<MjValue, Error> {
            let [parts, floor] = arguments("duration", &args, &kwargs, ["parts", "floor"])?;
            let parts = match parts {
                None => 2,
                Some(p) => {
                    let n = whole("duration", "parts", &p)?;
                    usize::try_from(n)
                        .ok()
                        .filter(|n| *n >= 1)
                        .ok_or_else(|| op_error("duration(): parts must be at least 1"))?
                }
            };
            let floor = match floor.as_ref().filter(|f| !is_absent(f)) {
                None => Unit::Milli,
                Some(f) => f
                    .as_str()
                    .and_then(Unit::from_name)
                    .ok_or_else(|| op_error("duration(): floor must be one of d, h, m, s, ms"))?,
            };
            if is_absent(&value) {
                return Ok(UNDEFINED);
            }
            let secs = number("duration", &value)?;
            Ok(MjValue::from(render::duration_with(
                Duration::from_millis((secs * 1000.0).round() as u64),
                parts,
                floor,
            )))
        },
    );
}

/// `d | seconds`: a duration as seconds, a whole number when it is one, else a
/// float. A number passes through; text is the one grammar.
fn seconds(value: MjValue) -> Result<MjValue, Error> {
    if is_absent(&value) {
        return Ok(UNDEFINED);
    }
    let millis = if value.kind() == ValueKind::Number {
        (number("seconds", &value)? * 1000.0).round() as u64
    } else {
        let d = duration::from_value(&value).map_err(|e| op_error(format!("seconds(): {e}")))?;
        d.as_millis() as u64
    };
    Ok(if millis % 1000 == 0 {
        MjValue::from((millis / 1000) as i64)
    } else {
        MjValue::from(millis as f64 / 1000.0)
    })
}

/// A non-negative number of seconds.
fn number(name: &str, value: &MjValue) -> Result<f64, Error> {
    if value.kind() != ValueKind::Number {
        return Err(op_error(format!(
            "{name}(): expected a number of seconds, not {value}"
        )));
    }
    let secs = f64::try_from(value.clone())
        .map_err(|_| op_error(format!("{name}(): expected a number of seconds")))?;
    if !secs.is_finite() || secs < 0.0 {
        return Err(op_error(format!(
            "{name}(): a duration is not negative, got {value}"
        )));
    }
    Ok(secs)
}

#[cfg(test)]
mod tests {
    use crate::library::testing::eval;

    #[test]
    fn duration_filter() {
        assert_eq!(eval("7500 | duration").unwrap(), "2h5m");
        assert_eq!(eval("2530 | duration(floor='m')").unwrap(), "42m");
        assert_eq!(eval("292000 | duration").unwrap(), "3d9h");
        assert_eq!(eval("2530 | duration(1, 's')").unwrap(), "42m");
        assert_eq!(eval("2530 | duration(parts=3)").unwrap(), "42m10s");
        assert_eq!(eval("1.5 | duration").unwrap(), "1s500ms");
        assert_eq!(eval("0 | duration").unwrap(), "0s");
        assert_eq!(eval("0 | duration(floor='m')").unwrap(), "0m");
        assert_eq!(eval("30 | duration(floor='h')").unwrap(), "0h");
        assert_eq!(eval("(none | duration) is undefined").unwrap(), "true");
        assert_eq!(eval("(missing | duration) is undefined").unwrap(), "true");
        assert_eq!(
            eval("(none | duration(floor=none))is undefined").unwrap(),
            "true"
        );
    }

    #[test]
    fn duration_checks_its_arguments_first() {
        for bad in [
            "5 | duration(0)",
            "5 | duration(parts=-1)",
            "5 | duration(parts=1.5)",
            "5 | duration(floor='w')",
            "5 | duration(floor=3)",
            "5 | duration(2, 's', 'x')",
            "5 | duration(2, parts=3)",
            "5 | duration(flor='s')",
            "-5 | duration",
            "'5' | duration",
            "'1h' | duration",
            "none | duration(floor='w')",
        ] {
            let err = eval(bad).unwrap_err();
            assert!(err.contains("duration():"), "`{bad}` gave `{err}`");
        }
    }

    #[test]
    fn seconds_filter() {
        assert_eq!(eval("'1h30m' | seconds").unwrap(), "5400");
        assert_eq!(eval("'250ms' | seconds").unwrap(), "0.25");
        assert_eq!(eval("'2d3h' | seconds").unwrap(), "183600");
        assert_eq!(eval("'90' | seconds").unwrap(), "90");
        assert_eq!(eval("45 | seconds").unwrap(), "45");
        assert_eq!(eval("1.5 | seconds").unwrap(), "1.5");
        assert_eq!(eval("(none | seconds) is undefined").unwrap(), "true");
        for bad in ["'soon'", "''", "-3", "'-1s'", "true", "[1]"] {
            let err = eval(&format!("{bad} | seconds")).unwrap_err();
            assert!(err.contains("seconds():"), "`{bad}` gave `{err}`");
        }
    }

    #[test]
    fn seconds_and_duration_round_trip() {
        assert_eq!(eval("'1h30m' | seconds | duration").unwrap(), "1h30m");
        assert_eq!(eval("'2d3h' | seconds | duration").unwrap(), "2d3h");
    }
}
