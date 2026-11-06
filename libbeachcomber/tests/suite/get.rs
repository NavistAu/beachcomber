// `Client::get` and `Session::get` are what `comb get` prints: the value
// `libbeachcomber::resolve` makes of the key, a template rendered, a miss
// `None`. The unrendered stored value is a facet address read through the
// same `get`. Text is `render::render_value` of that value.
//
// Exercised against a real in-process daemon (DaemonGuard).

use crate::common::client::client_for;
use crate::common::daemon::DaemonGuard;

use libbeachcomber::render::{Format, render_value};
use libbeachcomber::resolve::{FetchError, ResolveError};
use serde_json::json;

#[test]
fn client_get_answers_a_stored_value_and_none_on_a_miss() {
    let guard = DaemonGuard::spawn();
    let client = client_for(&guard.path);

    client
        .put("get_scalar", json!({"x": 1}), None, None, false)
        .expect("put");

    assert_eq!(client.get("get_scalar.x", None).unwrap(), Some(json!(1)));
    assert_eq!(
        client.get("get_scalar", None).unwrap(),
        Some(json!({"x": 1}))
    );
    assert_eq!(client.get("get_scalar.nope", None).unwrap(), None);
}

/// A template-only node is rendered, as `comb get` renders it — not handed
/// back as its source. The source is the facet's value.
#[test]
fn client_get_of_a_template_only_node_is_the_rendered_value() {
    let guard = DaemonGuard::spawn();
    let client = client_for(&guard.path);

    client
        .put(
            "get_tmpl:template",
            json!({"t": "{{ 1 + 1 }}"}),
            None,
            None,
            false,
        )
        .expect("put");

    assert_eq!(client.get("get_tmpl.t", None).unwrap(), Some(json!(2)));
    assert_eq!(
        client.get("get_tmpl:template.t", None).unwrap(),
        Some(json!("{{ 1 + 1 }}")),
        "the facet address is the stored template"
    );
}

/// A facet address reads the stored layer, not the rendered value: the
/// template over a stored value wins on `get`, the value is in `:put`.
#[test]
fn client_get_of_a_facet_is_the_stored_value() {
    let guard = DaemonGuard::spawn();
    let client = client_for(&guard.path);

    client
        .put("p", json!({"v": 1}), None, None, false)
        .expect("put");
    client
        .put("p:template.v", json!("{{ 2 }}"), None, None, false)
        .expect("put template over data");

    assert_eq!(client.get("p.v", None).unwrap(), Some(json!(2)));
    assert_eq!(client.get("p:put.v", None).unwrap(), Some(json!(1)));
}

#[test]
fn text_is_render_value_of_get() {
    let guard = DaemonGuard::spawn();
    let client = client_for(&guard.path);

    client
        .put(
            "get_obj",
            json!({"z": "last", "a": "it's $HOME"}),
            None,
            None,
            false,
        )
        .expect("put");

    let value = client.get("get_obj", None).unwrap();
    assert_eq!(
        render_value("get_obj", value.as_ref(), Format::Text),
        "a=it's $HOME\nz=last\n",
        "an object renders as sorted key=value lines"
    );
    assert_eq!(
        render_value("get_obj", value.as_ref(), Format::Sh),
        "a='it'\\''s $HOME'\nz='last'\n"
    );
}

#[test]
fn session_get_answers_the_value() {
    let guard = DaemonGuard::spawn();
    let client = client_for(&guard.path);
    let mut session = client.session().expect("session");

    session
        .put("get_session", json!({"x": "two words"}), None, None, false)
        .expect("put");
    session
        .put(
            "get_stmpl:template",
            json!({"t": "{{ 3 }}"}),
            None,
            None,
            false,
        )
        .expect("put");

    assert_eq!(
        session.get("get_session.x", None).unwrap(),
        Some(json!("two words"))
    );
    assert_eq!(session.get("get_stmpl.t", None).unwrap(), Some(json!(3)));
}

#[test]
fn client_get_reports_daemon_rejection() {
    let guard = DaemonGuard::spawn();
    let client = client_for(&guard.path);

    match client.get("a:b:c.x", None) {
        Err(ResolveError::Fetch(FetchError::Rejected(msg))) => assert_eq!(
            msg,
            "invalid facet key: a:b:c (a facet is template, macro, memo, put, source or cache)",
        ),
        other => panic!("expected a rejection, got {other:?}"),
    }
}

#[test]
fn session_error_leaves_connection_usable_for_the_next_request() {
    let guard = DaemonGuard::spawn();
    let client = client_for(&guard.path);
    let mut session = client.session().expect("session");

    session
        .put("get_after_error", json!({"x": 42}), None, None, false)
        .expect("put");

    let err = session.get("a:b:c.x", None);
    assert!(matches!(
        err,
        Err(ResolveError::Fetch(FetchError::Rejected(_)))
    ));

    assert_eq!(
        session.get("get_after_error.x", None).unwrap(),
        Some(json!(42)),
        "session must still be usable after a prior error response"
    );
}
