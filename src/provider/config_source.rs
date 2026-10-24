//! `ConfigSource`: a Source whose body is jinja.
//!
//! `root` says which directory an instance answers for, `data` is the body
//! (one engine call, cwd = the root, env = the daemon's own), `result` is the
//! field lines over `data`. The schedule comes from the Source's `return`
//! chain, already recorded as [`Recorded`].
//!
//! Each line sees only what its stage may (the spec's evaluation table), and
//! every line also sees `_`, the prototype's own name (`_.name`):
//!
//! | line | sees |
//! |---|---|
//! | `root` | `env`, `_` |
//! | `data` | `env`, `comb`, `sources`, `daemon`, `_` |
//! | `result.*` | `data`, `_` |
//!
//! A name outside that list is a load error, not a silent `none`.
//!
//! `data` calls other Sources: `sources.<s>()` for a sibling and
//! `daemon.providers.<p>.sources.<s>()` for any provider's. A call answers
//! with the called Source's record as a read from this instance's directory
//! would find it (see [`crate::provider::source_call`]); one that has nothing
//! stored is run inline, once. A Source cannot call itself, and a cycle through
//! others is an error when it runs.

use crate::engine::{CallFn, Calls, PluginSet, daemon_engine_env};
use crate::provider::source_call::{Called, Running, called_in};
use crate::provider::{
    FieldSchema, FieldType, InvalidationStrategy, Source, SourceMetadata, SourceResult, Value,
};
use crate::shape::decorators::Recorded;
use libbeachcomber::engine::EngineCtx;
use libbeachcomber::eval::{discover_refs, eval_line, render_none_as_empty, undeclared_names};
use libbeachcomber::virtual_fields::Ref;
use minijinja::Environment;
use serde_json::{Value as Json, json};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A daemon-side read of one provider node at a directory entry: put over
/// source, up the directory chain, never a template. What `comb.<p>` sees.
///
/// `read` only reads stored records and never executes anything: a Source's
/// `data` line runs inside a refresh, and a read that ran Sources could
/// recurse into the one being refreshed. So `comb.<own provider>` in a `data`
/// line is that provider's own last stored value, never a fresh run.
pub trait DaemonRead: Send + Sync {
    /// `Ok(None)` is nothing stored. `Err` is a read that could not be
    /// answered (a root that failed to work out); it fails the `data` render.
    fn read(
        &self,
        provider: &str,
        dir: Option<&str>,
    ) -> Result<Option<HashMap<String, Value>>, String>;

    /// A run of the Source at `dir` has ended having called `called`: what the
    /// daemon takes that instance to call from now on, in place of what its
    /// last run called.
    fn ran_calling(
        &self,
        provider: &str,
        source: &str,
        dir: Option<&str>,
        called: crate::provider::registry::CalleeMap,
    );

    /// The record of `provider`'s Source `source` as a read from `dir` would
    /// find it (`None`: nothing stored), running a cold instance inline once.
    /// The caller is a `data` line, on a blocking thread.
    fn call(&self, provider: &str, source: &str, dir: Option<&str>)
    -> Result<Option<Json>, String>;
}

/// The `result` of a Source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResultSpec {
    /// One line over `data` whose object owns the provider node.
    Whole(String),
    /// `(field, line)` pairs.
    Fields(Vec<(String, String)>),
}

#[derive(Debug, Clone)]
pub struct ConfigSourceSpec {
    pub provider: String,
    pub name: String,
    /// Jinja line, already prototype-resolved.
    pub root: Option<String>,
    pub data: String,
    pub result: ResultSpec,
    pub recorded: Recorded,
}

const ROOT_NAMES: &[&str] = &["env", "_"];
const DATA_NAMES: &[&str] = &["env", "comb", "sources", "daemon", "_"];
const RESULT_NAMES: &[&str] = &["data", "_"];

pub struct ConfigSource {
    spec: ConfigSourceSpec,
    meta: SourceMetadata,
    env: Arc<HashMap<String, String>>,
    plugins: Arc<PluginSet>,
    reader: Arc<dyn DaemonRead>,
    /// The providers the `data` line reads through `comb`, each once.
    comb_providers: Vec<String>,
    /// The Sources the `data` line calls.
    calls: Vec<Called>,
}

/// `<provider>.<source>: <stage>: <message>`: the one shape of every error,
/// at load and at run.
fn stage_error(spec: &ConfigSourceSpec, stage: &str, message: impl std::fmt::Display) -> String {
    format!(
        "{}.{}: {stage}: {}",
        spec.provider,
        spec.name,
        without_engine_frame(&message.to_string())
    )
}

