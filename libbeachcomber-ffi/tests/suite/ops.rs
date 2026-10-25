//! Operations: `bc_get`, `bc_put`, `bc_refresh`, `bc_status`,
//! `bc_introspect`, `bc_hello`. Task 3.4 of
//! docs/superpowers/plans/2026-08-15-client-abi-and-sdk-refactor.md.
//!
//! Runs against a real in-process daemon spawned by `tests/suite/common/daemon.rs`.

use crate::common::daemon::DaemonGuard;

use std::ffi::{CStr, CString, c_char};
use std::path::Path;
use std::ptr;
use std::time::Duration;

use beachcomber::envelope::bc_string_free;
use beachcomber::{
    BC_GET_FORCE, BC_GET_WAIT, BcClient, bc_client_free, bc_client_new, bc_get, bc_hello,
    bc_introspect, bc_put, bc_refresh, bc_status,
};

fn client_for(sock: &Path) -> *mut BcClient {
    let json = format!(
        r#"{{"socket_path":"{}","autostart":false,"timeout_ms":2000}}"#,
        sock.display()
    );
    let cs = CString::new(json).unwrap();
    unsafe { bc_client_new(cs.as_ptr()) }
}

fn cs(s: &str) -> CString {
    CString::new(s).unwrap()
}

fn read(ptr: *mut c_char) -> serde_json::Value {
    assert!(!ptr.is_null(), "call_ffi must never return NULL");
    let body = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap().to_string();
    unsafe { bc_string_free(ptr) };
    serde_json::from_str(&body).expect("envelope must be valid JSON")
}

#[test]
fn get_returns_ok_envelope_for_a_real_key() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let key = cs("hostname.short");

    let v = read(unsafe { bc_get(client, key.as_ptr(), ptr::null(), 0) });
    assert_eq!(v["ok"], serde_json::json!(true));

    unsafe { bc_client_free(client) };
}

#[test]
fn get_reserved_flag_bit_is_rejected_not_ignored() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let key = cs("hostname.short");

    let v = read(unsafe { bc_get(client, key.as_ptr(), ptr::null(), 1 << 5) });
    assert_eq!(v["ok"], serde_json::json!(false));
    assert_eq!(v["error"]["kind"], serde_json::json!("bad_flags"));

    unsafe { bc_client_free(client) };
}

#[test]
fn get_accepts_the_documented_flag_combination() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let key = cs("hostname.short");

    let v = read(unsafe {
        bc_get(
            client,
            key.as_ptr(),
            ptr::null(),
            BC_GET_FORCE | BC_GET_WAIT,
        )
    });
    assert_eq!(v["ok"], serde_json::json!(true));

    unsafe { bc_client_free(client) };
}

#[test]
fn put_get_round_trip() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    // `put`'s key names the static namespace; `data`'s top-level keys become
    // its records (see tests/conformance/put/nested_object_data.json).
    let provider_key = cs("myvirtual");
    let field_key = cs("myvirtual.field");
    let data = cs(r#"{"field":"hello"}"#);

    let put_v = read(unsafe {
        bc_put(
            client,
            provider_key.as_ptr(),
            data.as_ptr(),
            ptr::null(),
            ptr::null(),
        )
    });
    assert_eq!(put_v["ok"], serde_json::json!(true));

    let get_v = read(unsafe { bc_get(client, field_key.as_ptr(), ptr::null(), 0) });
    assert_eq!(get_v["ok"], serde_json::json!(true));
    assert_eq!(get_v["data"]["data"], serde_json::json!("hello"));

    unsafe { bc_client_free(client) };
}

#[test]
fn bc_put_with_null_data_removes() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let provider_key = cs("nullrm");
    let node_key = cs("nullrm.field");
    let data = cs(r#"{"field":"hello"}"#);
    let null = cs("null");

    let v = read(unsafe {
        bc_put(
            client,
            provider_key.as_ptr(),
            data.as_ptr(),
            ptr::null(),
            ptr::null(),
        )
    });
    assert_eq!(v["ok"], serde_json::json!(true));

    let v = read(unsafe {
        bc_put(
            client,
            node_key.as_ptr(),
            null.as_ptr(),
            ptr::null(),
            ptr::null(),
        )
    });
    assert_eq!(v["ok"], serde_json::json!(true));

    let v = read(unsafe { bc_get(client, node_key.as_ptr(), ptr::null(), 0) });
    assert_eq!(v["data"]["data"], serde_json::Value::Null);

    unsafe { bc_client_free(client) };
}

