// Coverage for the text sub-protocol (`get_text` / `get_formatted` /
// `get_formatted_with_flags`) on both `Client` and `Session`.
//
// This is a second wire sub-protocol distinct from the one-JSON-line
// framing every other op uses: the daemon (`format_data` in src/server.rs)
// writes zero or more content lines followed by a blank-line terminator,
// and signals failure by prefixing only the *first* line `error: ` instead
// of rendering data.
//
// Exercised primarily against a real in-process daemon (DaemonGuard) rather
// than a scripted responder: a scripted server that shares the same
// (possibly wrong) assumption about the terminator as the client code being
// tested would pass even if that assumption is wrong, and getting the
// terminator wrong hangs a real client. The real daemon is the source of
// truth for the framing here.

mod common;
use common::daemon::DaemonGuard;

use libbeachcomber::{Client, ClientConfig, CombError};
use std::time::Duration;

fn client_for(sock: &std::path::Path) -> Client {
    Client::with_config(ClientConfig {
        timeout: Duration::from_secs(2),
        auto_start: false,
        exec: false,
    })
    .with_socket_path(sock.to_path_buf())
}

#[test]
fn client_get_text_renders_scalar_field() {
    let guard = DaemonGuard::spawn();
    let client = client_for(&guard.path);

    client
        .put("fmt_scalar", serde_json::json!({"x": 1}), None, None)
        .expect("put");

    let text = client.get_text("fmt_scalar.x", None).expect("get_text");
    assert_eq!(
        text, "1",
        "scalar text response must be trimmed of the blank terminator"
    );
}

#[test]
fn client_get_text_of_a_template_only_node_is_the_template_text() {
    let guard = DaemonGuard::spawn();
    let client = client_for(&guard.path);

    client
        .put(
            "fmt_tmpl:template",
            serde_json::json!({"t": "{{ 1 + 1 }}"}),
            None,
            None,
        )
        .expect("put");

    // Raw, like `get`: the template text itself, not a rendering and not a miss.
    let text = client.get_text("fmt_tmpl.t", None).expect("get_text");
    assert_eq!(text, "{{ 1 + 1 }}");
    let sh = client
        .get_formatted("fmt_tmpl.t", None, "sh")
        .expect("get_formatted");
    assert_eq!(sh, "'{{ 1 + 1 }}'");
}

#[test]
fn client_get_formatted_renders_whole_object_sorted() {
    let guard = DaemonGuard::spawn();
    let client = client_for(&guard.path);

    client
        .put(
            "fmt_obj",
            serde_json::json!({"z": "last", "a": "first"}),
            None,
            None,
        )
        .expect("put");

    let text = client
        .get_formatted("fmt_obj", None, "text")
        .expect("get_formatted");
    assert_eq!(
        text, "a=first\nz=last",
        "multi-field object renders as sorted key=value lines with no trailing blank line"
    );
}

#[test]
fn client_get_formatted_first_line_only_error_prefix_is_not_data_content() {
    // A data line that happens to start with "error:" must NOT be mistaken
    // for the wire error convention when it isn't the first line of the
    // response — only the response's first line carries that meaning.
    let guard = DaemonGuard::spawn();
    let client = client_for(&guard.path);

    client
        .put(
            "fmt_lookalike",
            serde_json::json!({"a": "first", "b": "error: not a real error"}),
            None,
            None,
        )
        .expect("put");

    let text = client
        .get_formatted("fmt_lookalike", None, "text")
        .expect("get_formatted must not treat a later data line as an error");
    assert_eq!(text, "a=first\nb=error: not a real error");
}

#[test]
fn client_get_formatted_sh_shell_quotes_every_value() {
    // Canon field_resolution.md invariant 8: `sh` shell-escapes. A value with
    // a space, a single quote and a `$` must come back POSIX-single-quoted so
    // the block is safe to `eval`; keys stay bare.
    let guard = DaemonGuard::spawn();
    let client = client_for(&guard.path);

    client
        .put(
            "fmt_sh_obj",
            serde_json::json!({"a": "it's a $HOME env", "b": 42}),
            None,
            None,
        )
        .expect("put");

    let sh = client
        .get_formatted("fmt_sh_obj", None, "sh")
        .expect("get_formatted");
    assert_eq!(sh, "a='it'\\''s a $HOME env'\nb='42'");

    // The same query under `text` is untouched.
    let text = client
        .get_formatted("fmt_sh_obj", None, "text")
        .expect("get_formatted");
    assert_eq!(text, "a=it's a $HOME env\nb=42");
}

#[test]
fn session_get_formatted_sh_shell_quotes_scalar() {
    let guard = DaemonGuard::spawn();
    let client = client_for(&guard.path);
    let mut session = client.session().expect("session");

    session
        .put(
            "fmt_sh_scalar",
            serde_json::json!({"x": "two words"}),
            None,
            None,
        )
        .expect("put");

    let sh = session
        .get_formatted_with_flags("fmt_sh_scalar.x", None, "sh", false, false)
        .expect("get_formatted_with_flags");
    assert_eq!(sh, "'two words'");
}

#[test]
fn client_get_formatted_reports_daemon_rejection() {
    let guard = DaemonGuard::spawn();
    let client = client_for(&guard.path);

    let result = client.get_formatted("no_such_provider.x", None, "text");

    match result {
        Err(CombError::ServerError(msg)) => {
            assert_eq!(
                msg, "unknown provider: no_such_provider",
                "the wire's `error: ` line prefix must be stripped, matching the bare \
                 message ServerError carries for the JSON-per-line ops"
            );
        }
        other => panic!("expected ServerError, got {other:?}"),
    }
}

#[test]
fn session_get_formatted_with_flags_forwards_force() {
    // Mirrors src/client.rs's ClientSession::get_formatted_with_flags contract:
    // Session must expose the same text sub-protocol as Client, since the CLI's
    // multi-key path issues one get_formatted_with_flags call per key on a
    // single shared session.
    let guard = DaemonGuard::spawn();
    let client = client_for(&guard.path);
    let mut session = client.session().expect("session");

    session
        .put(
            "fmt_session",
            serde_json::json!({"x": "seeded"}),
            None,
            None,
        )
        .expect("put");

    let text = session
        .get_formatted_with_flags("fmt_session.x", None, "text", false, false)
        .expect("get_formatted_with_flags");
    assert_eq!(text, "seeded");
}

#[test]
fn session_error_response_leaves_connection_usable_for_the_next_request() {
    // The CLI's multi-key loop (src/cli/commands/get.rs) keeps issuing
    // get_formatted_with_flags calls on the same session even after one key
    // comes back as a daemon rejection. The connection must not be left with
    // an unread trailing blank line from the error response, or the next
    // request's response would desync.
    let guard = DaemonGuard::spawn();
    let client = client_for(&guard.path);
    let mut session = client.session().expect("session");

    session
        .put("fmt_after_error", serde_json::json!({"x": 42}), None, None)
        .expect("put");

    let err = session.get_formatted_with_flags("bogus_provider.x", None, "text", false, false);
    assert!(matches!(err, Err(CombError::ServerError(_))));

    let text = session
        .get_formatted_with_flags("fmt_after_error.x", None, "text", false, false)
        .expect("session must still be usable after a prior error response");
    assert_eq!(text, "42");
}