/// `message` without the frame the engine put round an error it raised: a
/// leading `invalid operation: ` and a trailing ` (in <expression>:1)`. Only
/// those two places, once: the text between is the error's own, a command's
/// stderr included, and is not touched. A render that fails because a Source it
/// calls failed would otherwise show the frame once per Source down the chain.
fn without_engine_frame(message: &str) -> String {
    let body = message
        .strip_prefix("invalid operation: ")
        .unwrap_or(message);
    if let Some(open) = body.rfind(" (in <")
        && let Some(rest) = body[open..].strip_suffix(')')
        && let Some((place, line)) = rest[" (in <".len()..].split_once(">:")
        && !place.is_empty()
        && place.chars().all(|c| c.is_ascii_lowercase() || c == '-')
        && !line.is_empty()
        && line.chars().all(|c| c.is_ascii_digit())
    {
        return body[..open].to_string();
    }
    body.to_string()
}

/// The load check of a Source's lines: every name a line reads is one its
/// stage sees, and `data` reads `comb` the one way it can. `who` prefixes the
/// error (`demo.s`, or a `_` table's path); any line may be absent, as in a
/// prototype. Nothing is rendered.
pub fn lint_lines(
    who: &str,
    root: Option<&str>,
    data: Option<&str>,
    result: Option<&ResultSpec>,
    env: &Arc<HashMap<String, String>>,
    plugins: &Arc<PluginSet>,
) -> Result<(), String> {
    let probe = engine(env, plugins, None);
    let mut lines: Vec<(String, &str, &[&str])> = Vec::new();
    if let Some(root) = root {
        lines.push(("root".into(), root, ROOT_NAMES));
    }
    if let Some(data) = data {
        lines.push(("data".into(), data, DATA_NAMES));
    }
    match result {
        Some(ResultSpec::Whole(line)) => lines.push(("result".into(), line, RESULT_NAMES)),
        Some(ResultSpec::Fields(pairs)) => lines.extend(
            pairs
                .iter()
                .map(|(n, l)| (format!("result.{n}"), l.as_str(), RESULT_NAMES)),
        ),
        None => {}
    }
    for (stage, line, allowed) in lines {
        let names =
            undeclared_names(&probe, line, false).map_err(|e| format!("{who}: {stage}: {e}"))?;
        if let Some(name) = names.iter().find(|n| !allowed.contains(&n.as_str())) {
            return Err(format!(
                "{who}: {stage}: unknown name '{name}' (this line sees {})",
                allowed.join(", ")
            ));
        }
    }
    if let Some(data) = data {
        let used = undeclared_names(&probe, data, true).map_err(|e| format!("{who}: data: {e}"))?;
        if used.contains("comb") {
            return Err(format!(
                "{who}: data: comb is read as comb.<provider>[.<field>]; comb[...] and comb as \
                 a whole are not supported"
            ));
        }
        if used.contains("sources") {
            return Err(format!(
                "{who}: data: a sibling Source is called, sources.<source>(); sources[...] and \
                 sources as a whole are not supported"
            ));
        }
        called_in(&probe, data).map_err(|e| format!("{who}: data: {e}"))?;
    }
    Ok(())
}

/// What a value is, for an error message that must not carry the value (a
/// `url()` body can carry tokens).
fn kind(value: &Json) -> &'static str {
    match value {
        Json::Null => "none",
        Json::Bool(_) => "a bool",
        Json::Number(_) => "a number",
        Json::String(_) => "a string",
        Json::Array(_) => "a list",
        Json::Object(_) => "a map",
    }
}

impl ConfigSource {
    /// What the Source is made of apart from when it runs: its `root`, `data`
    /// and `result` lines as resolved, `_` prototypes included. Two Sources
    /// with the same definition compute the same records.
    pub fn definition(&self) -> String {
        format!(
            "root: {:?}\ndata: {:?}\nresult: {:?}",
            self.spec.root, self.spec.data, self.spec.result
        )
    }

