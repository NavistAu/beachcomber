//! The template library: every filter and function a template can call, in one
//! table.
//!
//! [`ITEMS`] is the registry. Each item names itself, says whether it is a
//! filter, a function or both, says what it needs ([`Tier`]) and carries the
//! function that installs it. The engine builders install by tier, and a
//! surface that must do no IO (a provider's lines, a decorator render) is a
//! reading of the same table ([`Surface`], [`first_refused`]): no second list
//! of what is available where exists.
//!
//! The items live in modules by what they work on: [`style`] (colour, text),
//! [`data`] (objects and lists), [`path`] (path text), [`fs`] (files) and
//! [`parse`] (text to data). `exec` is bound to a render's process rules and
//! lives in [`crate::engine`]; `url` and `plugin` live in the daemon.

pub mod absent;
pub mod data;
pub mod fs;
pub mod parse;
pub mod path;
pub mod style;

use crate::engine::{EngineCtx, op_error};
use minijinja::value::{Kwargs, Rest, Value as MjValue};
use minijinja::{Environment, Error};
use std::sync::Arc;

/// What an item reads, and so where it may run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Its arguments, and the render `env` for `~` and the colour depth.
    Pure,
    /// Files.
    Io,
    /// Processes. A client render gates it on `allow_exec`.
    Exec,
    /// The network and plugins: only the daemon has them.
    Daemon,
}

/// Where a template runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    /// The client: a template leaf, `-f`, `comb eval`, `status` rows, SDKs.
    Client,
    /// A Source's `root`, `data` and `result` lines.
    Source,
    /// A provider's `template`, `result` and `return` lines.
    Provider,
    /// The decorator render of a Source's `return`.
    Decorator,
}

impl Tier {
    /// Whether an item of this tier is available on `surface`. A client's
    /// [`Tier::Daemon`] items exist only to refuse clearly: not available.
    pub fn available(self, surface: Surface) -> bool {
        matches!(
            (self, surface),
            (Tier::Pure, _)
                | (Tier::Io | Tier::Exec, Surface::Client | Surface::Source)
                | (Tier::Daemon, Surface::Source)
        )
    }
}

/// How an item is called.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Form {
    Filter,
    Function,
    /// Both spellings of one item (`merge`, `exec`).
    Both,
}

/// One entry of the library.
pub struct Item {
    pub name: &'static str,
    pub form: Form,
    pub tier: Tier,
    /// Installs the item into an environment, bound to the render's `ctx`.
    add: fn(&mut Environment<'_>, &Arc<EngineCtx>),
}

macro_rules! item {
    ($name:literal, $form:ident, $tier:ident, $add:expr) => {
        Item {
            name: $name,
            form: Form::$form,
            tier: Tier::$tier,
            add: $add,
        }
    };
}

/// Every item of the library, and the only list of them.
pub const ITEMS: &[Item] = &[
    item!("truncate", Filter, Pure, style::add_truncate),
    item!("fg", Filter, Pure, style::add_fg),
    item!("bg", Filter, Pure, style::add_bg),
    item!("bold", Filter, Pure, style::add_bold),
    item!("dim", Filter, Pure, style::add_dim),
    item!("basename", Filter, Pure, path::add_basename),
    item!("dirname", Filter, Pure, path::add_dirname),
    item!("pluck", Filter, Pure, data::add_pluck),
    item!("merge", Both, Pure, data::add_merge),
    item!("map", Filter, Pure, data::add_map),
    item!("fromjson", Filter, Pure, parse::add_fromjson),
    item!("fromini", Filter, Pure, parse::add_fromini),
    item!("read", Filter, Io, fs::add_read),
    item!("lup", Function, Io, fs::add_lup),
    item!("exec", Both, Exec, crate::engine::add_exec),
    item!("url", Function, Daemon, add_url_refusal),
    item!("plugin", Function, Daemon, add_plugin_refusal),
];

/// The item called `name`.
pub fn item(name: &str) -> Option<&'static Item> {
    ITEMS.iter().find(|i| i.name == name)
}

/// Install every item of `tier` into `env`.
pub(crate) fn install(env: &mut Environment<'_>, ctx: &Arc<EngineCtx>, tier: Tier) {
    for item in ITEMS.iter().filter(|i| i.tier == tier) {
        (item.add)(env, ctx);
    }
}

