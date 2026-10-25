//! The daemon's engine functions: `url` and `plugin`.
//!
//! [`daemon_engine_env`] is libbeachcomber's engine environment plus the two
//! functions only the daemon has. They live here, not in libbeachcomber, so
//! `ureq` and `libloading` stay out of every SDK's native library; a client
//! template calling either is refused by libbeachcomber's engine environment
//! (`url() runs only in a Source's lines`).

use crate::boundaries::http;
use crate::boundaries::library::{LibraryLoader, LoadedLibrary};
use libbeachcomber::engine::{EngineCtx, build_engine_env, build_pure_engine_env, op_error};
use libbeachcomber::library::data::merge_values;
use libbeachcomber::path::expand_tilde;
use minijinja::value::{Enumerator, Kwargs, Object, ObjectRepr, Rest, Value as MjValue, ValueKind};
use minijinja::{Environment, Error, State};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

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
    state: Mutex<PluginState>,
    loader: Arc<dyn LibraryLoader>,
}

#[derive(Default)]
struct PluginState {
    /// Each name or library path that has loaded, to its library, for the
    /// daemon's life: it is never looked up on disk again, so a plugin rebuilt
    /// while the daemon runs is picked up at its next start.
    by_name: HashMap<String, Arc<dyn LoadedLibrary>>,
    /// The loaded images, by the loader's identity for them: `dlopen` hands
    /// back one image for a file it already holds, so every name that reaches
    /// it shares one library and the one lock that serialises calls into it.
    by_image: HashMap<usize, Arc<dyn LoadedLibrary>>,
}

impl PluginSet {
    pub fn new(search: Vec<PathBuf>, loader: Arc<dyn LibraryLoader>) -> Self {
        Self {
            search,
            state: Mutex::new(PluginState::default()),
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

    /// Take over the libraries `from` has loaded. The set is `Arc`s of loaded
    /// images, so a library one set let go of stays open here: a reload
    /// replaces the set the Sources call through without ever closing a
    /// library, or loading one a second time.
    pub fn adopt_loaded(&self, from: &PluginSet) {
        let theirs = from.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut mine = self.state.lock().unwrap_or_else(|e| e.into_inner());
        for (name, lib) in &theirs.by_name {
            mine.by_name.insert(name.clone(), lib.clone());
        }
        for (image, lib) in &theirs.by_image {
            mine.by_image.insert(*image, lib.clone());
        }
    }

    fn library(&self, name: &str) -> Result<Arc<dyn LoadedLibrary>, String> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(lib) = state.by_name.get(name) {
            return Ok(lib.clone());
        }
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
                        "plugin(): {file} not found on [daemon.plugin] path ({})",
                        dirs.join(", ")
                    )
                })?
        };
        let loaded = self
            .loader
            .load(path.to_string_lossy().into_owned())
            .map_err(|e| format!("plugin(): {e}"))?;
        let image = loaded.identity();
        let lib = match state.by_image.get(&image) {
            // Another name already holds this image; the handle just taken is
            // a second reference to it, released here.
            Some(lib) => lib.clone(),
            None => {
                let lib: Arc<dyn LoadedLibrary> = Arc::from(loaded);
                state.by_image.insert(image, lib.clone());
                lib
            }
        };
        state.by_name.insert(name.to_string(), lib.clone());
        Ok(lib)
    }
}

const LIBRARY_EXT: &str = if cfg!(target_os = "macos") {
    "dylib"
} else {
    "so"
};

/// libbeachcomber's engine environment plus the daemon-only functions:
/// `url(address, method='GET', headers={}, body=none, timeout='10s')` and
/// `plugin(name, symbol, args...)`.
///
/// Both block: `url` until the response or `timeout`, `plugin` for as long
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
            let name = expand_tilde(name, ctx.env.get("HOME").map(String::as_str))
                .map_err(|e| op_error(format!("plugin(): {e}")))?
                .to_string_lossy()
                .into_owned();
            let json_args = serde_json::to_value(rest)
                .map_err(|e| op_error(format!("plugin(): arguments are not JSON: {e}")))?;
            let result = plugins.call(&name, symbol, &json_args).map_err(op_error)?;
            Ok(MjValue::from_serialize(result))
        },
    );
    jinja
}