    pub fn new(
        spec: ConfigSourceSpec,
        env: Arc<HashMap<String, String>>,
        plugins: Arc<PluginSet>,
        reader: Arc<dyn DaemonRead>,
    ) -> Result<Self, String> {
        let fields = match &spec.result {
            ResultSpec::Whole(_) => vec![FieldSchema {
                name: "<node>".into(),
                field_type: FieldType::Object,
            }],
            ResultSpec::Fields(pairs) if pairs.is_empty() => {
                return Err(stage_error(&spec, "result", "declares no fields"));
            }
            ResultSpec::Fields(pairs) => {
                let mut fields = Vec::new();
                for (name, _) in pairs {
                    if name.is_empty() || name.contains('.') || name.starts_with('<') {
                        return Err(stage_error(
                            &spec,
                            "result",
                            format!(
                                "field name '{name}' must be non-empty, without '.', \
                                 and not start with '<'"
                            ),
                        ));
                    }
                    fields.push(FieldSchema {
                        name: name.clone(),
                        // Nominal for config Sources: a single-tag line keeps
                        // its runtime type (an int, bool or float), and
                        // nothing validates a field against its declared
                        // type, until phase 7 ships Sources as config.
                        field_type: FieldType::String,
                    });
                }
                fields
            }
        };

        lint_lines(
            &format!("{}.{}", spec.provider, spec.name),
            spec.root.as_deref(),
            Some(&spec.data),
            Some(&spec.result),
            &env,
            &plugins,
        )?;

        let comb_providers: Vec<String> = discover_refs(&spec.data)
            .into_iter()
            .filter_map(|r| match r {
                Ref::CombProvider(p) | Ref::CombField(p, _) => Some(p),
                _ => None,
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();

        let who = format!("{}.{}", spec.provider, spec.name);
        let calls = called_in(&engine(&env, &plugins, None), &spec.data)
            .map_err(|e| format!("{who}: data: {e}"))?;
        let itself = |c: &Called| match c {
            Called::Sibling(s) => *s == spec.name,
            Called::Of(p, s) => *p == spec.provider && *s == spec.name,
        };
        if calls.iter().any(itself) {
            return Err(format!("{who}: data: {who} cannot call itself"));
        }

        let recorded = &spec.recorded;
        if spec.root.is_none()
            && let Some(problem) = crate::provider::one_instance_problem(
                &format!("{}.{}", spec.provider, spec.name),
                &recorded.invalidation,
                recorded.nearest,
            )
        {
            return Err(stage_error(&spec, "return", problem));
        }
        let meta = SourceMetadata {
            name: spec.name.clone(),
            fields,
            invalidation: recorded.invalidation.clone(),
            failback: recorded.failback,
            fsevents_reinstate: !matches!(recorded.invalidation, InvalidationStrategy::Poll { .. }),
            decay: recorded.decay,
            budget_execute: recorded.budget_execute,
            nearest: recorded.nearest,
        };
        Ok(Self {
            spec,
            meta,
            env,
            plugins,
            reader,
            comb_providers,
            calls,
        })
    }

    /// The Sources the `data` line calls.
    pub fn calls(&self) -> &[Called] {
        &self.calls
    }

    /// The `~` instance's directory: the daemon's own `HOME`.
    fn home(&self) -> Option<&str> {
        self.env.get("HOME").map(String::as_str)
    }

    fn underscore(&self) -> Json {
        json!({"name": self.spec.name})
    }

    /// The `data` line's `comb`: one daemon read at `dir` per provider the
    /// line names; a provider with nothing stored is not bound.
    fn comb(&self, dir: Option<&str>) -> Result<Json, String> {
        let mut comb = serde_json::Map::new();
        for provider in &self.comb_providers {
            let read = self
                .reader
                .read(provider, dir)
                .map_err(|e| stage_error(&self.spec, "data", format!("comb.{provider}: {e}")))?;
            if let Some(node) = read {
                let node = serde_json::to_value(node).map_err(|e| {
                    stage_error(
                        &self.spec,
                        "data",
                        format!("comb.{provider} is not representable: {e}"),
                    )
                })?;
                comb.insert(provider.clone(), node);
            }
        }
        Ok(comb.into())
    }

    /// Run the lines, and when the run ends, however it ends, say what it
    /// called: that, and no more, is what this instance calls.
    fn run(&self, path: Option<&str>) -> Result<SourceResult, String> {
        let running = Running::enter(&self.spec.provider, &self.spec.name);
        let result = self.run_lines(path);
        if !self.calls.is_empty() {
            self.reader.ran_calling(
                &self.spec.provider,
                &self.spec.name,
                path,
                running.take_calls(),
            );
        }
        result
    }

    fn run_lines(&self, path: Option<&str>) -> Result<SourceResult, String> {
        let cwd: PathBuf = match path {
            Some(p) => p.into(),
            None => self
                .home()
                .ok_or_else(|| {
                    stage_error(
                        &self.spec,
                        "cwd",
                        "the daemon has no HOME for the ~ instance",
                    )
                })?
                .into(),
        };
        let mut jinja = engine(&self.env, &self.plugins, Some(cwd));
        if !self.calls.is_empty() {
            let (reader, dir) = (self.reader.clone(), path.map(str::to_string));
            let call: CallFn =
                Arc::new(move |provider, source| reader.call(provider, source, dir.as_deref()));
            jinja.add_global(
                "sources",
                Calls::sources_of(&self.spec.provider, call.clone()),
            );
            jinja.add_global("daemon", Calls::daemon(call));
        }
        let data_context = json!({
            "env": &*self.env,
            "comb": self.comb(path)?,
            "_": self.underscore(),
        });
        let data = eval_line(&jinja, &self.spec.data, &data_context)
            .map_err(|e| stage_error(&self.spec, "data", e))?;
        let result_context = json!({"data": data, "_": self.underscore()});

        match &self.spec.result {
            ResultSpec::Whole(line) => {
                let stage = |e| stage_error(&self.spec, "result", e);
                match eval_line(&jinja, line, &result_context).map_err(stage)? {
                    Some(Json::Object(map)) => Ok(SourceResult::from_json_object(&map)),
                    None => Ok(SourceResult::new()),
                    Some(other) => Err(stage(format!(
                        "a whole node must be an object, got {}",
                        kind(&other)
                    ))),
                }
            }
            ResultSpec::Fields(pairs) => {
                let mut out = SourceResult::new();
                for (name, line) in pairs {
                    let json = eval_line(&jinja, line, &result_context)
                        .map_err(|e| stage_error(&self.spec, &format!("result.{name}"), e))?;
                    if let Some(v) = json.as_ref().and_then(Value::from_json) {
                        out.insert(name.clone(), v);
                    }
                }
                Ok(out)
            }
        }
    }
}

impl Source for ConfigSource {
    fn metadata(&self) -> &SourceMetadata {
        &self.meta
    }

    /// A render error is a failure (`Err`): the scheduler's failure accounting
    /// turns it into `failback`. A `result` that renders nothing is a result:
    /// the instance stores no fields.
    fn execute(&self, dir: Option<&str>) -> Result<SourceResult, String> {
        self.run(dir)
    }

    /// The `root` line rendered with cwd = `dir`: `Ok(Some(directory))`, or
    /// `Ok(None)` for the `~` instance (no root line, or it renders empty or
    /// none). A line that fails to render, or renders something other than an
    /// absolute directory, is an `Err`: never the instance at `~`.
    fn root(&self, dir: &str) -> Result<Option<String>, String> {
        let Some(line) = self.spec.root.as_deref() else {
            return Ok(None);
        };
        let jinja = engine(&self.env, &self.plugins, Some(dir.into()));
        let context = json!({"env": &*self.env, "_": self.underscore()});
        let rendered =
            eval_line(&jinja, line, &context).map_err(|e| stage_error(&self.spec, "root", e))?;
        match rendered {
            None => Ok(None),
            Some(Json::String(s)) if s.is_empty() => Ok(None),
            Some(Json::String(s)) if Path::new(&s).is_absolute() => Ok(Some(s)),
            Some(other) => {
                let got = match other {
                    Json::String(_) => "a relative path",
                    other => kind(&other),
                };
                Err(stage_error(
                    &self.spec,
                    "root",
                    format!("must render an absolute directory, got {got}"),
                ))
            }
        }
    }

    fn has_root(&self) -> bool {
        self.spec.root.is_some()
    }
}

/// The daemon's engine for a Source's lines, `exec` on, `cwd` its directory.
fn engine<'a>(
    env: &HashMap<String, String>,
    plugins: &Arc<PluginSet>,
    cwd: Option<PathBuf>,
) -> Environment<'a> {
    let ctx = EngineCtx {
        env: env.clone(),
        cwd,
        allow_exec: true,
    };
    let mut jinja = daemon_engine_env(Arc::new(ctx), plugins.clone());
    render_none_as_empty(&mut jinja);
    jinja
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boundaries::library::MockLibraryLoader;
    use crate::provider::FailbackConfig;
    use std::sync::Mutex;
    use std::time::Duration;

