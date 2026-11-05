//! The phase-6 scanline config renders scanline's recorded widget cells.
//!
//! `fixtures/scanline/10-scanline.toml` is the file scanline generates,
//! rewritten with phase 6's macros and library (the phase-6 plan, §5.4). Each
//! of its twelve widgets is rendered in process, over a map `Fetch` and with
//! the clock fixed, at each of the harness's states and widths
//! (`states.json`), and its cells are held to what the pre-phase-6 file drew
//! there (`cells.json`, see the fixtures' README): character, colours and
//! attributes, column by column. The rows where phase 6 deliberately draws
//! something else are [`PINNED`], each with its reason.
//!
//! The fetch answers as scanline's will: the drop-in's templates and macros
//! from the config, `scanline.*` data from the put, and the groups no width
//! changes ([`SHARED`]) resolved once per state and read as data after.
//!
//! The daemon's part of the file runs here too: the Sources' `result` lines
//! shape the status page and the worktree name from the bodies and the `.git`
//! file the harness served, through the one line evaluator a daemon uses.

use libbeachcomber::engine::{EngineCtx, build_engine_env};
use libbeachcomber::eval::eval_line;
use libbeachcomber::key;
use libbeachcomber::resolve::{Fetch, FetchError, Got, Inputs, Slot, resolve};
use serde_json::{Map, Value, json};
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/scanline");

/// The directory every read addresses: the session's locus. The fetch below
/// answers the same tree wherever it is asked.
const LOCUS: &str = "/scanline/locus";

/// The rows where the phase-6 file draws other cells than the recorded ones
/// on purpose (plan §5.2), and why. `cells.json` holds what each draws now.
const PINNED: &[(&str, &str)] = &[
    (
        "B/N/rate_7d/20",
        "§5.2.1: the time left drops a zero part, 6d0h is 6d",
    ),
    (
        "B/W/rate_7d/26",
        "§5.2.1: the time left drops a zero part, 6d0h is 6d",
    ),
];

fn read_json(name: &str) -> Value {
    let path = format!("{FIXTURES}/{name}");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path}: {e}"))
}

// ── The config ────────────────────────────────────────────────────────────────

/// What a load of the drop-in gives each side: the client's template tree
/// and macro libraries for `scanline`, and the daemon's Source lines.
struct Config {
    templates: Value,
    macros: Value,
    daemon: Value,
}

impl Config {
    fn load(path: &str) -> Self {
        let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path}: {e}"));
        let table: toml::Table = text.parse().unwrap_or_else(|e| panic!("{path}: {e}"));
        let doc = serde_json::to_value(table).unwrap();
        let scanline = &doc["client"]["providers"]["scanline"];
        Self {
            templates: scanline["template"].clone(),
            macros: scanline.get("macro").cloned().unwrap_or(json!({})),
            daemon: doc["daemon"]["providers"].clone(),
        }
    }

    /// The text of the daemon line at `path` (`provider.sources.source.line…`).
    fn line(&self, path: &str) -> &str {
        path.split('.')
            .fold(&self.daemon, |v, k| &v[k])
            .as_str()
            .unwrap_or_else(|| panic!("no daemon line {path}"))
    }
}

/// One daemon line over `data`, evaluated as a Source evaluates it.
fn source_line(src: &str, data: &Value, now: SystemTime) -> Option<Value> {
    let ctx = EngineCtx {
        now,
        ..EngineCtx::default()
    };
    let env = build_engine_env(Arc::new(ctx));
    eval_line(&env, src, &json!({ "data": data })).unwrap_or_else(|e| panic!("{src}: {e}"))
}

// ── The fetch ─────────────────────────────────────────────────────────────────

/// The groups no widget's width changes, which scanline resolves once per
/// render and answers as data from then on (scanline-one-tree §6.3).
const SHARED: &[&str] = &["theme", "status_page", "calc.windows"];

/// The daemon as one render sees it: `scanline`'s put under its template
/// tree, its macro libraries at `scanline:macro`, and each other provider's
/// data, at every directory. A `null` in data is absent. Once [`SHARED`] is
/// resolved, a read inside one of its groups is answered from what it gave.
struct Scanline<'a> {
    config: &'a Config,
    put: Value,
    providers: Map<String, Value>,
    shared: Vec<(Vec<String>, Value)>,
}

impl<'a> Scanline<'a> {
    fn new<const N: usize>(config: &'a Config, put: &Value, providers: [(&str, Value); N]) -> Self {
        Self {
            config,
            put: without_nulls(put),
            providers: providers
                .iter()
                .map(|(name, data)| (name.to_string(), without_nulls(data)))
                .collect(),
            shared: Vec::new(),
        }
    }
}

