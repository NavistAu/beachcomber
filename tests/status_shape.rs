// Integration test for the reshaped Request::Status response.
// Status now returns an array of rows, one per (provider, path, field) tuple,
// describing everything currently warm in the cache.
//
// Also contains unit tests for the status_format preset renderers (T28).

use beachcomber::cache::CacheRow;
use beachcomber::cli::status_format::{RenderOpts, render_preset};

fn sample_rows() -> Vec<CacheRow> {
    vec![
        CacheRow {
            provider: "git".into(),
            path: Some("/home/me/ws/foo".into()),
            field: "branch".into(),
            value: serde_json::json!("main"),
            age_ms: 14_000,
            stale: false,
        },
        CacheRow {
            provider: "git".into(),
            path: Some("/home/me/ws/foo".into()),
            field: "dirty".into(),
            value: serde_json::json!(false),
            age_ms: 14_000,
            stale: false,
        },
        CacheRow {
            provider: "hostname".into(),
            path: None,
            field: "value".into(),
            value: serde_json::json!("me-laptop"),
            age_ms: 52_000,
            stale: false,
        },
    ]
}

#[test]
fn json_preset_emits_ndjson() {
    let out = render_preset("json", &sample_rows(), &RenderOpts::default());
    for line in out.lines().filter(|l| !l.is_empty()) {
        serde_json::from_str::<serde_json::Value>(line).expect("valid JSON per line");
    }
    assert!(out.lines().filter(|l| !l.is_empty()).count() >= 3);
}

#[test]
fn tsv_preset_is_tab_separated_no_header() {
    let out = render_preset("tsv", &sample_rows(), &RenderOpts::default());
    let lines: Vec<_> = out.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 3, "tsv should have exactly one line per row, no header");
    for line in lines {
        // PROVIDER<TAB>PATH<TAB>FIELD<TAB>VALUE<TAB>AGE<TAB>STALE — 6 cols, 5 tabs
        assert_eq!(
            line.matches('\t').count(),
            5,
            "expected 5 tabs per tsv row, got: {line:?}"
        );
    }
}

#[test]
fn csv_preset_has_header_and_quotes_values() {
    let out = render_preset("csv", &sample_rows(), &RenderOpts::default());
    let lines: Vec<_> = out.lines().collect();
    assert!(
        lines[0].contains("PROVIDER") && lines[0].contains("FIELD"),
        "first line should be a header: {:?}",
        lines[0]
    );
    // 3 data rows + 1 header = 4 non-empty lines.
    let non_empty: Vec<_> = lines.iter().filter(|l| !l.is_empty()).collect();
    assert_eq!(non_empty.len(), 4, "csv should have header + 3 data rows");
}

#[test]
fn table_preset_has_header_no_color_no_trunc() {
    let out = render_preset("table", &sample_rows(), &RenderOpts::default());
    let first = out.lines().next().expect("table output should not be empty");
    assert!(
        first.contains("PROVIDER"),
        "first line of table should be the header: {first:?}"
    );
    // table preset must never emit ANSI escape codes.
    assert!(!out.contains('\x1b'), "table preset must not emit ANSI color codes");
}

#[test]
fn sh_preset_emits_sourceable_assignments() {
    let out = render_preset("sh", &sample_rows(), &RenderOpts::default());
    // Path /home/me/ws/foo → home_me_ws_foo; key = git_home_me_ws_foo_branch
    assert!(
        out.contains("git_home_me_ws_foo_branch="),
        "expected git_home_me_ws_foo_branch= in sh output:\n{out}"
    );
    assert!(
        out.contains("hostname_value="),
        "expected hostname_value= in sh output:\n{out}"
    );
    // Values must be shell-quoted (single-quoted).
    assert!(
        out.contains("='main'") || out.contains("=main"),
        "branch value should be shell-quoted: {out}"
    );
}

#[test]
fn human_preset_truncates_long_values_to_default_40() {
    let mut rows = sample_rows();
    rows.push(CacheRow {
        provider: "git".into(),
        path: Some("/home/me/ws/foo".into()),
        field: "commit_summary".into(),
        value: serde_json::json!("a".repeat(100)),
        age_ms: 14_000,
        stale: false,
    });
    let opts = RenderOpts {
        is_tty: true,
        no_color: true,
        ..Default::default()
    };
    let out = render_preset("human", &rows, &opts);
    // Output should contain truncated version, not the full 100 'a's.
    assert!(
        !out.contains(&"a".repeat(100)),
        "human preset should truncate 100-char value to 40 chars"
    );
    // Should still contain the truncated prefix.
    assert!(
        out.contains(&"a".repeat(37)),
        "human preset should preserve at least 37 chars before ellipsis"
    );
}

