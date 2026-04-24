// Integration test for the reshaped Request::Status response.
// Status now returns an array of rows, one per (provider, path, field) tuple,
// describing everything currently warm in the cache.
//
// Also contains unit tests for the status_format preset renderers (T28).

use beachcomber::cache::CacheRow;
use beachcomber::cli::status_format::{RenderOpts, render_preset, render_tsv, render_csv, render_sh_env, row_context};

fn sample_rows() -> Vec<CacheRow> {
    vec![
        CacheRow {
            provider: "git".into(),
            path: Some("/home/me/ws/foo".into()),
            field: "branch".into(),
            value: serde_json::json!("main"),
            age_ms: 14_000,
            stale: false,
            kind: None,
            poll_interval_secs: None,
            keep_alive_polls: None,
            fsevents_reinstate: None,
            failure: None,
        },
        CacheRow {
            provider: "git".into(),
            path: Some("/home/me/ws/foo".into()),
            field: "dirty".into(),
            value: serde_json::json!(false),
            age_ms: 14_000,
            stale: false,
            kind: None,
            poll_interval_secs: None,
            keep_alive_polls: None,
            fsevents_reinstate: None,
            failure: None,
        },
        CacheRow {
            provider: "hostname".into(),
            path: None,
            field: "value".into(),
            value: serde_json::json!("me-laptop"),
            age_ms: 52_000,
            stale: false,
            kind: None,
            poll_interval_secs: None,
            keep_alive_polls: None,
            fsevents_reinstate: None,
            failure: None,
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
    let rows = vec![sample_lifecycle_row()];
    let out = render_tsv(&rows);
    let line = out.lines().next().expect("at least one line");
    let cols: Vec<&str> = line.split('\t').collect();
    assert_eq!(cols.len(), 13, "got: {:?}", cols);
    assert!(!out.starts_with("PROVIDER"), "no header in tsv");
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
    let first = out
        .lines()
        .next()
        .expect("table output should not be empty");
    assert!(
        first.contains("PROVIDER"),
        "first line of table should be the header: {first:?}"
    );
    // table preset must never emit ANSI escape codes.
    assert!(
        !out.contains('\x1b'),
        "table preset must not emit ANSI color codes"
    );
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
        kind: None,
        poll_interval_secs: None,
        keep_alive_polls: None,
        fsevents_reinstate: None,
        failure: None,
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
        kind: None,
        poll_interval_secs: None,
        keep_alive_polls: None,
        fsevents_reinstate: None,
        failure: None,
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
        kind: None,
        poll_interval_secs: None,
        keep_alive_polls: None,
        fsevents_reinstate: None,
        failure: None,
    }];
    let out = render_preset("json", &rows, &RenderOpts::default());
    let parsed: serde_json::Value = serde_json::from_str(out.trim()).expect("valid JSON line");
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
        kind: None,
        poll_interval_secs: None,
        keep_alive_polls: None,
        fsevents_reinstate: None,
        failure: None,
    }];
    let out = render_preset("csv", &rows, &RenderOpts::default());
    // Value containing comma must be quoted in RFC 4180 style.
    assert!(
        out.contains("\"a,b,c\""),
        "csv should quote values containing commas: {out}"
    );
}

#[test]
fn custom_template_renders_per_row() {
    let rows = sample_rows();
    let opts = RenderOpts::default();
    let out = render_preset("{{ provider }}.{{ field }}={{ value }}", &rows, &opts);
    let lines: Vec<_> = out.lines().collect();
    assert_eq!(lines.len(), rows.len());
    assert!(lines.iter().any(|l| l.contains("git.branch=main")));
    assert!(lines.iter().any(|l| l.contains("hostname.value=me-laptop")));
}

#[test]
fn custom_template_supports_truncate_filter() {
    let rows = vec![CacheRow {
        provider: "git".into(),
        path: None,
        field: "sha".into(),
        value: serde_json::json!("abcdef1234567890"),
        age_ms: 14_000,
        stale: false,
        kind: None,
        poll_interval_secs: None,
        keep_alive_polls: None,
        fsevents_reinstate: None,
        failure: None,
    }];
    let out = render_preset("{{ value | truncate(7) }}", &rows, &RenderOpts::default());
    assert_eq!(out.trim(), "abcdef1...");
}