/// The `get` envelope carries the template half (one-tree design §3.4, plan
/// 3 design decision 3): keys are added, never changed. A template-only node
/// is the null triple plus `template`; a valued node with a template has
/// both; a data-only node and a miss are exactly what they were.
#[test]
fn get_envelope_carries_the_template_half() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let keys = |v: &serde_json::Value| -> Vec<String> {
        let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        k.sort();
        k
    };

    let template_ns = cs("p:template");
    let template_data = cs(r#"{"t":"{{ 1 }}","v":"{{ 2 }}"}"#);
    let ns = cs("p");
    let data = cs(r#"{"v":1,"d":"plain"}"#);
    for (key, body) in [(&template_ns, &template_data), (&ns, &data)] {
        let put_v = read(unsafe {
            bc_put(
                client,
                key.as_ptr(),
                body.as_ptr(),
                ptr::null(),
                ptr::null(),
            )
        });
        assert_eq!(put_v["ok"], serde_json::json!(true), "{put_v}");
    }

    let template_only = cs("p.t");
    let v = read(unsafe { bc_get(client, template_only.as_ptr(), ptr::null(), 0) });
    assert_eq!(v["ok"], serde_json::json!(true), "{v}");
    assert_eq!(keys(&v["data"]), ["age_ms", "data", "stale", "template"]);
    assert_eq!(v["data"]["data"], serde_json::Value::Null);
    assert_eq!(v["data"]["template"], serde_json::json!("{{ 1 }}"));

    let past_a_string = cs("p.t.x");
    let v = read(unsafe { bc_get(client, past_a_string.as_ptr(), ptr::null(), 0) });
    assert_eq!(
        keys(&v["data"]),
        ["age_ms", "data", "rest", "stale", "template"]
    );
    assert_eq!(v["data"]["rest"], serde_json::json!("x"));

    let both = cs("p.v");
    let v = read(unsafe { bc_get(client, both.as_ptr(), ptr::null(), 0) });
    assert_eq!(keys(&v["data"]), ["age_ms", "data", "stale", "template"]);
    assert_eq!(v["data"]["data"], serde_json::json!(1));
    assert_eq!(v["data"]["template"], serde_json::json!("{{ 2 }}"));
    assert!(v["data"]["age_ms"].is_number(), "{v}");
    assert!(v["data"]["stale"].is_boolean(), "{v}");

    let data_only = cs("p.d");
    let v = read(unsafe { bc_get(client, data_only.as_ptr(), ptr::null(), 0) });
    assert_eq!(keys(&v["data"]), ["age_ms", "data", "stale"]);
    assert_eq!(v["data"]["data"], serde_json::json!("plain"));

    let miss = cs("p.nope");
    let v = read(unsafe { bc_get(client, miss.as_ptr(), ptr::null(), 0) });
    assert_eq!(keys(&v["data"]), ["age_ms", "data", "stale"]);
    assert_eq!(
        v["data"],
        serde_json::json!({"data": null, "age_ms": null, "stale": null})
    );

    unsafe { bc_client_free(client) };
}

#[test]
fn refresh_returns_ok_envelope() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let key = cs("hostname.short");

    let v = read(unsafe { bc_refresh(client, key.as_ptr(), ptr::null()) });
    assert_eq!(v["ok"], serde_json::json!(true));

    unsafe { bc_client_free(client) };
}

/// A `server_error` envelope's message is the daemon's words alone — the
/// same text `comb refresh` prints — since `kind` already says whose words
/// they are. The C SDK reads this envelope verbatim, so this is its proof.
#[test]
fn refresh_refusal_envelope_carries_the_daemons_words_alone() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let key = cs("sourceless");
    let data = cs(r#"{"v":1}"#);

    let put_v = read(unsafe {
        bc_put(
            client,
            key.as_ptr(),
            data.as_ptr(),
            ptr::null(),
            ptr::null(),
        )
    });
    assert_eq!(put_v["ok"], serde_json::json!(true));

    let v = read(unsafe { bc_refresh(client, key.as_ptr(), ptr::null()) });
    assert_eq!(v["ok"], serde_json::json!(false));
    assert_eq!(v["error"]["kind"], serde_json::json!("server_error"));
    // A prefix, not equality: on a connection with detected version skew
    // the client appends ` (client is …, daemon is …)`, which a dirty tree
    // can produce between this crate and the in-process daemon.
    let message = v["error"]["message"].as_str().unwrap();
    assert!(
        message.starts_with("nothing to refresh: sourceless has no sources"),
        "the daemon's words alone, got {message:?}"
    );

    unsafe { bc_client_free(client) };
}

