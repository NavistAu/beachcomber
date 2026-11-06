//! Time and durations: `seconds` and `duration`, and on the clock tier `now`,
//! `strftime`, `fromiso`, `ago` and `spinner`.
//!
//! A duration is a number of seconds or text in the one grammar
//! ([`crate::duration`]): units `d`, `h`, `m`, `s` and `ms`. A time is a number
//! of seconds since the Unix epoch.

use super::{UNDEFINED, arguments, is_absent, is_absent_text, text, whole};
use crate::duration::{self, Unit};
use crate::engine::{EngineCtx, op_error};
use crate::render;
use jiff::tz::TimeZone;
use jiff::{Timestamp, civil};
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
            let secs = number("duration", &value, false)?;
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
        (number("seconds", &value, false)? * 1000.0).round() as u64
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

/// A number of seconds; with `signed` false, a non-negative one.
fn number(name: &str, value: &MjValue, signed: bool) -> Result<f64, Error> {
    if value.kind() != ValueKind::Number {
        return Err(op_error(format!(
            "{name}(): expected a number of seconds, not {value}"
        )));
    }
    let secs = f64::try_from(value.clone())
        .map_err(|_| op_error(format!("{name}(): expected a number of seconds")))?;
    if !secs.is_finite() || (!signed && secs < 0.0) {
        return Err(op_error(format!(
            "{name}(): a duration is not negative, got {value}"
        )));
    }
    Ok(secs)
}

pub(super) fn add_now(jinja: &mut Environment<'_>, ctx: &Arc<EngineCtx>) {
    let ctx = ctx.clone();
    jinja.add_function("now", move |args: Rest<MjValue>, kwargs: Kwargs| {
        if !args.is_empty() || kwargs.args().next().is_some() {
            return Err(op_error("now(): takes no arguments"));
        }
        Ok(MjValue::from((ctx.now_millis() / 1000) as i64))
    });
}

pub(super) fn add_strftime(jinja: &mut Environment<'_>, ctx: &Arc<EngineCtx>) {
    let ctx = ctx.clone();
    jinja.add_filter(
        "strftime",
        move |value: MjValue, args: Rest<MjValue>, kwargs: Kwargs| -> Result<MjValue, Error> {
            let [fmt, tz] = arguments("strftime", &args, &kwargs, ["fmt", "tz"])?;
            let fmt = fmt.ok_or_else(|| op_error("strftime(): takes a format"))?;
            let fmt = match fmt.as_str() {
                Some(f) => f.to_string(),
                None => return Err(op_error("strftime(): the format must be a string")),
            };
            let zone = zone("strftime", &ctx, tz.as_ref())?;
            if is_absent(&value) {
                return Ok(UNDEFINED);
            }
            let at = timestamp("strftime", &value)?.to_zoned(zone);
            jiff::fmt::strtime::format(&fmt, &at)
                .map(MjValue::from)
                .map_err(|e| op_error(format!("strftime(): {e}")))
        },
    );
}

pub(super) fn add_fromiso(jinja: &mut Environment<'_>, ctx: &Arc<EngineCtx>) {
    let ctx = ctx.clone();
    jinja.add_filter("fromiso", move |value: MjValue| -> Result<MjValue, Error> {
        if is_absent_text(&value) {
            return Ok(UNDEFINED);
        }
        let source = text("fromiso", &value)?;
        let source = source.trim();
        let bad = || op_error(format!("fromiso(): not an ISO 8601 date-time: {source:?}"));
        // With an offset (or `Z`) it is an instant; without one it is a time
        // in the render's zone, and a date alone is its midnight.
        let at = match source.parse::<Timestamp>() {
            Ok(ts) => ts,
            Err(_) => {
                let civil = source
                    .parse::<civil::DateTime>()
                    .or_else(|_| {
                        source
                            .parse::<civil::Date>()
                            .map(|d| d.to_datetime(civil::Time::midnight()))
                    })
                    .map_err(|_| bad())?;
                let zone = zone("fromiso", &ctx, None)?;
                civil.to_zoned(zone).map_err(|_| bad())?.timestamp()
            }
        };
        let (secs, nanos) = (at.as_second(), at.subsec_nanosecond());
        Ok(if nanos == 0 {
            MjValue::from(secs)
        } else {
            MjValue::from(secs as f64 + f64::from(nanos) / 1e9)
        })
    });
}