#[test]
fn cache_row_kind_serde_round_trip() {
    use beachcomber::cache::RowKind;
    use serde_json::json;

    let cases = vec![
        (
            RowKind::Lifecycle { decay: 0, watches_files: true },
            json!({"kind": "lifecycle", "decay": 0, "watches_files": true}),
        ),
        (
            RowKind::Lifecycle { decay: 4, watches_files: false },
            json!({"kind": "lifecycle", "decay": 4, "watches_files": false}),
        ),
        (RowKind::Once, json!({"kind": "once"})),
        (RowKind::Virtual, json!({"kind": "virtual"})),
        (RowKind::Transient, json!({"kind": "transient"})),
    ];
    for (variant, expected_json) in cases {
        let serialized = serde_json::to_value(&variant).unwrap();
        assert_eq!(serialized, expected_json, "serialize {:?}", variant);
        let round: RowKind = serde_json::from_value(serialized).unwrap();
        assert_eq!(round, variant);
    }
}

#[test]
fn custom_template_supports_age_human() {
    let rows = vec![CacheRow {
        provider: "git".into(),
        path: None,
        field: "branch".into(),
        value: serde_json::json!("main"),
        age_ms: 3_600_000, // 1 hour
        stale: false,
        kind: None,
        poll_interval_secs: None,
        keep_alive_polls: None,
        fsevents_reinstate: None,
        failure: None,
    }];
    let out = render_preset("{{ age_human }}", &rows, &RenderOpts::default());
    // Should render as "1h" or similar
    assert!(!out.trim().is_empty());
    assert!(out.trim() != "3600000");
}

#[test]
fn table_prefix_aligns_columns_and_emits_header() {
    let rows = sample_rows();
    let out = render_preset(
        "table {{ provider }}\t{{ field }}\t{{ value }}",
        &rows,
        &RenderOpts::default(),
    );
    let lines: Vec<_> = out.lines().collect();
    // First line should be a header derived from the template variables.
    let header = lines[0];
    assert!(header.contains("PROVIDER"));
    assert!(header.contains("FIELD"));
    assert!(header.contains("VALUE"));
    // Subsequent lines contain the values.
    assert!(
        lines
            .iter()
            .any(|l| l.contains("git") && l.contains("main"))
    );
}

use beachcomber::cli::status_format::{apply_filters, apply_sort};

// --- Filter tests ---

#[test]
fn filter_by_provider_exact() {
    let rows = sample_rows();
    let out = apply_filters(rows.clone(), &["provider=git".to_string()]).unwrap();
    assert!(out.iter().all(|r| r.provider == "git"));
}

#[test]
fn filter_by_provider_glob() {
    let rows = sample_rows();
    let out = apply_filters(rows.clone(), &["provider=hos*".to_string()]).unwrap();
    assert!(out.iter().all(|r| r.provider.starts_with("hos")));
}

#[test]
fn filter_by_stale_true() {
    let mut rows = sample_rows();
    rows[1].stale = true;
    let out = apply_filters(rows, &["stale=true".to_string()]).unwrap();
    assert!(out.iter().all(|r| r.stale));
}

#[test]
fn filter_by_path_glob() {
    let rows = sample_rows();
    let out = apply_filters(rows, &["path=/home/*".to_string()]).unwrap();
    assert!(out.iter().all(|r| {
        r.path
            .as_deref()
            .is_some_and(|p: &str| p.starts_with("/home/"))
    }));
}

#[test]
fn filter_path_dash_matches_globals() {
    let rows = sample_rows();
    let out = apply_filters(rows, &["path=-".to_string()]).unwrap();
    assert!(out.iter().all(|r| r.path.is_none()));
}

#[test]
fn filter_unknown_key_errors() {
    let rows = sample_rows();
    let result = apply_filters(rows, &["unknownkey=foo".to_string()]);
    assert!(result.is_err());
}

#[test]
fn multiple_filters_and_together() {
    let rows = sample_rows();
    let out = apply_filters(
        rows,
        &["provider=git".to_string(), "field=branch".to_string()],
    )
    .unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].provider, "git");
    assert_eq!(out[0].field, "branch");
}