#[test]
fn status_returns_an_array_envelope() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);

    let v = read(unsafe { bc_status(client) });
    assert_eq!(v["ok"], serde_json::json!(true));
    assert!(v["data"].is_array());

    unsafe { bc_client_free(client) };
}

/// A `bc_status` row is the daemon's row: the same fields, `next_poll_in_secs`
/// among them, and an absent one omitted rather than `null` — one schema, not
/// a copy of it kept in step by hand.
#[test]
fn status_rows_are_the_daemons_rows() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    read(unsafe { bc_get(client, cs("hostname").as_ptr(), ptr::null(), 0) });
    let put = read(unsafe {
        bc_put(
            client,
            cs("polled").as_ptr(),
            cs(r#"{"x":1}"#).as_ptr(),
            cs("1m").as_ptr(),
            ptr::null(),
        )
    });
    assert_eq!(put["ok"], true, "{put}");

    let wire = {
        use std::io::{BufRead, Write};
        let mut stream = std::os::unix::net::UnixStream::connect(&daemon.path).unwrap();
        stream.write_all(b"{\"op\":\"status\"}\n").unwrap();
        let mut line = String::new();
        std::io::BufReader::new(stream)
            .read_line(&mut line)
            .unwrap();
        serde_json::from_str::<serde_json::Value>(&line).unwrap()["data"].clone()
    };
    let abi = read(unsafe { bc_status(client) })["data"].clone();

    let shape = |rows: &serde_json::Value| -> Vec<(String, Vec<String>)> {
        let mut shape: Vec<_> = rows
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                let id = format!("{}.{}", row["provider"], row["field"]);
                let mut keys: Vec<String> = row.as_object().unwrap().keys().cloned().collect();
                keys.sort();
                (id, keys)
            })
            .collect();
        shape.sort();
        shape
    };
    assert!(!shape(&wire).is_empty(), "hostname left rows: {wire}");
    assert_eq!(shape(&abi), shape(&wire));
    assert!(
        wire.as_array()
            .unwrap()
            .iter()
            .any(|row| row.get("next_poll_in_secs").is_some()),
        "a row with a ttl says when it polls next: {wire}"
    );

    unsafe { bc_client_free(client) };
}

#[test]
fn introspect_daemon_subject() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let subject = cs("daemon");

    let v = read(unsafe { bc_introspect(client, subject.as_ptr(), ptr::null()) });
    assert_eq!(v["ok"], serde_json::json!(true));
    assert!(v["data"]["pid"].is_number());
    assert!(v["data"]["version"].is_string());

    unsafe { bc_client_free(client) };
}

#[test]
fn introspect_procs_subject_honours_duration_secs_option() {
    // The `procs` subject samples exec events for `duration_secs` before
    // reporting, succeeding or not depending on OS tracing privileges
    // (`eslogger` on macOS, akin to `uptime_provider_executes` needing an
    // unsandboxed environment). What this test can assert regardless of
    // that: the option actually reached the daemon rather than being
    // dropped — proven by the call taking ~1s (the requested duration), not
    // the 2s default `handle_introspect_procs` falls back to when
    // `duration_secs` is absent.
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let subject = cs("procs");
    let options = cs(r#"{"duration_secs":1}"#);

    let start = std::time::Instant::now();
    let v = read(unsafe { bc_introspect(client, subject.as_ptr(), options.as_ptr()) });
    let elapsed = start.elapsed();

    assert!(v["ok"].is_boolean(), "envelope must have an ok field: {v}");
    assert!(
        elapsed < Duration::from_millis(1800),
        "duration_secs=1 must not fall back to the 2s default; took {elapsed:?}"
    );

    unsafe { bc_client_free(client) };
}

#[test]
fn hello_returns_both_versions() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);

    let v = read(unsafe { bc_hello(client) });
    assert_eq!(v["ok"], serde_json::json!(true));
    assert!(v["data"]["protocol_version"].is_string());
    assert!(v["data"]["daemon_version"].is_string());

    unsafe { bc_client_free(client) };
}