    #[derive(Default)]
    struct FakeReader {
        calls: Mutex<Vec<(String, Option<String>)>>,
        answer: HashMap<String, HashMap<String, Value>>,
        /// `(provider, source, dir)` of each Source call.
        called: Mutex<Vec<(String, String, Option<String>)>>,
        records: HashMap<(String, String), Json>,
    }

    impl DaemonRead for FakeReader {
        fn ran_calling(
            &self,
            _: &str,
            _: &str,
            _: Option<&str>,
            _: crate::provider::registry::CalleeMap,
        ) {
        }

        fn call(
            &self,
            provider: &str,
            source: &str,
            dir: Option<&str>,
        ) -> Result<Option<Json>, String> {
            self.called.lock().unwrap().push((
                provider.to_string(),
                source.to_string(),
                dir.map(str::to_string),
            ));
            Ok(self
                .records
                .get(&(provider.to_string(), source.to_string()))
                .cloned())
        }

        fn read(
            &self,
            provider: &str,
            dir: Option<&str>,
        ) -> Result<Option<HashMap<String, Value>>, String> {
            self.calls
                .lock()
                .unwrap()
                .push((provider.to_string(), dir.map(str::to_string)));
            Ok(self.answer.get(provider).cloned())
        }
    }

    fn recorded() -> Recorded {
        Recorded {
            invalidation: InvalidationStrategy::Poll {
                interval_secs: 60,
                count: 12,
            },
            failback: FailbackConfig {
                reattempts: 3,
                interval_secs: 1,
            },
            budget_execute: Duration::from_millis(50),
            decay: None,
            nearest: false,
        }
    }

    fn spec(root: Option<&str>, data: &str, result: ResultSpec) -> ConfigSourceSpec {
        ConfigSourceSpec {
            provider: "demo".into(),
            name: "s".into(),
            root: root.map(str::to_string),
            data: data.into(),
            result,
            recorded: recorded(),
        }
    }

