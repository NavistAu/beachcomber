// Integration test for the reshaped Request::Status response.
// Status now returns an array of rows, one per (provider, path, field) tuple,
// describing everything currently warm in the cache.
//
// Also contains unit tests for the status_format preset renderers (T28).

use crate::common::fetch::fetch;
use crate::common::git::GitRepoFixture;
use crate::common::poll::{poll_until, poll_until_async};
use beachcomber::cache::CacheRow;
use beachcomber::cli::status_format::{RenderOpts, render_preset, render_sh_env, row_context};

fn sample_rows() -> Vec<CacheRow> {
    vec![
        CacheRow {
            shadowed: false,
            provider: "git".into(),
            path: Some("/home/me/ws/foo".into()),
            source: "refs".into(),
            field: "branch".into(),
            value: serde_json::json!("main"),
            age_ms: Some(14_000),
            stale: false,
            kind: None,
            poll_interval_secs: None,
            poll_count: None,
            fsevents_reinstate: None,
            polls_elapsed: None,
            next_poll_in_secs: None,
            failure: None,
        },
        CacheRow {
            shadowed: false,
            provider: "git".into(),
            path: Some("/home/me/ws/foo".into()),
            source: "status".into(),
            field: "dirty".into(),
            value: serde_json::json!(false),
            age_ms: Some(14_000),
            stale: false,
            kind: None,
            poll_interval_secs: None,
            poll_count: None,
            fsevents_reinstate: None,
            polls_elapsed: None,
            next_poll_in_secs: None,
            failure: None,
        },
        CacheRow {
            shadowed: false,
            provider: "hostname".into(),
            path: None,
            source: "default".into(),
            field: "value".into(),
            value: serde_json::json!("me-laptop"),
            age_ms: Some(52_000),
            stale: false,
            kind: None,
            poll_interval_secs: None,
            poll_count: None,
            fsevents_reinstate: None,
            polls_elapsed: None,
            next_poll_in_secs: None,
            failure: None,
        },
    ]
}

#[test]
fn json_preset_emits_ndjson() {
    let out = render_preset(
        &status_preset("json"),
        &sample_rows(),
        &RenderOpts::default(),
    );
    for line in out.lines().filter(|l| !l.is_empty()) {
        serde_json::from_str::<serde_json::Value>(line).expect("valid JSON per line");
    }
    assert!(out.lines().filter(|l| !l.is_empty()).count() >= 3);
}

#[test]
fn tsv_preset_is_tab_separated_no_header() {
    let rows = vec![sample_lifecycle_row()];
    let out = render_preset(&status_preset("tsv"), &rows, &RenderOpts::default());
    let line = out.lines().next().expect("at least one line");
    let cols: Vec<&str> = line.split('\t').collect();
    assert_eq!(cols.len(), 14, "got: {:?}", cols);
    assert!(!out.starts_with("PROVIDER"), "no header in tsv");
}

#[test]
fn csv_preset_has_header_and_quotes_values() {
    let out = render_preset(
        &status_preset("CSV"),
        &sample_rows(),
        &RenderOpts::default(),
    );
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
    let out = render_preset(
        &status_preset("table"),
        &sample_rows(),
        &RenderOpts::default(),
    );
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
    let out = render_preset(&status_preset("sh"), &sample_rows(), &RenderOpts::default());
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
fn human_preset_truncates_value_to_meet_total_cap() {
    // max_width is now a *total table cap*. The default RenderOpts has max_width=Some(40)
    // which is intentionally narrow to exercise truncation — VALUE shrinks to meet the budget.
    // Use a wider total cap so the VALUE column still gets some chars before truncation.
    let mut rows = sample_rows();
    rows.push(CacheRow {
        shadowed: false,
        provider: "git".into(),
        path: Some("/home/me/ws/foo".into()),
        source: "refs".into(),
        field: "commit_summary".into(),
        value: serde_json::json!("a".repeat(100)),
        age_ms: Some(14_000),
        stale: false,
        kind: None,
        poll_interval_secs: None,
        poll_count: None,
        fsevents_reinstate: None,
        polls_elapsed: None,
        next_poll_in_secs: None,
        failure: None,
    });
    // With a 120-col total cap, the non-VALUE columns for these rows fit comfortably,
    // leaving VALUE some room — but not enough for all 100 'a's.
    let opts = RenderOpts {
        is_tty: true,
        no_color: true,
        max_width: Some(120),
        no_trunc: false,
        ascii: false,
        depth: None,
    };
    let out = render_preset(&status_preset("human"), &rows, &opts);
    // Output should contain truncated version, not the full 100 'a's.
    assert!(
        !out.contains(&"a".repeat(100)),
        "human preset should truncate 100-char value when total table cap is 120"
    );
    // Should still contain some truncated prefix (at minimum 8 chars = 5 + "...").
    assert!(
        out.contains(&"a".repeat(5)),
        "human preset should preserve at least a few chars before ellipsis"
    );
}

#[test]
fn human_preset_color_on_stale_rows() {
    let rows = vec![CacheRow {
        shadowed: false,
        provider: "git".into(),
        path: None,
        source: "refs".into(),
        field: "branch".into(),
        value: serde_json::json!("main"),
        age_ms: Some(9999),
        stale: true,
        kind: None,
        poll_interval_secs: None,
        poll_count: None,
        fsevents_reinstate: None,
        polls_elapsed: None,
        next_poll_in_secs: None,
        failure: None,
    }];
    let opts = RenderOpts {
        is_tty: true,
        no_color: false,
        max_width: Some(40),
        no_trunc: false,
        ascii: false,
        depth: None,
    };
    let out = render_preset(&status_preset("human"), &rows, &opts);
    assert!(
        out.contains('\x1b'),
        "human preset with is_tty=true and stale row should emit ANSI codes"
    );
}

#[test]
fn json_preset_path_none_serializes_as_null() {
    let rows = vec![CacheRow {
        shadowed: false,
        provider: "hostname".into(),
        path: None,
        source: "default".into(),
        field: "value".into(),
        value: serde_json::json!("myhost"),
        age_ms: Some(1000),
        stale: false,
        kind: None,
        poll_interval_secs: None,
        poll_count: None,
        fsevents_reinstate: None,
        polls_elapsed: None,
        next_poll_in_secs: None,
        failure: None,
    }];
    let out = render_preset(&status_preset("json"), &rows, &RenderOpts::default());
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
        shadowed: false,
        provider: "test".into(),
        path: None,
        source: "default".into(),
        field: "tags".into(),
        value: serde_json::json!("a,b,c"),
        age_ms: Some(0),
        stale: false,
        kind: None,
        poll_interval_secs: None,
        poll_count: None,
        fsevents_reinstate: None,
        polls_elapsed: None,
        next_poll_in_secs: None,
        failure: None,
    }];
    let out = render_preset(&status_preset("csv"), &rows, &RenderOpts::default());
    // Value containing comma must be quoted in RFC 4180 style.
    assert!(
        out.contains("\"a,b,c\""),
        "csv should quote values containing commas: {out}"
    );
}

#[test]
fn custom_template_renders_per_row() {
    let rows = sample_rows();
    let out = render_status_template("{{ provider }}.{{ field }}={{ value }}", &rows);
    let lines: Vec<_> = out.lines().collect();
    assert_eq!(lines.len(), rows.len());
    assert!(lines.iter().any(|l| l.contains("git.branch=main")));
    assert!(lines.iter().any(|l| l.contains("hostname.value=me-laptop")));
}

#[test]
fn custom_template_supports_truncate_filter() {
    let rows = vec![CacheRow {
        shadowed: false,
        provider: "git".into(),
        path: None,
        source: "refs".into(),
        field: "sha".into(),
        value: serde_json::json!("abcdef1234567890"),
        age_ms: Some(14_000),
        stale: false,
        kind: None,
        poll_interval_secs: None,
        poll_count: None,
        fsevents_reinstate: None,
        polls_elapsed: None,
        next_poll_in_secs: None,
        failure: None,
    }];
    let out = render_status_template("{{ value | truncate(7) }}", &rows);
    assert_eq!(out.trim(), "abcdef1...");
}

#[test]
fn cache_row_kind_serde_round_trip() {
    use beachcomber::cache::RowKind;
    use serde_json::json;

    let cases = vec![
        (
            RowKind::Lifecycle {
                decay: 0,
                depth: 4,
                watches_files: true,
            },
            json!({"kind": "lifecycle", "decay": 0, "depth": 4, "watches_files": true}),
        ),
        (
            RowKind::Lifecycle {
                decay: 4,
                depth: 4,
                watches_files: false,
            },
            json!({"kind": "lifecycle", "decay": 4, "depth": 4, "watches_files": false}),
        ),
        (RowKind::Once, json!({"kind": "once"})),
        (RowKind::Static, json!({"kind": "static"})),
        (RowKind::Transient, json!({"kind": "transient"})),
        (RowKind::Template, json!({"kind": "template"})),
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
        shadowed: false,
        provider: "git".into(),
        path: None,
        source: "refs".into(),
        field: "branch".into(),
        value: serde_json::json!("main"),
        age_ms: Some(3_600_000), // 1 hour
        stale: false,
        kind: None,
        poll_interval_secs: None,
        poll_count: None,
        fsevents_reinstate: None,
        polls_elapsed: None,
        next_poll_in_secs: None,
        failure: None,
    }];
    let out = render_status_template("{{ age_human }}", &rows);
    // Should render as "1h" or similar
    assert!(!out.trim().is_empty());
    assert!(out.trim() != "3600000");
}

#[test]
fn table_prefix_aligns_columns_and_emits_header() {
    let rows = sample_rows();
    let out = render_status_template("table {{ provider }}	{{ field }}	{{ value }}", &rows);
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
fn filter_path_tilde_matches_globals() {
    let rows = sample_rows();
    let out = apply_filters(rows, &["path=~".to_string()]).unwrap();
    assert!(!out.is_empty());
    assert!(out.iter().all(|r| r.path.is_none()));
    // `-` is not a spelling of the global entry.
    let err = apply_filters(sample_rows(), &["path=-".to_string()]).unwrap_err();
    assert!(err.contains("path=~"), "{err}");
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

fn facet_rows() -> Vec<CacheRow> {
    let mut rows = sample_rows();
    let mut tpl = rows[0].clone();
    tpl.provider = "git:template".into();
    tpl.source = "config:/c/config.toml".into();
    tpl.value = serde_json::json!("{{ env.B }}");
    let mut absent = rows[0].clone();
    absent.source = "template".into();
    absent.field = "nothing".into();
    absent.value = serde_json::Value::Null;
    rows.push(tpl);
    rows.push(absent);
    rows
}

#[test]
fn not_equal_negates_a_filter() {
    let out = apply_filters(sample_rows(), &["provider!=git".to_string()]).unwrap();
    assert!(!out.is_empty());
    assert!(out.iter().all(|r| r.provider != "git"));
}

#[test]
fn provider_matches_the_base_name_of_a_facet_row() {
    let out = apply_filters(facet_rows(), &["provider=git".to_string()]).unwrap();
    assert!(out.iter().any(|r| r.provider == "git:template"));
    assert!(out.iter().all(|r| r.provider.starts_with("git")));
}

#[test]
fn facet_dash_is_no_facet_and_a_name_or_star_selects_facet_rows() {
    let none = apply_filters(facet_rows(), &["facet=-".to_string()]).unwrap();
    assert!(none.iter().all(|r| !r.provider.contains(':')));
    let tpl = apply_filters(facet_rows(), &["facet=template".to_string()]).unwrap();
    assert_eq!(tpl.len(), 1);
    assert_eq!(tpl[0].provider, "git:template");
    let any = apply_filters(facet_rows(), &["facet=*".to_string()]).unwrap();
    assert_eq!(any.len(), 1);
}

#[test]
fn source_globs_the_displayed_label() {
    let out = apply_filters(facet_rows(), &["source=config*".to_string()]).unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].provider, "git:template");
}