/// The first item `used` reports a use of that `surface` may not call.
pub fn first_refused(surface: Surface, used: impl Fn(&str) -> bool) -> Option<&'static Item> {
    ITEMS
        .iter()
        .find(|i| !i.tier.available(surface) && used(i.name))
}

/// A client has no `url` or `plugin`: it says why, rather than "unknown
/// function". The daemon's environment replaces both.
fn add_url_refusal(env: &mut Environment<'_>, _: &Arc<EngineCtx>) {
    env.add_function(
        "url",
        |_: Rest<MjValue>, _: Kwargs| -> Result<MjValue, Error> {
            Err(op_error("url() runs only in a Source's lines"))
        },
    );
}

fn add_plugin_refusal(env: &mut Environment<'_>, _: &Arc<EngineCtx>) {
    env.add_function(
        "plugin",
        |_: Rest<MjValue>, _: Kwargs| -> Result<MjValue, Error> {
            Err(op_error("plugin() runs only in a Source's lines"))
        },
    );
}

/// What an item gives when it has nothing to give: undefined, so `| default(x)`
/// fires and `~` joins it as nothing.
pub(crate) const UNDEFINED: MjValue = MjValue::UNDEFINED;

/// Whether `v` is absent: undefined or `none`.
pub(crate) fn is_absent(v: &MjValue) -> bool {
    v.is_undefined() || v.is_none()
}

/// Whether `v` is absent, or the empty string. The parse and path filters read
/// `''` as absent.
pub(crate) fn is_absent_text(v: &MjValue) -> bool {
    is_absent(v) || v.as_str() == Some("")
}