/// The engine environment a provider's `template`, `result` and `return` lines
/// render in: libbeachcomber's pure library (the tier table's `Pure` items), plus
/// [`facet`] and a `merge` that reads a provider's [`Sources`]. There is no
/// `exec`, `read`, `lup`, `url` or `plugin`: these lines run on a read, which
/// must not wait on a process, a file, the network or a library.
pub fn provider_engine_env<'a>(ctx: Arc<EngineCtx>) -> Environment<'a> {
    let mut jinja = build_pure_engine_env(ctx);
    libbeachcomber::eval::render_none_as_empty(&mut jinja);
    jinja.add_filter("facet", facet);
    jinja.add_function(
        "merge",
        |state: &State, args: Rest<MjValue>| -> Result<MjValue, Error> {
            let called;
            let operands: &[MjValue] = match args.as_slice() {
                [only] if only.downcast_object_ref::<Sources>().is_some() => {
                    let sources = only.downcast_object_ref::<Sources>().expect("checked");
                    called = sources.call_all(state)?;
                    &called
                }
                [list] if list.kind() == ValueKind::Seq => {
                    called = list.try_iter()?.collect();
                    &called
                }
                all => all,
            };
            // A Source with nothing at this entry contributes nothing:
            // `merge_values` skips it.
            merge_values(operands.iter().cloned())
        },
    );
    jinja
}

/// What `facet(name, obj)` writes into: the facets a line attached, in order.
/// A render holds one in its context, under a name no line can write.
#[derive(Debug, Default)]
pub struct FacetSink {
    facets: Mutex<Vec<(String, serde_json::Value)>>,
}