#[test]
fn value_dash_is_absent_and_a_glob_matches_only_present_values() {
    let absent = apply_filters(facet_rows(), &["value=-".to_string()]).unwrap();
    assert_eq!(absent.len(), 1);
    assert_eq!(absent[0].field, "nothing");
    let present = apply_filters(facet_rows(), &["value=*".to_string()]).unwrap();
    assert!(present.iter().all(|r| !r.value.is_null()));
    assert_eq!(present.len(), facet_rows().len() - 1);
    let main = apply_filters(facet_rows(), &["value=ma*".to_string()]).unwrap();
    assert_eq!(main.len(), 1);
}

#[test]
fn the_defaults_hide_facets_absent_values_and_shadowed_records_unless_their_key_is_named() {
    use beachcomber::cli::status_format::effective_filters;
    let shown = apply_filters(facet_rows(), &effective_filters(&[], false)).unwrap();
    assert!(
        shown
            .iter()
            .all(|r| !r.provider.contains(':') && !r.value.is_null())
    );
    assert_eq!(shown.len(), sample_rows().len());

    // Naming the key, with either operator, replaces its default only.
    let facets = effective_filters(&["facet=template".to_string()], false);
    assert_eq!(
        facets,
        vec![
            "facet=template".to_string(),
            "value!=-".to_string(),
            "shadowed=false".to_string()
        ]
    );
    let absent = effective_filters(&["value!=x".to_string()], false);
    assert_eq!(
        absent,
        vec![
            "value!=x".to_string(),
            "facet=-".to_string(),
            "shadowed=false".to_string()
        ]
    );
    let shadowed = effective_filters(&["shadowed=true".to_string()], false);
    assert_eq!(
        shadowed,
        vec![
            "shadowed=true".to_string(),
            "facet=-".to_string(),
            "value!=-".to_string()
        ]
    );

    // --all drops them all.
    assert!(effective_filters(&[], true).is_empty());
    let all = apply_filters(facet_rows(), &effective_filters(&[], true)).unwrap();
    assert_eq!(all.len(), facet_rows().len());
}

#[test]
fn an_absent_value_shows_as_null_in_the_table() {
    let rows: Vec<CacheRow> = facet_rows()
        .into_iter()
        .filter(|r| r.value.is_null())
        .collect();
    let out = render_preset(&status_preset("table"), &rows, &RenderOpts::default());
    assert!(
        out.lines().nth(1).is_some_and(|l| l.contains(" null ")),
        "{out}"
    );
}

// --- Sort tests ---

#[test]
fn sort_by_age_ascending() {
    let mut rows = sample_rows();
    rows[0].age_ms = Some(30_000);
    rows[1].age_ms = Some(10_000);
    rows[2].age_ms = Some(20_000);
    let out = apply_sort(rows, "age").unwrap();
    let ages: Vec<_> = out.iter().map(|r| r.age_ms).collect();
    assert_eq!(ages, vec![Some(10_000), Some(20_000), Some(30_000)]);
}

#[test]
fn sort_default_by_path() {
    let rows = sample_rows();
    let out = apply_sort(rows, "path").unwrap();
    // Simple check: rows with None paths sort as one group.
    assert!(!out.is_empty());
}

#[test]
fn the_default_sort_is_provider_then_plain_before_facet_then_field_then_path() {
    let row = |provider: &str, field: &str, path: Option<&str>| CacheRow {
        provider: provider.into(),
        field: field.into(),
        path: path.map(String::from),
        ..sample_rows()[0].clone()
    };
    let rows = vec![
        row("git:template", "branch", None),
        row("zed", "a", None),
        row("git", "dirty", Some("/r")),
        row("aws", "region", Some("/r")),
        row("git", "branch", Some("/r")),
        row("git", "branch", None),
        row("git:cache", "_", None),
    ];
    let out = apply_sort(rows, "default").unwrap();
    let got: Vec<(String, String, Option<String>)> = out
        .into_iter()
        .map(|r| (r.provider, r.field, r.path))
        .collect();
    let want = [
        ("aws", "region", Some("/r")),
        ("git", "branch", None),
        ("git", "branch", Some("/r")),
        ("git", "dirty", Some("/r")),
        ("git:cache", "_", None),
        ("git:template", "branch", None),
        ("zed", "a", None),
    ];
    let want: Vec<(String, String, Option<String>)> = want
        .iter()
        .map(|(p, f, path)| (p.to_string(), f.to_string(), path.map(String::from)))
        .collect();
    assert_eq!(got, want);
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
        ascii: false,
        depth: None,
    };
    let out = render_preset(&status_preset("human"), &rows, &opts);
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
        ascii: false,
        depth: None,
    };
    let out = render_preset(&status_preset("human"), &rows, &opts);
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
        ascii: false,
        depth: None,
    };
    let out = render_preset(&status_preset("human"), &rows, &opts);
    assert!(!out.contains("\x1b["));
}

use beachcomber::client::Client;
use beachcomber::config::Config;
use beachcomber::scheduler::{Scheduler, SchedulerMessage};

async fn setup_daemon() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    Client,
    tokio::task::JoinHandle<()>,
) {
    let tmp = tempfile::TempDir::new().unwrap();
    let sock = tmp.path().join("test.sock");
    let config = Config::default();
    let (handle, ready) = beachcomber::daemon::start_in_process_polling(sock.clone(), config);
    ready.await.expect("daemon ready");
    let client = Client::new(sock.clone());
    (tmp, sock, client, handle)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_returns_rows_per_field() {
    let (_tmp, sock, client, handle) = setup_daemon().await;

    // Warm up a global provider so at least something is in cache.
    let _ = fetch(&sock, "hostname", None).expect("get hostname");

    // Poll status until the warmed provider shows up, rather than sleeping and
    // hoping the cache has settled.
    let status_rows = poll_until(
        "status rows after warming hostname",
        "a row with provider=hostname",
        std::time::Duration::from_secs(5),
        std::time::Duration::from_millis(20),
        || client.status().expect("status"),
        |rows: &Vec<CacheRow>| rows.iter().any(|r| r.provider == "hostname"),
    );
    let data = serde_json::to_value(&status_rows).expect("rows serialize");
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
    let opts2 = FormatOptions { ascii: true };
    assert!(opts2.ascii);
}

// ---------------------------------------------------------------------------
// Task 1.10: ColorMode + resolve_color matrix
// ---------------------------------------------------------------------------

#[test]
fn color_resolution_matrix() {
    use beachcomber::cli::status_format::{ColorMode, resolve_color};
    assert!(!resolve_color(ColorMode::Never, false, true));
    assert!(!resolve_color(ColorMode::Always, true, true)); // NO_COLOR wins
    assert!(!resolve_color(ColorMode::Auto, true, true)); // NO_COLOR wins
    assert!(resolve_color(ColorMode::Always, false, false)); // explicit opt-in
    assert!(resolve_color(ColorMode::Auto, false, true)); // TTY
    // Pipe with no explicit --color: no colour. There is no environment
    // escape hatch — `watch(1)` never exported WATCH_INTERVAL, it reads it.
    assert!(!resolve_color(ColorMode::Auto, false, false));
}

// ---------------------------------------------------------------------------
// Task 1.11: resolve_max_width
// ---------------------------------------------------------------------------

#[test]
fn resolve_max_width_cases() {
    use beachcomber::cli::status_format::resolve_max_width;
    use pretty_assertions::assert_eq;

    struct Case {
        name: &'static str,
        arg: Option<&'static str>,
        terminal: Option<usize>,
        expected: usize,
    }

    let cases: &[Case] = &[
        Case {
            name: "explicit int overrides terminal width",
            arg: Some("80"),
            terminal: Some(200),
            expected: 80,
        },
        Case {
            name: "unset arg with terminal width uses terminal",
            arg: None,
            terminal: Some(200),
            expected: 200,
        },
        Case {
            name: "unset arg with no terminal falls back to 120",
            arg: None,
            terminal: None,
            expected: 120,
        },
        Case {
            name: "auto with terminal width uses terminal",
            arg: Some("auto"),
            terminal: Some(200),
            expected: 200,
        },
        Case {
            name: "auto with no terminal falls back to 120",
            arg: Some("auto"),
            terminal: None,
            expected: 120,
        },
    ];

    for case in cases {
        assert_eq!(
            resolve_max_width(case.arg, case.terminal),
            case.expected,
            "case: {}",
            case.name
        );
    }
}

// ---------------------------------------------------------------------------
// max-width is a total table cap: VALUE shrinks, others keep natural widths
// ---------------------------------------------------------------------------

#[test]
fn max_width_value_column_shrinks_not_others() {
    use beachcomber::cache::CacheRow;
    use beachcomber::cli::status_format::{RenderOpts, render_preset};
    use serde_json::json;

    // One row: long value, short provider/path/field.
    let row = CacheRow {
        shadowed: false,
        provider: "git".into(),
        path: Some("/repo".into()),
        source: "refs".into(),
        field: "branch".into(),
        value: json!("a".repeat(80)),
        age_ms: Some(1000),
        stale: false,
        kind: None,
        poll_interval_secs: None,
        poll_count: None,
        fsevents_reinstate: None,
        polls_elapsed: None,
        next_poll_in_secs: None,
        failure: None,
    };
    // Use a narrow max_width that cannot fit the full value.
    let opts = RenderOpts {
        is_tty: false,
        no_color: true,
        max_width: Some(60),
        no_trunc: false,
        ascii: false,
        depth: None,
    };
    let out = render_preset(&status_preset("human"), &[row], &opts);
    // The full 80-char value must not appear (it was shrunk).
    assert!(
        !out.contains(&"a".repeat(80)),
        "VALUE should be truncated when max_width is a tight total cap: {out}"
    );
    // Provider, path and field must still be present verbatim.
    for line in out.lines() {
        if line.contains("git") {
            assert!(line.contains("git"), "provider intact: {line}");
            assert!(line.contains("/repo"), "path intact: {line}");
            assert!(line.contains("branch"), "field intact: {line}");
        }
    }
}

#[test]
fn max_width_total_row_width_does_not_exceed_cap() {
    use beachcomber::cache::CacheRow;
    use beachcomber::cli::status_format::{RenderOpts, render_preset};
    use serde_json::json;

    // Row with a long value that would easily blow past a 80-col cap.
    let row = CacheRow {
        shadowed: false,
        provider: "mise".into(),
        path: Some("-".into()),
        source: "global".into(),
        field: "global".into(),
        value: json!({"ansible-base":"2.10.17","aws-vault":"7.2.0","awscli":"2.16.9","go":"1.24.9"}),
        age_ms: Some(158_000),
        stale: false,
        kind: None,
        poll_interval_secs: None,
        poll_count: None,
        fsevents_reinstate: None,
        polls_elapsed: None,
        next_poll_in_secs: None,
        failure: None,
    };
    let total_cap = 80usize;
    let opts = RenderOpts {
        is_tty: false,
        no_color: true,
        max_width: Some(total_cap),
        no_trunc: false,
        ascii: true, // ascii so TTL glyph width is predictable
        depth: None,
    };
    let out = render_preset(&status_preset("human"), &[row], &opts);
    // Every data row (skip header) must fit within total_cap columns.
    for (i, line) in out.lines().enumerate() {
        if i == 0 {
            continue; // skip header — it may exceed cap if non-VALUE columns already fill it
        }
        let cols = line.chars().count();
        assert!(
            cols <= total_cap,
            "row {} is {} chars wide, exceeds cap {}: {:?}",
            i,
            cols,
            total_cap,
            line
        );
    }
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
        ascii: false,
        depth: None,
    };
    let out = render_preset(&status_preset("human"), &rows, &opts);
    assert!(
        out.contains("PROVIDER"),
        "human preset should emit PROVIDER header regardless of is_tty: {out}"
    );
}