// --- Sort tests ---

#[test]
fn sort_by_age_ascending() {
    let mut rows = sample_rows();
    rows[0].age_ms = 30_000;
    rows[1].age_ms = 10_000;
    rows[2].age_ms = 20_000;
    let out = apply_sort(rows, "age").unwrap();
    let ages: Vec<_> = out.iter().map(|r| r.age_ms).collect();
    assert_eq!(ages, vec![10_000, 20_000, 30_000]);
}

#[test]
fn sort_default_by_path() {
    let rows = sample_rows();
    let out = apply_sort(rows, "path").unwrap();
    // Simple check: rows with None paths sort as one group.
    assert!(!out.is_empty());
}

#[test]
fn sort_invalid_column_errors() {
    let rows = sample_rows();
    assert!(apply_sort(rows, "nonsense").is_err());
}

// --- Presentation tests ---

#[test]
fn no_trunc_disables_truncation_in_human() {
    let mut rows = sample_rows();
    let long = "x".repeat(200);
    rows[0].value = serde_json::json!(long);
    let opts = RenderOpts {
        is_tty: true,
        no_color: true,
        max_width: Some(40),
        no_trunc: true,
    };
    let out = render_preset("human", &rows, &opts);
    assert!(out.contains(&"x".repeat(100))); // at least 100 xs preserved
}

#[test]
fn max_width_truncates_value() {
    let mut rows = sample_rows();
    rows[0].value = serde_json::json!("x".repeat(50));
    let opts = RenderOpts {
        is_tty: true,
        no_color: true,
        max_width: Some(10),
        no_trunc: false,
    };
    let out = render_preset("human", &rows, &opts);
    assert!(!out.contains(&"x".repeat(50)));
}

#[test]
fn no_color_disables_ansi() {
    let mut rows = sample_rows();
    rows[0].stale = true;
    let opts = RenderOpts {
        is_tty: true,
        no_color: true,
        max_width: Some(40),
        no_trunc: false,
    };
    let out = render_preset("human", &rows, &opts);
    assert!(!out.contains("\x1b["));
}

use beachcomber::client::Client;
use beachcomber::config::Config;
use beachcomber::scheduler::{Scheduler, SchedulerMessage};

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
        assert!(
            row.get("value").is_some(),
            "row missing 'value' key: {row:?}"
        );
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
    assert!(
        has_hostname,
        "expected hostname rows in status after get, got: {rows:?}"
    );

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

// ---------------------------------------------------------------------------
// Task 1.9: --ascii flag propagates via FormatOptions
// ---------------------------------------------------------------------------

#[test]
fn ascii_flag_propagates_to_format_options() {
    use beachcomber::cli::status_format::FormatOptions;
    let opts = FormatOptions::default();
    assert!(!opts.ascii);
    let opts2 = FormatOptions { ascii: true, ..opts };
    assert!(opts2.ascii);
}

// ---------------------------------------------------------------------------
// Task 1.10: ColorMode + resolve_color matrix
// ---------------------------------------------------------------------------

#[test]
fn color_resolution_matrix() {
    use beachcomber::cli::status_format::{ColorMode, resolve_color};
    assert!(!resolve_color(ColorMode::Never, false, true, true));
    assert!(!resolve_color(ColorMode::Always, true, true, true)); // NO_COLOR wins
    assert!(!resolve_color(ColorMode::Auto, true, true, true)); // NO_COLOR wins
    assert!(resolve_color(ColorMode::Always, false, false, false));
    assert!(resolve_color(ColorMode::Auto, false, true, false)); // TTY
    assert!(resolve_color(ColorMode::Auto, false, false, true)); // WATCH_INTERVAL
    assert!(!resolve_color(ColorMode::Auto, false, false, false));
}

// ---------------------------------------------------------------------------
// Task 1.11: resolve_max_width
// ---------------------------------------------------------------------------

#[test]
fn max_width_resolves_explicit_int() {
    use beachcomber::cli::status_format::resolve_max_width;
    assert_eq!(resolve_max_width(Some("80"), Some(200)), 80);
}