    fn fields(pairs: &[(&str, &str)]) -> ResultSpec {
        ResultSpec::Fields(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }

    fn daemon_env() -> Arc<HashMap<String, String>> {
        Arc::new(std::env::vars().collect())
    }

    fn build(spec: ConfigSourceSpec, reader: Arc<dyn DaemonRead>) -> ConfigSource {
        ConfigSource::new(
            spec,
            daemon_env(),
            Arc::new(PluginSet::new(vec![], Arc::new(MockLibraryLoader::new()))),
            reader,
        )
        .unwrap()
    }

    fn source(spec: ConfigSourceSpec) -> ConfigSource {
        build(spec, Arc::new(FakeReader::default()))
    }

    fn s(v: &str) -> Value {
        Value::String(v.into())
    }

    fn with_env(spec: ConfigSourceSpec, snapshot: HashMap<String, String>) -> ConfigSource {
        ConfigSource::new(
            spec,
            Arc::new(snapshot),
            Arc::new(PluginSet::new(vec![], Arc::new(MockLibraryLoader::new()))),
            Arc::new(FakeReader::default()),
        )
        .unwrap()
    }

    fn new_err(spec: ConfigSourceSpec) -> String {
        match ConfigSource::new(
            spec,
            daemon_env(),
            Arc::new(PluginSet::new(vec![], Arc::new(MockLibraryLoader::new()))),
            Arc::new(FakeReader::default()),
        ) {
            Ok(_) => panic!("expected a load error"),
            Err(e) => e,
        }
    }

    fn text(got: &HashMap<String, Value>, field: &str) -> String {
        match got.get(field) {
            Some(Value::String(s)) => s.clone(),
            other => panic!("{field}: not a string: {other:?}"),
        }
    }

    #[test]
    fn data_runs_in_the_root_with_the_daemon_env() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        temp_env::with_var("X", Some("daemon-x"), || {
            let src = source(spec(
                None,
                "{{ exec('pwd') ~ ':' ~ env.X }}",
                fields(&[("out", "{{ data }}")]),
            ));
            let got = src.execute(Some(root.to_str().unwrap())).unwrap();
            assert_eq!(
                text(&got.fields, "out").trim(),
                format!("{}:daemon-x", root.display())
            );
        });
    }

    #[test]
    fn env_is_the_daemons_never_a_callers() {
        temp_env::with_var("X", Some("daemon-x"), || {
            let src = source(spec(
                None,
                "{{ env.X ~ '/' ~ exec('echo $X') }}",
                fields(&[("out", "{{ data }}")]),
            ));
            // The environment moves on after the snapshot; the Source keeps
            // the daemon's own.
            temp_env::with_var("X", Some("changed"), || {
                let got = src.execute(None).unwrap();
                assert_eq!(text(&got.fields, "out").trim(), "daemon-x/daemon-x");
            });
        });
    }