#[test]
fn failure_snapshot_serde_round_trip() {
    use beachcomber::cache::FailureSnapshot;
    use serde_json::json;

    let snap = FailureSnapshot {
        consecutive_failures: 5,
        suppressed_until_unix_ms: Some(1_700_000_000_000),
    };
    let v = serde_json::to_value(&snap).unwrap();
    assert_eq!(
        v,
        json!({"consecutive_failures": 5, "suppressed_until_unix_ms": 1_700_000_000_000u64})
    );
    let round: FailureSnapshot = serde_json::from_value(v).unwrap();
    assert_eq!(round, snap);

    // optional field absent when None
    let snap2 = FailureSnapshot {
        consecutive_failures: 1,
        suppressed_until_unix_ms: None,
    };
    let v2 = serde_json::to_value(&snap2).unwrap();
    assert_eq!(v2, json!({"consecutive_failures": 1}));
}

#[test]
fn cache_row_new_fields_serde_round_trip() {
    use beachcomber::cache::{CacheRow, RowKind};
    use serde_json::json;

    let row = CacheRow {
        shadowed: false,
        provider: "git".into(),
        path: Some("/repo".into()),
        source: "refs".into(),
        field: "branch".into(),
        value: json!("main"),
        age_ms: Some(14_000),
        stale: false,
        kind: Some(RowKind::Lifecycle {
            decay: 0,
            depth: 4,
            watches_files: true,
        }),
        poll_interval_secs: Some(60),
        poll_count: Some(12),
        fsevents_reinstate: Some(true),
        polls_elapsed: None,
        next_poll_in_secs: None,
        failure: None,
    };
    let v = serde_json::to_value(&row).unwrap();
    assert_eq!(v["poll_interval_secs"], 60);
    assert_eq!(v["poll_count"], 12);
    assert_eq!(v["fsevents_reinstate"], true);
    assert_eq!(v["kind"]["kind"], "lifecycle");
    assert!(v.get("failure").is_none(), "failure omitted when None");
    assert!(v.get("decay").is_none(), "old decay field is gone");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lifecycle_snapshots_message_returns_per_entry_data() {
    use beachcomber::cache::Cache;
    use beachcomber::provider::registry::ProviderRegistry;
    use beachcomber::query::Instance;
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

    // Demand a Poll-strategy provider to populate the lifecycle registry.
    // Once providers (like hostname) deliberately bypass the lifecycle.
    handle
        .send(SchedulerMessage::QueryActivity {
            provider: "load".to_string(),
            instances: vec![Instance {
                source: "loadavg".to_string(),
                root: None,
            }],
        })
        .await;

    // Poll the snapshot map itself: the demand message is processed
    // asynchronously and populates the lifecycle registry when it lands.
    let snapshots = poll_until_async(
        "lifecycle snapshots after demanding load",
        "at least one entry",
        std::time::Duration::from_secs(5),
        std::time::Duration::from_millis(20),
        async || handle.get_lifecycle_snapshots().await,
        |snapshots| !snapshots.is_empty(),
    )
    .await;

    let entry = snapshots.values().next().expect("at least one snapshot");
    assert!(entry.poll_interval_secs > 0);
    let _ = entry.fsevents_reinstate;
    let _ = entry.decay;
    let _ = entry.watches_files;

    handle.send(SchedulerMessage::Shutdown).await;
    let _ = task.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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
    assert!(
        states.is_empty(),
        "expected empty failure states map, got: {:?}",
        states
    );

    handle.send(SchedulerMessage::Shutdown).await;
    let _ = task.await;
}

/// Status on a fresh daemon with an empty cache returns an empty array, not an error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_empty_cache_returns_empty_array() {
    let (_tmp, _sock, client, handle) = setup_daemon().await;

    let rows = client.status().expect("status");
    // May be empty — that's fine. Just verify the call succeeds.
    let _ = rows.len();

    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_response_lifecycle_row_carries_kind_and_fields() {
    let (_tmp, sock, client, handle) = setup_daemon().await;

    // Use a small dedicated temp git repo (not CARGO_MANIFEST_DIR) so the diff/status
    // git subprocesses are instant. The real workspace carries a huge untracked
    // `target/` working tree; `git status`/`git diff` over it on the Linux CI runner
    // took >30s and timed the test out.
    // Request the whole provider ("git") so both sources (refs + diff) are warmed.
    // A field request like "git.branch" only demands the refs source (canon §150/§268),
    // which would leave the diff source absent from the status output.
    let repo = GitRepoFixture::new();
    let repo_path = repo.path_str();
    let _ = fetch(&sock, "git", Some(repo_path)).expect("get git");

    // Poll until the scheduler has populated lifecycle state for both git
    // sources (refs + diff) rather than sleeping a fixed duration and hoping.
    //
    // Deadline was widened to 20s to absorb a daemon-side stall: the
    // scheduler registered fs watches synchronously in its demand arm, once
    // per git source for the same path (refs + status both watch the repo
    // root), so a single `get git` paid kernel FSEvents registration cost
    // twice — >6s total on a loaded host — during which every pending
    // response (including this test's status polls) sat undelivered. Fixed
    // by deduping the kernel watch() call to once per path and moving it off
    // the scheduler task via spawn_blocking (src/scheduler/mod.rs
    // `register_path_watch`), so this reverts to the original 5s deadline as
    // the regression guard.
    let rows = poll_until(
        "status rows for git after get",
        "rows containing provider=git source=refs and provider=git source=diff",
        std::time::Duration::from_secs(5),
        std::time::Duration::from_millis(20),
        || client.status().expect("status"),
        |rows: &Vec<CacheRow>| {
            rows.iter()
                .any(|r| r.provider == "git" && r.source == "source(refs)")
                && rows
                    .iter()
                    .any(|r| r.provider == "git" && r.source == "source(diff)")
        },
    );

    // Pick a git row from the `refs` source (fsevent strategy → watches_files=true).
    // After Phase 1 each row carries its source's strategy individually, so the diff
    // source (poll) has watches_files=false while refs has watches_files=true.
    let refs_row = rows
        .iter()
        .find(|r| r.provider == "git" && r.source == "source(refs)")
        .expect("git.refs row present after get in a real repo");

    use beachcomber::cache::RowKind;
    match refs_row.kind.as_ref().expect("kind present") {
        RowKind::Lifecycle {
            decay,
            watches_files,
            ..
        } => {
            assert_eq!(*decay, 0, "freshly queried row should be Active (decay=0)");
            assert!(
                *watches_files,
                "git.refs uses Watch strategy → watches_files=true"
            );
        }
        other => panic!("expected Lifecycle, got {:?}", other),
    }
    // refs is a pure Watch source — no poll path → poll_interval_secs is None.
    assert!(
        refs_row.poll_interval_secs.is_none(),
        "Watch-only source should have None poll_interval_secs"
    );
    assert!(refs_row.poll_count.is_none());
    assert_eq!(refs_row.failure, None);

    // Cross-check the diff source: poll strategy → poll_interval_secs is Some, watches_files=false.
    let diff_row = rows
        .iter()
        .find(|r| r.provider == "git" && r.source == "source(diff)")
        .expect("git.diff row present");
    match diff_row.kind.as_ref().expect("kind present") {
        RowKind::Lifecycle { watches_files, .. } => {
            assert!(
                !*watches_files,
                "git.diff uses Poll strategy → watches_files=false"
            );
        }
        other => panic!("expected Lifecycle for diff row, got {:?}", other),
    }
    assert!(
        diff_row.poll_interval_secs.is_some(),
        "Poll source should have Some poll_interval_secs"
    );
    assert!(
        diff_row.poll_count.is_some(),
        "Poll source should have Some poll_count"
    );
    assert!(
        diff_row.polls_elapsed.is_some(),
        "Poll source should have Some polls_elapsed"
    );

    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_response_once_row_has_kind_once() {
    let (_tmp, sock, client, handle) = setup_daemon().await;

    // hostname is a Watch-only global provider with 60s x 12 — the equivalent
    // of the old "Once" semantics. It enters the lifecycle map immediately on demand.
    let _ = fetch(&sock, "hostname.short", None).expect("get hostname.short");

    use beachcomber::cache::RowKind;

    // The cache row lands with the `get`, but its kind only becomes Lifecycle
    // once the scheduler has processed the demand for this source — poll the
    // status response until that has happened.
    let rows = poll_until(
        "status rows after get hostname.short",
        "a hostname row whose kind is Lifecycle",
        std::time::Duration::from_secs(5),
        std::time::Duration::from_millis(20),
        || client.status().expect("status"),
        |rows: &Vec<CacheRow>| {
            rows.iter()
                .find(|r| r.provider == "hostname")
                .is_some_and(|r| matches!(r.kind, Some(RowKind::Lifecycle { .. })))
        },
    );

    let row = rows
        .iter()
        .find(|r| r.provider == "hostname")
        .expect("hostname row present");

    // hostname uses Watch + 60s x 12 — shown as Lifecycle (decay=0, watches_files=true)
    match row.kind.as_ref().expect("kind present") {
        RowKind::Lifecycle {
            decay,
            watches_files,
            ..
        } => {
            assert_eq!(*decay, 0, "freshly queried row should be Active (decay=0)");
            // Watch strategy with empty patterns — watches_files may be true or false;
            // what matters is the Lifecycle variant is used (not Once/Transient/Static).
            let _ = watches_files;
        }
        other => panic!("expected RowKind::Lifecycle for hostname, got {:?}", other),
    }
    // hostname uses Watch + pure-watch global — no poll path
    // → poll_interval_secs is None and poll_count is None.
    assert!(
        row.poll_interval_secs.is_none(),
        "pure Watch source should have None poll_interval_secs"
    );
    assert!(row.poll_count.is_none());

    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_response_put_row_has_kind_static() {
    let (_tmp, _sock, client, handle) = setup_daemon().await;

    // Store a static entry via put.
    let _ = client
        .put("custom", serde_json::json!({"color": "blue"}), None, None)
        .expect("put custom");

    // `put` writes the cache entry and registers the static namespace before it
    // answers, so the row is there — poll for it rather than assume a schedule.
    let rows = poll_until(
        "status rows after put custom",
        "a row with provider=custom",
        std::time::Duration::from_secs(5),
        std::time::Duration::from_millis(20),
        || client.status().expect("status"),
        |rows: &Vec<CacheRow>| rows.iter().any(|r| r.provider == "custom"),
    );

    let row = rows
        .iter()
        .find(|r| r.provider == "custom")
        .expect("custom row present");

    use beachcomber::cache::RowKind;
    assert!(
        matches!(row.kind, Some(RowKind::Static)),
        "expected RowKind::Static for put entry, got {:?}",
        row.kind
    );

    handle.abort();
}

/// Status lists one put row per address, each with the address as its field.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_lists_one_put_row_per_address() {
    let (_tmp, _sock, client, handle) = setup_daemon().await;

    for (key, v) in [("fleet.a", 1), ("fleet.b", 2), ("fleet.c.d", 3)] {
        client
            .put(key, serde_json::json!(v), None, None)
            .expect("put");
    }

    let rows = client.status().expect("status");
    let mut fields: Vec<(String, serde_json::Value)> = rows
        .iter()
        .filter(|r| r.provider == "fleet")
        .map(|r| (r.field.clone(), r.value.clone()))
        .collect();
    fields.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        fields,
        [
            ("a".to_string(), serde_json::json!(1)),
            ("b".to_string(), serde_json::json!(2)),
            ("c.d".to_string(), serde_json::json!(3))
        ]
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
        shadowed: false,
        provider: "git".into(),
        path: Some("/repo".into()),
        source: "refs".into(),
        field: "branch".into(),
        value: serde_json::json!("main"),
        age_ms: Some(14_000),
        stale: false,
        kind: Some(RowKind::Lifecycle {
            decay: 0,
            depth: 4,
            watches_files: true,
        }),
        poll_interval_secs: Some(60),
        poll_count: Some(12),
        fsevents_reinstate: Some(true),
        polls_elapsed: None,
        next_poll_in_secs: None,
        failure: None,
    };
    let opts = FormatOptions { ascii: false };
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
        shadowed: false,
        provider: "git".into(),
        path: Some("/repo".into()),
        source: "refs".into(),
        field: "branch".into(),
        value: json!("main"),
        age_ms: Some(14_000),
        stale: false,
        kind: Some(RowKind::Lifecycle {
            decay: 0,
            depth: 4,
            watches_files: true,
        }),
        poll_interval_secs: Some(60),
        poll_count: Some(12),
        fsevents_reinstate: Some(true),
        polls_elapsed: None,
        next_poll_in_secs: None,
        failure: None,
    }
}

