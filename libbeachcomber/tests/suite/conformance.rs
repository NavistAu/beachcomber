// Conformance runner — drives tests/conformance/**/*.json against
// libbeachcomber. See tests/conformance/README.md for fixture shape.
//
// Each fixture spawns its own daemon for isolation. A fixture that
// breaks any `expect` rule fails a Rust test bearing the fixture's `name`.

use crate::common::daemon::DaemonGuard;

use libbeachcomber::resolve::{self, Inputs, Slot};
use libbeachcomber::watch::{ResolvedWatch, WatchOutcome};
use libbeachcomber::{Client, ClientConfig, IntrospectResponse, IntrospectSubject};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug)]
struct Fixture {
    name: String,
    description: String,
    setup: Vec<OpDescriptor>,
    test: OpDescriptor,
    expect: Value,
    source_path: PathBuf,
    /// `get`/`eval`/`watch` op context, parsed from the fixture's top-level
    /// `env`/`cwd`. Templates are not fixture context: a fixture declares
    /// one by a `setup` `put` to `<provider>:template`, like any caller.
    env_vars: HashMap<String, String>,
    cwd: Option<String>,
}

/// Per-fixture context a `get`, `eval` or `watch` op is evaluated against.
struct ResolveCtx<'a> {
    env_vars: &'a HashMap<String, String>,
    cwd: &'a str,
}

impl ResolveCtx<'_> {
    /// The resolver's inputs: the fixture's `cwd` is the slot every fetch
    /// addresses (the convention `bc_get`/`bc_eval` follow), and `exec()`
    /// is off, as it is for an SDK client without the option.
    fn inputs(&self) -> Inputs<'_> {
        Inputs {
            cwd: self.cwd,
            env: self.env_vars,
            slot: Slot::Path(self.cwd.to_string()),
            force: false,
            wait: false,
            allow_exec: false,
            now: std::time::SystemTime::now(),
        }
    }
}

#[derive(Debug, Clone)]
struct OpDescriptor {
    op: String,
    args: Value,
}

fn client_for(sock: &Path) -> Client {
    Client::with_config(ClientConfig {
        timeout: Duration::from_secs(2),
        auto_start: false,
        exec: false,
    })
    .with_socket_path(sock.to_path_buf())
}

fn load_fixtures() -> Vec<Fixture> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("tests")
        .join("conformance");
    let mut out = Vec::new();
    for op_dir in fs::read_dir(&root).expect("conformance root") {
        let op_dir = op_dir.unwrap();
        if !op_dir.file_type().unwrap().is_dir() {
            continue;
        }
        for entry in fs::read_dir(op_dir.path()).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let text = fs::read_to_string(&path).expect("read fixture");
            let v: Value = serde_json::from_str(&text).expect("parse fixture");
            assert!(
                v.get("virtual").is_none(),
                "{}: the `virtual` block is gone — declare a template by a setup put to <p>:template",
                path.display()
            );
            out.push(Fixture {
                name: v["name"].as_str().unwrap().to_string(),
                description: v["description"].as_str().unwrap().to_string(),
                setup: parse_ops(&v["setup"]),
                test: parse_op(&v["test"]).expect("test op required"),
                expect: v["expect"].clone(),
                source_path: path,
                // A fixture with no `env` is a call with none: the process's.
                env_vars: if v["env"].is_null() {
                    libbeachcomber::env::process()
                } else {
                    parse_str_map(&v["env"])
                },
                cwd: v["cwd"].as_str().map(|s| s.to_string()),
            });
        }
    }
    out
}

fn parse_str_map(v: &Value) -> HashMap<String, String> {
    v.as_object()
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, val)| val.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

fn parse_ops(v: &Value) -> Vec<OpDescriptor> {
    v.as_array()
        .map(|arr| arr.iter().filter_map(parse_op).collect())
        .unwrap_or_default()
}

fn parse_op(v: &Value) -> Option<OpDescriptor> {
    let op = v.get("op")?.as_str()?.to_string();
    let args = v.get("args").cloned().unwrap_or(Value::Null);
    Some(OpDescriptor { op, args })
}

