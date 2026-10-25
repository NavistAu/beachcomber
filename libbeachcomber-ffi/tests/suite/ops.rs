//! Operations: `bc_get`, `bc_put`, `bc_refresh`, `bc_status`,
//! `bc_introspect`, `bc_hello`. Task 3.4 of
//! docs/superpowers/plans/2026-08-15-client-abi-and-sdk-refactor.md.
//!
//! Runs against a real in-process daemon spawned by `tests/suite/common/daemon.rs`.

use crate::common::client::{client_for, cs, read};
use crate::common::daemon::DaemonGuard;

use std::ptr;
use std::time::Duration;

use beachcomber::{
    BC_GET_FORCE, BC_GET_WAIT, bc_client_free, bc_get, bc_hello, bc_introspect, bc_put, bc_refresh,
    bc_status,
};

#[test]
fn get_returns_ok_envelope_for_a_real_key() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let key = cs("hostname.short");

    let v = read(unsafe { bc_get(client, key.as_ptr(), ptr::null(), ptr::null(), 0) });
    assert_eq!(v["ok"], serde_json::json!(true));

    unsafe { bc_client_free(client) };
}

#[test]
fn get_reserved_flag_bit_is_rejected_not_ignored() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let key = cs("hostname.short");

    let v = read(unsafe { bc_get(client, key.as_ptr(), ptr::null(), ptr::null(), 1 << 5) });
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

    let get_v = read(unsafe { bc_get(client, field_key.as_ptr(), ptr::null(), ptr::null(), 0) });
    assert_eq!(get_v["ok"], serde_json::json!(true));
    assert_eq!(get_v["data"], serde_json::json!("hello"));

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

    let v = read(unsafe { bc_get(client, node_key.as_ptr(), ptr::null(), ptr::null(), 0) });
    assert_eq!(v["data"], serde_json::Value::Null);

    unsafe { bc_client_free(client) };
}

/// `bc_get` is what `comb get` prints: a template renders, and wins over the
/// cached value it shares an address with; a miss is `null`. The unrendered
/// halves are facets, read like any key.
#[test]
fn get_renders_and_the_facets_read_what_is_stored() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let template_ns = cs("p:template");
    let template_data = cs(r#"{"t":"{{ 1 }}","v":"{{ 2 }}","s":"{{ {'x': 3} }}"}"#);
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
    let get = |key: &str| {
        let key = cs(key);
        read(unsafe { bc_get(client, key.as_ptr(), ptr::null(), ptr::null(), 0) })
    };

    for (key, value) in [
        ("p.t", serde_json::json!(1)),
        ("p.v", serde_json::json!(2)),
        ("p.s.x", serde_json::json!(3)),
        ("p.d", serde_json::json!("plain")),
        ("p.nope", serde_json::Value::Null),
        ("p:template.v", serde_json::json!("{{ 2 }}")),
        ("p:put.v", serde_json::json!(1)),
    ] {
        let v = get(key);
        assert_eq!(v["ok"], serde_json::json!(true), "{key}: {v}");
        assert_eq!(v["data"], value, "{key}: {v}");
    }

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
    read(unsafe { bc_get(client, cs("hostname").as_ptr(), ptr::null(), ptr::null(), 0) });
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