// Task 1.18: TSV — 14 columns (SOURCE follows FIELD)
#[test]
fn tsv_preset_14_columns_lifecycle_row() {
    let rows = vec![sample_lifecycle_row()];
    let out = render_preset(&status_preset("tsv"), &rows, &RenderOpts::default());
    let line = out.lines().next().expect("at least one line");
    let cols: Vec<&str> = line.split('\t').collect();
    assert_eq!(cols.len(), 14, "got: {:?}", cols);
    assert!(!out.starts_with("PROVIDER"), "no header in tsv");
    // Spot-check specific columns
    assert_eq!(cols[0], "git", "provider col");
    assert_eq!(cols[3], "refs", "source col");
    assert_eq!(cols[7], "lifecycle", "kind col");
    assert_eq!(cols[8], "0", "decay col");
    assert_eq!(cols[9], "60", "poll_interval_secs col");
    assert_eq!(cols[10], "12", "poll_count col");
    assert_eq!(cols[11], "true", "fsevents_reinstate col");
    assert_eq!(
        cols[12], "",
        "failure_consecutive_failures empty when no failure"
    );
    assert_eq!(cols[13], "", "failure_suppressed empty when no failure");
}

// Task 1.19: CSV — 14 columns with header (SOURCE follows FIELD)
#[test]
fn csv_preset_has_full_header_and_all_columns() {
    let rows = vec![sample_lifecycle_row()];
    let out = render_preset(&status_preset("CSV"), &rows, &RenderOpts::default());
    let header = out.lines().next().unwrap();
    assert_eq!(
        header,
        "PROVIDER,PATH,FIELD,SOURCE,VALUE,AGE_MS,STALE,KIND,DECAY,POLL_INTERVAL_SECS,POLL_COUNT,FSEVENTS_REINSTATE,FAILURE_CONSECUTIVE_FAILURES,FAILURE_SUPPRESSED_UNTIL_UNIX_MS"
    );
    let body = out.lines().nth(1).unwrap();
    let cols: Vec<&str> = body.split(',').collect();
    assert_eq!(cols.len(), 14);
    assert_eq!(cols[3], "refs", "source col");
}

// Task 1.20: sh — new env-var lines for lifecycle fields
#[test]
fn sh_preset_exposes_new_fields() {
    let rows = vec![sample_lifecycle_row()];
    let out = render_sh_env(&rows);
    // The key pattern is sanitize_sh_key("git", Some("/repo"), "branch") = "git_repo_branch"
    assert!(
        out.contains("git_repo_branch_POLL_INTERVAL_SECS='60'"),
        "got: {}",
        out
    );
    assert!(
        out.contains("git_repo_branch_POLL_COUNT='12'"),
        "got: {}",
        out
    );
    assert!(
        out.contains("git_repo_branch_FSEVENTS_REINSTATE='true'"),
        "got: {}",
        out
    );
    assert!(
        out.contains("git_repo_branch_KIND='lifecycle'"),
        "got: {}",
        out
    );
    assert!(out.contains("git_repo_branch_DECAY='0'"), "got: {}", out);
    assert!(
        out.contains("git_repo_branch_SOURCE='refs'"),
        "got: {}",
        out
    );
}

// Task 1.21: minijinja row_context — exposes new fields
#[test]
fn minijinja_row_context_exposes_new_fields() {
    let row = sample_lifecycle_row();
    // `row_context` is already a `serde_json::Value` object — inspect it directly.
    let v = row_context(&row);
    assert_eq!(v.get("source").and_then(|v| v.as_str()), Some("refs"));
    assert_eq!(v.get("kind").and_then(|v| v.as_str()), Some("lifecycle"));
    assert_eq!(v.get("decay").and_then(|v| v.as_u64()), Some(0));
    assert_eq!(
        v.get("poll_interval_secs").and_then(|v| v.as_u64()),
        Some(60)
    );
    assert_eq!(v.get("poll_count").and_then(|v| v.as_u64()), Some(12));
    assert_eq!(
        v.get("fsevents_reinstate").and_then(|v| v.as_bool()),
        Some(true)
    );
}

// ---------------------------------------------------------------------------
// Tasks 1.22: new filter keys — lifecycle, fsevents_reinstate
// ---------------------------------------------------------------------------

fn lc_row(provider: &str, decay: u8) -> beachcomber::cache::CacheRow {
    use beachcomber::cache::{CacheRow, RowKind};
    use serde_json::json;
    CacheRow {
        shadowed: false,
        provider: provider.into(),
        path: None,
        source: "default".into(),
        field: "x".into(),
        value: json!(0),
        age_ms: Some(0),
        stale: false,
        kind: Some(RowKind::Lifecycle {
            decay,
            depth: 4,
            watches_files: false,
        }),
        poll_interval_secs: Some(60),
        poll_count: Some(12),
        fsevents_reinstate: Some(false),
        polls_elapsed: None,
        next_poll_in_secs: None,
        failure: None,
    }
}

fn once_row(provider: &str) -> beachcomber::cache::CacheRow {
    use beachcomber::cache::{CacheRow, RowKind};
    use serde_json::json;
    CacheRow {
        shadowed: false,
        provider: provider.into(),
        path: None,
        source: "default".into(),
        field: "x".into(),
        value: json!(0),
        age_ms: Some(0),
        stale: false,
        kind: Some(RowKind::Once),
        poll_interval_secs: None,
        poll_count: None,
        fsevents_reinstate: None,
        polls_elapsed: None,
        next_poll_in_secs: None,
        failure: None,
    }
}

#[test]
fn filter_lifecycle_active_matches_only_decay_zero() {
    let rows = vec![lc_row("git", 0), lc_row("battery", 2), once_row("hostname")];
    let out = apply_filters(rows, &["lifecycle=active".to_string()]).unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].provider, "git");
}

#[test]
fn filter_lifecycle_decay4_matches_only_about_to_evict() {
    let rows = vec![lc_row("a", 4), lc_row("b", 3), lc_row("c", 0)];
    let out = apply_filters(rows, &["lifecycle=decay4".to_string()]).unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].provider, "a");
}

#[test]
fn filter_lifecycle_once_matches_once_kind() {
    let rows = vec![lc_row("git", 0), once_row("hostname")];
    let out = apply_filters(rows, &["lifecycle=once".to_string()]).unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].provider, "hostname");
}

#[test]
fn filter_fsevents_reinstate_true() {
    let mut rows = vec![lc_row("a", 0), lc_row("b", 0)];
    rows[0].fsevents_reinstate = Some(true);
    rows[1].fsevents_reinstate = Some(false);
    let out = apply_filters(rows, &["fsevents_reinstate=true".to_string()]).unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].provider, "a");
}

// ---------------------------------------------------------------------------
// Task 1.23: new sort keys — lifecycle, poll_interval
// ---------------------------------------------------------------------------

#[test]
fn sort_lifecycle_orders_most_decayed_first() {
    let rows = vec![
        lc_row("a", 0),
        lc_row("b", 4),
        once_row("c"),
        lc_row("d", 2),
    ];
    let sorted = apply_sort(rows, "lifecycle").unwrap();
    let providers: Vec<_> = sorted.iter().map(|r| r.provider.clone()).collect();
    assert_eq!(providers, vec!["b", "d", "a", "c"]);
}

#[test]
fn sort_poll_interval_orders_slowest_first() {
    let mut rows = vec![lc_row("a", 0), lc_row("b", 0), lc_row("c", 0)];
    rows[0].poll_interval_secs = Some(30);
    rows[1].poll_interval_secs = Some(300);
    rows[2].poll_interval_secs = Some(60);
    let rows = apply_sort(rows, "poll_interval").unwrap();
    let p: Vec<_> = rows.iter().map(|r| r.poll_interval_secs.unwrap()).collect();
    assert_eq!(p, vec![300, 60, 30]);
}

// ---------------------------------------------------------------------------
// Task 1.24: defensive tests
// ---------------------------------------------------------------------------

#[test]
fn wire_format_round_trip_full_cache_row() {
    let original = sample_lifecycle_row();
    let json = serde_json::to_string(&original).unwrap();
    let round: beachcomber::cache::CacheRow = serde_json::from_str(&json).unwrap();
    assert_eq!(round.provider, original.provider);
    assert_eq!(round.kind, original.kind);
    assert_eq!(round.poll_interval_secs, original.poll_interval_secs);
    assert_eq!(round.poll_count, original.poll_count);
    assert_eq!(round.fsevents_reinstate, original.fsevents_reinstate);
    assert_eq!(round.failure, original.failure);
}

#[test]
fn human_renders_p_zero_k_zero_without_panic() {
    use beachcomber::cache::{CacheRow, RowKind};
    use serde_json::json;
    let row = CacheRow {
        shadowed: false,
        provider: "edge".into(),
        path: None,
        source: "default".into(),
        field: "x".into(),
        value: json!(null),
        age_ms: Some(0),
        stale: false,
        kind: Some(RowKind::Lifecycle {
            decay: 0,
            depth: 4,
            watches_files: false,
        }),
        poll_interval_secs: Some(0),
        poll_count: Some(0),
        fsevents_reinstate: Some(false),
        polls_elapsed: None,
        next_poll_in_secs: Some(0),
        failure: None,
    };
    let opts = beachcomber::cli::status_format::FormatOptions::default();
    let out = beachcomber::cli::status_format::render_human(&[row], &opts);
    assert!(
        out.contains("0s\u{00d7}00 (0s)"),
        "P=0 K=0 expected: {}",
        out
    );
    // POLL column also renders 0s without panicking.
    assert!(out.contains("0s"), "POLL column expected 0s: {}", out);
}

// ---------------------------------------------------------------------------
// Task 1.17: Failure-state ⚠ end-to-end — warn glyph + red row
// ---------------------------------------------------------------------------

#[test]
fn failure_state_renders_warn_and_red_row() {
    use beachcomber::cache::{CacheRow, FailureSnapshot, RowKind};
    use serde_json::json;

    let row = CacheRow {
        shadowed: false,
        provider: "flaky".into(),
        path: None,
        source: "default".into(),
        field: "thing".into(),
        value: json!("x"),
        age_ms: Some(1000),
        stale: false,
        kind: Some(RowKind::Lifecycle {
            decay: 1,
            depth: 4,
            watches_files: false,
        }),
        poll_interval_secs: Some(60),
        poll_count: Some(12),
        fsevents_reinstate: Some(false),
        polls_elapsed: None,
        next_poll_in_secs: None,
        failure: Some(FailureSnapshot {
            consecutive_failures: 3,
            suppressed_until_unix_ms: None,
        }),
    };
    // Use render_preset("human", ...) with is_tty=true so color is enabled.
    let opts = RenderOpts {
        is_tty: true,
        no_color: false,
        max_width: Some(120),
        no_trunc: false,
        ascii: false,
        depth: None,
    };
    let out = render_preset(&status_preset("human"), &[row], &opts);
    assert!(out.contains("\u{26a0}"), "warn glyph missing: {:?}", out);
    assert!(out.contains("\x1b[31m"), "red ANSI missing: {:?}", out);
}