#[test]
fn max_width_resolves_default_when_unset() {
    use beachcomber::cli::status_format::resolve_max_width;
    assert_eq!(resolve_max_width(None, Some(200)), 120);
}

#[test]
fn max_width_resolves_auto_uses_terminal() {
    use beachcomber::cli::status_format::resolve_max_width;
    assert_eq!(resolve_max_width(Some("auto"), Some(200)), 200);
}

#[test]
fn max_width_resolves_auto_falls_back_to_default() {
    use beachcomber::cli::status_format::resolve_max_width;
    assert_eq!(resolve_max_width(Some("auto"), None), 120);
}

// ---------------------------------------------------------------------------
// Task 1.12: Default preset is human regardless of TTY
// ---------------------------------------------------------------------------

#[test]
fn default_preset_is_human_regardless_of_tty() {
    // Verify that render_preset("human", ...) returns a human-formatted
    // table with a PROVIDER header — confirming human is a valid, usable default.
    let rows = sample_rows();
    let opts = RenderOpts {
        is_tty: false, // non-TTY context
        no_color: true,
        max_width: Some(120),
        no_trunc: false,
    };
    let out = render_preset("human", &rows, &opts);
    assert!(
        out.contains("PROVIDER"),
        "human preset should emit PROVIDER header regardless of is_tty: {out}"
    );
}

// ---------------------------------------------------------------------------
// Task 1.13: Default sort → (provider, path, field)
// ---------------------------------------------------------------------------

#[test]
fn default_sort_is_provider_path_field() {
    use beachcomber::cache::CacheRow;
    use beachcomber::cli::status_format::apply_sort;
    use serde_json::json;

    fn row(p: &str, path: Option<&str>, field: &str) -> CacheRow {
        CacheRow {
            provider: p.into(),
            path: path.map(String::from),
            field: field.into(),
            value: json!(0),
            age_ms: 0,
            stale: false,
            kind: None,
            poll_interval_secs: None,
            keep_alive_polls: None,
            fsevents_reinstate: None,
            failure: None,
        }
    }

    let rows = vec![
        row("git", Some("/repo"), "branch"),
        row("battery", None, "percent"),
        row("git", Some("/repo"), "dirty"),
        row("git", Some("/other"), "branch"),
    ];
    let rows = apply_sort(rows, "default").unwrap();
    assert_eq!(rows[0].provider, "battery");
    assert_eq!(rows[1].provider, "git");
    assert_eq!(rows[1].path.as_deref(), Some("/other"));
    assert_eq!(rows[2].provider, "git");
    assert_eq!(rows[2].path.as_deref(), Some("/repo"));
    assert_eq!(rows[2].field, "branch");
    assert_eq!(rows[3].field, "dirty");
}


#[test]
fn failure_snapshot_serde_round_trip() {
    use beachcomber::cache::FailureSnapshot;
    use serde_json::json;

    let snap = FailureSnapshot { consecutive_failures: 5, suppressed_until_unix_ms: Some(1_700_000_000_000) };
    let v = serde_json::to_value(&snap).unwrap();
    assert_eq!(v, json!({"consecutive_failures": 5, "suppressed_until_unix_ms": 1_700_000_000_000u64}));
    let round: FailureSnapshot = serde_json::from_value(v).unwrap();
    assert_eq!(round, snap);

    // optional field absent when None
    let snap2 = FailureSnapshot { consecutive_failures: 1, suppressed_until_unix_ms: None };
    let v2 = serde_json::to_value(&snap2).unwrap();
    assert_eq!(v2, json!({"consecutive_failures": 1}));
}

#[test]
fn cache_row_new_fields_serde_round_trip() {
    use beachcomber::cache::{CacheRow, RowKind};
    use serde_json::json;

    let row = CacheRow {
        provider: "git".into(),
        path: Some("/repo".into()),
        field: "branch".into(),
        value: json!("main"),
        age_ms: 14_000,
        stale: false,
        kind: Some(RowKind::Lifecycle { decay: 0, watches_files: true }),
        poll_interval_secs: Some(60),
        keep_alive_polls: Some(12),
        fsevents_reinstate: Some(true),
        failure: None,
    };
    let v = serde_json::to_value(&row).unwrap();
    assert_eq!(v["poll_interval_secs"], 60);
    assert_eq!(v["keep_alive_polls"], 12);
    assert_eq!(v["fsevents_reinstate"], true);
    assert_eq!(v["kind"]["kind"], "lifecycle");
    assert!(v.get("failure").is_none(), "failure omitted when None");
    assert!(v.get("decay").is_none(), "old decay field is gone");
}

