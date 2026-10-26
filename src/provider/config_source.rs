//! `ConfigSource`: a Source whose body is jinja.
//!
//! `root` says which directory an instance answers for, `data` is the body
//! (one engine call, cwd = the root, env = the daemon's own), `result` is the
//! field lines over `data`. The schedule comes from the Source's `return`
//! chain, already recorded as [`Recorded`].
//!
//! The files the `data` and `result` lines read (`read`, `stat`, `realpath`,
//! `lup`, `glob`) are the record's stat inputs, beside its `watch(...)`
//! declaration: each is stamped before it is read, into the account of the run
//! executing the lines ([`crate::run::reads_here`]), and a read checks them
//! before it answers from the record. The `root` line's walk is not: the root
//! a directory finds is kept by the registry, not checked per record.
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
//! stored is run once, and the call waits for it. A Source cannot call itself,
//! and a cycle through others is an error when it runs.

use crate::engine::{CallFn, Calls, PluginSet, daemon_engine_env};
use crate::provider::source_call::{Called, Running, called_in};
use crate::provider::{FieldSchema, FieldType, Source, SourceMetadata, SourceResult, Value};
use crate::shape::decorators::Recorded;
use libbeachcomber::engine::EngineCtx;
use libbeachcomber::eval::{
    Finding, Known, Line, discover_refs, eval_line, eval_line_with, lint, undeclared_names,
};
use libbeachcomber::fs::{MOST_TOUCHED, Touched};
use libbeachcomber::virtual_fields::Ref;
use minijinja::Environment;
use minijinja::value::Value as MjValue;
use serde_json::{Value as Json, json};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// A daemon-side read of one provider node at a directory entry: put over
/// source, up the directory chain, through the provider's `return` line,
/// never a template. What `comb.<p>` sees.
///
/// `read` is a read like any other: a cold Source of the provider runs first,
/// once. A Source never waits on itself, so `comb.<own provider>` in a
/// `data` line, or a ring of such reads, finds the running Source's own last
/// stored value. A Source call (`sources.<s>()`) differs where that is the
/// point: it reads one Source's raw records, without a `put` over them or the
/// provider's lines, and calling oneself is an error.
pub trait DaemonRead: Send + Sync {
    /// `Ok(None)` is nothing stored. `Err` is a read that could not be
    /// answered (a root that failed to work out, a fault in a line it ran);
    /// it fails the `data` render.
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
    /// find it (`None`: nothing stored), running a cold instance once first.
    /// The caller is a `data` line, on a blocking thread.
    fn call(&self, provider: &str, source: &str, dir: Option<&str>)
    -> Result<Option<Json>, String>;
}

/// A provider's stored records as the JSON a line reads, `Null` for nothing
/// stored.
fn representable(provider: &str, node: Option<HashMap<String, Value>>) -> Result<Json, String> {
    match node {
        Some(node) => serde_json::to_value(node)
            .map_err(|e| format!("comb.{provider} is not representable: {e}")),
        None => Ok(Json::Null),
    }
}

/// A `data` line's `comb`: every provider is a member, read at the instance's
/// directory when the line first asks for it — `comb.<p>`, `comb['<p>']` or a
/// computed `comb[name]` alike — and kept for the rest of the render. A read
/// only reads stored records and never runs a Source. An engine object cannot
/// return an error, so a read that fails is kept and fails the render
/// afterwards.
struct LazyComb {
    reader: Arc<dyn DaemonRead>,
    dir: Option<String>,
    /// What has been read, `Null` for nothing stored.
    read: Mutex<serde_json::Map<String, Json>>,
    failed: Mutex<Option<String>>,
}

impl std::fmt::Debug for LazyComb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<comb>")
    }
}

impl minijinja::value::Object for LazyComb {
    fn repr(self: &Arc<Self>) -> minijinja::value::ObjectRepr {
        minijinja::value::ObjectRepr::Map
    }

    fn get_value(self: &Arc<Self>, key: &MjValue) -> Option<MjValue> {
        let name = key.as_str()?;
        // A member is a provider head; anything else names nothing.
        if !matches!(libbeachcomber::key::split_key(name), Ok((head, rest)) if head == name && rest.is_empty())
        {
            return None;
        }
        let mut read = self.read.lock().ok()?;
        if !read.contains_key(name) {
            let node = self
                .reader
                .read(name, self.dir.as_deref())
                .map_err(|e| format!("comb.{name}: {e}"))
                .and_then(|node| representable(name, node));
            match node {
                Ok(node) => {
                    read.insert(name.to_string(), node);
                }
                Err(e) => {
                    if let Ok(mut failed) = self.failed.lock() {
                        failed.get_or_insert(e);
                    }
                    return None;
                }
            }
        }
        read.get(name)
            .filter(|node| !node.is_null())
            .map(MjValue::from_serialize)
    }