#[test]
fn render_preset_ascii_flag_swaps_glyphs() {
    use beachcomber::cache::{CacheRow, RowKind};
    use beachcomber::cli::status_format::{RenderOpts, render_preset};
    use serde_json::json;

    let row = CacheRow {
        shadowed: false,
        provider: "git".into(),
        path: None,
        source: "refs".into(),
        field: "branch".into(),
        value: json!("main"),
        age_ms: Some(1000),
        stale: false,
        kind: Some(RowKind::Lifecycle {
            decay: 0,
            depth: 4,
            watches_files: true,
        }),
        poll_interval_secs: Some(60),
        poll_count: Some(12),
        fsevents_reinstate: Some(true),
        polls_elapsed: None,
        next_poll_in_secs: None,
        failure: None,
    };
    let opts_unicode = RenderOpts {
        is_tty: true,
        no_color: true,
        max_width: Some(120),
        no_trunc: false,
        ascii: false,
        depth: None,
    };
    let opts_ascii = RenderOpts {
        is_tty: true,
        no_color: true,
        max_width: Some(120),
        no_trunc: false,
        ascii: true,
        depth: None,
    };
    let out_unicode = render_preset(
        &status_preset("human"),
        std::slice::from_ref(&row),
        &opts_unicode,
    );
    let out_ascii = render_preset(&status_preset("human"), &[row], &opts_ascii);
    assert!(
        out_unicode.contains("\u{2605}"),
        "unicode star expected: {}",
        out_unicode
    );
    assert!(
        !out_ascii.contains("\u{2605}"),
        "unicode star should be absent in ascii mode: {}",
        out_ascii
    );
    assert!(
        out_ascii.contains("*"),
        "ascii star expected: {}",
        out_ascii
    );
}

#[test]
fn render_preset_color_respects_caller_decision_not_re_anding_with_tty() {
    use beachcomber::cache::{CacheRow, RowKind};
    use beachcomber::cli::status_format::{RenderOpts, render_preset};
    use serde_json::json;

    let row = CacheRow {
        shadowed: false,
        provider: "git".into(),
        path: None,
        source: "refs".into(),
        field: "branch".into(),
        value: json!("main"),
        age_ms: Some(1000),
        stale: false,
        kind: Some(RowKind::Lifecycle {
            decay: 0,
            depth: 4,
            watches_files: true,
        }),
        poll_interval_secs: Some(60),
        poll_count: Some(12),
        fsevents_reinstate: Some(true),
        polls_elapsed: None,
        next_poll_in_secs: None,
        failure: None,
    };
    // Simulate `--color=always` into a pipe: the caller resolved color=true
    // even though stdout is not a TTY. The renderer must honour that decision
    // rather than re-deriving it from is_tty — this is what makes
    // `watch -c 'comb status --color=always'` work.
    let opts = RenderOpts {
        is_tty: false,
        no_color: false,
        max_width: Some(120),
        no_trunc: false,
        ascii: false,
        depth: None,
    };
    let out = render_preset(&status_preset("human"), &[row], &opts);
    assert!(
        out.contains("\x1b[92m"),
        "bright green ANSI expected when no_color=false even with is_tty=false: {}",
        out
    );
}

// ---------------------------------------------------------------------------
// Home-path compaction (`~`) — human preset only
// ---------------------------------------------------------------------------

#[test]
fn human_preset_compacts_home_path() {
    use beachcomber::cache::CacheRow;
    use beachcomber::cli::status_format::{RenderOpts, render_preset};
    use serde_json::json;

    let row = CacheRow {
        shadowed: false,
        provider: "git".into(),
        path: Some("/Users/x/ws/beachcomber".into()),
        source: "refs".into(),
        field: "branch".into(),
        value: json!("main"),
        age_ms: Some(0),
        stale: false,
        kind: None,
        poll_interval_secs: None,
        poll_count: None,
        fsevents_reinstate: None,
        polls_elapsed: None,
        next_poll_in_secs: None,
        failure: None,
    };
    let opts = RenderOpts::default();

    let out = temp_env::with_var("HOME", Some("/Users/x"), || {
        render_preset(&status_preset("human"), &[row], &opts)
    });
    assert!(
        out.contains("~/ws/beachcomber"),
        "expected compacted path: {out}"
    );
    assert!(
        !out.contains("/Users/x/ws/beachcomber"),
        "raw home-prefixed path should not appear: {out}"
    );
}

#[test]
fn machine_presets_keep_real_path_under_home() {
    use beachcomber::cache::CacheRow;
    use beachcomber::cli::status_format::{RenderOpts, render_preset};
    use serde_json::json;

    let row = CacheRow {
        shadowed: false,
        provider: "git".into(),
        path: Some("/home/testuser/ws/beachcomber".into()),
        source: "refs".into(),
        field: "branch".into(),
        value: json!("main"),
        age_ms: Some(0),
        stale: false,
        kind: None,
        poll_interval_secs: None,
        poll_count: None,
        fsevents_reinstate: None,
        polls_elapsed: None,
        next_poll_in_secs: None,
        failure: None,
    };
    let opts = RenderOpts::default();

    temp_env::with_var("HOME", Some("/home/testuser"), || {
        // json/csv/tsv carry `path` verbatim; sh flattens it into a
        // sanitized shell-variable key (slashes → underscores) rather than
        // embedding the literal path string, so it's checked separately —
        // the shared assertion across all four is simply "no `~` leaks in".
        // Both tables compact it (`human_preset_compacts_home_path`).
        for preset in ["json", "csv", "tsv"] {
            let out = render_preset(&status_preset(preset), std::slice::from_ref(&row), &opts);
            assert!(
                out.contains("/home/testuser/ws/beachcomber"),
                "{preset} preset should keep the real path: {out}"
            );
            assert!(
                !out.contains('~'),
                "{preset} preset should not compact the path: {out}"
            );
        }
        let sh_out = render_preset(&status_preset("sh"), std::slice::from_ref(&row), &opts);
        assert!(
            !sh_out.contains('~'),
            "sh preset should not compact the path: {sh_out}"
        );
    });
}

#[test]
fn human_preset_path_compaction_frees_width_for_value() {
    use beachcomber::cache::CacheRow;
    use beachcomber::cli::status_format::{RenderOpts, render_preset};
    use serde_json::json;

    let row = CacheRow {
        shadowed: false,
        provider: "p".into(),
        path: Some("/home/testuser/p".into()),
        source: "default".into(),
        field: "f".into(),
        value: json!("a".repeat(20)),
        age_ms: Some(0),
        stale: false,
        kind: None,
        poll_interval_secs: None,
        poll_count: None,
        fsevents_reinstate: None,
        polls_elapsed: None,
        next_poll_in_secs: None,
        failure: None,
    };
    let opts = RenderOpts {
        is_tty: true,
        no_color: true,
        // 70 before SOURCE joined the table; the new column and its separator
        // cost 9 characters of the same budget, and this test is about what
        // compaction frees, not about where the cap sits.
        max_width: Some(79),
        no_trunc: false,
        ascii: false,
        depth: None,
    };

    // HOME matches the row's path: compaction shrinks PATH from 16 to 3
    // chars, freeing width the budget hands to VALUE — the full 20-char
    // value fits uncut.
    let compacted = temp_env::with_var("HOME", Some("/home/testuser"), || {
        render_preset(&status_preset("human"), std::slice::from_ref(&row), &opts)
    });
    assert!(
        compacted.contains(&"a".repeat(20)),
        "compaction should free enough width for the full VALUE: {compacted}"
    );
    assert!(
        compacted.contains("~/p"),
        "path should compact: {compacted}"
    );

    // Same total-width budget, but HOME doesn't match this row's path: PATH
    // stays at its full 16-char length, so VALUE must truncate.
    let uncompacted = temp_env::with_var("HOME", Some("/no/match/here"), || {
        render_preset(&status_preset("human"), &[row], &opts)
    });
    assert!(
        !uncompacted.contains(&"a".repeat(20)),
        "without compaction the same width budget should truncate VALUE: {uncompacted}"
    );
}

// ---------------------------------------------------------------------------
// watch(1) / viddy safety: rows must not fill the terminal edge-to-edge, and
// must carry no control characters other than the line terminator.
// ---------------------------------------------------------------------------

/// A row whose VALUE is long enough that the total-width budget is the binding
/// constraint, so the rendered line is pushed to the cap.
fn wide_value_row() -> CacheRow {
    CacheRow {
        shadowed: false,
        provider: "mise".into(),
        path: Some("/home/me/ws/foo".into()),
        source: "global".into(),
        field: "global".into(),
        value: serde_json::json!("v".repeat(200)),
        age_ms: Some(158_000),
        stale: false,
        kind: Some(beachcomber::cache::RowKind::Lifecycle {
            decay: 0,
            depth: 4,
            watches_files: true,
        }),
        poll_interval_secs: Some(60),
        poll_count: Some(12),
        fsevents_reinstate: Some(true),
        polls_elapsed: Some(3),
        next_poll_in_secs: Some(42),
        failure: None,
    }
}

#[test]
fn human_rows_leave_the_right_margin_unpainted() {
    // The right margin is reserved in the width budget but never printed.
    // Painting it made every row exactly `max_width` — exactly the terminal
    // width — which viddy wraps, because it reserves the last column for its
    // scrollbar. Rows must stop short of the cap and end in no trailing space.
    // 80 was the tightest cap this fixture's floors could still be laid out
    // within; SOURCE and its separator claim 8 more characters of the same
    // budget, so the tightest feasible cap moved with them.
    for cap in [88usize, 100, 120, 160] {
        let opts = RenderOpts {
            is_tty: false,
            no_color: true,
            max_width: Some(cap),
            no_trunc: false,
            ascii: true, // ascii so glyph char-counts are unambiguous
            depth: None,
        };
        let out = render_preset(&status_preset("human"), &[wide_value_row()], &opts);
        for line in out.lines() {
            assert!(
                line.chars().count() <= cap - 2,
                "line is {} chars, must leave the 2-col right margin free under cap {cap}: {line:?}",
                line.chars().count()
            );
            assert!(
                !line.ends_with(' '),
                "line must not end in trailing whitespace: {line:?}"
            );
        }
    }

    // A `config:<file>` source label is a whole path — long enough that, at
    // the old flat 24-char cap, it alone pushed a modest, realistic
    // config-seeded row (Static, like the ones
    // `list_rows_marks_config_seeded_source_static` covers) past the margin
    // at cap 80 (79 chars — the flex columns' floors are non-negotiable, so
    // the row simply overflowed). Budget-aware SOURCE truncation
    // (MAX_SOURCE_WIDTH.min(cap / 5)) leaves enough room that it does not.
    let config_row = beachcomber::cache::CacheRow {
        shadowed: false,
        provider: "myapp".into(),
        path: Some("/repo".into()),
        source: "config:~/.config/beachcomber/defaults/env.toml".into(),
        field: "greeting".into(),
        value: serde_json::json!("1.2.3-rc1"),
        age_ms: Some(5_000),
        stale: false,
        kind: Some(beachcomber::cache::RowKind::Static),
        poll_interval_secs: None,
        poll_count: None,
        fsevents_reinstate: None,
        polls_elapsed: None,
        next_poll_in_secs: None,
        failure: None,
    };
    let opts = RenderOpts {
        is_tty: false,
        no_color: true,
        max_width: Some(80),
        no_trunc: false,
        ascii: true,
        depth: None,
    };
    let out = render_preset(&status_preset("human"), &[config_row], &opts);
    for line in out.lines() {
        assert!(
            line.chars().count() <= 78,
            "line is {} chars, must leave the 2-col right margin free under cap 80 with a long SOURCE label: {line:?}",
            line.chars().count()
        );
        assert!(
            !line.ends_with(' '),
            "line must not end in trailing whitespace: {line:?}"
        );
    }
}

