//! The daemon's engine functions: `url` and `plugin`.
//!
//! [`daemon_engine_env`] is libbeachcomber's engine environment plus the two
//! functions only the daemon has. They live here, not in libbeachcomber, so
//! `ureq` and `libloading` stay out of every SDK's native library; a client
//! template calling either gets minijinja's unknown-function error.

use crate::boundaries::http::{HttpFetcher, UreqHttpFetcher};
use crate::boundaries::library::{LibraryLoader, LoadedLibrary};
use libbeachcomber::engine::{EngineCtx, build_engine_env, expand_home, op_error};
use minijinja::value::{Kwargs, Rest, Value as MjValue, ValueKind};
use minijinja::{Environment, Error};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The plugin libraries a daemon can call, found on a search path and loaded
/// once per resolved path for the daemon's life.
///
/// # The plugin author's contract
///
/// A plugin symbol is `extern "C" fn(*const c_char) -> *const c_char`. The
/// input is the call's arguments as a NUL-terminated JSON array. The output is
/// a NUL-terminated JSON value owned by the library, which must stay valid
/// until the next call into the same library; `NULL` means failure. The
/// function must not unwind: a panic out of an `extern "C" fn` aborts the
/// daemon.
pub struct PluginSet {
    search: Vec<PathBuf>,
    loaded: Mutex<HashMap<PathBuf, Arc<dyn LoadedLibrary>>>,
    loader: Arc<dyn LibraryLoader>,
}

impl PluginSet {
    pub fn new(search: Vec<PathBuf>, loader: Arc<dyn LibraryLoader>) -> Self {
        Self {
            search,
            loaded: Mutex::new(HashMap::new()),
            loader,
        }
    }

    /// Call `symbol` in the library `name` with `args` (a JSON array). A
    /// `name` containing `/` is the library path, which must be absolute;
    /// otherwise `lib<name>.<dylib|so>` is searched for on the search path, in
    /// order, the first directory holding it winning. Errors are complete
    /// `plugin(): ...` messages.
    pub fn call(
        &self,
        name: &str,
        symbol: &str,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let lib = self.library(name)?;
        let output = lib
            .call_json(symbol.to_string(), args.to_string())
            .map_err(|reason| format!("plugin(): {symbol} in {name}: {reason}"))?;
        serde_json::from_str(&output)
            .map_err(|e| format!("plugin(): {symbol} in {name}: returned invalid JSON: {e}"))
    }

    fn library(&self, name: &str) -> Result<Arc<dyn LoadedLibrary>, String> {
        let path = if name.contains('/') {
            let path = PathBuf::from(name);
            if !path.is_absolute() {
                return Err(format!("plugin(): a library path must be absolute: {name}"));
            }
            path
        } else {
            let file = format!("lib{name}.{LIBRARY_EXT}");
            self.search
                .iter()
                .map(|dir| dir.join(&file))
                .find(|candidate| candidate.is_file())
                .ok_or_else(|| {
                    let dirs: Vec<_> = self
                        .search
                        .iter()
                        .map(|d| d.display().to_string())
                        .collect();
                    format!(
                        "plugin(): {file} not found on plugin.path ({})",
                        dirs.join(", ")
                    )
                })?
        };
        let mut loaded = self.loaded.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(lib) = loaded.get(&path) {
            return Ok(lib.clone());
        }
        let lib: Arc<dyn LoadedLibrary> = Arc::from(
            self.loader
                .load(path.to_string_lossy().into_owned())
                .map_err(|e| format!("plugin(): {e}"))?,
        );
        loaded.insert(path, lib.clone());
        Ok(lib)
    }
}

const LIBRARY_EXT: &str = if cfg!(target_os = "macos") {
    "dylib"
} else {
    "so"
};