/// `v` as the text a text filter works on: a string is itself, a number or a
/// bool is its text, anything else is an error `name(): ...`.
pub(crate) fn text(name: &str, v: &MjValue) -> Result<String, Error> {
    use minijinja::value::ValueKind;
    match v.kind() {
        ValueKind::String => Ok(v.as_str().unwrap_or_default().to_string()),
        ValueKind::Number => Ok(v.to_string()),
        ValueKind::Bool => Ok(if v.is_true() { "true" } else { "false" }.to_string()),
        ValueKind::Seq | ValueKind::Iterable => {
            Err(op_error(format!("{name}(): expected text, not a list")))
        }
        ValueKind::Map => Err(op_error(format!("{name}(): expected text, not a map"))),
        _ => Err(op_error(format!("{name}(): expected text"))),
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use crate::engine::{EngineCtx, build_engine_env};
    use minijinja::Value as MjValue;
    use std::collections::HashMap;
    use std::sync::Arc;

    pub fn ctx_with(env: &[(&str, &str)], cwd: Option<&std::path::Path>) -> Arc<EngineCtx> {
        let mut map = HashMap::from([("PATH".to_string(), "/usr/bin:/bin".to_string())]);
        map.extend(env.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        Arc::new(EngineCtx {
            env: map,
            cwd: cwd.map(Into::into),
            allow_exec: true,
        })
    }

    pub fn eval_in(ctx: Arc<EngineCtx>, expr: &str) -> Result<String, String> {
        let env = build_engine_env(ctx);
        let compiled = env.compile_expression(expr).map_err(|e| e.to_string())?;
        let value = compiled
            .eval(MjValue::from_serialize(serde_json::json!({})))
            .map_err(|e| e.to_string())?;
        Ok(value.to_string())
    }

    /// The expression's value as compact JSON (keys sorted).
    pub fn eval_json(expr: &str) -> String {
        let env = build_engine_env(ctx_with(&[], None));
        let value = env
            .compile_expression(expr)
            .unwrap()
            .eval(MjValue::from_serialize(serde_json::json!({})))
            .unwrap();
        serde_json::to_string(&value).unwrap()
    }

    pub fn eval(expr: &str) -> Result<String, String> {
        eval_in(ctx_with(&[], None), expr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{build_engine_env, build_pure_engine_env};
    use minijinja::ErrorKind;

    fn has_filter(env: &Environment<'_>, name: &str) -> bool {
        env.empty_state()
            .apply_filter(name, &[])
            .map_or_else(|e| e.kind() != ErrorKind::UnknownFilter, |_| true)
    }

    fn has_function(env: &Environment<'_>, name: &str) -> bool {
        env.globals().any(|(n, _)| n == name)
    }

    fn present(env: &Environment<'_>, item: &Item) -> bool {
        match item.form {
            Form::Filter => has_filter(env, item.name),
            Form::Function => has_function(env, item.name),
            Form::Both => has_filter(env, item.name) && has_function(env, item.name),
        }
    }

    fn absent(env: &Environment<'_>, item: &Item) -> bool {
        match item.form {
            Form::Filter => !has_filter(env, item.name),
            Form::Function => !has_function(env, item.name),
            Form::Both => !has_filter(env, item.name) && !has_function(env, item.name),
        }
    }

    #[test]
    fn every_item_is_on_its_surfaces_and_refused_elsewhere() {
        let pure = build_pure_engine_env(Default::default());
        let client = build_engine_env(Default::default());
        for item in ITEMS {
            // The pure builder is the provider and decorator surfaces.
            for surface in [Surface::Provider, Surface::Decorator] {
                if item.tier.available(surface) {
                    assert!(present(&pure, item), "{} missing on {surface:?}", item.name);
                } else {
                    assert!(absent(&pure, item), "{} leaked onto {surface:?}", item.name);
                }
            }
            // The client has every item, the daemon's as a clear refusal.
            assert!(
                present(&client, item),
                "{} missing on the client",
                item.name
            );
            if item.tier == Tier::Daemon {
                assert!(!item.tier.available(Surface::Client));
                let err = client
                    .compile_expression(&format!("{}()", item.name))
                    .unwrap()
                    .eval(())
                    .unwrap_err()
                    .to_string();
                assert!(
                    err.contains(&format!("{}() runs only in a Source's lines", item.name)),
                    "{err}"
                );
            } else {
                assert!(item.tier.available(Surface::Client), "{}", item.name);
            }
            assert!(item.tier.available(Surface::Source), "{}", item.name);
        }
    }

    /// Every error an item raises for bad input reads `name(): why`.
    #[test]
    fn every_error_is_name_then_why() {
        use super::testing::eval;
        for (name, bad) in [
            ("truncate", "[1] | truncate(3)"),
            ("fg", "'x' | fg('nope')"),
            ("bg", "'x' | bg('nope')"),
            ("bold", "{} | bold"),
            ("dim", "[1] | dim"),
            ("basename", "[1] | basename"),
            ("dirname", "{} | dirname"),
            ("pluck", "{} | pluck('a[')"),
            ("map", "{'a': 1} | map(attribute='x')"),
            ("fromjson", "'{' | fromjson"),
            ("fromini", "[1] | fromini"),
            ("read", "[1] | read"),
            ("lup", "lup('x', files=true, dirs=true)"),
            ("exec", "exec(1)"),
        ] {
            let err = eval(bad).unwrap_err();
            let why = err.strip_prefix("invalid operation: ").unwrap_or(&err);
            assert!(
                why.starts_with(&format!("{name}(): ")),
                "{name}: `{bad}` gave `{err}`"
            );
        }
    }

    #[test]
    fn a_name_is_listed_once() {
        for item in ITEMS {
            assert_eq!(
                ITEMS.iter().filter(|i| i.name == item.name).count(),
                1,
                "{} listed twice",
                item.name
            );
            assert_eq!(super::item(item.name).unwrap().name, item.name);
        }
    }

    #[test]
    fn first_refused_reads_the_tier_table() {
        let used = |n: &str| n == "read" || n == "dirname";
        assert_eq!(first_refused(Surface::Provider, used).unwrap().name, "read");
        assert!(first_refused(Surface::Source, used).is_none());
        assert!(first_refused(Surface::Decorator, |n| n == "dirname").is_none());
    }

    #[test]
    fn minijinja_builtins_still_work_on_the_pure_env() {
        let env = build_pure_engine_env(Default::default());
        let out = env
            .render_str(
                "{{ missing | default('fallback') }} {{ s | upper }}",
                minijinja::context!(s => "abc"),
            )
            .unwrap();
        assert_eq!(out, "fallback ABC");
    }
}
