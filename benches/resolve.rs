//! The client-side resolver alone: no daemon, every answer from a map.
//!
//! `widget` is a status-line widget in the shape scanline's one-tree config
//! takes: a theme group of eleven leaves, a shared derived leaf that reads
//! the theme, and a width-dependent leaf that reads both. The widget reads
//! all three, so one resolution reads the theme four times and the shared
//! leaf twice. `env_read` is one render of one variable (`{{ env.X }}`): what
//! a resolution costs before any template work, and the floor a memoised
//! leaf's frame and hash are measured against. `exec` is one leaf that spawns
//! (`{{ 'echo x' | exec() }}`): what a leaf costs that a memo exists to skip.
//! Each runs with a one-variable environment and with a shell's worth (140
//! variables), since every render sees the whole environment and every spawn
//! is handed it.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use libbeachcomber::resolve::{Fetch, FetchError, Got, Inputs, Slot, resolve};
use serde_json::{Value, json};
use std::collections::HashMap;

/// The daemon's answers, by key; every path answers the same.
struct MapFetch(HashMap<&'static str, Got>);

impl Fetch for MapFetch {
    fn get(
        &mut self,
        key: &str,
        _path: Option<&str>,
        _force: bool,
        _wait: libbeachcomber::Wait,
    ) -> Result<Option<Got>, FetchError> {
        Ok(self.0.get(key).cloned())
    }
}

fn template(t: Value) -> Got {
    Got {
        template: Some(t),
        ..Got::default()
    }
}

fn data(d: Value) -> Got {
    Got {
        data: Some(d),
        age_ms: Some(100),
        stale: Some(false),
        ..Got::default()
    }
}

fn widget_fetch() -> MapFetch {
    let theme = json!({
        "green": "#a6e22e", "yellow": "#e6db74", "orange": "#fd971f", "red": "#f92672",
        "blue": "#66d9ef", "purple": "#ae81ff", "cyan": "#a1efe4", "dir": "#a0a090",
        "bar_empty": "#3e3d32", "bar_dark_fg": "#1a1a14", "bar_label_fg": "#ffffff",
    });
    let windows = r#"
{%- set th = scanline.theme -%}
{%- set ns = namespace(d={}) -%}
{%- for name, pct in [["five_hour", scanline.rate.five_hour], ["seven_day", scanline.rate.seven_day]] -%}
    {%- set colour = th.green if pct < 50 else (th.orange if pct < 80 else th.red) -%}
    {%- set ns.d = ns.d | merge({name: {"pct": pct, "colour": colour}}) -%}
{%- endfor -%}
{{- ns.d | tojson -}}
"#;
    let rate = r#"
{%- set w = env.SCANLINE_WIDTH | int -%}
{%- set th = scanline.theme -%}
{%- set win = scanline.calc.windows | fromjson -%}
{%- set ns = namespace(out=[]) -%}
{%- for name in ["five_hour", "seven_day"] -%}
    {%- set ns.out = ns.out + [{"w": w // 2, "fill": win[name].colour, "bg": th.bar_empty}] -%}
{%- endfor -%}
{{- ns.out | tojson -}}
"#;
    let widget = r#"
{%- set w = env.SCANLINE_WIDTH | int -%}
{%- set th = scanline.theme -%}
{%- set win = scanline.calc.windows | fromjson -%}
{%- set segs = scanline.calc.rate | fromjson -%}
{%- set c = namespace(out="") -%}
{%- for s in segs -%}
    {%- for _i in range(s.w) -%}
        {%- set c.out = c.out ~ (" " | fg(th.bar_label_fg) | bg(s.fill)) -%}
    {%- endfor -%}
{%- endfor -%}
{{- c.out ~ (win.five_hour.pct | string) -}}
"#;
    MapFetch(HashMap::from([
        (
            "scanline.width",
            template(json!("{{ env.SCANLINE_WIDTH }}")),
        ),
        ("scanline.widget", template(json!(widget))),
        ("scanline.spawn", template(json!("{{ 'echo x' | exec() }}"))),
        ("scanline.theme", template(theme)),
        ("scanline.calc.windows", template(json!(windows))),
        ("scanline.calc.rate", template(json!(rate))),
        (
            "scanline.rate",
            data(json!({ "five_hour": 30, "seven_day": 55 })),
        ),
    ]))
}

/// A shell's environment: `n` variables, one of them the widget's width.
fn shell_env(n: usize) -> HashMap<String, String> {
    let mut env: HashMap<String, String> = (1..n)
        .map(|i| {
            (
                format!("VAR_{i:03}"),
                format!("/some/typical/value/{i}/{}", "x".repeat(40)),
            )
        })
        .collect();
    env.insert("SCANLINE_WIDTH".into(), "24".into());
    env
}

fn bench_resolve(c: &mut Criterion) {
    let mut group = c.benchmark_group("resolve");
    for (name, key, allow_exec) in [
        ("widget", "scanline.widget", false),
        ("env_read", "scanline.width", false),
        ("exec", "scanline.spawn", true),
    ] {
        for vars in [1usize, 140] {
            let env = shell_env(vars);
            let mut fetch = widget_fetch();
            let inputs = Inputs {
                cwd: "/tmp",
                env: &env,
                slot: Slot::Cwd,
                force: false,
                wait: libbeachcomber::Wait::Budget,
                allow_exec,
                now: std::time::SystemTime::now(),
            };
            assert!(
                matches!(resolve(key, &inputs, &mut fetch), Ok(Some(_))),
                "{key} resolves"
            );
            group.bench_with_input(BenchmarkId::new(name, vars), &vars, |b, _| {
                b.iter(|| criterion::black_box(resolve(key, &inputs, &mut fetch)))
            });
        }
    }
    group.finish();
}

criterion_group!(benches, bench_resolve);
criterion_main!(benches);