impl FacetSink {
    /// The facets attached so far, in order; the last of a name is its value.
    pub fn take(&self) -> Vec<(String, serde_json::Value)> {
        std::mem::take(&mut self.facets.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

impl Object for FacetSink {
    fn repr(self: &Arc<Self>) -> ObjectRepr {
        ObjectRepr::Plain
    }
}

/// The context name a render's [`FacetSink`] is bound to.
pub const FACET_SINK: &str = "facets__";

/// Why `name` cannot be the name `facet()` attaches, or `None` when it is one
/// of the closed set. A literal name is checked at load too.
pub fn facet_name_error(name: &str) -> Option<String> {
    libbeachcomber::key::Facet::from_name(name)
        .is_none()
        .then(|| {
            format!(
                "facet(): '{name}' is not a facet ({})",
                libbeachcomber::key::Facet::LIST
            )
        })
}

/// `result | facet(name, obj)`: hands `result` on and attaches `obj` as the
/// facet `name` beside it. An `obj` that is none attaches nothing. `name` is
/// one of the facets; only `template` is read off a `return`.
fn facet(state: &State, input: MjValue, name: String, obj: MjValue) -> Result<MjValue, Error> {
    if let Some(message) = facet_name_error(&name) {
        return Err(op_error(message));
    }
    let sink = state.lookup(FACET_SINK);
    let Some(sink) = sink
        .as_ref()
        .and_then(|s| s.downcast_object_ref::<FacetSink>())
    else {
        return Err(op_error(
            "facet(): there is nothing here to attach a facet to",
        ));
    };
    if !(obj.is_undefined() || obj.is_none()) {
        let json = serde_json::to_value(&obj)
            .map_err(|e| op_error(format!("facet(): the {name} facet is not JSON: {e}")))?;
        sink.facets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((name, json));
    }
    Ok(input)
}

/// `sources` in a provider's `result` line: a map of callables over the
/// provider's own Sources, in declaration order. `sources.head()` is that
/// Source's stored record at the entry being read, or none when it has none.
/// A call never executes anything: the read has already brought the Sources up
/// to date.
#[derive(Debug)]
pub struct Sources {
    calls: Vec<(String, Arc<SourceCall>)>,
}

impl Sources {
    /// `(name, record)` per Source, in declaration order; none for a Source
    /// with nothing at the entry.
    pub fn new(records: Vec<(String, Option<serde_json::Value>)>) -> Self {
        Self {
            calls: records
                .into_iter()
                .map(|(name, record)| {
                    let record = record.map_or_else(|| MjValue::from(()), MjValue::from_serialize);
                    (name, Arc::new(SourceCall { record }))
                })
                .collect(),
        }
    }

    /// Every Source called once, in declaration order.
    fn call_all(&self, state: &State) -> Result<Vec<MjValue>, Error> {
        self.calls
            .iter()
            .map(|(_, call)| call.call(state, &[]))
            .collect()
    }
}

impl Object for Sources {
    fn repr(self: &Arc<Self>) -> ObjectRepr {
        ObjectRepr::Map
    }

    fn get_value(self: &Arc<Self>, key: &MjValue) -> Option<MjValue> {
        let key = key.as_str()?;
        self.calls
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, call)| MjValue::from_dyn_object(call.clone()))
    }

    fn enumerate(self: &Arc<Self>) -> Enumerator {
        Enumerator::Values(
            self.calls
                .iter()
                .map(|(n, _)| MjValue::from(n.as_str()))
                .collect(),
        )
    }
}

/// One Source of [`Sources`], callable.
#[derive(Debug)]
struct SourceCall {
    record: MjValue,
}

impl Object for SourceCall {
    fn repr(self: &Arc<Self>) -> ObjectRepr {
        ObjectRepr::Plain
    }

    fn call(self: &Arc<Self>, _state: &State<'_, '_>, args: &[MjValue]) -> Result<MjValue, Error> {
        if !args.is_empty() {
            return Err(op_error("a Source in sources takes no arguments"));
        }
        Ok(self.record.clone())
    }
}

/// What a call to a Source gives: its record, none when it has nothing, or why
/// it could not be read.
pub type CallResult = Result<Option<serde_json::Value>, String>;

/// What answers a call to a Source: `(provider, source)` to its record, or
/// none when it has nothing.
pub type CallFn = Arc<dyn Fn(&str, &str) -> CallResult + Send + Sync>;

/// Where in `daemon.providers.<p>.sources.<s>` a [`Calls`] stands.
#[derive(Clone)]
enum Level {
    Daemon,
    Providers,
    Provider(String),
    Sources(String),
}

/// The callable tree a line reads Sources through: `daemon.providers.<p>.
/// sources.<s>()`, and, from a Source's `data` line, `sources.<s>()` over its
/// own provider's. A call gives the Source's record (none when it has none);
/// what a call does when the Source has nothing stored is `call`'s.
pub struct Calls {
    call: CallFn,
    level: Level,
}

impl Calls {
    /// `daemon`.
    pub fn daemon(call: CallFn) -> MjValue {
        MjValue::from_object(Self {
            call,
            level: Level::Daemon,
        })
    }

    /// `sources`, over `provider`'s Sources.
    pub fn sources_of(provider: &str, call: CallFn) -> MjValue {
        MjValue::from_object(Self {
            call,
            level: Level::Sources(provider.to_string()),
        })
    }
}

impl std::fmt::Debug for Calls {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Calls")
    }
}

impl Object for Calls {
    fn repr(self: &Arc<Self>) -> ObjectRepr {
        ObjectRepr::Plain
    }