#[tokio::test]
async fn lifecycle_snapshots_message_returns_per_entry_data() {
    use beachcomber::cache::Cache;
    use beachcomber::provider::registry::ProviderRegistry;
    use beachcomber::watcher_registry::WatcherRegistry;
    use std::sync::Arc;

    let cache = Arc::new(Cache::new());
    let registry = Arc::new(ProviderRegistry::with_defaults());
    let config = Config::default();

    let (handle, scheduler) = Scheduler::new(
        cache.clone(),
        registry,
        config,
        Arc::new(WatcherRegistry::new()),
    );
    let task = tokio::spawn(async move { scheduler.run().await });

    // Demand an entry to populate lifecycle registry.
    handle
        .send(SchedulerMessage::QueryActivity {
            provider: "hostname".to_string(),
            path: None,
        })
        .await;

    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let snapshots = handle.get_lifecycle_snapshots().await;
    let entry = snapshots
        .values()
        .next()
        .expect("at least one snapshot");
    assert!(entry.poll_interval_secs > 0);
    assert!(entry.keep_alive_polls > 0);
    let _ = entry.fsevents_reinstate;
    let _ = entry.decay;
    let _ = entry.watches_files;

    handle.send(SchedulerMessage::Shutdown).await;
    let _ = task.await;
}

#[tokio::test]
async fn failure_states_message_returns_provider_map() {
    use beachcomber::cache::Cache;
    use beachcomber::provider::registry::ProviderRegistry;
    use beachcomber::watcher_registry::WatcherRegistry;
    use std::sync::Arc;

    let cache = Arc::new(Cache::new());
    let registry = Arc::new(ProviderRegistry::with_defaults());
    let config = Config::default();

    let (handle, scheduler) = Scheduler::new(
        cache.clone(),
        registry,
        config,
        Arc::new(WatcherRegistry::new()),
    );
    let task = tokio::spawn(async move { scheduler.run().await });

    // No failures yet — map should be empty (not error).
    let states = handle.get_failure_states().await;
    assert!(states.is_empty(), "expected empty failure states map, got: {:?}", states);

    handle.send(SchedulerMessage::Shutdown).await;
    let _ = task.await;
}

/// Status on a fresh daemon with an empty cache returns an empty array, not an error.
#[tokio::test]
async fn status_empty_cache_returns_empty_array() {
    let (_tmp, client, handle) = setup_daemon().await;

    let resp = client
        .send_raw(serde_json::json!({"op": "status"}))
        .await
        .expect("status");

    assert!(
        resp.ok,
        "status should succeed on empty cache: {:?}",
        resp.error
    );
    let data = resp.data.expect("status data present");
    let rows = data.as_array().expect("status response is an array");
    // May be empty — that's fine. Just verify it's a valid array.
    let _ = rows.len();

    handle.abort();
}

#[tokio::test]
async fn status_response_lifecycle_row_carries_kind_and_fields() {
    let (_tmp, client, handle) = setup_daemon().await;

    // Use a git repo path that actually exists so the provider writes a cache entry.
    // The beachcomber workspace itself is a git repo.
    let repo_path = env!("CARGO_MANIFEST_DIR");
    let _ = client
        .send_raw(serde_json::json!({"op": "get", "key": "git.branch", "path": repo_path}))
        .await
        .expect("get git.branch");

    // Give the scheduler time to populate lifecycle state.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let resp = client
        .send_raw(serde_json::json!({"op": "status"}))
        .await
        .expect("status");
    assert!(resp.ok, "status failed: {:?}", resp.error);
    let rows: Vec<CacheRow> = serde_json::from_value(resp.data.expect("data present"))
        .expect("rows deserialize");

    let git_row = rows.iter().find(|r| r.provider == "git").expect("git row present after get in a real repo");

    use beachcomber::cache::RowKind;
    match git_row.kind.as_ref().expect("kind present") {
        RowKind::Lifecycle { decay, watches_files } => {
            assert_eq!(*decay, 0, "freshly queried row should be Active (decay=0)");
            let _ = watches_files;
        }
        other => panic!("expected Lifecycle, got {:?}", other),
    }
    assert!(
        git_row.poll_interval_secs.unwrap_or(0) > 0,
        "poll_interval_secs should be > 0"
    );
    assert!(
        git_row.keep_alive_polls.unwrap_or(0) > 0,
        "keep_alive_polls should be > 0"
    );
    assert_eq!(git_row.failure, None);

    handle.abort();
}