impl Fetch for Scanline<'_> {
    fn get(
        &mut self,
        key: &str,
        _path: Option<&str>,
        _force: bool,
        _wait: libbeachcomber::Wait,
    ) -> Result<Option<Got>, FetchError> {
        let (head, address) = key::split_key(key).map_err(FetchError::Rejected)?;
        let segments: Vec<String> = key::segments(&address)
            .into_iter()
            .map(|s| s.into_owned())
            .collect();
        let shared = self
            .shared
            .iter()
            .find(|(at, _)| head == "scanline" && segments.starts_with(at));
        let (data, template, rest) = match head {
            "scanline" if shared.is_some() => {
                let (at, value) = shared.unwrap();
                (walk(value, &segments[at.len()..]), None, None)
            }
            "scanline" => {
                let (template, rest) = walk_template(&self.config.templates, &segments);
                (walk(&self.put, &segments), template, rest)
            }
            "scanline:macro" => (walk(&self.config.macros, &segments), None, None),
            provider => (
                self.providers
                    .get(provider)
                    .and_then(|v| walk(v, &segments)),
                None,
                None,
            ),
        };
        if data.is_none() && template.is_none() {
            return Ok(None);
        }
        let age_ms = data.as_ref().map(|_| 0);
        Ok(Some(Got {
            data,
            template,
            rest,
            memo: None,
            age_ms,
        }))
    }
    fn memo(&mut self, _write: &libbeachcomber::resolve::MemoWrite) -> Result<(), FetchError> {
        Ok(())
    }
}

/// The data at `segments` below `v`, if any.
fn walk(mut v: &Value, segments: &[String]) -> Option<Value> {
    for s in segments {
        v = match v {
            Value::Object(m) => m.get(s)?,
            Value::Array(a) => a.get(key::array_index(s, a.len())?)?,
            _ => return None,
        };
    }
    (!v.is_null()).then(|| v.clone())
}

/// The template in effect at `segments`: the node there, or the string leaf
/// met on the way with the address left over past it.
fn walk_template(mut v: &Value, segments: &[String]) -> (Option<Value>, Option<String>) {
    for (i, s) in segments.iter().enumerate() {
        match v {
            Value::Object(m) => match m.get(s) {
                Some(next) => v = next,
                None => return (None, None),
            },
            Value::String(_) => {
                return (Some(v.clone()), Some(key::format_address(&segments[i..])));
            }
            _ => return (None, None),
        }
    }
    (Some(v.clone()), None)
}

/// Data as the daemon serves it, a put's or a Source's: `null` is absent, so
/// an ongoing incident has no `end` and `inc.end | default(t)` is `t`.
fn without_nulls(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k.clone(), without_nulls(v)))
                .collect(),
        ),
        Value::Array(a) => Value::Array(
            a.iter()
                .filter(|v| !v.is_null())
                .map(without_nulls)
                .collect(),
        ),
        other => other.clone(),
    }
}

// ── One state ─────────────────────────────────────────────────────────────────

struct State<'a> {
    name: &'a str,
    now: SystemTime,
    fetch: Scanline<'a>,
}

impl<'a> State<'a> {
    /// The daemon's values for `state`, from the drop-in's own Source lines.
    fn new(config: &'a Config, name: &'a str, state: &Value) -> Self {
        let now = UNIX_EPOCH + Duration::from_secs(state["now"].as_u64().unwrap());
        let worktree = source_line(
            config.line("gitstats.sources.worktree.result.worktree"),
            &state["git_file"],
            now,
        );
        assert_eq!(
            worktree.as_ref(),
            state["gitstats"].get("worktree"),
            "{name}: the worktree name the old Source gave"
        );
        let mut gitstats = Map::new();
        gitstats.extend(worktree.map(|w| ("worktree".to_string(), w)));
        let mut claude_status = Map::new();
        for (source, field, body) in [
            ("summary", "status", "summary"),
            ("incidents", "incidents", "incidents"),
        ] {
            let line = config.line(&format!("claude_status.sources.{source}.result.{field}"));
            if let Some(v) = source_line(line, &state[body], now) {
                claude_status.insert(field.to_string(), v);
            }
        }
        let mut state = Self {
            name,
            now,
            fetch: Scanline::new(
                config,
                &state["put"],
                [
                    ("git", state["git"].clone()),
                    ("gitstats", Value::Object(gitstats)),
                    ("claude_status", Value::Object(claude_status)),
                ],
            ),
        };
        for group in SHARED {
            // With no width in `env`: a shared group must not read one.
            let value = state
                .resolve(&format!("scanline.{group}"), None)
                .unwrap_or_else(|| panic!("{name}: scanline.{group} is absent"));
            let at = group.split('.').map(str::to_string).collect();
            state.fetch.shared.push((at, value));
        }
        state
    }