/// Run an op through the SDK, return a canonical response shape that
/// expectation-checking can uniformly consume.
struct CanonicalResponse {
    ok: bool,
    data: Option<Value>,
    data_as_text: Option<String>,
    error: Option<String>,
}

fn run_op(client: &Client, descriptor: &OpDescriptor, resolve: &ResolveCtx) -> CanonicalResponse {
    match descriptor.op.as_str() {
        "hello" => match client.hello() {
            Ok(info) => CanonicalResponse {
                ok: true,
                data: Some(serde_json::json!({
                    "protocol_version": info.protocol_version,
                    "daemon_version": info.daemon_version,
                })),
                data_as_text: None,
                error: None,
            },
            Err(e) => error_response(e),
        },
        // `get` is what `comb get` prints, through the one resolver every
        // surface shares (`libbeachcomber::resolve`): the fixture's `env` is
        // the caller's environment, its `cwd` the directory every fetch
        // addresses, and templates come from the daemon — the fixture's
        // `setup` `put`s to `<p>:template` seed them.
        "get" => {
            let key = descriptor.args["key"].as_str().unwrap_or("");
            match resolve::resolve(key, &resolve.inputs(), &mut { client }) {
                Ok(v) => CanonicalResponse {
                    ok: true,
                    data_as_text: v
                        .clone()
                        .and_then(|v| libbeachcomber::CombData::from_json(v).as_text()),
                    data: v,
                    error: None,
                },
                Err(e) => err_resp(e.to_string()),
            }
        }
        "refresh" => {
            let key = descriptor.args["key"].as_str().unwrap_or("");
            let path = descriptor.args["path"].as_str();
            match client.refresh(key, path) {
                Ok(()) => CanonicalResponse {
                    ok: true,
                    data: None,
                    data_as_text: None,
                    error: None,
                },
                Err(e) => error_response(e),
            }
        }
        "context" => {
            // Context is stateful per-connection; the one-shot Client can't
            // express this via a fire-and-forget call. Session exposes it —
            // for conformance we just assert via a fresh Session.
            let socket_path = descriptor.args["path"].as_str().unwrap_or("/tmp");
            match client.session() {
                Ok(mut s) => match s.set_context(socket_path) {
                    Ok(()) => CanonicalResponse {
                        ok: true,
                        data: None,
                        data_as_text: None,
                        error: None,
                    },
                    Err(e) => error_response(e),
                },
                Err(e) => error_response(e),
            }
        }
        "put" => {
            let key = descriptor.args["key"].as_str().unwrap_or("");
            let data = descriptor.args["data"].clone();
            let ttl = descriptor.args["ttl"].as_str();
            let path = descriptor.args["path"].as_str();
            match client.put(key, data, ttl, path) {
                Ok(()) => CanonicalResponse {
                    ok: true,
                    data: None,
                    data_as_text: None,
                    error: None,
                },
                Err(e) => error_response(e),
            }
        }
        "status" => match client.status() {
            Ok(rows) => {
                let arr: Vec<Value> = rows
                    .into_iter()
                    .map(|r| {
                        serde_json::json!({
                            "provider": r.provider,
                            "field": r.field,
                            "path": r.path,
                            "value": r.value,
                            "age_ms": r.age_ms,
                            "stale": r.stale,
                        })
                    })
                    .collect();
                CanonicalResponse {
                    ok: true,
                    data: Some(Value::Array(arr)),
                    data_as_text: None,
                    error: None,
                }
            }
            Err(e) => error_response(e),
        },
        // `watch` is `resolve` over time (`libbeachcomber::watch`): it opens
        // through the same `Inputs` the `resolve`/`eval` arms build from the
        // fixture's `cwd`/`env`, and its first event is the value that
        // `resolve` would answer for `key` right now — no daemon round trip
        // needed to observe it.
        "watch" => {
            let key = descriptor.args["key"].as_str().unwrap_or("");
            match ResolvedWatch::open(client, key, &resolve.inputs()) {
                Ok(mut watch) => match watch.next(Some(Duration::from_secs(5))) {
                    Ok(WatchOutcome::Value(v)) => CanonicalResponse {
                        ok: true,
                        data: v,
                        data_as_text: None,
                        error: None,
                    },
                    Ok(other) => err_resp(format!("watch: unexpected outcome {other:?}")),
                    Err(e) => err_resp(e.to_string()),
                },
                Err(e) => err_resp(e.to_string()),
            }
        }
        "introspect" => {
            let subject_name = descriptor.args["subject"].as_str().unwrap_or("daemon");
            let subject: IntrospectSubject = subject_name
                .parse()
                .unwrap_or_else(|e| panic!("in fixture: {e}"));
            let dur = descriptor.args["duration_secs"].as_u64();
            match client.introspect(subject, dur) {
                Ok(IntrospectResponse::Daemon(health)) => CanonicalResponse {
                    ok: true,
                    data: Some(serde_json::json!({
                        "pid": health.pid,
                        "version": health.version,
                        "uptime_secs": health.uptime_secs,
                        "socket_path": health.socket_path,
                        "config_path": health.config_path,
                        "requests_total": health.requests_total,
                        "in_flight": health.in_flight,
                        "active_watchers": health.active_watchers,
                        "cache_entries": health.cache_entries,
                    })),
                    data_as_text: None,
                    error: None,
                },
                Ok(IntrospectResponse::Other(v)) => CanonicalResponse {
                    ok: true,
                    data: Some(v),
                    data_as_text: None,
                    error: None,
                },
                Err(e) => error_response(e),
            }
        }
        // `eval` renders a template stored nowhere, with the inputs `get`
        // reads with.
        "eval" => {
            let template = descriptor.args["template"].as_str().unwrap_or("");
            match resolve::eval(template, &resolve.inputs(), &mut { client }) {
                Ok(v) => CanonicalResponse {
                    ok: true,
                    data: v,
                    data_as_text: None,
                    error: None,
                },
                Err(e) => err_resp(e.to_string()),
            }
        }
        other => panic!("unknown op in fixture: {other}"),
    }
}