pub(super) fn add_ago(jinja: &mut Environment<'_>, ctx: &Arc<EngineCtx>) {
    let ctx = ctx.clone();
    jinja.add_filter(
        "ago",
        move |value: MjValue, args: Rest<MjValue>, kwargs: Kwargs| -> Result<MjValue, Error> {
            let [parts, floor] = arguments("ago", &args, &kwargs, ["parts", "floor"])?;
            let parts = parts_arg("ago", parts)?;
            let floor = floor_arg("ago", floor, Unit::Second)?;
            if is_absent(&value) {
                return Ok(UNDEFINED);
            }
            let at = number("ago", &value, true)?;
            let apart_ms = (ctx.now_millis() as f64 - at * 1000.0).round();
            let span = Duration::from_millis(apart_ms.abs() as u64);
            if span.as_millis() < u128::from(floor.millis()) {
                return Ok(MjValue::from("now"));
            }
            let shown = render::duration_with(span, parts, floor);
            Ok(MjValue::from(if apart_ms < 0.0 {
                format!("in {shown}")
            } else {
                format!("{shown} ago")
            }))
        },
    );
}

pub(super) fn add_spinner(jinja: &mut Environment<'_>, ctx: &Arc<EngineCtx>) {
    let ctx = ctx.clone();
    jinja.add_function(
        "spinner",
        move |args: Rest<MjValue>, kwargs: Kwargs| -> Result<MjValue, Error> {
            let [frames, step] = arguments("spinner", &args, &kwargs, ["frames", "step"])?;
            let frames = frames.ok_or_else(|| op_error("spinner(): takes frames"))?;
            let step = match step.as_ref().filter(|s| !is_absent(s)) {
                None => Duration::from_secs(1),
                Some(s) => {
                    duration::from_value(s).map_err(|e| op_error(format!("spinner(): step {e}")))?
                }
            };
            if step.is_zero() {
                return Err(op_error("spinner(): step must be at least 1ms"));
            }
            if is_absent(&frames) {
                return Ok(UNDEFINED);
            }
            let frames: Vec<MjValue> = match frames.kind() {
                ValueKind::String => frames
                    .as_str()
                    .unwrap_or_default()
                    .chars()
                    .map(|c| MjValue::from(c.to_string()))
                    .collect(),
                ValueKind::Seq => frames.try_iter()?.collect(),
                _ => return Err(op_error("spinner(): frames is a string or a list")),
            };
            if frames.is_empty() {
                return Err(op_error("spinner(): frames is empty"));
            }
            let tick = ctx.now_millis() / step.as_millis();
            Ok(frames[(tick % frames.len() as u128) as usize].clone())
        },
    );
}

/// The zone a clock item works in: its `tz=`, else the render's `TZ`, else
/// the system's.
fn zone(name: &str, ctx: &EngineCtx, tz: Option<&MjValue>) -> Result<TimeZone, Error> {
    let named = match tz.filter(|t| !is_absent(t)) {
        Some(t) => Some(
            t.as_str()
                .ok_or_else(|| op_error(format!("{name}(): tz must be a zone name")))?
                .to_string(),
        ),
        None => ctx.env.get("TZ").filter(|t| !t.is_empty()).cloned(),
    };
    match named {
        Some(zone) => {
            TimeZone::get(&zone).map_err(|_| op_error(format!("{name}(): unknown zone {zone:?}")))
        }
        None => Ok(TimeZone::system()),
    }
}

/// A time: seconds since the epoch, an integer or a float.
fn timestamp(name: &str, value: &MjValue) -> Result<Timestamp, Error> {
    let secs = number(name, value, true)?;
    Timestamp::from_nanosecond((secs * 1e9).round() as i128)
        .map_err(|_| op_error(format!("{name}(): {value} is outside the range of times")))
}

