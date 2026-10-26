//! `comb get -f json` and `bc_get` answer identically (design
//! `2026-09-05-one-tree-resolution-design.md` §10 "FFI and SDKs", outcome 24).
//!
//! Both surfaces are clients of `libbeachcomber::resolve`; this is the test
//! that keeps them there. One daemon, one cwd, one environment, and a fixed
//! key list covering every row of the resolver's table — plain fields,
//! templated leaves and groups, a cached leaf (one surface stores its memo,
//! the other is served it), a string template walked into by key and by
//! index, misses, `env.*`, a cache facet, and the four error shapes. Each
//! key is classified on both surfaces as a hit (with its value), a miss, or an
//! error, and the classes and values must agree. Every disagreement is
//! collected and reported at once, so one run shows the whole picture.
//!
//! The CLI side runs the built `comb` binary from the cwd with the environment
//! map as its *entire* environment (plus the five variables it needs to reach
//! the isolated daemon); the FFI side calls the `extern "C"` functions in
//! process with the same map as `env_json`. Nothing ambient reaches one
//! surface and not the other.

use crate::common::daemon::{TestDaemon, isolate};
use libbeachcomber_ffi::envelope::bc_string_free;
use libbeachcomber_ffi::{BcClient, bc_client_free, bc_client_new, bc_eval, bc_get, bc_hello};
use serde_json::{Value, json};
use std::ffi::{CStr, CString, c_char};
use std::path::Path;
use std::process::Command;

/// The one environment both surfaces see — the fixed map spec §10 names.
const ENV: &[(&str, &str)] = &[("SCANLINE_FOO", "hi"), ("PYENV_VERSION", "3.12"), ("A", "")];

/// What a surface answered for one key.
#[derive(Debug, Clone, PartialEq)]
enum Class {
    Hit(Value),
    Miss,
    /// The surface's own message — the CLI's stderr, the FFI's `error.message`.
    Error(String),
}

impl Class {
    fn name(&self) -> &'static str {
        match self {
            Class::Hit(_) => "hit",
            Class::Miss => "miss",
            Class::Error(_) => "error",
        }
    }
}