fn error_response(e: libbeachcomber::CombError) -> CanonicalResponse {
    CanonicalResponse {
        ok: false,
        data: None,
        data_as_text: None,
        error: Some(e.to_string()),
    }
}

fn err_resp(msg: String) -> CanonicalResponse {
    CanonicalResponse {
        ok: false,
        data: None,
        data_as_text: None,
        error: Some(msg),
    }
}

/// Every expectation kind documented in tests/conformance/README.md. A
/// fixture using a key outside this set fails loudly rather than being
/// silently ignored — otherwise a fixture can pass vacuously, asserting
/// something no runner checks.
const KNOWN_EXPECT_KEYS: &[&str] = &[
    "status",
    "data_type",
    "data_equals",
    "data_as_text",
    "data_contains_field",
    "data_field_equals",
    "error_contains",
];

fn check_expect(fixture: &Fixture, resp: &CanonicalResponse) -> Result<(), String> {
    let expect = &fixture.expect;

    if let Some(obj) = expect.as_object() {
        for key in obj.keys() {
            if !KNOWN_EXPECT_KEYS.contains(&key.as_str()) {
                return Err(format!(
                    "fixture uses unknown expectation key {key:?} — the runner has no check for it"
                ));
            }
        }
    }

    // status
    if let Some(status) = expect.get("status").and_then(|v| v.as_str()) {
        match status {
            "ok" => {
                if !resp.ok {
                    return Err(format!(
                        "status=ok expected but response was error: {:?}",
                        resp.error
                    ));
                }
            }
            "hit" => {
                if !resp.ok {
                    return Err(format!(
                        "status=hit expected but response was error: {:?}",
                        resp.error
                    ));
                }
                if resp.data.is_none() {
                    return Err("status=hit expected but data was absent".into());
                }
            }
            "miss" => {
                if !resp.ok {
                    return Err(format!(
                        "status=miss expected but response was error: {:?}",
                        resp.error
                    ));
                }
                if resp.data.is_some() {
                    return Err("status=miss expected but data was present".into());
                }
            }
            "error" => {
                if resp.ok {
                    return Err("status=error expected but response was ok".into());
                }
            }
            other => return Err(format!("unknown status: {other}")),
        }
    }

    // data_type
    if let Some(dtype) = expect.get("data_type").and_then(|v| v.as_str()) {
        let actual = match &resp.data {
            Some(Value::String(_)) => "string",
            Some(Value::Number(_)) => "number",
            Some(Value::Bool(_)) => "bool",
            Some(Value::Object(_)) => "object",
            Some(Value::Array(_)) => "array",
            Some(Value::Null) | None => "null",
        };
        if actual != dtype {
            return Err(format!(
                "data_type={dtype} expected but got {actual}: data={:?}",
                resp.data
            ));
        }
    }

    // data_equals
    if let Some(expected) = expect.get("data_equals") {
        match &resp.data {
            Some(actual) if actual == expected => {}
            other => {
                return Err(format!(
                    "data_equals failed: expected {expected}, got {other:?}"
                ));
            }
        }
    }

    // data_as_text
    if let Some(expected) = expect.get("data_as_text").and_then(|v| v.as_str()) {
        let actual = resp.data_as_text.as_deref().unwrap_or("");
        if actual != expected {
            return Err(format!(
                "data_as_text={expected:?} expected but got {actual:?}"
            ));
        }
    }

    // data_contains_field
    if let Some(field) = expect.get("data_contains_field").and_then(|v| v.as_str()) {
        match &resp.data {
            Some(Value::Object(obj)) if obj.contains_key(field) => {}
            other => {
                return Err(format!(
                    "data_contains_field={field} failed: data={other:?}"
                ));
            }
        }
    }

    // data_field_equals
    if let Some(spec) = expect.get("data_field_equals") {
        let field = spec.get("field").and_then(|v| v.as_str()).unwrap_or("");
        let expected = spec.get("value").cloned().unwrap_or(Value::Null);
        match &resp.data {
            Some(Value::Object(obj)) => match obj.get(field) {
                Some(actual) if *actual == expected => {}
                other => {
                    return Err(format!(
                        "data_field_equals failed for {field}: expected {expected}, got {other:?}"
                    ));
                }
            },
            other => {
                return Err(format!(
                    "data_field_equals: data is not an object: {other:?}"
                ));
            }
        }
    }

    // error_contains
    if let Some(substr) = expect.get("error_contains").and_then(|v| v.as_str()) {
        let actual = resp.error.as_deref().unwrap_or("");
        if !actual.contains(substr) {
            return Err(format!(
                "error_contains={substr:?} expected but error was {actual:?}"
            ));
        }
    }

    Ok(())
}

