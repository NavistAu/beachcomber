// Coverage for the `format` parameter on `Client::watch` and the raw-line
// read path it requires on `WatchStream`.
//
// The daemon renders `text`/`sh` watch output the same way it renders
// `get --format text`: each event is written as a content line followed by
// a blank-line separator (see `format_data` in `src/server.rs`). A watch
// opened with `format: None` keeps the default JSON-per-line framing and
// stays readable via `next_event`; a watch opened with `Some("text")` (or
// `"sh"`) must be read via `next_line` instead, since the lines are no
// longer valid `WatchEvent` JSON.
//
// Exercised against a real in-process daemon (DaemonGuard) rather than a
// scripted responder, so the framing assumptions below are checked against
// the actual server-side renderer instead of a fixture that could bake in
// the same wrong assumption as the client code.

mod common;
use common::daemon::DaemonGuard;

use libbeachcomber::{Client, ClientConfig};
use std::time::Duration;

fn client_for(sock: &std::path::Path) -> Client {
    Client::with_config(ClientConfig {
        timeout: Duration::from_secs(2),
        auto_start: false,
    })
    .with_socket_path(sock.to_path_buf())
}

#[test]
fn watch_with_text_format_emits_raw_lines() {
    let guard = DaemonGuard::spawn();
    let client = client_for(&guard.path);

    client
        .put("watch_fmt_text", serde_json::json!({"x": 1}), None, None)
        .expect("put");

    let mut stream = client
        .watch("watch_fmt_text.x", None, Some("text"))
        .expect("watch");

    // The daemon writes "1\n\n" for this event — a content line, then a
    // blank separator line. next_line() must surface each as its own read.
    let first = stream
        .next_line()
        .expect("read line")
        .expect("non-empty stream");
    assert_eq!(first, "1\n");

    let second = stream
        .next_line()
        .expect("read separator line")
        .expect("non-empty stream");
    assert_eq!(
        second, "\n",
        "text format separates events with a blank line"
    );
}

#[test]
fn watch_without_format_still_reads_as_typed_json_events() {
    let guard = DaemonGuard::spawn();
    let client = client_for(&guard.path);

    client
        .put("watch_fmt_json", serde_json::json!({"x": 1}), None, None)
        .expect("put");

    // format: None must preserve the pre-existing JSON framing so next_event
    // keeps working unchanged.
    let mut stream = client.watch("watch_fmt_json.x", None, None).expect("watch");
    let event = stream
        .next_event()
        .expect("watch event")
        .expect("non-empty stream");
    let v = event.data.expect("event carries data");
    assert_eq!(v.get_i64("watch_fmt_json.x"), Some(1));
}
