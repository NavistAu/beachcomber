// Conformance runner — drives tests/conformance/**/*.json against
// beachcomber_client. See tests/conformance/README.md for fixture shape.
//
// Each fixture spawns its own daemon for isolation. A fixture that
// breaks any `expect` rule fails a Rust test bearing the fixture's `name`.

mod common;
use common::socket::IsolatedSocket;

use beachcomber::config::Config;
use beachcomber::daemon;
use libbeachcomber::{Client, ClientConfig, CombResult, IntrospectResponse, IntrospectSubject};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

#[derive(Debug)]
struct Fixture {
    name: String,
    description: String,
    setup: Vec<OpDescriptor>,
    test: OpDescriptor,
    expect: Value,
    source_path: PathBuf,
}

#[derive(Debug, Clone)]
struct OpDescriptor {
    op: String,
    args: Value,
}

fn spawn_daemon() -> (IsolatedSocket, PathBuf) {
    let iso = IsolatedSocket::new();
    let sock = iso.path.clone();
    let sock_clone = sock.clone();
    thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let handle = daemon::start_in_process(sock_clone, Config::default());
            tokio::time::sleep(Duration::from_millis(500)).await;
            handle.await.ok();
        });
    });
    thread::sleep(Duration::from_millis(200));
    (iso, sock)
}

fn client_for(sock: &Path) -> Client {
    Client::with_config(ClientConfig {
        timeout: Duration::from_secs(2),
        auto_start: false,
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
            out.push(Fixture {
                name: v["name"].as_str().unwrap().to_string(),
                description: v["description"].as_str().unwrap().to_string(),
                setup: parse_ops(&v["setup"]),
                test: parse_op(&v["test"]).expect("test op required"),
                expect: v["expect"].clone(),
                source_path: path,
            });
        }
    }
    out
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
    age_ms: Option<u64>,
    stale: Option<bool>,
    error: Option<String>,
}

fn run_op(client: &Client, descriptor: &OpDescriptor) -> CanonicalResponse {
    match descriptor.op.as_str() {
        "hello" => match client.hello() {
            Ok(info) => CanonicalResponse {
                ok: true,
                data: Some(serde_json::json!({
                    "protocol_version": info.protocol_version,
                    "daemon_version": info.daemon_version,
                })),
                data_as_text: None,
                age_ms: None,
                stale: None,
                error: None,
            },
            Err(e) => error_response(e),
        },
        "get" => {
            let key = descriptor.args["key"].as_str().unwrap_or("");
            let path = descriptor.args["path"].as_str();
            match client.get(key, path) {
                Ok(CombResult::Hit {
                    data,
                    age_ms,
                    stale,
                }) => CanonicalResponse {
                    ok: true,
                    data: Some(data.as_value().clone()),
                    data_as_text: data.as_text(),
                    age_ms: Some(age_ms as u64),
                    stale: Some(stale),
                    error: None,
                },
                Ok(CombResult::Miss) => CanonicalResponse {
                    ok: true,
                    data: None,
                    data_as_text: None,
                    age_ms: None,
                    stale: None,
                    error: None,
                },
                Err(e) => error_response(e),
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
                    age_ms: None,
                    stale: None,
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
                        age_ms: None,
                        stale: None,
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
                    age_ms: None,
                    stale: None,
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
                    age_ms: None,
                    stale: None,
                    error: None,
                }
            }
            Err(e) => error_response(e),
        },
        "watch" => {
            let key = descriptor.args["key"].as_str().unwrap_or("");
            let path = descriptor.args["path"].as_str();
            match client.watch(key, path) {
                Ok(mut stream) => match stream.next_event() {
                    Ok(Some(ev)) => {
                        let data = ev.data.as_ref().map(|d| d.as_value().clone());
                        let data_as_text = ev.data.as_ref().and_then(|d| d.as_text());
                        CanonicalResponse {
                            ok: true,
                            data,
                            data_as_text,
                            age_ms: Some(ev.age_ms),
                            stale: Some(ev.stale),
                            error: None,
                        }
                    }
                    Ok(None) => CanonicalResponse {
                        ok: true,
                        data: None,
                        data_as_text: None,
                        age_ms: None,
                        stale: None,
                        error: None,
                    },
                    Err(e) => error_response(e),
                },
                Err(e) => error_response(e),
            }
        }
        "introspect" => {
            let subject_name = descriptor.args["subject"].as_str().unwrap_or("daemon");
            let subject = match subject_name {
                "daemon" => IntrospectSubject::Daemon,
                "providers" => IntrospectSubject::Providers,
                "config" => IntrospectSubject::Config,
                "cache" => IntrospectSubject::Cache,
                "lifecycle" => IntrospectSubject::Lifecycle,
                "watches" => IntrospectSubject::Watches,
                "timers" => IntrospectSubject::Timers,
                "demand" => IntrospectSubject::Demand,
                "procs" => IntrospectSubject::Procs,
                other => panic!("unknown introspect subject in fixture: {other}"),
            };
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
                    age_ms: None,
                    stale: None,
                    error: None,
                },
                Ok(IntrospectResponse::Other(v)) => CanonicalResponse {
                    ok: true,
                    data: Some(v),
                    data_as_text: None,
                    age_ms: None,
                    stale: None,
                    error: None,
                },
                Err(e) => error_response(e),
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
        age_ms: None,
        stale: None,
        error: Some(e.to_string()),
    }
}

fn check_expect(fixture: &Fixture, resp: &CanonicalResponse) -> Result<(), String> {
    let expect = &fixture.expect;

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

    // age_ms_present
    if let Some(expected) = expect.get("age_ms_present").and_then(|v| v.as_bool()) {
        let actual = resp.age_ms.is_some();
        if actual != expected {
            return Err(format!(
                "age_ms_present={expected} expected but got {actual}"
            ));
        }
    }

    // stale
    if let Some(expected) = expect.get("stale").and_then(|v| v.as_bool()) {
        match resp.stale {
            Some(actual) if actual == expected => {}
            other => {
                return Err(format!("stale={expected} expected but got {other:?}"));
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
        let (_iso, sock) = spawn_daemon();
        let client = client_for(&sock);

        // Run setup ops, ignoring their responses.
        for setup_op in &fixture.setup {
            let _ = run_op(&client, setup_op);
        }

        let resp = run_op(&client, &fixture.test);
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