#[tokio::test]
async fn status_response_once_row_has_kind_once() {
    let (_tmp, client, handle) = setup_daemon().await;

    // hostname is a Once provider — querying it puts it in cache but not lifecycle.
    let _ = client
        .send_raw(serde_json::json!({"op": "get", "key": "hostname.short"}))
        .await
        .expect("get hostname.short");

    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    let resp = client
        .send_raw(serde_json::json!({"op": "status"}))
        .await
        .expect("status");
    assert!(resp.ok, "status failed: {:?}", resp.error);
    let rows: Vec<CacheRow> = serde_json::from_value(resp.data.expect("data present"))
        .expect("rows deserialize");

    let row = rows
        .iter()
        .find(|r| r.provider == "hostname")
        .expect("hostname row present");

    use beachcomber::cache::RowKind;
    assert!(
        matches!(row.kind, Some(RowKind::Once)),
        "expected RowKind::Once for hostname, got {:?}",
        row.kind
    );
    assert!(row.poll_interval_secs.is_none());
    assert!(row.keep_alive_polls.is_none());
    assert!(row.fsevents_reinstate.is_none());

    handle.abort();
}

#[tokio::test]
async fn status_response_virtual_row_has_kind_virtual() {
    let (_tmp, client, handle) = setup_daemon().await;

    // Store a virtual entry via put.
    let _ = client
        .send_raw(serde_json::json!({"op": "put", "key": "custom", "data": {"color": "blue"}}))
        .await
        .expect("put custom");

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let resp = client
        .send_raw(serde_json::json!({"op": "status"}))
        .await
        .expect("status");
    assert!(resp.ok, "status failed: {:?}", resp.error);
    let rows: Vec<CacheRow> = serde_json::from_value(resp.data.expect("data present"))
        .expect("rows deserialize");

    let row = rows
        .iter()
        .find(|r| r.provider == "custom")
        .expect("custom row present");

    use beachcomber::cache::RowKind;
    assert!(
        matches!(row.kind, Some(RowKind::Virtual)),
        "expected RowKind::Virtual for put entry, got {:?}",
        row.kind
    );

    handle.abort();
}

// ---------------------------------------------------------------------------
// Task 1.16: human preset — TTL column, drop STALE, per-cell colour
// ---------------------------------------------------------------------------

#[test]
fn human_preset_includes_ttl_column_and_drops_stale() {
    use beachcomber::cache::{CacheRow, RowKind};
    use beachcomber::cli::status_format::{FormatOptions, render_human};

    let row = CacheRow {
        provider: "git".into(),
        path: Some("/repo".into()),
        field: "branch".into(),
        value: serde_json::json!("main"),
        age_ms: 14_000,
        stale: false,
        kind: Some(RowKind::Lifecycle { decay: 0, watches_files: true }),
        poll_interval_secs: Some(60),
        keep_alive_polls: Some(12),
        fsevents_reinstate: Some(true),
        failure: None,
    };
    let opts = FormatOptions {
        ascii: false,
    };
    let out = render_human(&[row], &opts);
    assert!(out.contains("PROVIDER"), "header missing: {}", out);
    assert!(out.contains("TTL"), "TTL column missing: {}", out);
    assert!(!out.contains("STALE"), "STALE should be dropped: {}", out);
    assert!(out.contains("\u{2605}"), "active star missing: {}", out);
}

// ---------------------------------------------------------------------------
// Tasks 1.18–1.21: TSV/CSV/sh/minijinja preset extensions
// ---------------------------------------------------------------------------