    /// `key`, as scanline asks for it (plan §5.3: the widget's width, `TERM`
    /// and `COLORTERM` in `env`).
    fn resolve(&mut self, key: &str, width: Option<u64>) -> Option<Value> {
        let mut env: HashMap<String, String> =
            [("TERM", "xterm-256color"), ("COLORTERM", "truecolor")]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
        env.extend(width.map(|w| ("SCANLINE_WIDTH".to_string(), w.to_string())));
        let inputs = Inputs {
            cwd: LOCUS,
            env: &env,
            slot: Slot::Path(LOCUS.to_string()),
            force: false,
            wait: libbeachcomber::Wait::Budget,
            allow_exec: false,
            now: self.now,
        };
        resolve(key, &inputs, &mut self.fetch)
            .unwrap_or_else(|e| panic!("{}: {key}@{width:?}: {e}", self.name))
    }

    /// `scanline.<widget>` at `width`. An absent value draws nothing.
    fn render(&mut self, widget: &str, width: u64) -> String {
        match self.resolve(&format!("scanline.{widget}"), Some(width)) {
            Some(Value::String(s)) => s,
            None => String::new(),
            Some(other) => panic!("{}: {widget}@{width} is not text: {other}", self.name),
        }
    }
}

// ── Cells ─────────────────────────────────────────────────────────────────────

/// A cell's look: its SGR attributes and colours.
#[derive(Default)]
struct Look {
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
    invert: bool,
    fg: Option<String>,
    bg: Option<String>,
}

impl Look {
    fn describe(&self) -> String {
        let flags = [
            (self.bold, "b"),
            (self.dim, "d"),
            (self.italic, "i"),
            (self.underline, "u"),
            (self.invert, "r"),
        ];
        let mut parts: Vec<String> = flags
            .iter()
            .filter(|(on, _)| *on)
            .map(|(_, f)| f.to_string())
            .collect();
        parts.extend(self.fg.as_ref().map(|c| format!("fg={c}")));
        parts.extend(self.bg.as_ref().map(|c| format!("bg={c}")));
        parts.join(" ")
    }

    /// Apply one SGR sequence's parameters; none is a reset.
    fn apply(&mut self, params: &str) {
        if params.is_empty() {
            *self = Look::default();
            return;
        }
        let mut ps = params
            .split(';')
            .map(|p| p.parse::<u8>().unwrap_or_else(|_| panic!("SGR {params}")));
        while let Some(p) = ps.next() {
            match p {
                0 => *self = Look::default(),
                1 => self.bold = true,
                2 => self.dim = true,
                3 => self.italic = true,
                4 => self.underline = true,
                7 => self.invert = true,
                22 => (self.bold, self.dim) = (false, false),
                23 => self.italic = false,
                24 => self.underline = false,
                27 => self.invert = false,
                30..=37 | 90..=97 => self.fg = Some(format!("sgr{p}")),
                40..=47 | 100..=107 => self.bg = Some(format!("sgr{p}")),
                39 => self.fg = None,
                49 => self.bg = None,
                38 | 48 => {
                    let colour = match ps.next() {
                        Some(2) => {
                            let mut rgb = [0u8; 3];
                            for c in &mut rgb {
                                *c = ps.next().unwrap_or_else(|| panic!("SGR {params}"));
                            }
                            format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])
                        }
                        Some(5) => {
                            format!("@{}", ps.next().unwrap_or_else(|| panic!("SGR {params}")))
                        }
                        _ => panic!("SGR {params}"),
                    };
                    if p == 38 {
                        self.fg = Some(colour);
                    } else {
                        self.bg = Some(colour);
                    }
                }
                _ => panic!("an SGR parameter this check does not read: {params}"),
            }
        }
    }
}

/// `text`'s cells, adjacent cells of one look joined into one run: each
/// run's characters and its look (`b d i u r fg=… bg=…`). Anything but SGR
/// fails, so nothing a terminal would draw is skipped.
fn cells(text: &str) -> Vec<(String, String)> {
    let mut runs: Vec<(String, String)> = Vec::new();
    let mut look = Look::default();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            assert_eq!(
                chars.next(),
                Some('['),
                "an escape other than CSI in {text:?}"
            );
            let mut params = String::new();
            let end = loop {
                match chars.next() {
                    Some(c @ '\x40'..='\x7e') => break c,
                    Some(c) => params.push(c),
                    None => panic!("an unterminated CSI in {text:?}"),
                }
            };
            assert_eq!(end, 'm', "a CSI other than SGR in {text:?}");
            look.apply(&params);
            continue;
        }
        assert!(!c.is_control(), "a control character in {text:?}");
        let described = look.describe();
        match runs.last_mut() {
            Some((run, at)) if *at == described => run.push(c),
            _ => runs.push((c.to_string(), described)),
        }
    }
    runs
}