/// What both surfaces must answer for a key: the class, and for a hit whose
/// value is fixed by the setup, the value. `Hit(None)` is a hit whose value
/// depends on the machine (a hostname, a rendered object carrying one, an
/// age) and is only compared across the surfaces.
#[derive(Debug, Clone, Copy)]
enum Expect {
    Hit(Option<&'static str>),
    Miss,
    Error,
}

/// `(key, expectation)`.
const KEYS: &[(&str, Expect)] = &[
    ("scanline.widgets.foo", Expect::Hit(Some(r#""hi""#))),
    // A cached leaf: the CLI's read renders and stores it, `bc_get`'s is
    // served the memo, and the two agree.
    ("scanline.cached", Expect::Hit(Some(r#""HI""#))),
    ("scanline.widgets.bar", Expect::Hit(None)),
    ("scanline.widgets", Expect::Hit(None)),
    ("scanline", Expect::Hit(None)),
    ("scanline.lit", Expect::Hit(Some("1"))),
    ("scanline.stats.cpu", Expect::Hit(Some("40"))),
    ("scanline.list.1", Expect::Hit(Some(r#""b""#))),
    ("scanline.stats.nope", Expect::Miss),
    ("scanline.nope", Expect::Miss),
    ("python.version", Expect::Hit(Some(r#""3.12""#))),
    ("python", Expect::Hit(None)),
    ("hostname.name", Expect::Hit(None)),
    ("hostname", Expect::Hit(None)),
    // The template wins over the cached branch; `main!` is a value the
    // repository itself can never hold, so a dropped template half shows.
    ("git.branch", Expect::Hit(Some(r#""main!""#))),
    ("git", Expect::Hit(None)),
    ("git.lines_added", Expect::Hit(Some("0"))),
    ("env.SCANLINE_FOO", Expect::Hit(Some(r#""hi""#))),
    ("env.NOPE", Expect::Miss),
    // Compared as a class only: both are a hit, the ages differ.
    ("git:cache.branch", Expect::Hit(None)),
    ("a:b:c.x", Expect::Error),
    ("nope.x", Expect::Miss),
    ("err.a", Expect::Error),
    ("err.bad", Expect::Error),
    ("err", Expect::Error),
];

/// The scalar hits `comb eval '{{ key }}'` is checked against `bc_eval` for.
const EVAL_KEYS: &[&str] = &[
    "scanline.widgets.foo",
    "scanline.cached",
    "scanline.lit",
    "python.version",
    "hostname.name",
    "git.branch",
    "env.SCANLINE_FOO",
];

// ── the CLI surface ──────────────────────────────────────────────────────────

/// The built `comb`, run from `cwd` with exactly `ENV` plus what it needs to
/// find the isolated daemon and nothing else from this process.
fn comb(daemon: &TestDaemon, cwd: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_comb"));
    cmd.env_clear();
    isolate(&mut cmd, &daemon.home, &daemon.xdg, &daemon.socket.path)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .envs(ENV.iter().copied())
        .current_dir(cwd);
    cmd
}

fn cli_put(daemon: &TestDaemon, cwd: &Path, key: &str, data: &str) {
    let out = comb(daemon, cwd)
        .args(["put", key, data])
        .output()
        .expect("run comb put");
    assert!(
        out.status.success(),
        "comb put {key}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn cli_get(daemon: &TestDaemon, cwd: &Path, key: &str) -> Class {
    let out = comb(daemon, cwd)
        .args(["get", "-f", "json", key])
        .output()
        .expect("run comb get");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    match out.status.code() {
        Some(0) if stdout == "null\n" => {
            assert!(stderr.is_empty(), "a miss is not an error, got {stderr:?}");
            Class::Miss
        }
        Some(0) => Class::Hit(
            serde_json::from_str(&stdout)
                .unwrap_or_else(|e| panic!("comb get -f json {key}: not JSON ({e}): {stdout:?}")),
        ),
        Some(2) => Class::Error(stderr.trim_end().to_string()),
        code => panic!("comb get {key}: unexpected exit {code:?}; stderr {stderr:?}"),
    }
}

/// `comb eval` prints text: a hit's rendering, or an error on stderr.
fn cli_eval(daemon: &TestDaemon, cwd: &Path, src: &str) -> Result<String, String> {
    let out = comb(daemon, cwd)
        .args(["eval", src])
        .output()
        .expect("run comb eval");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    match out.status.code() {
        Some(0) => Ok(stdout),
        Some(2) => Err(stderr.trim_end().to_string()),
        code => panic!("comb eval {src}: unexpected exit {code:?}; stderr {stderr:?}"),
    }
}

// ── the FFI surface ──────────────────────────────────────────────────────────

fn cs(s: &str) -> CString {
    CString::new(s).unwrap()
}

/// Take ownership of an envelope the ABI returned, free it, parse it.
fn envelope(ptr: *mut c_char) -> Value {
    assert!(!ptr.is_null(), "call_ffi must never return NULL");
    let body = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap().to_string();
    unsafe { bc_string_free(ptr) };
    serde_json::from_str(&body).expect("envelope must be valid JSON")
}

/// `exec` on, as `comb` has it: the inputs to the two resolutions are then
/// identical in every field, not only the ones this key list exercises.
fn ffi_client(daemon: &TestDaemon) -> *mut BcClient {
    let options = json!({
        "socket_path": daemon.socket.path,
        "autostart": false,
        "exec": true,
    });
    let options = cs(&options.to_string());
    unsafe { bc_client_new(options.as_ptr()) }
}

fn env_json() -> String {
    let map: serde_json::Map<String, Value> = ENV
        .iter()
        .map(|(k, v)| ((*k).to_string(), Value::String((*v).to_string())))
        .collect();
    Value::Object(map).to_string()
}

fn classify_envelope(v: Value) -> Class {
    if v["ok"] == json!(true) {
        match &v["data"] {
            Value::Null => Class::Miss,
            data => Class::Hit(data.clone()),
        }
    } else {
        Class::Error(format!(
            "{}: {}",
            v["error"]["kind"].as_str().unwrap_or_default(),
            v["error"]["message"].as_str().unwrap_or_default()
        ))
    }
}

fn ffi_get(client: *mut BcClient, key: &str, cwd: &str, env: &str) -> Class {
    let key = cs(key);
    let cwd = cs(cwd);
    let env = cs(env);
    classify_envelope(envelope(unsafe {
        bc_get(
            client,
            key.as_ptr(),
            cwd.as_ptr(),
            env.as_ptr(),
            std::ptr::null(),
        )
    }))
}

fn ffi_eval(client: *mut BcClient, src: &str, cwd: &str, env: &str) -> Result<Value, String> {
    let src = cs(src);
    let cwd = cs(cwd);
    let env = cs(env);
    match classify_envelope(envelope(unsafe {
        bc_eval(client, src.as_ptr(), cwd.as_ptr(), env.as_ptr())
    })) {
        Class::Hit(v) => Ok(v),
        Class::Miss => Ok(Value::Null),
        Class::Error(e) => Err(e),
    }
}

/// How `comb eval` prints a scalar: a string bare, anything else as its JSON
/// text.
fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

// ── the test ─────────────────────────────────────────────────────────────────

#[test]
fn comb_get_json_and_bc_get_answer_identically() {
    let daemon = TestDaemon::spawn();

    // The cwd: a repository under the daemon's `$HOME`, so the `git.*` keys
    // have a work tree and the `put`s below land on a path the chain reaches.
    // `GitRepoFixture` owns a tempdir of its own, outside `$HOME`, so the
    // repository is made here with its recipe instead.
    let cwd = daemon.home.join("ws").join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    crate::common::git::git(&cwd, &["init", "--initial-branch=main"]);
    std::fs::write(cwd.join("README.md"), "# test").unwrap();
    crate::common::git::git(&cwd, &["add", "."]);
    crate::common::git::git(&cwd, &["commit", "-m", "init"]);
    let cwd = cwd.canonicalize().unwrap();
    let cwd_str = cwd.to_str().unwrap();

    // Setup, through `comb put` from the cwd.
    cli_put(
        &daemon,
        &cwd,
        "scanline:template",
        r#"{"theme":"{{ env.SCANLINE_THEME or \"dark\" }}","widgets":{"foo":"{{ env.SCANLINE_FOO }}","bar":"{{ comb.hostname.name | truncate(3) }}"},"stats":"{{ {\"cpu\": 40, \"mem\": 12} }}","list":"{{ [\"a\",\"b\"] }}","cached":"{{ env.SCANLINE_FOO | cache() | upper }}"}"#,
    );
    cli_put(&daemon, &cwd, "scanline", r#"{"lit":1}"#);
    cli_put(
        &daemon,
        &cwd,
        "git:template.branch",
        r#""{{ \"main\" ~ \"!\" }}""#,
    );
    cli_put(
        &daemon,
        &cwd,
        "err:template",
        r#"{"a":"{{ err.b }}","b":"{{ err.a }}","bad":"{{ err.bad }}"}"#,
    );

    let client = ffi_client(&daemon);
    // A dead transport must fail here, naming the socket — not 24 keys later
    // as a nextest timeout with no table.
    let hello = envelope(unsafe { bc_hello(client) });
    assert_eq!(
        hello["ok"],
        Value::Bool(true),
        "the FFI client cannot reach the daemon at {}: {hello}",
        daemon.socket.path.display()
    );
    let env = env_json();
    let mut report: Vec<String> = Vec::new();
    let mut table: Vec<String> = Vec::new();
    let mut cli_hits: Vec<(&str, Value)> = Vec::new();

    for (key, expect) in KEYS {
        let cli = cli_get(&daemon, &cwd, key);
        let ffi = ffi_get(client, key, cwd_str, &env);
        table.push(format!(
            "{key:24} cli={:5} ffi={:5}",
            cli.name(),
            ffi.name()
        ));

        // Both surfaces must produce the expected class — an agreement of two
        // failures would prove nothing.
        let expected = match expect {
            Expect::Hit(_) => "hit",
            Expect::Miss => "miss",
            Expect::Error => "error",
        };
        for (surface, class) in [("cli", &cli), ("ffi", &ffi)] {
            if class.name() != expected {
                report.push(format!(
                    "{key}: {surface} is {} ({class:?}), expected {expected}",
                    class.name()
                ));
            }
        }

        match (&cli, &ffi) {
            (Class::Hit(c), Class::Hit(f)) => {
                if key.contains(":cache") {
                    // Two reads a moment apart differ in age, so ages are
                    // compared for presence only: a stored record carries
                    // one, a template answer none. The rest must agree.
                    let without_age = |v: &Value| {
                        let mut v = v.clone();
                        if let Value::Object(map) = &mut v {
                            map.remove("age_ms");
                        }
                        v
                    };
                    if c["age_ms"].is_number() != f["age_ms"].is_number()
                        || without_age(c) != without_age(f)
                    {
                        report.push(format!("{key}: cache answers differ; cli={c} ffi={f}"));
                    }
                } else if c != f {
                    report.push(format!(
                        "{key}: values differ\n    cli: {}\n    ffi: {}",
                        serde_json::to_string(c).unwrap(),
                        serde_json::to_string(f).unwrap()
                    ));
                }
                if let Expect::Hit(Some(want)) = expect {
                    let want: Value = serde_json::from_str(want).unwrap();
                    if c != &want {
                        report.push(format!("{key}: cli value {c}, expected {want}"));
                    }
                    if f != &want {
                        report.push(format!("{key}: ffi value {f}, expected {want}"));
                    }
                }
                cli_hits.push((key, c.clone()));
            }
            (Class::Miss, Class::Miss) => {}
            (Class::Error(_), Class::Error(_)) => {}
            (c, f) => report.push(format!(
                "{key}: classes differ\n    cli: {c:?}\n    ffi: {f:?}"
            )),
        }
    }

    // `comb eval '{{ key }}'` prints text; `bc_eval` returns the typed value.
    // The text must be the value rendered as `comb eval` renders a scalar,
    // and the value must be the one `comb get` gave for the same key.
    for key in EVAL_KEYS {
        let src = format!("{{{{ {key} }}}}");
        let cli = cli_eval(&daemon, &cwd, &src);
        let ffi = ffi_eval(client, &src, cwd_str, &env);
        match (&cli, &ffi) {
            (Ok(text), Ok(value)) => {
                if *text != text_of(value) {
                    report.push(format!(
                        "eval {src}: cli printed {text:?}, ffi value {value} renders {:?}",
                        text_of(value)
                    ));
                }
                match cli_hits.iter().find(|(k, _)| k == key) {
                    Some((_, got)) if got != value => report.push(format!(
                        "eval {src}: bc_eval gave {value}, comb get gave {got}"
                    )),
                    Some(_) => {}
                    None => report.push(format!("eval {src}: comb get had no hit for {key}")),
                }
            }
            (c, f) => report.push(format!(
                "eval {src}: outcomes differ\n    cli: {c:?}\n    ffi: {f:?}"
            )),
        }
    }

    unsafe { bc_client_free(client) };

    eprintln!("observed classes:\n  {}", table.join("\n  "));
    assert!(
        report.is_empty(),
        "{} disagreement(s) between comb get -f json and bc_get:\n  {}",
        report.len(),
        report.join("\n  ")
    );
}