/// libbeachcomber's engine environment plus the daemon-only functions:
/// `url(address, method='GET', headers={}, body=none, timeout_ms=10000)` and
/// `plugin(name, symbol, args...)`.
///
/// Both block: `url` until the response or `timeout_ms`, `plugin` for as long
/// as the library takes. A render through this environment must run on a
/// blocking thread.
pub fn daemon_engine_env<'a>(ctx: Arc<EngineCtx>, plugins: Arc<PluginSet>) -> Environment<'a> {
    let mut jinja = build_engine_env(ctx.clone());
    jinja.add_function("url", url);
    jinja.add_function(
        "plugin",
        move |args: Rest<MjValue>, kwargs: Kwargs| -> Result<MjValue, Error> {
            kwargs.assert_all_used()?;
            let [name, symbol, rest @ ..] = args.as_slice() else {
                return Err(op_error("plugin(): needs a library name and a symbol"));
            };
            let (Some(name), Some(symbol)) = (name.as_str(), symbol.as_str()) else {
                return Err(op_error("plugin(): the name and symbol must be strings"));
            };
            let name = expand_home(&ctx, name).to_string_lossy().into_owned();
            let json_args = serde_json::to_value(rest)
                .map_err(|e| op_error(format!("plugin(): arguments are not JSON: {e}")))?;
            let result = plugins.call(&name, symbol, &json_args).map_err(op_error)?;
            Ok(MjValue::from_serialize(result))
        },
    );
    jinja
}

/// The address without userinfo, query or fragment: tokens like
/// `user:token@` or `?api_key=` must not reach the daemon's logs.
fn redacted(address: &str) -> String {
    let no_tail = address.split(['?', '#']).next().unwrap_or(address);
    let (scheme, rest) = match no_tail.split_once("://") {
        Some((scheme, rest)) => (format!("{scheme}://"), rest),
        None => (String::new(), no_tail),
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => rest.split_at(i),
        None => (rest, ""),
    };
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    format!("{scheme}{host}{path}")
}