#[test]
fn conformance_suite() {
    let fixtures = load_fixtures();
    assert!(
        !fixtures.is_empty(),
        "no fixtures found under tests/conformance/"
    );
    let mut failures = Vec::new();
    for fixture in &fixtures {
        let guard = DaemonGuard::spawn();
        let client = client_for(&guard.path);

        // Default cwd for fixtures that don't declare one: this
        // fixture's own temp directory (unique per DaemonGuard::spawn()).
        let default_cwd = guard
            .path
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let cwd = fixture.cwd.clone().unwrap_or(default_cwd);
        let resolve_ctx = ResolveCtx {
            env_vars: &fixture.env_vars,
            cwd: &cwd,
        };

        // Run setup ops, ignoring their responses.
        for setup_op in &fixture.setup {
            let _ = run_op(&client, setup_op, &resolve_ctx);
        }

        let resp = run_op(&client, &fixture.test, &resolve_ctx);
        if let Err(reason) = check_expect(fixture, &resp) {
            failures.push(format!(
                "[{}] {}\n  path: {}\n  {}",
                fixture.name,
                fixture.description,
                fixture.source_path.display(),
                reason
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} conformance failures out of {} fixtures:\n{}",
        failures.len(),
        fixtures.len(),
        failures.join("\n\n")
    );
    println!("conformance_suite: {} fixtures all passed", fixtures.len());
}