#[test]
fn human_caps_path_column_keeping_the_tail() {
    // A 150+ char session-keyed path (`/dir#session-id`) must not blow the
    // table budget: uncapped, it padded the PATH column of every row past the
    // cap, wrapping each line under watch/viddy. The session id compresses
    // to its last 4 chars first, then the directory keeps its own tail.
    let mut row = wide_value_row();
    let long_path = format!(
        "/private/tmp/{}/scratchpad/e2e/repo#e2e-session-0001",
        "x".repeat(90)
    );
    row.path = Some(long_path);
    let cap = 120usize;
    let opts = RenderOpts {
        is_tty: false,
        no_color: true,
        max_width: Some(cap),
        no_trunc: false,
        ascii: true,
        depth: None,
    };
    let out = render_preset(&status_preset("human"), &[row], &opts);
    for line in out.lines() {
        assert!(
            line.chars().count() <= cap - 2,
            "line is {} chars, exceeds cap {cap} minus reserved margin: {line:?}",
            line.chars().count()
        );
    }
    assert!(
        out.contains("repo#...0001"),
        "directory tail and compressed session id must survive: {out}"
    );
    assert!(
        !out.contains("/private/tmp/x"),
        "path head should be truncated away: {out}"
    );
}

#[test]
fn human_caps_source_column_keeping_the_tail() {
    // SOURCE truncates like PATH — from the head, keeping the tail that
    // tells one config file from another.
    let mut row = sample_lifecycle_row();
    row.source = "config:~/.config/beachcomber/conf.d/mine.toml".into(); // 45 chars
    let opts = RenderOpts {
        is_tty: false,
        no_color: true,
        max_width: Some(120),
        no_trunc: false,
        ascii: true,
        depth: None,
    };
    let out = render_preset(&status_preset("human"), &[row], &opts);
    assert!(
        out.contains("...mber/conf.d/mine.toml"),
        "SOURCE cell should truncate from the head and keep its tail: {out}"
    );
    assert!(
        !out.contains("config:~/.config/beachcomber/conf.d/mine.toml"),
        "the full 45-char label must not survive at cap 120: {out}"
    );
}

#[test]
fn no_trunc_keeps_the_full_source_label() {
    let mut row = sample_lifecycle_row();
    row.source = "config:~/.config/beachcomber/conf.d/mine.toml".into();
    let opts = RenderOpts {
        is_tty: false,
        no_color: true,
        max_width: Some(120),
        no_trunc: true,
        ascii: true,
        depth: None,
    };
    let out = render_preset(&status_preset("human"), &[row], &opts);
    assert!(
        out.contains("config:~/.config/beachcomber/conf.d/mine.toml"),
        "--no-trunc must keep the full SOURCE label: {out}"
    );
}

#[test]
fn truncation_uses_single_char_ellipsis_outside_ascii_mode() {
    // Truncation marks cost one column (…) unless --ascii, where the glyph
    // can't be trusted and "..." is used instead.
    let mut row = wide_value_row(); // 200-char value forces VALUE truncation
    row.path = Some(format!("/private/tmp/{}/repo#session-42", "y".repeat(120)));
    let opts = RenderOpts {
        is_tty: false,
        no_color: true,
        max_width: Some(100),
        no_trunc: false,
        ascii: false,
        depth: Some(1),
    };
    let out = render_preset(&status_preset("human"), &[row.clone()], &opts);
    assert!(out.contains('\u{2026}'), "unicode mode uses …: {out}");
    assert!(
        !out.contains("..."),
        "unicode mode never spends 3 cols: {out}"
    );
    assert!(
        out.contains("repo#\u{2026}n-42"),
        "session id compresses to its last 4 chars: {out}"
    );

    let ascii_out = render_preset(
        &status_preset("human"),
        &[row],
        &RenderOpts {
            ascii: true,
            ..opts
        },
    );
    assert!(
        ascii_out.contains("..."),
        "ascii mode uses dots: {ascii_out}"
    );
    assert!(
        !ascii_out.contains('\u{2026}'),
        "no glyph under --ascii: {ascii_out}"
    );
}

#[test]
fn age_column_is_right_aligned_with_a_stable_floor() {
    // Under watch, AGE ticking "9s" → "12s" used to shift every boundary to
    // its right. The column floors at 8 and right-aligns, so renders that
    // differ only in age line up byte-for-byte in width.
    let mk = |age_ms: u128| {
        let mut r = sample_lifecycle_row();
        r.age_ms = Some(age_ms);
        r
    };
    let opts = RenderOpts {
        is_tty: false,
        no_color: true,
        max_width: Some(120),
        no_trunc: false,
        ascii: true,
        depth: Some(1),
    };
    let short = render_preset(&status_preset("human"), &[mk(5_000)], &opts);
    let long = render_preset(&status_preset("human"), &[mk(3_540_000)], &opts);
    assert!(
        short.contains("      5s"),
        "5s right-aligns in an 8-wide AGE column: {short}"
    );
    let width = |s: &str, n: usize| s.lines().nth(n).map(|l| l.chars().count());
    assert_eq!(
        width(&short, 1),
        width(&long, 1),
        "age ticking must not change row width:\n{short}\n{long}"
    );
}

#[test]
fn no_trunc_keeps_full_path() {
    let mut row = wide_value_row();
    let long_path = format!("/private/tmp/{}/repo#session-1", "y".repeat(120));
    row.path = Some(long_path.clone());
    let opts = RenderOpts {
        is_tty: false,
        no_color: true,
        max_width: Some(40),
        no_trunc: true,
        ascii: true,
        depth: None,
    };
    let out = render_preset(&status_preset("human"), &[row], &opts);
    assert!(
        out.contains(&long_path),
        "--no-trunc must keep the full path: {out}"
    );
}

#[test]
fn human_output_has_no_control_chars_besides_newline() {
    // Under `watch` / `viddy` stdout is a pipe, so colour resolves off. The
    // bytes that reach the viewer must then be plain text: no ANSI escapes
    // (ESC), no cursor movement, no tabs, no carriage returns.
    let opts = RenderOpts {
        is_tty: false,
        no_color: true,
        max_width: Some(120),
        no_trunc: false,
        ascii: false,
        depth: None,
    };
    let mut rows = sample_rows();
    rows.push(wide_value_row());
    // A provider-supplied value carrying its own escape sequence must not
    // reach the terminal intact.
    let mut hostile = wide_value_row();
    hostile.value = serde_json::json!("\x1b[31mred\x1b[0m");
    rows.push(hostile);
    let out = render_preset(&status_preset("human"), &rows, &opts);
    assert!(!out.is_empty(), "expected rendered output");
    for (i, c) in out.char_indices() {
        assert!(
            c == '\n' || !c.is_control(),
            "control char {:?} (U+{:04X}) at byte {i} in human output",
            c,
            c as u32
        );
    }
}

#[test]
fn width_split_gives_value_priority_over_path() {
    // Two flexible columns competing for one budget. PATH used to claim its
    // natural width first, pinning VALUE at its 8-char floor across the whole
    // table so a long session path could truncate away the data being read.
    // VALUE now states its need first; PATH takes the remainder and floors at
    // MIN_PATH_WIDTH (20), enough to keep a recognizable tail.
    let mut long_value = wide_value_row(); // 200-char value
    long_value.path = Some("/repo".into());
    let mut long_path = wide_value_row();
    long_path.value = serde_json::json!("short");
    long_path.path = Some(format!("/private/tmp/{}/repo#sess-1", "p".repeat(125)));
    assert_eq!(
        long_path.path.as_deref().unwrap().chars().count(),
        150,
        "fixture should be a 150-char path"
    );

    // 120 before SOURCE joined the table: the new column and its separator
    // cost 8 characters, and these assertions are about how the flexible
    // columns split what is left, not about where the cap sits.
    let cap = 128usize;
    let opts = RenderOpts {
        is_tty: false,
        no_color: true,
        max_width: Some(cap),
        no_trunc: false,
        ascii: true,
        depth: None,
    };
    let out = render_preset(&status_preset("human"), &[long_value, long_path], &opts);

    for line in out.lines() {
        assert!(
            line.chars().count() <= cap - 2,
            "line is {} chars, exceeds cap {cap} minus the reserved margin: {line:?}",
            line.chars().count()
        );
    }

    // VALUE keeps well more than its floor — it should get the surplus the
    // path no longer hogs.
    let value_run = out
        .lines()
        .filter_map(|l| l.split_whitespace().find(|w| w.starts_with("vvv")))
        .map(|w| w.chars().count())
        .max()
        .expect("expected the long-value row to render");
    assert!(
        value_run > 40,
        "VALUE should get the surplus width, got {value_run} chars: {out}"
    );

    // PATH floors at MIN_PATH_WIDTH, keeping its directory tail and the
    // compressed session id.
    let path_run = out
        .lines()
        .filter_map(|l| l.split_whitespace().find(|w| w.ends_with("#...ss-1")))
        .map(|w| w.chars().count())
        .max()
        .expect("expected the long-path row to render");
    assert_eq!(
        path_run, 20,
        "PATH should floor at MIN_PATH_WIDTH: {path_run} chars in {out}"
    );
}

// ---------------------------------------------------------------------------
// Object destructuring (--depth)
// ---------------------------------------------------------------------------

fn nested_row() -> CacheRow {
    CacheRow {
        shadowed: false,
        provider: "agent".into(),
        path: Some("/repo".into()),
        source: "put".into(),
        field: "session".into(),
        value: serde_json::json!({
            "color": "blue",
            "inner": {"deep": 1},
            "list": [1, 2, 3],
        }),
        age_ms: Some(4_000),
        stale: true,
        kind: Some(beachcomber::cache::RowKind::Once),
        poll_interval_secs: Some(30),
        poll_count: Some(4),
        fsevents_reinstate: Some(true),
        polls_elapsed: Some(2),
        next_poll_in_secs: Some(9),
        failure: None,
    }
}

fn fields_of(rows: &[CacheRow]) -> Vec<String> {
    rows.iter().map(|r| r.field.clone()).collect()
}

#[test]
fn depth_one_is_todays_behaviour() {
    use beachcomber::cli::status_format::flatten_rows;
    let rows = flatten_rows(vec![nested_row()], 1);
    assert_eq!(fields_of(&rows), vec!["session"]);
    assert!(rows[0].value.is_object(), "value untouched at depth 1");
}

#[test]
fn depth_two_expands_exactly_one_level() {
    use beachcomber::cli::status_format::flatten_rows;
    let rows = flatten_rows(vec![nested_row()], 2);
    assert_eq!(
        fields_of(&rows),
        vec!["session.color", "session.inner", "session.list"]
    );
    // The nested object stops here — it is a blob at depth 2.
    let inner = rows.iter().find(|r| r.field == "session.inner").unwrap();
    assert!(inner.value.is_object());
}