    #[test]
    fn execute_at_home_is_the_home_instance() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().canonicalize().unwrap();
        let mut snapshot: HashMap<String, String> = (*daemon_env()).clone();
        snapshot.insert("HOME".into(), home.to_str().unwrap().into());
        let src = with_env(
            spec(
                Some(""),
                "{{ exec('pwd') }}",
                fields(&[("out", "{{ data }}")]),
            ),
            snapshot,
        );
        // Production passes Some($HOME) for the global entry, not None.
        let got = src.execute(Some(home.to_str().unwrap())).unwrap();
        assert_eq!(text(&got.fields, "out").trim(), home.to_str().unwrap());
        assert_eq!(src.execute(None).unwrap().fields, got.fields);
    }

    #[test]
    fn result_fields_type_naturally() {
        let src = source(spec(
            None,
            "{{ 1 }}",
            fields(&[
                ("i", "{{ 1 }}"),
                ("a", "{{ 'a' }}"),
                ("m", "x{{ 1 }}"),
                ("l", "lit"),
            ]),
        ));
        let got = src.execute(None).unwrap().fields;
        assert_eq!(got.get("i"), Some(&Value::Int(1)));
        assert_eq!(got.get("a"), Some(&s("a")));
        assert_eq!(got.get("m"), Some(&s("x1")));
        assert_eq!(got.get("l"), Some(&s("lit")));
    }

    #[test]
    fn a_whole_node_source_declares_the_node_sentinel() {
        let src = source(spec(
            None,
            "{{ {'a': 1, 'b': 'two'} }}",
            ResultSpec::Whole("{{ data }}".into()),
        ));
        let names: Vec<_> = src.metadata().fields.iter().map(|f| &f.name).collect();
        assert_eq!(names, vec!["<node>"]);
        assert!(crate::provider::is_sentinel(names[0]));
        let got = src.execute(None).unwrap().fields;
        assert_eq!(got.get("a"), Some(&Value::Int(1)));
        assert_eq!(got.get("b"), Some(&s("two")));
    }

    #[test]
    fn a_whole_node_must_be_an_object_and_none_is_empty() {
        let src = source(spec(
            None,
            "{{ 'secret-token' }}",
            ResultSpec::Whole("{{ data }}".into()),
        ));
        assert!(src.execute(None).is_err());
        let src = source(spec(
            None,
            "{{ none }}",
            ResultSpec::Whole("{{ data }}".into()),
        ));
        assert!(
            src.execute(None).unwrap().fields.is_empty(),
            "a node that renders nothing is a successful empty result"
        );
    }

    #[test]
    fn a_fields_source_declares_its_fields_and_the_schedule() {
        let src = source(spec(None, "x", fields(&[("a", "1"), ("b", "2")])));
        let meta = src.metadata();
        let names: Vec<_> = meta.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b"]);
        assert!(!meta.fsevents_reinstate);
        assert_eq!(meta.invalidation, recorded().invalidation);
        assert_eq!(meta.failback, recorded().failback);
    }

    #[test]
    fn the_recorded_schedule_passes_through() {
        let strategies = [
            (
                InvalidationStrategy::Watch {
                    patterns: vec![".git/HEAD".into()],
                    abs_paths: vec![],
                },
                true,
            ),
            (
                InvalidationStrategy::WatchAndPoll {
                    patterns: vec![".git/HEAD".into()],
                    abs_paths: vec![],
                    interval_secs: 30,
                    count: 2,
                },
                true,
            ),
            (
                InvalidationStrategy::Poll {
                    interval_secs: 30,
                    count: 2,
                },
                false,
            ),
        ];
        for (invalidation, reinstate) in strategies {
            let mut sp = spec(Some("/r"), "x", fields(&[("a", "1")]));
            sp.recorded = Recorded {
                invalidation: invalidation.clone(),
                failback: FailbackConfig {
                    reattempts: 7,
                    interval_secs: 9,
                },
                budget_execute: Duration::from_millis(321),
                decay: Some(1),
                nearest: false,
            };
            let meta = source(sp).metadata().clone();
            assert_eq!(meta.invalidation, invalidation);
            assert_eq!(meta.fsevents_reinstate, reinstate, "{invalidation:?}");
            assert_eq!(meta.failback.reattempts, 7);
            assert_eq!(meta.failback.interval_secs, 9);
            assert_eq!(meta.budget_execute, Duration::from_millis(321));
            assert_eq!(meta.decay, Some(1));
        }
    }

    #[test]
    fn a_relative_watch_needs_a_root_to_be_relative_to() {
        let mut sp = spec(None, "x", fields(&[("a", "1")]));
        sp.recorded.invalidation = InvalidationStrategy::Watch {
            patterns: vec![".git/HEAD".into()],
            abs_paths: vec![],
        };
        let err = new_err(sp.clone());
        assert!(
            err.starts_with(
                "demo.s: return: watch('.git/HEAD') is relative, but demo.s has one instance"
            ),
            "{err}"
        );
        // With a root line the same watch is fine, and so is an absolute one
        // without.
        sp.root = Some("{{ lup('.git') | dirname }}".into());
        assert!(source(sp.clone()).has_root());
        sp.root = None;
        sp.recorded.invalidation = InvalidationStrategy::Watch {
            patterns: vec![],
            abs_paths: vec!["/etc/hosts".into()],
        };
        assert!(!source(sp).has_root());
    }

    #[test]
    fn a_none_field_is_absent() {
        let src = source(spec(
            None,
            "{{ none }}",
            fields(&[("gone", "{{ data }}"), ("kept", "k")]),
        ));
        let got = src.execute(None).unwrap().fields;
        assert!(!got.contains_key("gone"));
        assert_eq!(got.get("kept"), Some(&s("k")));
    }

    #[test]
    fn a_render_error_fails_the_execute() {
        let src = source(spec(None, "{{ exec('false') }}", fields(&[("kept", "k")])));
        let err = src.execute(None).unwrap_err();
        assert!(err.starts_with("demo.s: data: "), "{err}");
        let src = source(spec(None, "{{ 1 }}", fields(&[("a", "{{ 'a' + 1 }}")])));
        let err = src.execute(None).unwrap_err();
        assert!(err.starts_with("demo.s: result.a: "), "{err}");
    }

    #[test]
    fn empty_root_is_the_home_instance() {
        let with_home = |root: &str| {
            let mut snapshot: HashMap<String, String> = (*daemon_env()).clone();
            snapshot.insert("HOME".into(), "/home/someone".into());
            with_env(spec(Some(root), "x", fields(&[("a", "1")])), snapshot)
        };
        for root in ["", "{{ none }}"] {
            let src = with_home(root);
            assert_eq!(src.root("/tmp"), Ok(None));
        }
    }

    #[test]
    fn a_root_that_fails_to_render_is_an_error_not_the_home_instance() {
        let src = source(spec(
            Some("{{ exec('false') }}"),
            "{{ 1 }}",
            fields(&[("a", "1")]),
        ));
        let err = src.root("/tmp").unwrap_err();
        assert!(err.starts_with("demo.s: root: "), "{err}");
    }

    #[test]
    fn a_root_must_render_an_absolute_directory() {
        for (root, kind) in [
            ("{{ 1 }}", "a number"),
            ("{{ true }}", "a bool"),
            ("{{ ['/a'] }}", "a list"),
            ("{{ {'a': 1} }}", "a map"),
            ("a/b", "a relative path"),
        ] {
            let src = source(spec(Some(root), "x", fields(&[("a", "1")])));
            let err = src.root("/tmp").unwrap_err();
            assert_eq!(
                err,
                format!("demo.s: root: must render an absolute directory, got {kind}"),
                "{root}"
            );
        }
    }

    #[test]
    fn a_root_line_renders_in_the_requested_directory() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().canonicalize().unwrap();
        std::fs::create_dir(repo.join(".git")).unwrap();
        let sub = repo.join("a/b");
        std::fs::create_dir_all(&sub).unwrap();
        let src = source(spec(
            Some("{{ lup('.git') | dirname }}"),
            "x",
            fields(&[("a", "1")]),
        ));
        let want = repo.to_str().unwrap().to_string();
        assert_eq!(src.root(sub.to_str().unwrap()), Ok(Some(want.clone())));
    }

    #[test]
    fn comb_ref_reads_put_over_source() {
        let mut answer = HashMap::new();
        answer.insert(
            "aws".to_string(),
            HashMap::from([("region".to_string(), s("eu-1"))]),
        );
        let reader = Arc::new(FakeReader {
            answer,
            ..Default::default()
        });
        let src = build(
            spec(
                None,
                "{{ comb.aws.region ~ comb.aws.region ~ comb.missing.x }}",
                fields(&[("r", "{{ data }}")]),
            ),
            reader.clone(),
        );
        let got = src.execute(Some("/work/proj")).unwrap().fields;
        assert_eq!(got.get("r"), Some(&s("eu-1eu-1")));
        // One read per provider the data line names, a miss included, at the
        // directory being executed.
        let mut calls = reader.calls.lock().unwrap().clone();
        calls.sort();
        assert_eq!(
            calls,
            vec![
                ("aws".to_string(), Some("/work/proj".to_string())),
                ("missing".to_string(), Some("/work/proj".to_string())),
            ]
        );
    }

    #[test]
    fn a_comb_read_error_fails_the_data_render() {
        struct Failing;
        impl DaemonRead for Failing {
            fn ran_calling(
                &self,
                _: &str,
                _: &str,
                _: Option<&str>,
                _: crate::provider::registry::CalleeMap,
            ) {
            }
            fn call(&self, _: &str, _: &str, _: Option<&str>) -> Result<Option<Json>, String> {
                Ok(None)
            }
            fn read(
                &self,
                _provider: &str,
                _dir: Option<&str>,
            ) -> Result<Option<HashMap<String, Value>>, String> {
                Err("q.s: root: boom".into())
            }
        }
        let src = build(
            spec(None, "{{ comb.q.x }}", fields(&[("r", "{{ data }}")])),
            Arc::new(Failing),
        );
        assert_eq!(
            src.execute(Some("/work")).unwrap_err(),
            "demo.s: data: comb.q: q.s: root: boom"
        );
    }

    #[test]
    fn the_prototype_name_is_in_every_line() {
        let src = source(spec(
            Some("{{ '/r' if _.name == 's' else '' }}"),
            "{{ _.name }}",
            fields(&[("n", "{{ data ~ _.name }}")]),
        ));
        assert_eq!(src.root("/tmp"), Ok(Some("/r".into())));
        assert_eq!(text(&src.execute(None).unwrap().fields, "n"), "ss");
    }

    #[test]
    fn a_line_may_only_name_what_its_stage_sees() {
        let cases: [(ConfigSourceSpec, &str, &str); 10] = [
            (
                spec(Some("{{ comb.x }}"), "x", fields(&[("a", "1")])),
                "root:",
                "comb",
            ),
            (
                spec(None, "{{ comb['aws'].region }}", fields(&[("a", "1")])),
                "data:",
                "comb",
            ),
            (
                spec(None, "{{ comb | length }}", fields(&[("a", "1")])),
                "data:",
                "comb",
            ),
            (
                spec(None, "{{ comb['aws:put'].region }}", fields(&[("a", "1")])),
                "data:",
                "comb",
            ),
            (
                spec(None, "x", fields(&[("a", "{{ sources.other() }}")])),
                "result.a:",
                "sources",
            ),
            (
                spec(Some("{{ daemon.x }}"), "x", fields(&[("a", "1")])),
                "root:",
                "daemon",
            ),
            (
                spec(None, "x", fields(&[("a", "{{ env.X }}")])),
                "result.a:",
                "env",
            ),
            (
                spec(None, "x", fields(&[("a", "{{ comb.x }}")])),
                "result.a:",
                "comb",
            ),
            (
                spec(None, "x", fields(&[("a", "{{ dta.x }}")])),
                "result.a:",
                "dta",
            ),
            (
                spec(None, "x", ResultSpec::Whole("{{ comb.x }}".into())),
                "result:",
                "comb",
            ),
        ];
        for (sp, stage, name) in cases {
            let err = new_err(sp);
            assert!(
                err.starts_with(&format!("demo.s: {stage} ")) && err.contains(name),
                "{err}"
            );
        }
    }

    #[test]
    fn comb_is_read_as_comb_provider_field() {
        let src = source(spec(
            None,
            "{{ comb.aws['region'] }}",
            fields(&[("a", "{{ data }}")]),
        ));
        assert_eq!(src.comb_providers, vec!["aws".to_string()]);
    }

    #[test]
    fn a_line_that_does_not_compile_is_a_load_error_naming_its_stage() {
        for (sp, stage) in [
            (spec(Some("{{ "), "x", fields(&[("a", "1")])), "root"),
            (spec(None, "{{ ", fields(&[("a", "1")])), "data"),
            (spec(None, "x", fields(&[("a", "{{ ")])), "result.a"),
            (spec(None, "x", ResultSpec::Whole("{{ ".into())), "result"),
        ] {
            let err = new_err(sp);
            assert!(err.starts_with(&format!("demo.s: {stage}: ")), "{err}");
        }
    }

    #[test]
    fn result_fields_are_validated_at_load() {
        let err = new_err(spec(None, "x", ResultSpec::Fields(vec![])));
        assert!(err.contains("no fields"), "{err}");
        for bad in ["", "a.b", "<node>"] {
            let err = new_err(spec(None, "x", fields(&[(bad, "1")])));
            assert!(err.starts_with("demo.s: result: "), "{bad}: {err}");
        }
    }

    #[test]
    fn a_failed_execute_does_not_log_values() {
        // The kind is named, never the value (a url() body can carry tokens).
        let src = source(spec(
            None,
            "{{ 'secret-token' }}",
            ResultSpec::Whole("{{ data }}".into()),
        ));
        let err = src.run(None).unwrap_err();
        assert!(!err.contains("secret-token"), "{err}");
        assert!(err.starts_with("demo.s: result: "), "{err}");
    }

    #[test]
    fn sources_call_reads_a_siblings_record() {
        let reader = Arc::new(FakeReader {
            records: HashMap::from([
                (("demo".into(), "other".into()), json!({"x": 4})),
                (("zed".into(), "t".into()), json!({"y": 5})),
            ]),
            ..Default::default()
        });
        let src = build(
            spec(
                None,
                "{{ sources.other().x + daemon.providers.zed.sources.t().y }}",
                fields(&[("out", "{{ data }}")]),
            ),
            reader.clone(),
        );
        let got = src.execute(Some("/w")).unwrap().fields;
        assert_eq!(got.get("out"), Some(&Value::Int(9)));
        assert_eq!(
            *reader.called.lock().unwrap(),
            vec![
                (
                    "demo".to_string(),
                    "other".to_string(),
                    Some("/w".to_string())
                ),
                ("zed".to_string(), "t".to_string(), Some("/w".to_string())),
            ],
            "each call asks for the Source at the directory this one runs for"
        );
    }

    #[test]
    fn a_source_calling_itself_is_a_load_error() {
        for line in [
            "{{ sources.s().x }}",
            "{{ daemon.providers.demo.sources.s().x }}",
        ] {
            let err = new_err(spec(None, line, fields(&[("a", "{{ data }}")])));
            assert_eq!(err, "demo.s: data: demo.s cannot call itself", "{line}");
        }
        // Another provider's Source of the same name is not itself.
        source(spec(
            None,
            "{{ daemon.providers.other.sources.s() }}",
            fields(&[("a", "{{ data }}")]),
        ));
    }

    #[test]
    fn a_source_is_called_the_one_way() {
        for line in ["{{ sources }}", "{{ sources['b'] }}", "{{ daemon }}"] {
            let err = new_err(spec(None, line, fields(&[("a", "{{ data }}")])));
            assert!(err.starts_with("demo.s: data: "), "{line}: {err}");
        }
    }

    #[test]
    fn only_the_engines_own_frame_is_taken_off_an_error() {
        assert_eq!(
            without_engine_frame("invalid operation: boom (in <expression>:1)"),
            "boom"
        );
        assert_eq!(
            without_engine_frame("invalid operation: boom (in <template>:12)"),
            "boom"
        );
        for kept in [
            "sh said invalid operation: nope (in the middle) and went on",
            "a command wrote (in <expression>:1) in its stderr and then more",
            "plain",
        ] {
            assert_eq!(without_engine_frame(kept), kept);
        }
        assert_eq!(
            without_engine_frame("invalid operation: x: invalid operation: y (in <expression>:1)"),
            "x: invalid operation: y",
            "once: the text between is the error's own"
        );
    }
}
