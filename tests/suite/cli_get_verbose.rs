//! `comb get -v`: stdout is the value as without it, and stderr the read's
//! metadata, growing with the count.

use crate::common::daemon::TestDaemon;
use crate::common::socket::IsolatedSocket;

/// A daemon over Sources that log (`s`, and `h`, `ps` for the cases that
/// would disturb it), `sf` failing once `<home>/flag` exists, and templates.
fn daemon() -> (TestDaemon, std::path::PathBuf) {
    let socket = IsolatedSocket::new();
    let home = socket.dir.path().join("home");
    let xdg = socket.dir.path().join("xdg");
    std::fs::create_dir_all(xdg.join("beachcomber")).unwrap();
    std::fs::create_dir_all(home.join("proj")).unwrap();
    let flag = home.join("flag");
    std::fs::write(
        xdg.join("beachcomber").join("config.toml"),
        format!(
            r#"
[daemon.providers.s.sources.main]
data = "{{{{ _.log.warn('careful') }}}}{{{{ _.log.info('noted') }}}}{{{{ _.log.debug('detail') }}}}ok"
result.v = "{{{{ data }}}}"
[daemon.providers.sf.sources.main]
data = "{{% if '{flag}' | glob %}}{{{{ _.fail('no agent') }}}}{{% endif %}}{{{{ _.log.warn('careful') }}}}{{{{ _.log.info('noted') }}}}{{{{ _.log.debug('detail') }}}}ok"
result.v = "{{{{ data }}}}"
[daemon.providers.h.sources.main]
data = "{{{{ _.log.warn('careful') }}}}{{{{ _.log.info('noted') }}}}{{{{ _.log.debug('detail') }}}}ok"
result.v = "{{{{ data }}}}"
[daemon.providers.ps.sources.main]
data = "{{{{ _.log.warn('careful') }}}}{{{{ _.log.info('noted') }}}}{{{{ _.log.debug('detail') }}}}ok"
result.v = "{{{{ data }}}}"
[client.providers.t]
template.x = "{{{{ _.log.warn('rendered warn') }}}}r"
template.bad = "{{{{ _.fail('broken leaf') }}}}"
[daemon.providers.off]
enabled = false
[daemon.providers.off.sources.a]
data = "1"
result.v = "{{{{ data }}}}"
[client.providers.m]
template.v = "{{{{ 'v' | cache() | default(_.log.warn('computed')) }}}}"
[client.providers.m2]
template.v = "{{{{ 'v' | cache() | default(_.log.warn('computed')) }}}}"
"#,
            flag = flag.display()
        ),
    )
    .unwrap();
    (TestDaemon::serve_in(socket, home.clone(), xdg), home)
}

/// The cases, one daemon between them: each reads providers, keys and
/// directories of its own, or only adds to what the others read.
#[test]
fn the_cases_share_one_daemon() {
    let (d, home) = daemon();
    each_v_adds_rows_and_levels_and_stdout_stays_the_value(&d, &home);
    text_is_status_trees_rows_with_each_line_under_its_row(&d, &home);
    a_failure_hidden_today_is_an_error_line(&d, &home);
    several_keys_are_one_document_keyed_by_key(&d, &home);
    a_vvv_read_that_starts_debug_capture_says_when_its_lines_come(&d, &home);
    a_memo_hit_replays_the_lines_its_render_raised(&d, &home);
    a_memo_says_why_it_missed_or_hit(&d, &home);
    a_read_of_a_disabled_or_unknown_provider_is_a_warning(&d, &home);
    a_put_says_what_it_hides_replaces_and_stops_keeping(&d, &home);
    status_and_check_show_a_sources_latest_warning(&d, &home);
}

fn json(bytes: &[u8]) -> serde_json::Value {
    serde_json::from_slice(bytes).unwrap()
}

/// Every log line anywhere in a metadata tree, as `(level, msg)`: what a
/// test asserts a read said, wherever the tree put it.
pub fn lines(meta: &serde_json::Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    fn walk(v: &serde_json::Value, out: &mut Vec<(String, String)>) {
        match v {
            serde_json::Value::Object(map) => {
                if let (Some(level), Some(msg)) = (map.get("level"), map.get("msg")) {
                    out.push((
                        level.as_str().unwrap().to_string(),
                        msg.as_str().unwrap().to_string(),
                    ));
                }
                map.values().for_each(|v| walk(v, out));
            }
            serde_json::Value::Array(items) => items.iter().for_each(|v| walk(v, out)),
            _ => {}
        }
    }
    walk(meta, &mut out);
    out
}