fn runs_of(v: &Value) -> Vec<(String, String)> {
    v.as_array()
        .unwrap_or_else(|| panic!("not a list of runs: {v}"))
        .iter()
        .map(|run| {
            (
                run[0].as_str().unwrap().to_string(),
                run[1].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn chars_of(runs: &[(String, String)]) -> String {
    runs.iter().map(|(t, _)| t.as_str()).collect()
}

// ── The check ─────────────────────────────────────────────────────────────────

/// The recorded rows of `state`, `<state>/<width set>/<widget>/<width>`, each
/// with its widget and width.
fn rows<'a>(fixture: &'a Value, state: &str) -> Vec<(String, &'a str, u64)> {
    let mut rows = Vec::new();
    for (set, widths) in fixture["widths"].as_object().unwrap() {
        for entry in widths.as_array().unwrap() {
            let (widget, width) = (entry[0].as_str().unwrap(), entry[1].as_u64().unwrap());
            rows.push((format!("{state}/{set}/{widget}/{width}"), widget, width));
        }
    }
    rows
}

/// Every widget of `state` at every recorded width draws the recorded cells,
/// but for the [`PINNED`] rows, which draw their own.
fn draws_the_recorded_cells(state: &str) {
    let config = Config::load(&format!("{FIXTURES}/10-scanline.toml"));
    let fixture = read_json("states.json");
    let expected = read_json("cells.json");
    let mut render = State::new(&config, state, &fixture["states"][state]);
    let rows = rows(&fixture, state);
    let mut wrong = Vec::new();
    for (row, widget, width) in &rows {
        let got = cells(&render.render(widget, *width));
        let want = runs_of(
            expected["pinned"]
                .get(row)
                .unwrap_or(&expected["rows"][row]),
        );
        if got != want {
            wrong.push(format!(
                "{row}\n  want |{}|\n       {want:?}\n  got  |{}|\n       {got:?}",
                chars_of(&want),
                chars_of(&got)
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "{} rows differ:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
    assert_eq!(
        rows.len(),
        36,
        "{state}: twelve widgets at each of three width sets"
    );
}

/// A busy session: a quota bar with a reset, both rate windows, three
/// incidents, a worktree locus with a subdir, an active tool and agents,
/// cache stats.
#[test]
fn state_a_draws_the_recorded_cells() {
    draws_the_recorded_cells("A");
}

/// A balance overage, no quota, no components or incidents, no cache stats, a
/// long agent name, a clean tree with a parent delta.
#[test]
fn state_b_draws_the_recorded_cells() {
    draws_the_recorded_cells("B");
}

/// No rate windows, a quota with no reset time, two components (one status
/// the colour table does not know), an incident outside the window and one
/// with no timestamps.
#[test]
fn state_c_draws_the_recorded_cells() {
    draws_the_recorded_cells("C");
}

/// A "spent" overage, a 7-day window 30 s from its reset.
#[test]
fn state_d_draws_the_recorded_cells() {
    draws_the_recorded_cells("D");
}

/// A bare overage, no 5-hour window, a quota at 150% past its reset, 0%
/// context.
#[test]
fn state_e_draws_the_recorded_cells() {
    draws_the_recorded_cells("E");
}

/// The live status page's 50 incidents and 6 components, fetched 2026-10-26.
#[test]
fn state_r_draws_the_recorded_cells() {
    draws_the_recorded_cells("R");
}

/// The recording has every row of every state once, and pins only rows that
/// draw something other than their recorded cells, each with its reason.
#[test]
fn the_recording_covers_every_row_and_each_pin_has_a_reason() {
    let fixture = read_json("states.json");
    let expected = read_json("cells.json");
    let recorded = expected["rows"].as_object().unwrap();
    let pinned = expected["pinned"].as_object().unwrap();

    let states: Vec<&String> = fixture["states"].as_object().unwrap().keys().collect();
    let all: BTreeSet<String> = states
        .iter()
        .flat_map(|s| rows(&fixture, s))
        .map(|(row, _, _)| row)
        .collect();
    let widgets: BTreeSet<&str> = rows(&fixture, "A").into_iter().map(|(_, w, _)| w).collect();
    let sets = fixture["widths"].as_object().unwrap().len();
    assert_eq!(
        (states.len(), sets, widgets.len(), all.len()),
        (6, 3, 12, 216),
        "six states, three width sets, twelve widgets"
    );
    assert_eq!(
        recorded.keys().cloned().collect::<BTreeSet<_>>(),
        all,
        "cells.json records exactly the harness's rows"
    );

    assert_eq!(
        pinned.keys().map(String::as_str).collect::<BTreeSet<_>>(),
        PINNED.iter().map(|(row, _)| *row).collect::<BTreeSet<_>>(),
        "cells.json pins exactly the rows PINNED gives a reason for"
    );
    for (row, now) in pinned {
        assert_ne!(
            runs_of(now),
            runs_of(&recorded[row]),
            "{row} is pinned but draws its recorded cells"
        );
    }
}