    fn enumerate(self: &Arc<Self>) -> minijinja::value::Enumerator {
        minijinja::value::Enumerator::NonEnumerable
    }

    fn is_true(self: &Arc<Self>) -> bool {
        true
    }
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

pub struct ConfigSource {
    spec: ConfigSourceSpec,
    meta: SourceMetadata,
    env: Arc<HashMap<String, String>>,
    plugins: Arc<PluginSet>,
    reader: Arc<dyn DaemonRead>,
    /// The providers the `data` line reads through `comb`, each once.
    comb_providers: Vec<String>,
    /// Whether the `data` line reads through `comb` at all, by name or by a
    /// computed `comb[...]`: a read runs cold Sources, which the run notes.
    reads_comb: bool,
    /// The Sources the `data` line calls.
    calls: Vec<Called>,
    /// Whether a run has said its lines read more files than a record is
    /// checked for: said once per Source.
    said_capped: AtomicBool,
}

/// `<provider>.<source>: <stage>: <message>`: the one shape of every error,
/// at load and at run. A line's message comes from the one evaluator, which
/// has already taken the engine's frame off it, so a render that fails
/// because a Source it calls failed shows no frame per Source down the chain.
fn stage_error(spec: &ConfigSourceSpec, stage: &str, message: impl std::fmt::Display) -> String {
    format!("{}.{}: {stage}: {message}", spec.provider, spec.name)
}

/// The load check of a Source's lines ([`lint`]): each compiles and reads
/// only names its stage sees, and `data` reads `comb` and calls `sources` the
/// one way it can. Every line is checked, and each fault is one error
/// prefixed `{who} {stage}: ` (`who` the Source's table, or a `_` table's);
/// any line may be absent, as in a prototype. Nothing is rendered.
pub fn lint_lines(
    who: &str,
    root: Option<&str>,
    data: Option<&str>,
    result: Option<&ResultSpec>,
    env: &Arc<HashMap<String, String>>,
    plugins: &Arc<PluginSet>,
) -> Vec<String> {
    let probe = engine(env, plugins, None, None);
    let mut lines: Vec<(String, &str, Line)> = Vec::new();
    if let Some(root) = root {
        lines.push(("root".into(), root, Line::Root));
    }
    if let Some(data) = data {
        lines.push(("data".into(), data, Line::Data));
    }
    match result {
        Some(ResultSpec::Whole(line)) => lines.push(("result".into(), line, Line::Result)),
        Some(ResultSpec::Fields(pairs)) => lines.extend(
            pairs
                .iter()
                .map(|(n, l)| (format!("result.{n}"), l.as_str(), Line::Result)),
        ),
        None => {}
    }
    let mut errors = Vec::new();
    for (stage, line, kind) in lines {
        let found = lint(&probe, line, kind, &Known::default());
        let compiles = !found.iter().any(|f| matches!(f, Finding::Compile(_)));
        errors.extend(found.iter().map(|f| format!("{who} {stage}: {f}")));
        if kind == Line::Data && compiles {
            errors.extend(lint_data(&probe, line).map(|e| format!("{who} data: {e}")));
        }
    }
    errors
}

/// What only a `data` line can get wrong, once it compiles: reading
/// `sources` other than by calling one, or calling a Source no way names.
fn lint_data(probe: &Environment<'_>, data: &str) -> Option<String> {
    let used = undeclared_names(probe, data, true).ok()?;
    if used.contains("sources") {
        return Some(
            "a sibling Source is called, sources.<source>(); sources[...] and sources as a \
             whole are not supported"
                .into(),
        );
    }
    called_in(probe, data).err()
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
    ) -> Result<Self, Vec<String>> {
        // A load error names the Source's table, as every configuration error
        // does; a run's error (`stage_error`) names the Source.
        let at = format!("[daemon.providers.{}.sources.{}]", spec.provider, spec.name);
        let load_error = |stage: &str, message: &str| vec![format!("{at} {stage}: {message}")];
        let fields = match &spec.result {
            ResultSpec::Whole(_) => vec![FieldSchema {
                name: "<node>".into(),
                field_type: FieldType::Object,
            }],
            ResultSpec::Fields(pairs) if pairs.is_empty() => {
                return Err(load_error("result", "declares no fields"));
            }
            ResultSpec::Fields(pairs) => {
                let mut fields = Vec::new();
                for (name, _) in pairs {
                    if name.is_empty() || name.starts_with('<') {
                        return Err(load_error(
                            "result",
                            &format!(
                                "field name '{name}' must be non-empty and not start with '<'"
                            ),
                        ));
                    }
                    fields.push(FieldSchema {
                        // A declared field is an address; this one is a
                        // single name, so a `.` in it is quoted (`['c.d']`).
                        name: libbeachcomber::key::quote(name).into_owned(),
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

        let errors = lint_lines(
            &at,
            spec.root.as_deref(),
            Some(&spec.data),
            Some(&spec.result),
            &env,
            &plugins,
        );
        if !errors.is_empty() {
            return Err(errors);
        }

        let refs = discover_refs(&spec.data);
        let reads_comb = refs.iter().any(|r| {
            matches!(r, Ref::CombProvider(_) | Ref::CombField(..))
                || *r == Ref::Dynamic("comb".to_string())
        });
        let comb_providers: Vec<String> = refs
            .into_iter()
            .filter_map(|r| match r {
                Ref::CombProvider(p) | Ref::CombField(p, _) => Some(p),
                _ => None,
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();

        let who = format!("{}.{}", spec.provider, spec.name);
        let calls = called_in(&engine(&env, &plugins, None, None), &spec.data)
            .map_err(|e| load_error("data", &e))?;
        let itself = |c: &Called| match c {
            Called::Sibling(s) => *s == spec.name,
            Called::Of(p, s) => *p == spec.provider && *s == spec.name,
        };
        if calls.iter().any(itself) {
            return Err(load_error("data", &format!("{who} cannot call itself")));
        }

        let recorded = &spec.recorded;
        if spec.root.is_none()
            && let Some(problem) = crate::provider::one_instance_problem(
                &format!("{}.{}", spec.provider, spec.name),
                &recorded.invalidation,
                recorded.nearest,
            )
        {
            return Err(load_error("return", &problem));
        }
        let meta = SourceMetadata {
            name: spec.name.clone(),
            fields,
            invalidation: recorded.invalidation.clone(),
            failback: recorded.failback,
            decay: recorded.decay,
            budget: recorded.budget,
            timeout: recorded.timeout,
            nearest: recorded.nearest,
        };
        Ok(Self {
            spec,
            meta,
            env,
            plugins,
            reader,
            comb_providers,
            reads_comb,
            calls,
            said_capped: AtomicBool::new(false),
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
    /// line names, and a lazy read of any provider `comb[...]` names as the
    /// render asks for it.
    fn comb(&self, dir: Option<&str>) -> Result<LazyComb, String> {
        let mut read = serde_json::Map::new();
        for provider in &self.comb_providers {
            let node = self
                .reader
                .read(provider, dir)
                .map_err(|e| stage_error(&self.spec, "data", format!("comb.{provider}: {e}")))?;
            read.insert(
                provider.clone(),
                representable(provider, node).map_err(|e| stage_error(&self.spec, "data", e))?,
            );
        }
        Ok(LazyComb {
            reader: self.reader.clone(),
            dir: dir.map(str::to_string),
            read: Mutex::new(read),
            failed: Mutex::new(None),
        })
    }

    /// Run the lines, and when the run ends, however it ends, say what it
    /// called: that, and no more, is what this instance calls.
    fn run(&self, path: Option<&str>) -> Result<SourceResult, String> {
        let running = Running::enter(&self.spec.provider, &self.spec.name);
        let result = self.run_lines(path);
        if !self.calls.is_empty() || self.reads_comb {
            self.reader.ran_calling(
                &self.spec.provider,
                &self.spec.name,
                path,
                running.take_calls(),
            );
        }
        self.say_if_capped();
        result
    }

    /// A run whose lines read more files than a record is checked for says
    /// so, once per Source: past [`MOST_TOUCHED`], a change goes unseen by a
    /// read until the watch or the poll.
    fn say_if_capped(&self) {
        let Some(reads) = crate::run::reads_here() else {
            return;
        };
        let read = reads.count();
        if read > MOST_TOUCHED && !self.said_capped.swap(true, Ordering::Relaxed) {
            tracing::warn!(
                "{}.{}: {read} files read; the first {MOST_TOUCHED} are checked on read",
                self.spec.provider,
                self.spec.name
            );
        }
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
        // What the lines read is the run's to check its record by: noted in
        // the account the run keeps on this thread.
        let mut jinja = engine(
            &self.env,
            &self.plugins,
            Some(cwd),
            crate::run::reads_here(),
        );
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
        let comb = Arc::new(self.comb(path)?);
        let data_context: MjValue = [
            ("env", MjValue::from_serialize(&*self.env)),
            ("comb", MjValue::from_dyn_object(comb.clone())),
            ("_", MjValue::from_serialize(self.underscore())),
        ]
        .into_iter()
        .collect();
        let data = eval_line_with(&jinja, &self.spec.data, data_context);
        if let Some(failed) = comb.failed.lock().ok().and_then(|mut f| f.take()) {
            return Err(stage_error(&self.spec, "data", failed));
        }
        let data = data.map_err(|e| stage_error(&self.spec, "data", e))?;
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
        // A root is no run's input: what the walk finds is kept per directory
        // (`ProviderRegistry`'s kept roots), not checked as a record's files.
        let jinja = engine(&self.env, &self.plugins, Some(dir.into()), None);
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

/// The daemon's engine for a Source's lines, `exec` on, `cwd` its directory,
/// noting what its file items read in `touched` (a run's account).
fn engine<'a>(
    env: &HashMap<String, String>,
    plugins: &Arc<PluginSet>,
    cwd: Option<PathBuf>,
    touched: Option<Arc<Touched>>,
) -> Environment<'a> {
    let ctx = EngineCtx {
        env: env.clone(),
        cwd,
        allow_exec: true,
        // One clock for the run these lines are rendered in.
        now: std::time::SystemTime::now(),
        touched,
    };
    daemon_engine_env(Arc::new(ctx), plugins.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boundaries::library::MockLibraryLoader;
    use crate::provider::{FailbackConfig, InvalidationStrategy};
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
            budget: Duration::from_millis(50),
            timeout: crate::scheduler::lifecycle::DEFAULT_TIMEOUT,
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
            Err(e) => e.join("\n"),
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
        assert_eq!(meta.invalidation, recorded().invalidation);
        assert_eq!(meta.failback, recorded().failback);
    }

    #[test]
    fn the_recorded_schedule_passes_through() {
        let strategies = [
            InvalidationStrategy::Watch {
                patterns: vec![".git/HEAD".into()],
                abs_paths: vec![],
            },
            InvalidationStrategy::WatchAndPoll {
                patterns: vec![".git/HEAD".into()],
                abs_paths: vec![],
                interval_secs: 30,
                count: 2,
            },
            InvalidationStrategy::Poll {
                interval_secs: 30,
                count: 2,
            },
        ];
        for invalidation in strategies {
            let mut sp = spec(Some("/r"), "x", fields(&[("a", "1")]));
            sp.recorded = Recorded {
                invalidation: invalidation.clone(),
                failback: FailbackConfig {
                    reattempts: 7,
                    interval_secs: 9,
                },
                budget: Duration::from_millis(321),
                timeout: crate::scheduler::lifecycle::DEFAULT_TIMEOUT,
                decay: Some(1),
                nearest: false,
            };
            let meta = source(sp).metadata().clone();
            assert_eq!(meta.invalidation, invalidation);
            assert_eq!(meta.failback.reattempts, 7);
            assert_eq!(meta.failback.interval_secs, 9);
            assert_eq!(meta.budget, Duration::from_millis(321));
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
                "[daemon.providers.demo.sources.s] return: watch('.git/HEAD') is relative, but demo.s has one instance"
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
        let cases: [(ConfigSourceSpec, &str, &str); 7] = [
            (
                spec(Some("{{ comb.x }}"), "x", fields(&[("a", "1")])),
                "root:",
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
                err.starts_with(&format!("[daemon.providers.demo.sources.s] {stage} "))
                    && err.contains(name),
                "{err}"
            );
        }
    }

    /// `comb['<p>']` and a computed `comb[name]` read the provider they name,
    /// lazily, as `comb.<p>` does: stored records at the directory, once per
    /// render.
    #[test]
    fn comb_takes_a_subscript_and_reads_the_provider_it_names() {
        let reader = Arc::new(FakeReader {
            answer: HashMap::from([
                (
                    "aws".to_string(),
                    HashMap::from([("region".to_string(), s("eu-1"))]),
                ),
                (
                    "git:template".to_string(),
                    HashMap::from([("branch".to_string(), s("{{ x }}"))]),
                ),
            ]),
            ..FakeReader::default()
        });
        let src = build(
            spec(
                None,
                "{{ comb['aws'].region ~ '/' ~ comb[['a', 'ws'] | join].region ~ '/' ~ \
                 comb['git:template'].branch ~ '/' ~ (comb['nope'] or 'none') }}",
                fields(&[("r", "{{ data }}")]),
            ),
            reader.clone(),
        );
        let got = src.execute(Some("/work/proj")).unwrap().fields;
        assert_eq!(got.get("r"), Some(&s("eu-1/eu-1/{{ x }}/none")));
        let mut calls = reader.calls.lock().unwrap().clone();
        calls.sort();
        assert_eq!(
            calls,
            vec![
                ("aws".to_string(), Some("/work/proj".to_string())),
                ("git:template".to_string(), Some("/work/proj".to_string())),
                ("nope".to_string(), Some("/work/proj".to_string())),
            ],
            "each provider is read once, however it is named"
        );
    }

    #[test]
    fn a_lazy_comb_read_error_fails_the_data_render() {
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
            spec(
                None,
                "{{ comb['q'].x or 'fallback' }}",
                fields(&[("r", "{{ data }}")]),
            ),
            Arc::new(Failing),
        );
        assert_eq!(
            src.execute(Some("/work")).unwrap_err(),
            "demo.s: data: comb.q: q.s: root: boom"
        );
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
            assert!(
                err.starts_with(&format!("[daemon.providers.demo.sources.s] {stage}: ")),
                "{err}"
            );
        }
    }

    /// A macro defined and called within one line runs on the daemon too;
    /// importing a library is the client's alone, and a load error here.
    #[test]
    fn a_data_line_may_define_and_call_a_macro() {
        let src = source(spec(
            None,
            "{% macro twice(x) %}{{ x * 2 }}{{ caller() if caller }}{% endmacro %}\
             {% call twice(20) %}!{% endcall %}+{{ twice(21) }}",
            fields(&[(
                "out",
                "{% macro tag(x) %}<{{ x }}>{% endmacro %}{{ tag(data) }}",
            )]),
        ));
        let got = src.execute(None).unwrap().fields;
        assert_eq!(got.get("out"), Some(&s("<40!+42>")));

        for (sp, stage) in [
            (
                spec(
                    None,
                    "{% import 'p:macro.x' as x %}{{ x.m() }}",
                    fields(&[("a", "1")]),
                ),
                "data",
            ),
            (
                spec(
                    None,
                    "x",
                    fields(&[("a", "{% from 'p:macro.x' import m %}{{ m() }}")]),
                ),
                "result.a",
            ),
        ] {
            assert_eq!(
                new_err(sp),
                format!(
                    "[daemon.providers.demo.sources.s] {stage}: macros are client templates: a \
                     Source's line cannot import"
                )
            );
        }
    }

    /// A `data` line of statements and one `{{ }}` keeps the expression's
    /// type, so a list built in a loop is a list, with no JSON round trip.
    #[test]
    fn a_data_line_of_statements_returns_a_list() {
        let src = source(spec(
            None,
            "{%- set ns = namespace(out=[]) -%}\n\
             {%- for x in [1, 2] -%}{%- set ns.out = ns.out + [x * 10] -%}{%- endfor -%}\n\
             {{- ns.out -}}",
            fields(&[("n", "{{ data | length }}"), ("first", "{{ data[0] }}")]),
        ));
        let got = src.execute(None).unwrap().fields;
        assert_eq!(got.get("n"), Some(&Value::Int(2)));
        assert_eq!(got.get("first"), Some(&Value::Int(10)));
    }

    #[test]
    fn result_fields_are_validated_at_load() {
        let err = new_err(spec(None, "x", ResultSpec::Fields(vec![])));
        assert!(err.contains("no fields"), "{err}");
        for bad in ["", "<node>"] {
            let err = new_err(spec(None, "x", fields(&[(bad, "1")])));
            assert!(
                err.starts_with("[daemon.providers.demo.sources.s] result: "),
                "{bad}: {err}"
            );
        }
        // A name holding a `.` is one field, read as `['a.b']`.
        source(spec(None, "x", fields(&[("a.b", "1")])));
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
            assert_eq!(
                err, "[daemon.providers.demo.sources.s] data: demo.s cannot call itself",
                "{line}"
            );
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
            assert!(
                err.starts_with("[daemon.providers.demo.sources.s] data: "),
                "{line}: {err}"
            );
        }
    }
}