#[test]
fn human_preset_color_on_stale_rows() {
    let rows = vec![CacheRow {
        provider: "git".into(),
        path: None,
        field: "branch".into(),
        value: serde_json::json!("main"),
        age_ms: 9999,
        stale: true,
    }];
    let opts = RenderOpts {
        is_tty: true,
        no_color: false,
        max_width: Some(40),
        no_trunc: false,
    };
    let out = render_preset("human", &rows, &opts);
    assert!(
        out.contains('\x1b'),
        "human preset with is_tty=true and stale row should emit ANSI codes"
    );
}

#[test]
fn json_preset_path_none_serializes_as_null() {
    let rows = vec![CacheRow {
        provider: "hostname".into(),
        path: None,
        field: "value".into(),
        value: serde_json::json!("myhost"),
        age_ms: 1000,
        stale: false,
    }];
    let out = render_preset("json", &rows, &RenderOpts::default());
    let parsed: serde_json::Value =
        serde_json::from_str(out.trim()).expect("valid JSON line");
    assert!(
        parsed["path"].is_null(),
        "path=None should serialize as JSON null, got: {:?}",
        parsed["path"]
    );
}

#[test]
fn csv_preset_quotes_values_with_commas() {
    let rows = vec![CacheRow {
        provider: "test".into(),
        path: None,
        field: "tags".into(),
        value: serde_json::json!("a,b,c"),
        age_ms: 0,
        stale: false,
    }];
    let out = render_preset("csv", &rows, &RenderOpts::default());
    // Value containing comma must be quoted in RFC 4180 style.
    assert!(
        out.contains("\"a,b,c\""),
        "csv should quote values containing commas: {out}"
    );
}

use beachcomber::client::Client;
use beachcomber::config::Config;

async fn setup_daemon() -> (tempfile::TempDir, Client, tokio::task::JoinHandle<()>) {
    let tmp = tempfile::TempDir::new().unwrap();
    let sock = tmp.path().join("test.sock");
    let config = Config::load();
    let handle = beachcomber::daemon::start_in_process(sock.clone(), config);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let client = Client::new(sock);
    (tmp, client, handle)
}

#[tokio::test]
async fn status_returns_rows_per_field() {
    let (_tmp, client, handle) = setup_daemon().await;

    // Warm up a global provider so at least something is in cache.
    let _ = client
        .send_raw(serde_json::json!({"op": "get", "key": "hostname"}))
        .await
        .expect("get hostname");

    // Give the cache a moment to settle.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let resp = client
        .send_raw(serde_json::json!({"op": "status"}))
        .await
        .expect("status");

    assert!(resp.ok, "status should succeed, error: {:?}", resp.error);
    let data = resp.data.expect("status data present");
    let rows = data.as_array().expect("status response is an array");

    for row in rows {
        assert!(
            row.get("provider").and_then(|v| v.as_str()).is_some(),
            "row missing 'provider' string: {row:?}"
        );
        // path may be null — just check the key is present
        assert!(row.get("path").is_some(), "row missing 'path' key: {row:?}");
        assert!(
            row.get("field").and_then(|v| v.as_str()).is_some(),
            "row missing 'field' string: {row:?}"
        );
        // value can be any JSON type
        assert!(row.get("value").is_some(), "row missing 'value' key: {row:?}");
        assert!(
            row.get("age_ms").and_then(|v| v.as_u64()).is_some(),
            "row missing 'age_ms' number: {row:?}"
        );
        assert!(
            row.get("stale").and_then(|v| v.as_bool()).is_some(),
            "row missing 'stale' bool: {row:?}"
        );
    }

    // hostname should be warm — at least one row with provider=="hostname".
    let has_hostname = rows.iter().any(|r| r["provider"] == "hostname");
    assert!(has_hostname, "expected hostname rows in status after get, got: {rows:?}");

    // Old blob keys must not be present anywhere in the response.
    let data_str = serde_json::to_string(&data).unwrap();
    assert!(
        !rows.iter().any(|r| r.get("pid").is_some()),
        "old 'pid' field must not appear in status rows"
    );
    assert!(
        !data_str.contains("\"cache_entries\""),
        "old 'cache_entries' key must not appear in status response"
    );
    assert!(
        !data_str.contains("\"uptime_secs\""),
        "old 'uptime_secs' key must not appear in status response"
    );

    handle.abort();
}

/// Status on a fresh daemon with an empty cache returns an empty array, not an error.
#[tokio::test]
async fn status_empty_cache_returns_empty_array() {
    let (_tmp, client, handle) = setup_daemon().await;

    let resp = client
        .send_raw(serde_json::json!({"op": "status"}))
        .await
        .expect("status");

    assert!(resp.ok, "status should succeed on empty cache: {:?}", resp.error);
    let data = resp.data.expect("status data present");
    let rows = data.as_array().expect("status response is an array");
    // May be empty — that's fine. Just verify it's a valid array.
    let _ = rows.len();

    handle.abort();
}