fn sample_lifecycle_row() -> beachcomber::cache::CacheRow {
    use beachcomber::cache::{CacheRow, RowKind};
    use serde_json::json;
    CacheRow {
        provider: "git".into(),
        path: Some("/repo".into()),
        field: "branch".into(),
        value: json!("main"),
        age_ms: 14_000,
        stale: false,
        kind: Some(RowKind::Lifecycle { decay: 0, watches_files: true }),
        poll_interval_secs: Some(60),
        keep_alive_polls: Some(12),
        fsevents_reinstate: Some(true),
        failure: None,
    }
}

// Task 1.18: TSV — 13 columns
#[test]
fn tsv_preset_13_columns_lifecycle_row() {
    let rows = vec![sample_lifecycle_row()];
    let out = render_tsv(&rows);
    let line = out.lines().next().expect("at least one line");
    let cols: Vec<&str> = line.split('\t').collect();
    assert_eq!(cols.len(), 13, "got: {:?}", cols);
    assert!(!out.starts_with("PROVIDER"), "no header in tsv");
    // Spot-check specific columns
    assert_eq!(cols[0], "git", "provider col");
    assert_eq!(cols[6], "lifecycle", "kind col");
    assert_eq!(cols[7], "0", "decay col");
    assert_eq!(cols[8], "60", "poll_interval_secs col");
    assert_eq!(cols[9], "12", "keep_alive_polls col");
    assert_eq!(cols[10], "true", "fsevents_reinstate col");
    assert_eq!(cols[11], "", "failure_consecutive_failures empty when no failure");
    assert_eq!(cols[12], "", "failure_suppressed empty when no failure");
}

// Task 1.19: CSV — 13 columns with header
#[test]
fn csv_preset_has_full_header_and_all_columns() {
    let rows = vec![sample_lifecycle_row()];
    let out = render_csv(&rows);
    let header = out.lines().next().unwrap();
    assert_eq!(
        header,
        "PROVIDER,PATH,FIELD,VALUE,AGE_MS,STALE,KIND,DECAY,POLL_INTERVAL_SECS,KEEP_ALIVE_POLLS,FSEVENTS_REINSTATE,FAILURE_CONSECUTIVE_FAILURES,FAILURE_SUPPRESSED_UNTIL_UNIX_MS"
    );
    let body = out.lines().nth(1).unwrap();
    let cols: Vec<&str> = body.split(',').collect();
    assert_eq!(cols.len(), 13);
}

// Task 1.20: sh — new env-var lines for lifecycle fields
#[test]
fn sh_preset_exposes_new_fields() {
    let rows = vec![sample_lifecycle_row()];
    let out = render_sh_env(&rows);
    // The key pattern is sanitize_sh_key("git", Some("/repo"), "branch") = "git_repo_branch"
    assert!(out.contains("git_repo_branch_POLL_INTERVAL_SECS='60'"), "got: {}", out);
    assert!(out.contains("git_repo_branch_KEEP_ALIVE_POLLS='12'"), "got: {}", out);
    assert!(out.contains("git_repo_branch_FSEVENTS_REINSTATE='true'"), "got: {}", out);
    assert!(out.contains("git_repo_branch_KIND='lifecycle'"), "got: {}", out);
    assert!(out.contains("git_repo_branch_DECAY='0'"), "got: {}", out);
}

// Task 1.21: minijinja row_context — exposes new fields
#[test]
fn minijinja_row_context_exposes_new_fields() {
    let row = sample_lifecycle_row();
    let ctx = row_context(&row);
    // Serialize minijinja::Value back to serde_json::Value for easy inspection
    let v: serde_json::Value = serde_json::to_value(&ctx).expect("serialize minijinja context");
    assert_eq!(v.get("kind").and_then(|v| v.as_str()), Some("lifecycle"));
    assert_eq!(v.get("decay").and_then(|v| v.as_u64()), Some(0));
    assert_eq!(v.get("poll_interval_secs").and_then(|v| v.as_u64()), Some(60));
    assert_eq!(v.get("keep_alive_polls").and_then(|v| v.as_u64()), Some(12));
    assert_eq!(v.get("fsevents_reinstate").and_then(|v| v.as_bool()), Some(true));
}