fn parts_arg(name: &str, parts: Option<MjValue>) -> Result<usize, Error> {
    match parts.filter(|p| !is_absent(p)) {
        None => Ok(2),
        Some(p) => usize::try_from(whole(name, "parts", &p)?)
            .ok()
            .filter(|n| *n >= 1)
            .ok_or_else(|| op_error(format!("{name}(): parts must be at least 1"))),
    }
}

fn floor_arg(name: &str, floor: Option<MjValue>, default: Unit) -> Result<Unit, Error> {
    match floor.as_ref().filter(|f| !is_absent(f)) {
        None => Ok(default),
        Some(f) => f
            .as_str()
            .and_then(Unit::from_name)
            .ok_or_else(|| op_error(format!("{name}(): floor must be one of d, h, m, s, ms"))),
    }
}

#[cfg(test)]
mod tests {
    use crate::library::testing::{eval, eval_at};

    /// 2026-10-07T03:00:00Z.
    const T: u64 = 1_791_342_000;

    /// The clock at `T` seconds, in UTC.
    fn at(expr: &str) -> Result<String, String> {
        eval_at(T * 1000, &[("TZ", "UTC")], expr)
    }

    #[test]
    fn now_is_the_renders_clock() {
        assert_eq!(at("now()").unwrap(), T.to_string());
        assert_eq!(at("now() - 3600").unwrap(), (T - 3600).to_string());
        assert_eq!(eval_at(1500, &[], "now()").unwrap(), "1");
        for bad in ["now(1)", "now(x=1)"] {
            let err = at(bad).unwrap_err();
            assert!(err.contains("now():"), "{bad}: {err}");
        }
    }

    #[test]
    fn strftime_formats_in_the_zone() {
        assert_eq!(at("now() | strftime('%H:%M')").unwrap(), "03:00");
        assert_eq!(
            at("now() | strftime('%F %T')").unwrap(),
            "2026-10-07 03:00:00"
        );
        // tz= wins over TZ.
        assert_eq!(
            at("now() | strftime('%F %H', tz='Australia/Sydney')").unwrap(),
            "2026-10-07 14"
        );
        assert_eq!(
            at("1 | strftime('%F %H:%M', tz='UTC')").unwrap(),
            "1970-01-01 00:00"
        );
        // The render's TZ decides without tz=.
        assert_eq!(
            eval_at(
                T * 1000,
                &[("TZ", "Australia/Sydney")],
                "now() | strftime('%H')"
            )
            .unwrap(),
            "14"
        );
        assert_eq!(at("1.5 | strftime('%S', tz='UTC')").unwrap(), "01");
        assert_eq!(at("(none | strftime('%F')) is undefined").unwrap(), "true");
    }

    #[test]
    fn strftime_checks_its_arguments() {
        for bad in [
            "1 | strftime",
            "1 | strftime(3)",
            "1 | strftime('%F', tz='Mars/Olympus')",
            "1 | strftime('%F', tz=3)",
            "1 | strftime('%')",
            "'soon' | strftime('%F')",
            "[1] | strftime('%F')",
            "none | strftime('%F', tz='Mars/Olympus')",
            "1 | strftime('%F', 'UTC', 3)",
        ] {
            let err = at(bad).unwrap_err();
            assert!(err.contains("strftime():"), "`{bad}` gave `{err}`");
        }
    }