/// Assert `meta` holds a line at `level` whose text contains `text`.
#[track_caller]
pub fn assert_line(meta: &serde_json::Value, level: &str, text: &str) {
    let all = lines(meta);
    assert!(
        all.iter().any(|(l, m)| l == level && m.contains(text)),
        "no {level} line with {text:?} in {all:?}"
    );
}

fn each_v_adds_rows_and_levels_and_stdout_stays_the_value(d: &TestDaemon, home: &std::path::Path) {
    let get = |args: &[&str]| d.comb(home).arg("get").args(args).output().unwrap();

    let plain = get(&["s.v"]);
    assert_eq!(plain.stdout, b"ok");
    assert!(plain.stderr.is_empty());

    let one = get(&["s.v", "-v", "-f", "json"]);
    assert_eq!(one.stdout, get(&["s.v", "-f", "json"]).stdout);
    let meta = json(&one.stderr);
    let own = &meta["_"];
    assert_eq!(own["executing"], false);
    assert!(own["age_ms"].is_u64());
    assert_eq!(
        own["chain"],
        serde_json::json!([{"layer": "source(main)", "path": "~", "answered": true,
            "log": [{"level": "warn", "msg": "careful", "origin": "source(main)"}]}])
    );

    d.comb(home)
        .args(["put", "s", r#"{"v":"P"}"#, "./proj"])
        .assert()
        .success();
    let two = json(&get(&["s.v", "-vv", "-f", "json", "./proj"]).stderr);
    let chain = &two["_"]["chain"];
    assert_eq!(chain[0]["layer"], "put");
    assert_eq!(chain[0]["path"], "~/proj");
    assert_eq!(chain[0]["value"], "P");
    assert_eq!(chain[1]["layer"], "source(main)");
    assert_eq!(chain[1]["path"], "~");
    assert_eq!(chain[1]["value"], "ok");
    assert!(chain[1].get("answered").is_none());
    let levels =
        |m: &serde_json::Value| -> Vec<String> { lines(m).into_iter().map(|(l, _)| l).collect() };
    assert_eq!(levels(&two), ["warn", "info"]);

    // A read at -vvv asks for debug lines; the next run keeps them.
    let three = json(&get(&["s.v", "-vvv", "--force", "-f", "json"]).stderr);
    assert_eq!(levels(&three), ["warn", "info", "debug"]);
    assert_eq!(three["_"]["decay"], 4);
    assert!(three["_"]["poll"].is_object());
}

/// `text` metadata's rows, each row's AGE (its last cell) cut, as it
/// moves; the header and a log line kept whole.
fn rows(stderr: &[u8]) -> Vec<String> {
    String::from_utf8(stderr.to_vec())
        .unwrap()
        .lines()
        .map(|l| match l.contains('[') || l.starts_with("FIELD") {
            true => l.to_string(),
            false => l
                .rsplit_once(' ')
                .map_or(l, |(row, _)| row.trim_end())
                .to_string(),
        })
        .collect()
}

fn text_is_status_trees_rows_with_each_line_under_its_row(d: &TestDaemon, home: &std::path::Path) {
    d.comb(home)
        .args(["put", "s", r#"{"v":"P"}"#, "./proj"])
        .assert()
        .success();
    let get = |args: &[&str]| d.comb(home).arg("get").args(args).output().unwrap();
    let two = get(&["s.v", "-vv", "--ascii", "./proj"]);
    assert_eq!(
        rows(&two.stderr),
        [
            "FIELD        SOURCE        VALUE  PATH     AGE",
            "s.v          put           > P    ~/proj",
            "`- shadowed  source(main)  ok     ~",
            "     :       [warn] careful",
            "     :       [info] noted",
        ],
    );
    assert!(!two.stderr.contains(&0x1b), "no colour off a terminal");
    // -v: the field row alone, and the hidden Source's warning, naming it.
    assert_eq!(
        rows(&get(&["s.v", "-v", "./proj"]).stderr),
        [
            "FIELD  SOURCE  VALUE  PATH     AGE",
            "s.v    put     ▸ P    ~/proj",
            "  ┆    [warn] source(main) ~: careful",
        ],
    );
}

fn a_failure_hidden_today_is_an_error_line(d: &TestDaemon, home: &std::path::Path) {
    d.comb(home)
        .args(["get", "sf.v"])
        .assert()
        .success()
        .stdout("ok");
    std::fs::write(home.join("flag"), "").unwrap();
    let out = d
        .comb(home)
        .args(["get", "sf.v", "--force", "-v"])
        .output()
        .unwrap();
    assert_eq!(out.stdout, b"ok", "the last good value stands");
    assert_eq!(
        rows(&out.stderr),
        [
            "FIELD  SOURCE        VALUE  PATH   AGE",
            "sf.v   source(main)  ▸ ok   ~",
            "  ┆    [warn] careful",
            "  ┆    [error] data: no agent",
        ],
    );

    // A group leaf that failed is left out of the value, and said under
    // its own field row; one header over the group's leaves.
    let group = d.comb(home).args(["get", "t", "-v"]).output().unwrap();
    assert_eq!(group.stdout, b"x  r\n");
    let text = rows(&group.stderr);
    assert_eq!(text[0], "FIELD  SOURCE    VALUE  PATH  AGE");
    assert_eq!(text[1], "t.bad  rendered  null   ~");
    assert!(text[2].starts_with("  ┆    [error] t.bad: "), "{text:?}");
    assert!(text[2].ends_with("broken leaf"), "{text:?}");
    assert_eq!(text[3], "t.x    rendered  ▸ r    ~");
    assert_eq!(text[4], "  ┆    [warn] rendered warn");
    let tree = json(
        &d.comb(home)
            .args(["get", "t", "-v", "-f", "json"])
            .output()
            .unwrap()
            .stderr,
    );
    assert_line(&tree["bad"], "error", "broken leaf");
    assert_line(&tree["x"], "warn", "rendered warn");
}

fn several_keys_are_one_document_keyed_by_key(d: &TestDaemon, home: &std::path::Path) {
    let out = d
        .comb(home)
        .args(["get", "s.v", "t.x", "-v", "-f", "json"])
        .output()
        .unwrap();
    assert_eq!(json(&out.stdout), serde_json::json!(["ok", "r"]));
    let meta = json(&out.stderr);
    assert_eq!(meta["s.v"]["_"]["chain"][0]["layer"], "source(main)");
    assert_eq!(meta["t.x"]["_"]["chain"][0]["layer"], "template");
    assert_line(&meta["t.x"], "warn", "rendered warn");
}

fn a_vvv_read_that_starts_debug_capture_says_when_its_lines_come(
    d: &TestDaemon,
    home: &std::path::Path,
) {
    let get = |args: &[&str]| json(&d.comb(home).arg("get").args(args).output().unwrap().stderr);
    let hint = "debug lines are kept from now";
    // The stored run predates the ask, so it kept no debug line.
    d.comb(home).args(["get", "h.v"]).assert().success();
    assert_line(&get(&["h.v", "-vvv", "-f", "json"]), "info", hint);
    // Asked within the window, the read says nothing of it.
    let again = get(&["h.v", "-vvv", "-f", "json"]);
    assert!(
        !lines(&again).iter().any(|(_, m)| m.contains(hint)),
        "{again}"
    );
}

#[test]
fn debug_capture_lapses_once_no_read_asks_for_it() {
    let (d, home) = daemon();
    let get = |args: &[&str]| json(&d.comb(&home).arg("get").args(args).output().unwrap().stderr);
    let debug = |m: &serde_json::Value| lines(m).iter().any(|(l, _)| l == "debug");
    assert!(debug(&get(&["s.v", "-vvv", "--force", "-f", "json"])));

    // A run within the window still keeps them, a read at -vv not renewing it.
    d.advance(std::time::Duration::from_secs(6 * 60));
    get(&["s.v", "-vv", "--force", "-f", "json"]);
    d.advance(std::time::Duration::from_secs(5 * 60));
    get(&["s.v", "-vv", "--force", "-f", "json"]);
    assert!(debug(&get(&["s.v", "-vvv", "-f", "json"])));

    // Past it, a run keeps none: the stored run's lines, read at -vvv
    // without a run, have no debug line.
    d.advance(std::time::Duration::from_secs(13 * 60));
    get(&["s.v", "-vv", "--force", "-f", "json"]);
    assert!(!debug(&get(&["s.v", "-vvv", "-f", "json"])));
}

fn a_memo_hit_replays_the_lines_its_render_raised(d: &TestDaemon, home: &std::path::Path) {
    let get = |args: &[&str]| d.comb(home).arg("get").args(args).output().unwrap();
    // A read without -v renders, and stores its lines with the memo.
    assert_eq!(get(&["m.v"]).stdout, b"v");
    let hit = json(&get(&["m.v", "-v", "-f", "json"]).stderr);
    let chain = &hit["_"]["chain"];
    assert_eq!(chain[0]["layer"], "memo", "{hit}");
    assert_eq!(chain[0]["log"][0]["msg"], "computed", "{hit}");
    assert_eq!(chain[0]["log"][0]["origin"], "memo", "{hit}");
    // Under text the hit is the field row itself, its lines straight under it.
    let text = rows(&get(&["m.v", "-vv", "--ascii"]).stderr);
    assert!(text[1].starts_with("m.v "), "{text:?}");
    assert!(text[1].contains(" memo "), "{text:?}");
    assert!(text[2].starts_with("  :  "), "{text:?}");
    assert!(
        text.iter().any(|l| l.ends_with("[warn] computed")),
        "{text:?}"
    );
    assert!(
        text.iter()
            .all(|l| !l.starts_with("|-") && !l.starts_with("|  ")),
        "no memo layer row, no bar: {text:?}"
    );
}

fn a_memo_says_why_it_missed_or_hit(d: &TestDaemon, home: &std::path::Path) {
    let get = |args: &[&str]| json(&d.comb(home).arg("get").args(args).output().unwrap().stderr);
    let miss = get(&["m2.v", "-vv", "-f", "json"]);
    assert_line(&miss, "info", "memo miss: no memo");
    let hit = get(&["m2.v", "-vvv", "-f", "json"]);
    assert_line(&hit, "debug", "memo hit");
    // comb's reasons are the read's: a hit does not replay the miss's.
    assert!(
        !lines(&hit).iter().any(|(_, m)| m.contains("memo miss")),
        "{hit}"
    );
    let forced = get(&["m2.v", "-vv", "--force", "-f", "json"]);
    assert_line(&forced, "info", "--force renders past the memo");
}

fn a_read_of_a_disabled_or_unknown_provider_is_a_warning(d: &TestDaemon, home: &std::path::Path) {
    let get = |args: &[&str]| json(&d.comb(home).arg("get").args(args).output().unwrap().stderr);
    assert_line(
        &get(&["off.v", "-v", "-f", "json"]),
        "warn",
        "off is disabled",
    );
    assert_line(
        &get(&["sss.v", "-v", "-f", "json"]),
        "warn",
        "no provider is named 'sss'",
    );
    let plain = d.comb(home).args(["get", "sss.v"]).output().unwrap();
    assert!(plain.stderr.is_empty(), "only where a read asks");
}

fn a_put_says_what_it_hides_replaces_and_stops_keeping(d: &TestDaemon, home: &std::path::Path) {
    let put = |args: &[&str]| {
        let out = d.comb(home).arg("put").args(args).output().unwrap();
        assert!(out.status.success());
        String::from_utf8(out.stderr).unwrap()
    };
    d.comb(home).args(["get", "ps.v"]).assert().success();
    assert!(put(&["ps.v", r#""P""#]).contains("warning: ps.v: the put hides source(main) at ~"),);
    assert!(put(&["q", r#"{"a":1,"b":2}"#]).is_empty());
    let replaced = put(&["q", r#"{"a":1}"#]);
    assert!(
        replaced.contains("q: replaced, so these went: q.b"),
        "{replaced}"
    );
    put(&["q.a", "2", "--persist"]);
    let dropped = put(&["q.a", "3"]);
    assert!(dropped.contains("not --persist"), "{dropped}");
    let hidden = put(&["t.x", r#""p""#]);
    assert!(
        hidden.contains("a template answers here, over the put"),
        "{hidden}"
    );
}

fn status_and_check_show_a_sources_latest_warning(d: &TestDaemon, home: &std::path::Path) {
    d.comb(home).args(["get", "s.v"]).assert().success();
    let status = d.comb(home).arg("status").output().unwrap();
    let text = String::from_utf8(status.stderr).unwrap();
    assert!(
        text.contains("warning: s source(main) at ~: careful"),
        "{text}"
    );
    let json = d
        .comb(home)
        .args(["status", "-f", "json"])
        .output()
        .unwrap();
    let rows = json_rows(&json.stdout);
    assert!(rows.iter().any(|r| r["warn"] == "careful"), "{rows:?}");
    let check = d.comb(home).args(["check", "providers"]).output().unwrap();
    let text = String::from_utf8(check.stdout).unwrap();
    assert!(text.contains("s.main warned at ~: careful"), "{text}");
}

/// `status -f json`'s rows, one object per line.
fn json_rows(bytes: &[u8]) -> Vec<serde_json::Value> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}