    fn get_value(self: &Arc<Self>, key: &MjValue) -> Option<MjValue> {
        let key = key.as_str()?;
        let level = match &self.level {
            Level::Daemon if key == "providers" => Level::Providers,
            Level::Providers => Level::Provider(key.to_string()),
            Level::Provider(p) if key == "sources" => Level::Sources(p.clone()),
            Level::Sources(p) => {
                return Some(MjValue::from_object(Call {
                    call: self.call.clone(),
                    provider: p.clone(),
                    source: key.to_string(),
                }));
            }
            _ => return None,
        };
        Some(MjValue::from_object(Self {
            call: self.call.clone(),
            level,
        }))
    }
}

/// One Source of [`Calls`], callable.
struct Call {
    call: CallFn,
    provider: String,
    source: String,
}

impl std::fmt::Debug for Call {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Call({}.{})", self.provider, self.source)
    }
}

impl Object for Call {
    fn repr(self: &Arc<Self>) -> ObjectRepr {
        ObjectRepr::Plain
    }

    fn call(self: &Arc<Self>, _state: &State<'_, '_>, args: &[MjValue]) -> Result<MjValue, Error> {
        if !args.is_empty() {
            return Err(op_error("a Source takes no arguments"));
        }
        match (self.call)(&self.provider, &self.source).map_err(op_error)? {
            Some(record) => Ok(MjValue::from_serialize(record)),
            None => Ok(MjValue::from(())),
        }
    }
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

/// `url(address, method='GET', headers={}, body=none, timeout='10s')`: the
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
    let timeout = libbeachcomber::engine::timeout_kwarg(&kwargs, "url")?;
    kwargs.assert_all_used()?;

    let response = http::fetch(&method, address, &headers, body, timeout)
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
                lib.expect_identity().return_const(1usize);
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

