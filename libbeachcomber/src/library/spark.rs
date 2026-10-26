//! `spark`: a row of block glyphs for a list of numbers.

use super::{UNDEFINED, arguments, is_absent, number};
use crate::engine::{EngineCtx, op_error};
use minijinja::value::{Kwargs, Rest, Value as MjValue, ValueKind};
use minijinja::{Environment, Error};
use std::sync::Arc;

pub(super) fn add_spark(env: &mut Environment<'_>, _: &Arc<EngineCtx>) {
    env.add_function(
        "spark",
        |args: Rest<MjValue>, kwargs: Kwargs| -> Result<MjValue, Error> {
            let [values, lo, hi] = arguments("spark", &args, &kwargs, ["values", "lo", "hi"])?;
            spark(values, lo, hi)
        },
    );
}

/// Eight levels, lowest first.
const GLYPHS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// One side of the scale: a number, or `none` (or not given) for the data's own.
fn side(name: &str, v: Option<MjValue>) -> Result<Option<f64>, Error> {
    match v.filter(|v| !is_absent(v)) {
        None => Ok(None),
        Some(v) => number("spark", name, &v).map(Some),
    }
}

/// `spark(values, lo=none, hi=none)`: one glyph per value, `lo` the lowest and
/// `hi` the highest (the data's own minimum and maximum when `none`). A value
/// outside the scale is held to it, and a `none` value is a space. Equal ends
/// (all values the same) draw the middle glyph. The arguments are checked first.
fn spark(
    values: Option<MjValue>,
    lo: Option<MjValue>,
    hi: Option<MjValue>,
) -> Result<MjValue, Error> {
    let (lo, hi) = (side("lo", lo)?, side("hi", hi)?);
    if let (Some(l), Some(h)) = (lo, hi)
        && l > h
    {
        return Err(op_error(format!("spark(): lo {l} is above hi {h}")));
    }
    let Some(values) = values else {
        return Err(op_error("spark(): takes a list of values"));
    };
    if is_absent(&values) {
        return Ok(UNDEFINED);
    }
    if values.kind() != ValueKind::Seq {
        return Err(op_error(format!(
            "spark(): values must be a list, not {values}"
        )));
    }
    let mut points = Vec::new();
    for v in values.try_iter()? {
        points.push(if is_absent(&v) {
            None
        } else {
            Some(number("spark", "value", &v)?)
        });
    }
    let known = || points.iter().flatten().copied();
    let low = lo.or_else(|| known().reduce(f64::min));
    let high = hi.or_else(|| known().reduce(f64::max));
    let text: String = points
        .iter()
        .map(|p| match (p, low, high) {
            (None, ..) | (_, None, _) | (_, _, None) => ' ',
            (Some(v), Some(l), Some(h)) if h > l => {
                let level = ((v.clamp(l, h) - l) / (h - l) * 7.0).round();
                GLYPHS[level as usize]
            }
            _ => GLYPHS[GLYPHS.len() / 2],
        })
        .collect();
    Ok(MjValue::from(text))
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
    fn spark_scales_to_the_data() {
        assert_eq!(ok("spark([1, 5, 3, 8])"), "▁▅▃█");
        assert_eq!(ok("spark([0, 1, 2, 3, 4, 5, 6, 7])"), "▁▂▃▄▅▆▇█");
        assert_eq!(ok("spark([3, 1])"), "█▁");
        assert_eq!(ok("spark([-2, 0, 2])"), "▁▅█");
        assert_eq!(ok("spark([0.5, 1.5])"), "▁█");
        assert_eq!(ok("spark([7])"), "▅");
        assert_eq!(ok("spark([]) == ''"), "true");
    }

    #[test]
    fn spark_lo_and_hi_fix_the_scale() {
        assert_eq!(ok("spark([0, 50, 100], lo=0, hi=100)"), "▁▅█");
        assert_eq!(ok("spark([2, 4], lo=0, hi=8)"), "▃▅");
        assert_eq!(ok("spark([2, 4], 0, 8)"), "▃▅");
        // Outside the scale is held to it.
        assert_eq!(ok("spark([-5, 5, 500], lo=0, hi=10)"), "▁▅█");
        // One fixed side, the other the data's.
        assert_eq!(ok("spark([5, 10], lo=0)"), "▅█");
        assert_eq!(ok("spark([0, 5], hi=10)"), "▁▅");
        assert_eq!(ok("spark([1, 2], lo=none, hi=none)"), "▁█");
    }

    #[test]
    fn spark_of_equal_values_draws_the_middle() {
        assert_eq!(ok("spark([4, 4, 4])"), "▅▅▅");
        assert_eq!(ok("spark([4, 4], lo=4, hi=4)"), "▅▅");
        assert_eq!(ok("spark([0, 0])"), "▅▅");
    }

    #[test]
    fn spark_draws_a_none_value_as_a_space() {
        assert_eq!(ok("spark([1, none, 3])"), "▁ █");
        assert_eq!(ok("spark([none, none])"), "  ");
        assert_eq!(ok("spark([none, 2], lo=0, hi=4)"), " ▅");
    }

    #[test]
    fn spark_of_nothing_is_undefined_after_its_arguments_are_checked() {
        assert_eq!(ok("spark(none) is undefined"), "true");
        assert_eq!(ok("spark(nope) is undefined"), "true");
        let e = err("spark(none, lo=3, hi=1)");
        assert!(e.contains("spark(): lo 3 is above hi 1"), "{e}");
        let e = err("spark(none, lo='a')");
        assert!(e.contains("spark(): lo a is not a number"), "{e}");
    }

    #[test]
    fn spark_rejects_bad_input() {
        let e = err("spark(5)");
        assert!(e.contains("spark(): values must be a list"), "{e}");
        let e = err("spark('1,2')");
        assert!(e.contains("spark(): values must be a list"), "{e}");
        let e = err("spark([1, 'x'])");
        assert!(e.contains("spark(): value x is not a number"), "{e}");
        let e = err("spark([1], hi='x')");
        assert!(e.contains("spark(): hi x is not a number"), "{e}");
        let e = err("spark([1], lo=2, hi=1)");
        assert!(e.contains("spark(): lo 2 is above hi 1"), "{e}");
        let e = err("spark([1], 0, 1, 2)");
        assert!(e.contains("spark(): takes at most 3 arguments"), "{e}");
        let e = err("spark([1], top=1)");
        assert!(e.contains("spark(): unknown argument"), "{e}");
        let e = err("spark()");
        assert!(e.contains("spark(): takes a list of values"), "{e}");
    }
}
