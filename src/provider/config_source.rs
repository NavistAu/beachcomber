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
//! | `data` | `env`, `comb`, `_` |
//! | `result.*` | `data`, `_` |
//!
//! A name outside that list is a load error, not a silent `none`.

use crate::engine::{PluginSet, daemon_engine_env};
use crate::provider::{
    FieldSchema, FieldType, InvalidationStrategy, Source, SourceMetadata, SourceResult,
    SourceScope, Value,
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
use tracing::warn;

/// A daemon-side read of one provider node at a directory entry: put over
/// source, up the directory chain, never a template. What `comb.<p>` sees.
///
/// `read` only reads stored records and never executes anything: a Source's
/// `data` line runs inside a refresh, and a read that ran Sources could
/// recurse into the one being refreshed. So `comb.<own provider>` in a `data`
/// line is that provider's own last stored value, never a fresh run.
pub trait DaemonRead: Send + Sync {
    fn read(&self, provider: &str, dir: Option<&str>) -> Option<HashMap<String, Value>>;
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
const DATA_NAMES: &[&str] = &["env", "comb", "_"];
const RESULT_NAMES: &[&str] = &["data", "_"];

pub struct ConfigSource {
    spec: ConfigSourceSpec,
    meta: SourceMetadata,
    env: Arc<HashMap<String, String>>,
    plugins: Arc<PluginSet>,
    reader: Arc<dyn DaemonRead>,
    /// The providers the `data` line reads through `comb`, each once.
    comb_providers: Vec<String>,
}

/// `<provider>.<source>: <stage>: <message>`: the one shape of every error,
/// at load and at run.
fn stage_error(spec: &ConfigSourceSpec, stage: &str, message: impl std::fmt::Display) -> String {
    format!("{}.{}: {stage}: {message}", spec.provider, spec.name)
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

        let probe = engine(&env, &plugins, None);
        let mut lines: Vec<(String, &str, &[&str])> = Vec::new();
        if let Some(root) = &spec.root {
            lines.push(("root".into(), root, ROOT_NAMES));
        }
        lines.push(("data".into(), &spec.data, DATA_NAMES));
        match &spec.result {
            ResultSpec::Whole(line) => lines.push(("result".into(), line, RESULT_NAMES)),
            ResultSpec::Fields(pairs) => lines.extend(
                pairs
                    .iter()
                    .map(|(n, l)| (format!("result.{n}"), l.as_str(), RESULT_NAMES)),
            ),
        }
        for (stage, line, allowed) in lines {
            let names =
                undeclared_names(&probe, line, false).map_err(|e| stage_error(&spec, &stage, e))?;
            if let Some(name) = names.iter().find(|n| !allowed.contains(&n.as_str())) {
                return Err(stage_error(
                    &spec,
                    &stage,
                    format!(
                        "unknown name '{name}' (this line sees {})",
                        allowed.join(", ")
                    ),
                ));
            }
        }

        let used = undeclared_names(&probe, &spec.data, true)
            .map_err(|e| stage_error(&spec, "data", e))?;
        if used.contains("comb") {
            return Err(stage_error(
                &spec,
                "data",
                "comb is read as comb.<provider>[.<field>]; comb[...] and comb as a whole \
                 are not supported",
            ));
        }

        let comb_providers: Vec<String> = discover_refs(&spec.data)
            .into_iter()
            .filter_map(|r| match r {
                Ref::CombProvider(p) | Ref::CombField(p, _) => Some(p),
                _ => None,
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();

        let recorded = &spec.recorded;
        let meta = SourceMetadata {
            name: spec.name.clone(),
            fields,
            scope: SourceScope::PathScoped,
            invalidation: recorded.invalidation.clone(),
            failback: recorded.failback,
            fsevents_reinstate: !matches!(recorded.invalidation, InvalidationStrategy::Poll { .. }),
            decay: recorded.decay,
            budget_execute: recorded.budget_execute,
        };
        Ok(Self {
            spec,
            meta,
            env,
            plugins,
            reader,
            comb_providers,
        })
    }

    /// Render the root line with cwd = `dir`: `Ok(Some(directory))`, or
    /// `Ok(None)` for the `~` instance (no root line, or it renders empty or
    /// none). A line that fails to render, or renders something other than an
    /// absolute directory, is an `Err`.
    pub fn render_root(&self, dir: &str) -> Result<Option<String>, String> {
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
            if let Some(node) = self.reader.read(provider, dir) {
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

    fn run(&self, path: Option<&str>) -> Result<SourceResult, String> {
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
        let jinja = engine(&self.env, &self.plugins, Some(cwd));
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

    /// A render error fails the execute: the empty result is what the
    /// scheduler's failure accounting turns into `failback`.
    ///
    /// Interim, until a `Source::root()` hook replaces `SourceScope` and
    /// `canonical_path`: the root line is rendered at `path` first, and an instance whose root
    /// cannot render never produces data. Only the error is checked, not
    /// that the render equals `path`.
    fn execute(&self, path: Option<&str>) -> SourceResult {
        if let Some(dir) = path
            && let Err(e) = self.render_root(dir)
        {
            warn!("{e}");
            return SourceResult::new();
        }
        self.run(path).unwrap_or_else(|e| {
            warn!("{e}");
            SourceResult::new()
        })
    }

    /// Interim, until a `Source::root()` hook replaces `SourceScope` and
    /// `canonical_path`: the rendered root, or the daemon's `HOME` (which `query::normalise_path`
    /// turns into the global entry) when it renders empty. A root that fails
    /// to render is logged and declines (`None`), so `resolve_path` keys the
    /// request at the requested directory instead of at the `~` instance.
    fn canonical_path(&self, path: Option<&str>) -> Option<String> {
        let dir = path?;
        match self.render_root(dir) {
            Ok(Some(root)) => Some(root),
            Ok(None) => self.home().map(str::to_string),
            Err(e) => {
                warn!("{e}");
                None
            }
        }
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
    }

    impl DaemonRead for FakeReader {
        fn read(&self, provider: &str, dir: Option<&str>) -> Option<HashMap<String, Value>> {
            self.calls
                .lock()
                .unwrap()
                .push((provider.to_string(), dir.map(str::to_string)));
            self.answer.get(provider).cloned()
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
            let got = src.execute(Some(root.to_str().unwrap()));
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
                let got = src.execute(None);
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
        let got = src.execute(Some(home.to_str().unwrap()));
        assert_eq!(text(&got.fields, "out").trim(), home.to_str().unwrap());
        assert_eq!(src.execute(None).fields, got.fields);
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
        let got = src.execute(None).fields;
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
        let got = src.execute(None).fields;
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
        assert!(src.execute(None).fields.is_empty());
        let src = source(spec(
            None,
            "{{ none }}",
            ResultSpec::Whole("{{ data }}".into()),
        ));
        assert!(src.execute(None).fields.is_empty());
    }

    #[test]
    fn a_fields_source_declares_its_fields_and_the_schedule() {
        let src = source(spec(None, "x", fields(&[("a", "1"), ("b", "2")])));
        let meta = src.metadata();
        let names: Vec<_> = meta.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b"]);
        assert_eq!(meta.scope, SourceScope::PathScoped);
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
            let mut sp = spec(None, "x", fields(&[("a", "1")]));
            sp.recorded = Recorded {
                invalidation: invalidation.clone(),
                failback: FailbackConfig {
                    reattempts: 7,
                    interval_secs: 9,
                },
                budget_execute: Duration::from_millis(321),
                decay: Some(1),
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
    fn a_none_field_is_absent() {
        let src = source(spec(
            None,
            "{{ none }}",
            fields(&[("gone", "{{ data }}"), ("kept", "k")]),
        ));
        let got = src.execute(None).fields;
        assert!(!got.contains_key("gone"));
        assert_eq!(got.get("kept"), Some(&s("k")));
    }

    #[test]
    fn a_render_error_fails_the_execute() {
        let src = source(spec(None, "{{ exec('false') }}", fields(&[("kept", "k")])));
        assert!(src.execute(None).fields.is_empty());
        let src = source(spec(None, "{{ 1 }}", fields(&[("a", "{{ 'a' + 1 }}")])));
        assert!(src.execute(None).fields.is_empty());
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
            assert_eq!(src.render_root("/tmp"), Ok(None));
            assert_eq!(
                src.canonical_path(Some("/tmp")).as_deref(),
                Some("/home/someone")
            );
        }
    }

    #[test]
    fn a_root_that_fails_to_render_is_an_error_not_the_home_instance() {
        let src = source(spec(
            Some("{{ exec('false') }}"),
            "{{ 1 }}",
            fields(&[("a", "1")]),
        ));
        let err = src.render_root("/tmp").unwrap_err();
        assert!(err.starts_with("demo.s: root: "), "{err}");
        assert_eq!(src.canonical_path(Some("/tmp")), None);
        assert!(src.execute(Some("/tmp")).fields.is_empty());
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
            let err = src.render_root("/tmp").unwrap_err();
            assert_eq!(
                err,
                format!("demo.s: root: must render an absolute directory, got {kind}"),
                "{root}"
            );
            assert_eq!(src.canonical_path(Some("/tmp")), None);
            assert!(src.execute(Some("/tmp")).fields.is_empty());
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
        assert_eq!(
            src.render_root(sub.to_str().unwrap()),
            Ok(Some(want.clone()))
        );
        assert_eq!(src.canonical_path(Some(sub.to_str().unwrap())), Some(want));
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
        let got = src.execute(Some("/work/proj")).fields;
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
    fn the_prototype_name_is_in_every_line() {
        let src = source(spec(
            Some("{{ '/r' if _.name == 's' else '' }}"),
            "{{ _.name }}",
            fields(&[("n", "{{ data ~ _.name }}")]),
        ));
        assert_eq!(src.render_root("/tmp"), Ok(Some("/r".into())));
        assert_eq!(text(&src.execute(None).fields, "n"), "ss");
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
                spec(None, "{{ sources.other() }}", fields(&[("a", "1")])),
                "data:",
                "sources",
            ),
            (
                spec(None, "{{ daemon.x }}", fields(&[("a", "1")])),
                "data:",
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
}