    /// `timeout=` is a duration, as `exec()`'s is; the millisecond keyword
    /// says what took its place.
    #[test]
    fn url_timeout_is_a_duration() {
        let mut server = mockito::Server::new();
        let _m = server.mock("GET", "/v").with_body("ok").create();
        for timeout in ["'2s'", "2", "'1500ms'"] {
            let t = format!("{{{{ url('{}/v', timeout={timeout}) }}}}", server.url());
            assert_eq!(render(no_plugins(), &t).unwrap(), "ok", "{timeout}");
        }
        let err = url_error(&format!(
            "{{{{ url('{}/v', timeout_ms=100) }}}}",
            server.url()
        ));
        assert!(
            err.contains("url(): timeout_ms= is removed: write timeout='10s'"),
            "{err}"
        );
        let err = url_error(&format!("{{{{ url('{}/v', timeout='x') }}}}", server.url()));
        assert!(err.contains("url(): timeout="), "{err}");
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
            "{{{{ url('http://{}/', timeout='50ms') }}}}",
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

    /// A loader that yields libraries whose `bc_x` answers `1`, each reporting
    /// `image` as its identity, and counts its loads.
    fn counting_loader(
        image: usize,
        loads: Arc<std::sync::atomic::AtomicUsize>,
    ) -> MockLibraryLoader {
        let mut loader = MockLibraryLoader::new();
        loader.expect_load().returning(move |_| {
            loads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut lib = MockLoadedLibrary::new();
            lib.expect_identity().return_const(image);
            lib.expect_call_json().returning(|_, _| Ok("1".into()));
            Ok(Box::new(lib))
        });
        loader
    }

    fn loads_of(loads: &Arc<std::sync::atomic::AtomicUsize>) -> usize {
        loads.load(std::sync::atomic::Ordering::SeqCst)
    }

    #[test]
    fn every_name_for_one_image_is_one_library() {
        // dlopen returns the one image for a file it holds, so the loader's
        // handle is the identity, and the lock that serialises calls into the
        // image is shared by every name that reaches it.
        let dir = tempfile::tempdir().unwrap();
        for name in ["a", "b"] {
            std::fs::write(dir.path().join(format!("lib{name}.{EXT}")), b"").unwrap();
        }
        let loads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let plugins = Arc::new(PluginSet::new(
            vec![dir.path().to_path_buf()],
            Arc::new(counting_loader(7, loads.clone())),
        ));
        // Both names yield one image: the second library is dropped for the
        // first's Arc.
        let a = plugins.library("a").unwrap();
        let b = plugins.library("b").unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(
            loads_of(&loads),
            2,
            "each name loads once, to learn the image"
        );
    }

    #[test]
    fn distinct_images_stay_distinct() {
        let dir = tempfile::tempdir().unwrap();
        let mut loader = MockLibraryLoader::new();
        let mut image = 0usize;
        loader.expect_load().returning(move |_| {
            image += 1;
            let mut lib = MockLoadedLibrary::new();
            lib.expect_identity().return_const(image);
            Ok(Box::new(lib))
        });
        for name in ["a", "b"] {
            std::fs::write(dir.path().join(format!("lib{name}.{EXT}")), b"").unwrap();
        }
        let plugins = PluginSet::new(vec![dir.path().to_path_buf()], Arc::new(loader));
        let a = plugins.library("a").unwrap();
        let b = plugins.library("b").unwrap();
        assert!(!Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn a_loaded_name_is_never_looked_up_again() {
        // A rebuilt plugin is a new file: the name stays on the library it
        // loaded, and the new one is picked up at the daemon's next start.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(format!("libx.{EXT}"));
        std::fs::write(&file, b"old").unwrap();
        let loads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let plugins = PluginSet::new(
            vec![dir.path().to_path_buf()],
            Arc::new(counting_loader(1, loads.clone())),
        );
        let path = file.to_string_lossy().into_owned();
        let t = format!("{{{{ plugin('x', 'bc_x') }}}}{{{{ plugin('{path}', 'bc_x') }}}}");
        assert_eq!(
            render_with(EngineCtx::default(), plugins, &t).unwrap(),
            "11"
        );
        assert_eq!(loads_of(&loads), 2, "the name and the path load once each");
    }

    #[test]
    fn a_replaced_file_loads_nothing_new() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(format!("libx.{EXT}"));
        std::fs::write(&file, b"old").unwrap();
        let loads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let plugins = PluginSet::new(
            vec![dir.path().to_path_buf()],
            Arc::new(counting_loader(1, loads.clone())),
        );
        let path = file.to_string_lossy().into_owned();
        plugins.call("x", "bc_x", &serde_json::json!([])).unwrap();
        plugins.call(&path, "bc_x", &serde_json::json!([])).unwrap();
        assert_eq!(loads_of(&loads), 2);
        // A new inode under the same name.
        std::fs::remove_file(&file).unwrap();
        std::fs::write(&file, b"new").unwrap();
        plugins.call("x", "bc_x", &serde_json::json!([])).unwrap();
        plugins.call(&path, "bc_x", &serde_json::json!([])).unwrap();
        assert_eq!(
            loads_of(&loads),
            2,
            "neither the name nor the path loads again"
        );
    }

    #[test]
    fn a_name_keeps_the_file_it_first_resolved_to() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        std::fs::write(second.path().join(format!("libmoved.{EXT}")), b"2").unwrap();
        let loads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let plugins = PluginSet::new(
            vec![first.path().to_path_buf(), second.path().to_path_buf()],
            Arc::new(counting_loader(1, loads.clone())),
        );
        plugins
            .call("moved", "bc_x", &serde_json::json!([]))
            .unwrap();
        // A copy appears earlier on the path mid-run: the name does not move.
        std::fs::write(first.path().join(format!("libmoved.{EXT}")), b"1").unwrap();
        plugins
            .call("moved", "bc_x", &serde_json::json!([]))
            .unwrap();
        assert_eq!(loads_of(&loads), 1);
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
            "plugin(): libnope.{EXT} not found on [daemon.plugin] path ({})",
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