#[test]
fn depth_zero_expands_fully_but_never_arrays() {
    use beachcomber::cli::status_format::flatten_rows;
    let rows = flatten_rows(vec![nested_row()], 0);
    assert_eq!(
        fields_of(&rows),
        vec!["session.color", "session.inner.deep", "session.list"]
    );
    // Arrays stay whole — their indices are not field names.
    let list = rows.iter().find(|r| r.field == "session.list").unwrap();
    assert_eq!(list.value, serde_json::json!([1, 2, 3]));
}

#[test]
fn destructured_children_inherit_parent_metadata() {
    use beachcomber::cli::status_format::flatten_rows;
    let parent = nested_row();
    let rows = flatten_rows(vec![parent.clone()], 0);
    for child in &rows {
        assert_eq!(child.provider, parent.provider);
        assert_eq!(child.path, parent.path);
        assert_eq!(child.age_ms, parent.age_ms);
        assert_eq!(child.stale, parent.stale);
        assert_eq!(child.poll_interval_secs, parent.poll_interval_secs);
        assert_eq!(child.poll_count, parent.poll_count);
        assert_eq!(child.next_poll_in_secs, parent.next_poll_in_secs);
        assert!(matches!(
            child.kind,
            Some(beachcomber::cache::RowKind::Once)
        ));
    }
}

#[test]
fn empty_object_stays_its_own_row() {
    use beachcomber::cli::status_format::flatten_rows;
    let mut row = nested_row();
    row.value = serde_json::json!({});
    let rows = flatten_rows(vec![row], 0);
    assert_eq!(fields_of(&rows), vec!["session"], "must not vanish");
}

#[test]
fn default_depth_destructures_human_presets_only() {
    use beachcomber::cli::status_format::default_depth;
    // Two levels: enough to see structure, short of dumping a deep API blob
    // as thousands of rows. `--depth 0` still asks for everything.
    assert_eq!(default_depth(&status_preset("human")), 2);
    assert_eq!(default_depth(&status_preset("table")), 2);
    for machine in ["json", "tsv", "csv", "sh", "{{ provider }}"] {
        assert_eq!(
            default_depth(&status_preset(machine)),
            1,
            "machine preset {machine} must stay byte-identical by default"
        );
    }
}

#[test]
fn destructured_rows_render_as_separate_table_rows() {
    use beachcomber::cli::status_format::flatten_rows;
    let opts = RenderOpts {
        is_tty: false,
        no_color: true,
        max_width: Some(120),
        no_trunc: false,
        ascii: true,
        depth: None,
    };
    let rows = flatten_rows(vec![nested_row()], 0);
    let out = render_preset(&status_preset("human"), &rows, &opts);
    assert!(out.contains("session.color"), "{out}");
    assert!(out.contains("session.inner.deep"), "{out}");
    assert!(out.contains("blue"), "{out}");
}

// ---------------------------------------------------------------------------
// Sanitizing provider-controlled cell text
// ---------------------------------------------------------------------------

fn render_hostile(value: serde_json::Value, ascii: bool, no_color: bool) -> String {
    let mut row = wide_value_row();
    row.value = value;
    row.path = Some("/repo".into());
    let opts = RenderOpts {
        is_tty: false,
        no_color,
        max_width: Some(120),
        no_trunc: false,
        ascii,
        depth: None,
    };
    render_preset(&status_preset("human"), &[row], &opts)
}

#[test]
fn control_chars_render_as_control_pictures() {
    // ESC → ␛ (U+241B), BEL → ␇ (U+2407), TAB → ␉ (U+2409), DEL → ␡ (U+2421).
    let out = render_hostile(
        serde_json::json!("\x1b[31mred\x1b[0m\x07\x09\x7f"),
        false,
        true,
    );
    assert!(!out.contains('\x1b'), "raw ESC must not survive: {out:?}");
    assert!(out.contains("\u{241b}[31mred"), "ESC picture: {out:?}");
    assert!(out.contains('\u{2407}'), "BEL picture: {out:?}");
    assert!(out.contains('\u{2409}'), "TAB picture: {out:?}");
    assert!(out.contains('\u{2421}'), "DEL picture: {out:?}");
}

#[test]
fn c1_controls_become_the_replacement_char() {
    let out = render_hostile(serde_json::json!("a\u{0085}b"), false, true);
    assert!(out.contains("a\u{fffd}b"), "C1 → U+FFFD: {out:?}");
}

#[test]
fn ascii_mode_collapses_controls_to_question_marks() {
    // The Control Pictures block is not ASCII, so --ascii cannot use it.
    let out = render_hostile(serde_json::json!("\x1b[31mred"), true, true);
    assert!(!out.contains('\x1b'), "raw ESC must not survive: {out:?}");
    assert!(out.contains("?[31mred"), "controls → '?': {out:?}");
    assert!(!out.contains('\u{241b}'), "no unicode pictures: {out:?}");
}

#[test]
fn sanitized_rows_carry_a_marker() {
    let out = render_hostile(serde_json::json!("\x1b[31mred"), false, true);
    assert!(
        out.contains("\u{203c} \u{241b}"),
        "‼ marker on VALUE: {out}"
    );

    let ascii = render_hostile(serde_json::json!("\x1b[31mred"), true, true);
    assert!(ascii.contains("! ?[31mred"), "ascii marker: {ascii}");

    // Clean rows get no marker.
    let clean = render_hostile(serde_json::json!("plain"), false, true);
    assert!(!clean.contains('\u{203c}'), "no marker when clean: {clean}");
}

#[test]
fn taint_marker_is_coloured_and_does_not_break_alignment() {
    let out = render_hostile(serde_json::json!("\x1b[31mred"), false, false);
    // The only ESC bytes present are the ones the renderer itself emitted.
    assert!(
        out.contains("\x1b[33m\u{203c} \x1b[0m"),
        "marker wrapped in yellow: {out:?}"
    );
    // The sanitized payload sits after the marker, still picture-escaped.
    assert!(out.contains("\u{241b}[31mred"), "{out:?}");
    // Column alignment is computed on visible width, so the header's PATH
    // column and the row's must still start at the same offset.
    let lines: Vec<&str> = out.lines().collect();
    let strip = |s: &str| -> String {
        let mut o = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for c2 in chars.by_ref() {
                    if c2 == 'm' {
                        break;
                    }
                }
            } else {
                o.push(c);
            }
        }
        o
    };
    let header = strip(lines[0]);
    let row = strip(lines[1]);
    // Char index, not byte offset — ‼ and ␛ are multibyte.
    let col_of = |s: &str, needle: &str| {
        s.find(needle)
            .map(|b| s[..b].chars().count())
            .unwrap_or_else(|| panic!("{needle:?} not found in {s:?}"))
    };
    assert_eq!(
        col_of(&header, "PATH"),
        col_of(&row, "/repo"),
        "PATH column must line up:\n{header}\n{row}"
    );
}

#[test]
fn machine_presets_do_not_sanitize() {
    // JSON escapes control characters itself; TSV/CSV/sh consumers want the
    // bytes the provider actually produced.
    let mut row = wide_value_row();
    row.value = serde_json::json!("\x1b[31mred");
    let json = render_preset(
        &status_preset("json"),
        std::slice::from_ref(&row),
        &RenderOpts::default(),
    );
    assert!(json.contains("\\u001b"), "JSON escapes it: {json}");
    assert!(!json.contains('\u{241b}'), "no pictures in json: {json}");
    let tsv = render_preset(&status_preset("tsv"), &[row], &RenderOpts::default());
    assert!(!tsv.contains('\u{241b}'), "no pictures in tsv: {tsv}");
}

// ---------------------------------------------------------------------------
// JSON syntax highlighting in the human preset's VALUE cell
// ---------------------------------------------------------------------------