/// `url(address, method='GET', headers={}, body=none, timeout_ms=10000)`: the
/// response body as text. `method` is GET, POST, PUT or PATCH; `body` (a
/// string) needs a method other than GET. A non-2xx status is an error naming
/// it.
fn url(address: MjValue, kwargs: Kwargs) -> Result<MjValue, Error> {
    let Some(address) = address.as_str() else {
        return Err(op_error("url(): the address must be a string"));
    };
    let shown = redacted(address);
    // Validated here so no fetcher error ever echoes the address: ureq's
    // "bad uri" text prints it, query and all.
    let absolute = ureq::http::Uri::try_from(address).is_ok_and(|uri| {
        matches!(uri.scheme_str(), Some("http" | "https")) && uri.host().is_some()
    });
    if !absolute {
        return Err(op_error(format!(
            "url(): {shown}: not an absolute http or https URL"
        )));
    }
    let method = match kwargs.get::<Option<MjValue>>("method")? {
        None => "GET".to_string(),
        Some(m) => match m.as_str() {
            Some(m @ ("GET" | "POST" | "PUT" | "PATCH")) => m.to_string(),
            _ => {
                return Err(op_error(format!(
                    "url(): method= must be one of GET, POST, PUT, PATCH, got {m}"
                )));
            }
        },
    };
    let headers = match kwargs.get::<Option<MjValue>>("headers")? {
        None => Vec::new(),
        Some(map) if map.kind() == ValueKind::Map => {
            let mut pairs = Vec::new();
            for key in map.try_iter()? {
                let name = key
                    .as_str()
                    .ok_or_else(|| op_error("url(): header names must be strings"))?
                    .to_string();
                let value = map.get_item(&key)?;
                let value = value.as_str().map(str::to_string).ok_or_else(|| {
                    op_error(format!("url(): header {name} must have a string value"))
                })?;
                pairs.push((name, value));
            }
            pairs
        }
        Some(_) => return Err(op_error("url(): headers= must be a map")),
    };
    let body = match kwargs.get::<Option<MjValue>>("body")? {
        None => None,
        Some(v) => match v.as_str() {
            Some(s) => Some(s.as_bytes().to_vec()),
            None => return Err(op_error("url(): body= must be a string")),
        },
    };
    if body.is_some() && method == "GET" {
        return Err(op_error("url(): body= needs method POST, PUT or PATCH"));
    }
    let timeout_ms = kwargs
        .get::<Option<u64>>("timeout_ms")
        .map_err(|e| {
            op_error(format!(
                "url(): timeout_ms= {}",
                e.detail().unwrap_or("invalid value")
            ))
        })?
        .unwrap_or(10_000);
    kwargs.assert_all_used()?;

    let response = UreqHttpFetcher
        .fetch(
            method,
            address.to_string(),
            headers,
            body,
            Duration::from_millis(timeout_ms),
        )
        .map_err(|e| op_error(format!("url(): {shown}: {e}")))?;
    if !(200..300).contains(&response.status) {
        return Err(op_error(format!(
            "url(): {shown}: status {}",
            response.status
        )));
    }
    String::from_utf8(response.body)
        .map(MjValue::from)
        .map_err(|_| op_error(format!("url(): {shown}: the body is not UTF-8 text")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boundaries::library::{MockLibraryLoader, MockLoadedLibrary};
    use mockito::Matcher;

    const EXT: &str = LIBRARY_EXT;

    fn render_with(
        ctx: EngineCtx,
        plugins: PluginSet,
        template: &str,
    ) -> Result<String, minijinja::Error> {
        let env = daemon_engine_env(Arc::new(ctx), Arc::new(plugins));
        env.render_str(template, ())
    }

    fn render(plugins: PluginSet, template: &str) -> Result<String, minijinja::Error> {
        render_with(EngineCtx::default(), plugins, template)
    }

    fn no_plugins() -> PluginSet {
        PluginSet::new(vec![], Arc::new(MockLibraryLoader::new()))
    }

    fn url_error(template: &str) -> String {
        render(no_plugins(), template).unwrap_err().to_string()
    }

    /// A loader that expects exactly one load of `path`, yielding a library
    /// whose `symbol` calls answer with `reply(input)`.
    fn loader_for(
        path: &str,
        symbol: &'static str,
        reply: fn(&str) -> Result<String, String>,
    ) -> MockLibraryLoader {
        let mut loader = MockLibraryLoader::new();
        let want = path.to_string();
        loader
            .expect_load()
            .withf(move |p| *p == want)
            .times(1)
            .returning(move |_| {
                let mut lib = MockLoadedLibrary::new();
                lib.expect_call_json()
                    .withf(move |s, _| s == symbol)
                    .returning(move |_, input| reply(&input));
                Ok(Box::new(lib))
            });
        loader
    }

    #[test]
    fn url_returns_the_body_text() {
        let mut server = mockito::Server::new();
        let _m = server
            .mock("GET", "/v")
            .with_status(200)
            .with_body(r#"{"a":1}"#)
            .create();
        let t = format!("{{{{ (url('{}/v') | fromjson).a }}}}", server.url());
        assert_eq!(render(no_plugins(), &t).unwrap(), "1");
    }

    #[test]
    fn url_non_2xx_is_an_error() {
        let mut server = mockito::Server::new();
        let _m = server.mock("GET", "/gone").with_status(503).create();
        let err = url_error(&format!("{{{{ url('{}/gone') }}}}", server.url()));
        assert!(err.contains("url():"), "{err}");
        assert!(err.contains("status 503"), "{err}");
    }

    #[test]
    fn url_other_non_2xx_names_the_status_through_the_same_path() {
        let mut server = mockito::Server::new();
        let _m = server.mock("GET", "/nf").with_status(404).create();
        let _n = server.mock("GET", "/nm").with_status(304).create();
        let err = url_error(&format!("{{{{ url('{}/nf') }}}}", server.url()));
        assert!(err.contains("status 404"), "{err}");
        let err = url_error(&format!("{{{{ url('{}/nm') }}}}", server.url()));
        assert!(err.contains("status 304"), "{err}");
    }

    #[test]
    fn url_errors_do_not_carry_the_query_string() {
        let mut server = mockito::Server::new();
        let _m = server
            .mock("GET", "/k")
            .match_query(Matcher::Any)
            .with_status(500)
            .create();
        let err = url_error(&format!(
            "{{{{ url('{}/k?api_key=hunter2#frag') }}}}",
            server.url()
        ));
        assert!(err.contains("/k:"), "{err}");
        assert!(
            !err.contains("hunter2") && !err.contains("api_key"),
            "{err}"
        );
    }

    #[test]
    fn url_post_sends_headers_and_body() {
        let mut server = mockito::Server::new();
        let m = server
            .mock("POST", "/p")
            .match_header("x-token", "abc")
            .match_body("payload")
            .with_status(200)
            .with_body("ok")
            .create();
        let t = format!(
            "{{{{ url('{}/p', method='POST', headers={{'x-token': 'abc'}}, body='payload') }}}}",
            server.url()
        );
        assert_eq!(render(no_plugins(), &t).unwrap(), "ok");
        m.assert();
    }

    #[test]
    fn url_unsupported_method_is_an_error() {
        let err = url_error("{{ url('http://x/', method='DELETE') }}");
        assert!(
            err.contains("url(): method= must be one of GET, POST, PUT, PATCH, got DELETE"),
            "{err}"
        );
        let err = url_error("{{ url('http://x/', method=1) }}");
        assert!(
            err.contains("must be one of GET, POST, PUT, PATCH"),
            "{err}"
        );
    }

    #[test]
    fn url_body_with_get_is_an_error() {
        let err = url_error("{{ url('http://x/', body='b') }}");
        assert!(
            err.contains("url(): body= needs method POST, PUT or PATCH"),
            "{err}"
        );
    }

    #[test]
    fn url_headers_must_be_a_map_of_strings() {
        let err = url_error("{{ url('http://x/', headers='a') }}");
        assert!(err.contains("url(): headers= must be a map"), "{err}");
        let err = url_error("{{ url('http://x/', headers={'a': 1}) }}");
        assert!(
            err.contains("url(): header a must have a string value"),
            "{err}"
        );
    }

    #[test]
    fn url_address_must_be_a_string() {
        let err = url_error("{{ url(data.missing) }}");
        assert!(err.contains("url(): the address must be a string"), "{err}");
    }

    #[test]
    fn url_times_out_against_a_server_that_never_replies() {
        // Connections complete in the listen backlog; nothing ever answers.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let t = format!(
            "{{{{ url('http://{}/', timeout_ms=50) }}}}",
            listener.local_addr().unwrap()
        );
        let err = url_error(&t);
        assert!(err.contains("url():"), "{err}");
        assert!(err.to_lowercase().contains("timeout"), "{err}");
    }

    #[test]
    fn url_errors_do_not_carry_userinfo() {
        // Nothing listens on port 1: the failure message must not echo the token.
        let err = url_error("{{ url('http://user:tok3n@127.0.0.1:1/p?k=s3cret') }}");
        assert!(err.contains("url(): http://127.0.0.1:1/p:"), "{err}");
        assert!(!err.contains("tok3n") && !err.contains("s3cret"), "{err}");
    }

    #[test]
    fn url_requires_an_absolute_http_or_https_url() {
        for address in [
            "/p?k=s3cret#frag",
            "/p?k=s3cret",
            "//host/p?k=s3cret",
            "host:80/p?k=s3cret",
            "ftp://host/p?k=s3cret",
            "http://bad host/p?k=s3cret",
        ] {
            let err = url_error(&format!("{{{{ url('{address}') }}}}"));
            assert!(
                err.contains("not an absolute http or https URL"),
                "{address}: {err}"
            );
            assert!(!err.contains("s3cret"), "{address}: {err}");
        }
    }

    #[test]
    fn plugin_by_name_searches_plugin_path() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let lib = second.path().join(format!("libfoo.{EXT}"));
        std::fs::write(&lib, b"").unwrap();
        let loader = loader_for(&lib.to_string_lossy(), "bc_x", |_| Ok("7".into()));
        let plugins = PluginSet::new(
            vec![first.path().to_path_buf(), second.path().to_path_buf()],
            Arc::new(loader),
        );
        assert_eq!(render(plugins, "{{ plugin('foo', 'bc_x') }}").unwrap(), "7");
    }

    #[test]
    fn plugin_first_search_directory_wins() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        for dir in [&first, &second] {
            std::fs::write(dir.path().join(format!("libdup.{EXT}")), b"").unwrap();
        }
        let winner = first.path().join(format!("libdup.{EXT}"));
        let loader = loader_for(&winner.to_string_lossy(), "bc_x", |_| Ok("1".into()));
        let plugins = PluginSet::new(
            vec![first.path().to_path_buf(), second.path().to_path_buf()],
            Arc::new(loader),
        );
        assert_eq!(render(plugins, "{{ plugin('dup', 'bc_x') }}").unwrap(), "1");
    }

    #[test]
    fn plugin_full_path_is_used_as_given() {
        let loader = loader_for("/opt/odd/libbar.so", "bc_x", |_| Ok("\"hi\"".into()));
        let plugins = PluginSet::new(vec![], Arc::new(loader));
        assert_eq!(
            render(plugins, "{{ plugin('/opt/odd/libbar.so', 'bc_x') }}").unwrap(),
            "hi"
        );
    }

    #[test]
    fn plugin_relative_path_is_the_absolute_path_error() {
        let err = render(no_plugins(), "{{ plugin('rel/libx.so', 'bc_x') }}")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("plugin(): a library path must be absolute: rel/libx.so"),
            "{err}"
        );
    }

    #[test]
    fn plugin_tilde_expands_from_the_render_home() {
        let loader = loader_for("/home/u/p/libx.so", "bc_x", |_| Ok("3".into()));
        let ctx = EngineCtx {
            env: HashMap::from([("HOME".to_string(), "/home/u".to_string())]),
            ..Default::default()
        };
        let plugins = PluginSet::new(vec![], Arc::new(loader));
        assert_eq!(
            render_with(ctx, plugins, "{{ plugin('~/p/libx.so', 'bc_x') }}").unwrap(),
            "3"
        );
    }

    #[test]
    fn plugin_called_twice_loads_the_library_once() {
        let loader = loader_for("/p/libtwice.so", "bc_x", |_| Ok("1".into()));
        let plugins = PluginSet::new(vec![], Arc::new(loader));
        let t = "{{ plugin('/p/libtwice.so', 'bc_x') }}{{ plugin('/p/libtwice.so', 'bc_x') }}";
        assert_eq!(render(plugins, t).unwrap(), "11");
    }

    #[test]
    fn plugin_missing_library_is_the_named_error() {
        let dir = tempfile::tempdir().unwrap();
        let plugins = PluginSet::new(
            vec![dir.path().to_path_buf()],
            Arc::new(MockLibraryLoader::new()),
        );
        let err = render(plugins, "{{ plugin('nope', 'bc_x') }}").unwrap_err();
        let want = format!(
            "plugin(): libnope.{EXT} not found on plugin.path ({})",
            dir.path().display()
        );
        assert!(err.to_string().contains(&want), "{err}");
    }

    #[test]
    fn plugin_needs_a_name_and_a_symbol() {
        let err = render(no_plugins(), "{{ plugin('/p/libx.so') }}").unwrap_err();
        assert!(
            err.to_string()
                .contains("plugin(): needs a library name and a symbol"),
            "{err}"
        );
    }

    #[test]
    fn plugin_passes_args_as_a_json_array() {
        let loader = loader_for("/p/libecho.so", "bc_echo_args", |input| {
            Ok(input.to_string())
        });
        let plugins = PluginSet::new(vec![], Arc::new(loader));
        let out = render(
            plugins,
            "{% set r = plugin('/p/libecho.so', 'bc_echo_args', 1, 'a') %}{{ r | length }}:{{ r[0] }}:{{ r[1] }}",
        )
        .unwrap();
        assert_eq!(out, "2:1:a");
    }

    #[test]
    fn plugin_failure_names_symbol_library_and_reason() {
        let loader = loader_for("/p/libnull.so", "bc_x", |_| Err("returned NULL".into()));
        let plugins = PluginSet::new(vec![], Arc::new(loader));
        let err = render(plugins, "{{ plugin('/p/libnull.so', 'bc_x') }}").unwrap_err();
        assert!(
            err.to_string()
                .contains("plugin(): bc_x in /p/libnull.so: returned NULL"),
            "{err}"
        );
    }

    #[test]
    fn plugin_invalid_json_is_an_error() {
        let loader = loader_for("/p/libbad.so", "bc_x", |_| Ok("{nope".into()));
        let plugins = PluginSet::new(vec![], Arc::new(loader));
        let err = render(plugins, "{{ plugin('/p/libbad.so', 'bc_x') }}").unwrap_err();
        assert!(err.to_string().contains("returned invalid JSON"), "{err}");
    }
}