    #[test]
    fn fromiso_reads_iso_8601() {
        assert_eq!(
            at("'2026-10-07T03:00:00Z' | fromiso").unwrap(),
            T.to_string()
        );
        assert_eq!(
            at("'2026-10-07T13:00:00+10:00' | fromiso").unwrap(),
            T.to_string()
        );
        // No offset: the render's zone.
        assert_eq!(
            at("'2026-10-07T03:00:00' | fromiso").unwrap(),
            T.to_string()
        );
        assert_eq!(
            eval_at(
                0,
                &[("TZ", "Australia/Sydney")],
                "'2026-10-07T14:00:00' | fromiso"
            )
            .unwrap(),
            T.to_string()
        );
        // A date alone is midnight.
        assert_eq!(
            at("'2026-10-07' | fromiso").unwrap(),
            (T - 3 * 3600).to_string()
        );
        // A fraction gives a float.
        assert_eq!(
            at("'2026-10-07T03:00:00.5Z' | fromiso").unwrap(),
            format!("{T}.5")
        );
        assert_eq!(
            at("'2026-10-07T03:00:00Z' | fromiso | strftime('%F')").unwrap(),
            "2026-10-07"
        );
    }

    #[test]
    fn fromiso_absent_empty_and_malformed() {
        assert_eq!(at("('' | fromiso) is undefined").unwrap(), "true");
        assert_eq!(at("(none | fromiso) is undefined").unwrap(), "true");
        assert_eq!(at("(missing | fromiso) is undefined").unwrap(), "true");
        for bad in [
            "'yesterday'",
            "'2026-13-45'",
            "'12:00'",
            "'2026-10-07T25:00:00Z'",
            "[1]",
        ] {
            let err = at(&format!("{bad} | fromiso")).unwrap_err();
            assert!(err.contains("fromiso():"), "{bad}: {err}");
        }
        // scanline's "a missing timestamp means now".
        assert_eq!(
            at("(missing | fromiso) | default(now(), true)").unwrap(),
            T.to_string()
        );
    }

    #[test]
    fn ago_is_the_distance_to_now() {
        assert_eq!(at("(now() - 11520) | ago").unwrap(), "3h12m ago");
        assert_eq!(at("(now() + 11520) | ago").unwrap(), "in 3h12m");
        assert_eq!(at("(now() - 90) | ago").unwrap(), "1m30s ago");
        assert_eq!(at("(now() - 11520) | ago(1)").unwrap(), "3h ago");
        assert_eq!(at("(now() - 11520) | ago(floor='h')").unwrap(), "3h ago");
        // Under the floor, now.
        assert_eq!(at("now() | ago").unwrap(), "now");
        assert_eq!(at("(now() - 30) | ago(floor='m')").unwrap(), "now");
        assert_eq!(at("(now() - 0.4) | ago").unwrap(), "now");
        assert_eq!(at("(now() - 0.4) | ago(floor='ms')").unwrap(), "400ms ago");
        assert_eq!(at("(none | ago) is undefined").unwrap(), "true");
        for bad in [
            "'x' | ago",
            "5 | ago(0)",
            "5 | ago(floor='w')",
            "none | ago(parts=0)",
        ] {
            let err = at(bad).unwrap_err();
            assert!(err.contains("ago():"), "`{bad}` gave `{err}`");
        }
    }

    #[test]
    fn spinner_picks_a_frame_by_the_clock() {
        let frame = |ms: u64, expr: &str| eval_at(ms, &[], expr).unwrap();
        for (ms, want) in [(0, "a"), (999, "a"), (1000, "b"), (2000, "c"), (3000, "a")] {
            assert_eq!(frame(ms, "spinner('abc')"), want, "{ms}");
        }
        assert_eq!(frame(250, "spinner('abc', '250ms')"), "b");
        assert_eq!(frame(500, "spinner('abc', step='250ms')"), "c");
        assert_eq!(frame(10_000, "spinner(['x', 'y'], 5)"), "x");
        assert_eq!(frame(5_000, "spinner(['x', 'y'], 5)"), "y");
        assert_eq!(frame(0, "spinner(none) is undefined"), "true");
        for bad in [
            "spinner('')",
            "spinner([])",
            "spinner('ab', '0ms')",
            "spinner('ab', 'soon')",
            "spinner(3)",
            "spinner()",
            "spinner('ab', '1s', 3)",
        ] {
            let err = eval_at(0, &[], bad).unwrap_err();
            assert!(err.contains("spinner():"), "`{bad}` gave `{err}`");
        }
    }

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