/// Strip SGR sequences, returning the visible text.
fn strip_sgr(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for c2 in chars.by_ref() {
                if c2 == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn render_structured(no_color: bool) -> String {
    let mut row = wide_value_row();
    row.value = serde_json::json!({"name": "claude", "ctx": 42, "on": true, "off": null});
    row.path = Some("/repo".into());
    let opts = RenderOpts {
        is_tty: false,
        no_color,
        max_width: Some(120),
        no_trunc: false,
        ascii: true,
        depth: Some(1), // keep the object whole so there is something to highlight
    };
    render_preset(&status_preset("human"), &[row], &opts)
}

#[test]
fn structured_values_are_highlighted_when_colour_is_on() {
    let out = render_structured(false);
    assert!(out.contains("\x1b[2m\"ctx\"\x1b[0m"), "keys dim: {out:?}");
    assert!(
        out.contains("\x1b[92m\"claude\"\x1b[0m"),
        "string values green: {out:?}"
    );
    assert!(out.contains("\x1b[36m42\x1b[0m"), "numbers cyan: {out:?}");
    assert!(
        out.contains("\x1b[35mtrue\x1b[0m") && out.contains("\x1b[35mnull\x1b[0m"),
        "literals magenta: {out:?}"
    );
    // Punctuation is left uncoloured.
    assert!(strip_sgr(&out).contains(r#"{"ctx":42,"name":"claude","off":null,"on":true}"#));
}

#[test]
fn highlighting_is_absent_when_colour_is_off() {
    let out = render_structured(true);
    assert!(!out.contains('\x1b'), "no escapes with colour off: {out:?}");
}

#[test]
fn highlighting_does_not_disturb_column_alignment() {
    // Visible widths drive padding, so the coloured and plain renders must
    // agree once the escapes are stripped.
    assert_eq!(
        strip_sgr(&render_structured(false)),
        render_structured(true)
    );
}

#[test]
fn scalar_values_are_not_highlighted() {
    // Only object/array values get the treatment — a bare string must not be
    // parsed as JSON.
    let mut row = wide_value_row();
    row.value = serde_json::json!("true");
    row.path = Some("/repo".into());
    let opts = RenderOpts {
        is_tty: false,
        no_color: false,
        max_width: Some(120),
        no_trunc: false,
        ascii: true,
        depth: None,
    };
    let out = render_preset(&status_preset("human"), &[row], &opts);
    assert!(
        !out.contains("\x1b[35mtrue"),
        "a scalar string must not be tokenized: {out:?}"
    );
}

#[test]
fn a_template_rows_structured_value_opens_in_cyan() {
    // Design decision 5: cyan marks a rendered (Template) row's VALUE cell.
    // A structured body still gets its own JSON highlighting, and each
    // highlighted token carries a full reset, so on-screen only the cell's
    // opening brace stays cyan — the row is marked by that lead-in plus its
    // SOURCE, not by an unbroken cyan span.
    let mut row = wide_value_row();
    row.kind = Some(beachcomber::cache::RowKind::Template);
    row.value = serde_json::json!({"name": "claude"});
    row.path = Some("/repo".into());
    let opts = RenderOpts {
        is_tty: false,
        no_color: false,
        max_width: Some(120),
        no_trunc: false,
        ascii: true,
        depth: Some(1), // keep the object whole so there is something to highlight
    };
    let out = render_preset(&status_preset("human"), &[row], &opts);
    assert!(
        out.contains("\x1b[36m{"),
        "the JSON body must still be wrapped in cyan for a Template row: {out:?}"
    );
}

#[test]
fn highlighting_survives_a_truncated_value() {
    // Truncation runs first, so the tokenizer sees a string cut mid-token and
    // must not panic or emit a half escape sequence.
    let mut row = wide_value_row();
    row.value = serde_json::json!({ "blob": "x".repeat(400), "n": 1 });
    row.path = Some("/repo".into());
    let opts = RenderOpts {
        is_tty: false,
        no_color: false,
        max_width: Some(60),
        no_trunc: false,
        ascii: true,
        depth: Some(1),
    };
    let out = render_preset(&status_preset("human"), std::slice::from_ref(&row), &opts);
    let visible = strip_sgr(&out);
    assert!(visible.contains("..."), "value should be cut: {visible}");
    // Highlighting a cut value must add escapes and nothing else: the visible
    // text has to match what the same render produces with colour off.
    let plain = render_preset(
        &status_preset("human"),
        &[row],
        &RenderOpts {
            no_color: true,
            ..opts
        },
    );
    assert_eq!(visible, plain, "highlighting changed the visible text");
    // And every escape sequence it emitted is whole.
    assert_eq!(
        out.matches('\x1b').count(),
        out.matches("\x1b[").count(),
        "every ESC must open a complete SGR sequence: {out:?}"
    );
}

#[test]
fn three_flexible_columns_share_the_budget_by_priority() {
    // All three flexible columns oversized at once: a 75-char destructured
    // field name, a 150-char session path and a 200-char value, at cap 128.
    // FIELD takes its flex-relative ceiling and middle-truncates (keeping
    // head and leaf), PATH falls to its 20-char floor keeping its tail,
    // VALUE takes what remains, and the row still fits the budget.
    let mut row = wide_value_row(); // 200-char value
    row.field =
        "api_status.incidents.10.incident_updates.0.affected_components.0.new_status".into();
    assert_eq!(row.field.chars().count(), 75, "fixture should be 75 chars");
    row.path = Some(format!("/private/tmp/{}/repo#sess-1", "p".repeat(125)));
    assert_eq!(
        row.path.as_deref().unwrap().chars().count(),
        150,
        "fixture should be a 150-char path"
    );

    // 120 before SOURCE joined the table: the new column and its separator
    // cost 8 characters, and these assertions are about how the flexible
    // columns split what is left, not about where the cap sits.
    let cap = 128usize;
    let opts = RenderOpts {
        is_tty: false,
        no_color: true,
        max_width: Some(cap),
        no_trunc: false,
        ascii: true,
        depth: Some(1),
    };
    let out = render_preset(&status_preset("human"), &[row], &opts);

    for line in out.lines() {
        assert!(
            line.chars().count() <= cap - 2,
            "line is {} chars, exceeds cap {cap} minus the reserved margin: {line:?}",
            line.chars().count()
        );
    }

    let data = out.lines().nth(1).expect("expected a data row");
    let cell = |needle: &str| {
        data.split("  ")
            .find(|c| c.contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} not found in {data:?}"))
            .chars()
            .count()
    };

    let cell_text = |needle: &str| {
        data.split("  ")
            .find(|c| c.contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} not found in {data:?}"))
    };
    // FIELD middle-truncates: the head names the owning field, the tail is
    // the discriminating leaf key.
    let field_cell = cell_text("new_status");
    let path_cell = cell_text("repo#");
    assert!(
        field_cell.starts_with("api_status")
            && field_cell.contains("...")
            && field_cell.ends_with("new_status"),
        "field must keep its head and leaf key: {field_cell:?}"
    );
    assert!(
        path_cell.starts_with("...") && path_cell.ends_with("#...ss-1"),
        "path must keep its dir tail and compressed session id: {path_cell:?}"
    );
    // FIELD sits between its floor and its flex-relative ceiling — above the
    // floor (it outranks VALUE up to the ceiling) but well short of its
    // 75-char natural width.
    let field_w = cell("new_status");
    assert!(
        (20..=35).contains(&field_w),
        "FIELD should hold its ceiling, got {field_w}: {data:?}"
    );
    assert_eq!(cell("repo#"), 20, "PATH floors at 20: {data:?}");

    // VALUE keeps a readable share despite FIELD's ceiling.
    let value_w = cell("vvv");
    assert!(
        value_w >= 20,
        "VALUE should keep a readable share, got {value_w}: {data:?}"
    );
}

#[test]
fn human_renders_an_array_as_an_array_and_a_digit_keyed_object_as_an_object() {
    let opts = RenderOpts {
        is_tty: false,
        no_color: true,
        max_width: None,
        no_trunc: true,
        ascii: true,
        depth: Some(1),
    };
    let mut row = sample_lifecycle_row();
    row.value = serde_json::json!([["a", "b"], {"x": 1}, "c"]);
    let out = render_preset(&status_preset("human"), std::slice::from_ref(&row), &opts);
    assert!(out.contains(r#"[["a","b"],{"x":1},"c"]"#), "{out}");

    // An object whose keys happen to be digits is an object: arrays are
    // values of their own, so nothing has to be guessed back into one.
    let mut digits = sample_lifecycle_row();
    digits.value = serde_json::json!({"0": "a", "1": "b"});
    let out = render_preset(&status_preset("human"), &[digits], &opts);
    assert!(out.contains(r#"{"0":"a","1":"b"}"#), "{out}");
}

#[test]
fn human_stripes_alternate_data_rows_when_colour_on() {
    let rows = sample_rows(); // 3 data rows
    let opts = RenderOpts {
        is_tty: true,
        no_color: false,
        max_width: Some(120),
        no_trunc: false,
        ascii: false,
        depth: Some(1),
    };
    const BG: &str = "\x1b[48;5;234m"; // dark-terminal stripe
    let out = temp_env::with_var("COLORFGBG", None::<&str>, || {
        render_preset(&status_preset("human"), &rows, &opts)
    });
    let lines: Vec<&str> = out.lines().collect();
    // Header and first data row unstriped; second data row striped and
    // closed with a reset; third unstriped again. COLORFGBG unset defaults
    // to the dark stripe.
    assert!(
        !lines[0].contains(BG),
        "header must not stripe: {:?}",
        lines[0]
    );
    assert!(
        !lines[1].contains(BG),
        "row 1 must not stripe: {:?}",
        lines[1]
    );
    assert!(lines[2].contains(BG), "row 2 must stripe: {:?}", lines[2]);
    assert!(
        lines[2].ends_with("\x1b[0m"),
        "stripe must reset at line end: {:?}",
        lines[2]
    );
    assert!(
        !lines[3].contains(BG),
        "row 3 must not stripe: {:?}",
        lines[3]
    );

    // Colour off: no stripes anywhere.
    let plain = render_preset(
        &status_preset("human"),
        &sample_rows(),
        &RenderOpts {
            is_tty: false,
            no_color: true,
            max_width: Some(120),
            no_trunc: false,
            ascii: false,
            depth: Some(1),
        },
    );
    assert!(
        !plain.contains(BG),
        "no stripes when colour is off: {plain}"
    );
}

#[test]
fn stripe_polarity_follows_colorfgbg() {
    let opts = RenderOpts {
        is_tty: true,
        no_color: false,
        max_width: Some(120),
        no_trunc: false,
        ascii: false,
        depth: Some(1),
    };
    const DARK: &str = "\x1b[48;5;234m";
    const LIGHT: &str = "\x1b[48;5;254m";

    // bg 0 = dark terminal → dark stripe.
    let out = temp_env::with_var("COLORFGBG", Some("15;0"), || {
        render_preset(&status_preset("human"), &sample_rows(), &opts)
    });
    assert!(
        out.contains(DARK) && !out.contains(LIGHT),
        "bg 0 → dark stripe"
    );

    // bg 15 = light terminal → light stripe. Also exercises the
    // "fg;default;bg" three-field form some terminals export.
    let out = temp_env::with_var("COLORFGBG", Some("0;default;15"), || {
        render_preset(&status_preset("human"), &sample_rows(), &opts)
    });
    assert!(
        out.contains(LIGHT) && !out.contains(DARK),
        "bg 15 → light stripe"
    );

    // Garbage falls back to dark.
    let out = temp_env::with_var("COLORFGBG", Some("nonsense"), || {
        render_preset(&status_preset("human"), &sample_rows(), &opts)
    });
    assert!(out.contains(DARK), "unparsable COLORFGBG → dark stripe");
}

#[test]
fn modest_field_names_are_not_truncated_by_a_wide_value() {
    // A shallow destructured name (depth-2 scale, ~24 chars) outranks VALUE:
    // it must render whole, eating from the blob, not be floored to 20
    // because a JSON blob wants the width.
    let mut row = wide_value_row(); // 200-char value
    row.field = "api_status.components.0".into();
    assert_eq!(row.field.chars().count(), 23);
    let opts = RenderOpts {
        is_tty: false,
        no_color: true,
        max_width: Some(120),
        no_trunc: false,
        ascii: true,
        depth: Some(1),
    };
    let out = render_preset(&status_preset("human"), &[row], &opts);
    assert!(
        out.contains("api_status.components.0"),
        "modest field name must render untruncated: {out}"
    );
}

#[test]
fn tree_dims_a_shadowed_layer_when_colour_is_on() {
    use beachcomber::cache::LayerRecord;
    use beachcomber::cli::status_format::render_tree;
    let layer = |label: &str, answered: bool| LayerRecord {
        label: label.into(),
        facet: None,
        path: None,
        value: serde_json::json!(label),
        age_ms: 0,
        stale: false,
        answered,
    };
    let rows = vec![sample_rows()[0].clone()];
    let children = vec![vec![layer("taken", true), layer("hidden", false)]];
    let opts = RenderOpts {
        no_color: false,
        ..RenderOpts::default()
    };
    let out = render_tree(&status_preset("human"), &rows, &children, &opts);
    let hidden = out.lines().find(|l| l.contains("hidden")).expect(&out);
    let taken = out.lines().find(|l| l.contains("taken")).expect(&out);
    assert!(hidden.contains("\x1b[2m"), "{hidden:?}");
    assert!(!taken.contains("\x1b[2m"), "{taken:?}");
    assert!(
        hidden.contains("shadowed"),
        "the word is there with colour too: {hidden:?}"
    );
}

/// The layer the read took is the row itself, so it is not drawn again under
/// it: a value with nothing behind it is a row alone.
#[test]
fn tree_does_not_repeat_the_row_as_its_own_layer() {
    use beachcomber::cache::LayerRecord;
    use beachcomber::cli::status_format::render_tree;
    let row = sample_rows()[0].clone();
    let itself = LayerRecord {
        label: row.source.clone(),
        facet: None,
        path: row.path.clone(),
        value: row.value.clone(),
        age_ms: 0,
        stale: false,
        answered: true,
    };
    let opts = RenderOpts {
        no_color: true,
        ascii: true,
        ..RenderOpts::default()
    };
    let out = render_tree(&status_preset("human"), &[row], &[vec![itself]], &opts);
    let body: Vec<&str> = out
        .lines()
        .skip(1)
        .filter(|l| !l.trim().is_empty())
        .collect();
    assert_eq!(body.len(), 1, "the row alone: {out}");
}

/// A `-f` value as `status` reads it.
fn status_preset(value: &str) -> beachcomber::cli::output_format::Format {
    beachcomber::cli::output_format::STATUS
        .parse(value)
        .unwrap()
}

/// `status -f TEMPLATE` over `rows`, rendered through the resolver as
/// `comb status` renders it, with no daemon behind it.
fn render_status_template(template: &str, rows: &[CacheRow]) -> String {
    use libbeachcomber::resolve::{Fetch, FetchError, Got, Inputs, Slot, eval_with};
    struct NoDaemon;
    impl Fetch for NoDaemon {
        fn get(
            &mut self,
            _: &str,
            _: Option<&str>,
            _: bool,
            _: bool,
        ) -> Result<Option<Got>, FetchError> {
            Ok(None)
        }
    }
    let env = std::collections::HashMap::new();
    let inputs = Inputs {
        cwd: "",
        env: &env,
        slot: Slot::Global,
        force: false,
        wait: false,
        allow_exec: false,
    };
    let mut render = |src: &str, locals: &serde_json::Map<String, serde_json::Value>| {
        eval_with(src, "-f", &inputs, &mut NoDaemon, locals)
    };
    beachcomber::cli::status_format::render_template_format(template, rows, &mut render).unwrap()
}
