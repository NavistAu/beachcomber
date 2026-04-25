use crate::config::ScriptProviderConfig;
use crate::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, KeepAlive, Provider,
    ProviderMetadata, Source, SourceMetadata, SourceResult, SourceScope, Value,
};
use libloading::{Library, Symbol};
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::path::Path;
use std::sync::OnceLock;
use tracing::{debug, warn};

/// C ABI function signatures for shared library providers.
///
/// Libraries must export these symbols:
///
/// ```c
/// // Returns JSON: {"name":"...", "fields":{...}, "invalidation":{...}, "global":bool}
/// const char* beachcomber_provider_metadata(void);
///
/// // Returns JSON: {"field":"value", ...} or NULL on failure.
/// // `path` is NULL for global providers.
/// const char* beachcomber_provider_execute(const char* path);
///
/// // Frees a string previously returned by metadata or execute.
/// void beachcomber_provider_free(char* ptr);
/// ```
type MetadataFn = unsafe extern "C" fn() -> *const c_char;
type ExecuteFn = unsafe extern "C" fn(*const c_char) -> *const c_char;
type FreeFn = unsafe extern "C" fn(*mut c_char);

pub struct LibraryProvider {
    name: String,
    config: ScriptProviderConfig,
    library: Library,
}

// SAFETY: The shared library's exported functions are required to be thread-safe
// per the provider contract. The Library handle itself is Send+Sync once loaded.
unsafe impl Send for LibraryProvider {}
unsafe impl Sync for LibraryProvider {}

impl LibraryProvider {
    pub fn new(name: &str, config: ScriptProviderConfig) -> Option<Self> {
        let lib_path = config.library_path.as_ref()?;
        let expanded = shellexpand(lib_path);
        let path = Path::new(&expanded);

        if !path.exists() {
            warn!(
                "Library provider '{}': path does not exist: {}",
                name, expanded
            );
            return None;
        }

        // SAFETY: We trust the user-configured library path. Loading a shared library
        // executes its init functions, which is inherent to the libloading contract.
        let library = match unsafe { Library::new(path) } {
            Ok(lib) => lib,
            Err(e) => {
                warn!(
                    "Library provider '{}': failed to load {}: {}",
                    name, expanded, e
                );
                return None;
            }
        };

        // Validate that all required symbols exist at load time.
        {
            let _: Symbol<MetadataFn> =
                match unsafe { library.get(b"beachcomber_provider_metadata\0") } {
                    Ok(sym) => sym,
                    Err(e) => {
                        warn!(
                            "Library provider '{}': missing beachcomber_provider_metadata: {}",
                            name, e
                        );
                        return None;
                    }
                };
            let _: Symbol<ExecuteFn> =
                match unsafe { library.get(b"beachcomber_provider_execute\0") } {
                    Ok(sym) => sym,
                    Err(e) => {
                        warn!(
                            "Library provider '{}': missing beachcomber_provider_execute: {}",
                            name, e
                        );
                        return None;
                    }
                };
            let _: Symbol<FreeFn> = match unsafe { library.get(b"beachcomber_provider_free\0") } {
                Ok(sym) => sym,
                Err(e) => {
                    warn!(
                        "Library provider '{}': missing beachcomber_provider_free: {}",
                        name, e
                    );
                    return None;
                }
            };
        }

        Some(Self {
            name: name.to_string(),
            config,
            library,
        })
    }
}

impl Provider for LibraryProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: self.name.clone(),
            sources: vec![self.single_source_meta()],
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![Box::new(LibrarySingleSource {
            name: self.name.clone(),
            config: self.config.clone(),
            // SAFETY: Library lives as long as the LibraryProvider which owns this Source.
            // We wrap it in Arc so the Source can be kept alive independently.
            library: std::sync::Arc::new(LibraryHandle {
                inner: unsafe { std::mem::ManuallyDrop::new(Library::new(
                    shellexpand(self.config.library_path.as_deref().unwrap_or(""))
                ).expect("library already validated at LibraryProvider::new")) },
            }),
            meta: OnceLock::new(),
        })]
    }
}

impl LibraryProvider {
    fn single_source_meta(&self) -> SourceMetadata {
        // Try to read from the library's own metadata export.
        if let Some(json_str) = self.call_metadata_raw() {
            if let Some(meta) = parse_library_source_meta(&self.name, &json_str) {
                return meta;
            }
            debug!(
                "Library provider '{}': failed to parse metadata JSON, using config fallback",
                self.name
            );
        }
        build_source_meta_from_config(&self.name, &self.config)
    }

    fn call_metadata_raw(&self) -> Option<String> {
        // SAFETY: Symbol was validated at load time.
        unsafe {
            let metadata_fn: Symbol<MetadataFn> = self
                .library
                .get(b"beachcomber_provider_metadata\0")
                .expect("symbol validated at load");
            let ptr = metadata_fn();
            if ptr.is_null() {
                return None;
            }
            let cstr = CStr::from_ptr(ptr);
            let result = cstr.to_string_lossy().into_owned();
            let free_fn: Symbol<FreeFn> = self
                .library
                .get(b"beachcomber_provider_free\0")
                .expect("symbol validated at load");
            free_fn(ptr as *mut c_char);
            Some(result)
        }
    }
}

// ── Library handle wrapper for Arc sharing ────────────────────────────────────

/// Wraps a `Library` handle in a form safe to share via `Arc`.
/// The `ManuallyDrop` prevents the library from being unloaded when the
/// `Arc` clone is dropped — only the original `LibraryProvider` unloads it.
struct LibraryHandle {
    inner: std::mem::ManuallyDrop<Library>,
}

// SAFETY: see LibraryProvider's Send/Sync impl note.
unsafe impl Send for LibraryHandle {}
unsafe impl Sync for LibraryHandle {}

// ── LibrarySingleSource ───────────────────────────────────────────────────────

struct LibrarySingleSource {
    name: String,
    config: ScriptProviderConfig,
    library: std::sync::Arc<LibraryHandle>,
    meta: OnceLock<SourceMetadata>,
}

// SAFETY: LibraryHandle is Send+Sync.
unsafe impl Send for LibrarySingleSource {}
unsafe impl Sync for LibrarySingleSource {}

impl Source for LibrarySingleSource {
    fn metadata(&self) -> &SourceMetadata {
        self.meta.get_or_init(|| {
            // Try library's own metadata, fall back to config.
            let raw = self.call_metadata_raw();
            raw.as_deref()
                .and_then(|s| parse_library_source_meta(&self.name, s))
                .unwrap_or_else(|| build_source_meta_from_config(&self.name, &self.config))
        })
    }

    fn execute(&self, path: Option<&str>) -> SourceResult {
        let Some(json_str) = self.call_execute_raw(path) else {
            return SourceResult::new();
        };
        parse_json_result(&json_str).unwrap_or_else(SourceResult::new)
    }
}

impl LibrarySingleSource {
    fn call_metadata_raw(&self) -> Option<String> {
        // SAFETY: Library handle is valid for the lifetime of the Arc.
        unsafe {
            let metadata_fn: Symbol<MetadataFn> = self
                .library
                .inner
                .get(b"beachcomber_provider_metadata\0")
                .expect("symbol validated at load");
            let ptr = metadata_fn();
            if ptr.is_null() {
                return None;
            }
            let cstr = CStr::from_ptr(ptr);
            let result = cstr.to_string_lossy().into_owned();
            let free_fn: Symbol<FreeFn> = self
                .library
                .inner
                .get(b"beachcomber_provider_free\0")
                .expect("symbol validated at load");
            free_fn(ptr as *mut c_char);
            Some(result)
        }
    }

    fn call_execute_raw(&self, path: Option<&str>) -> Option<String> {
        // SAFETY: Symbol was validated at load time.
        unsafe {
            let execute_fn: Symbol<ExecuteFn> = self
                .library
                .inner
                .get(b"beachcomber_provider_execute\0")
                .expect("symbol validated at load");

            let c_path = path.and_then(|p| CString::new(p).ok());
            let path_ptr = c_path.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());

            let ptr = execute_fn(path_ptr);
            if ptr.is_null() {
                return None;
            }
            let cstr = CStr::from_ptr(ptr);
            let result = cstr.to_string_lossy().into_owned();
            let free_fn: Symbol<FreeFn> = self
                .library
                .inner
                .get(b"beachcomber_provider_free\0")
                .expect("symbol validated at load");
            free_fn(ptr as *mut c_char);
            Some(result)
        }
    }
}

// ── Metadata parsing ──────────────────────────────────────────────────────────

/// Parse library metadata JSON into a SourceMetadata.
///
/// If the library declares `"invalidation": {"once": true}` (legacy), substitute
/// with a pure-watch global source that runs once and never polls again
/// (strategy = Watch + abs_paths=[] + Global + KeepAlive::Never). This is the
/// semantic equivalent: run once on first demand, stay Active forever.
fn parse_library_source_meta(name: &str, json_str: &str) -> Option<SourceMetadata> {
    let parsed: serde_json::Value = serde_json::from_str(json_str).ok()?;
    let obj = parsed.as_object()?;

    let legacy_global = obj.get("global").and_then(|v| v.as_bool()).unwrap_or(true);
    let default_scope = if legacy_global {
        SourceScope::Global
    } else {
        SourceScope::PathScoped
    };

    let fields: Vec<FieldSchema> = obj
        .get("fields")
        .and_then(|f| f.as_object())
        .map(|f| {
            f.iter()
                .map(|(fname, ftype)| {
                    let type_str = match ftype {
                        serde_json::Value::String(s) => s.as_str(),
                        serde_json::Value::Object(o) => {
                            o.get("type").and_then(|v| v.as_str()).unwrap_or("string")
                        }
                        _ => "string",
                    };
                    FieldSchema {
                        name: fname.clone(),
                        field_type: match type_str {
                            "int" => FieldType::Int,
                            "bool" => FieldType::Bool,
                            "float" => FieldType::Float,
                            _ => FieldType::String,
                        },
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    let (invalidation, keep_alive, scope) = parse_invalidation_and_keep_alive(obj, default_scope);

    // Fall back to a <field> sentinel if no fields declared (dynamic library).
    let fields = if fields.is_empty() {
        vec![FieldSchema { name: "<field>".into(), field_type: FieldType::String }]
    } else {
        fields
    };

    Some(SourceMetadata {
        name: "main".into(),
        fields,
        scope,
        invalidation,
        keep_alive,
        failback: FailbackConfig { reattempts: 3, interval_secs: 60 },
        fsevents_reinstate: false,
    })
}

fn parse_invalidation_and_keep_alive(
    obj: &serde_json::Map<String, serde_json::Value>,
    scope: SourceScope,
) -> (InvalidationStrategy, KeepAlive, SourceScope) {
    let inv = match obj.get("invalidation").and_then(|v| v.as_object()) {
        Some(inv) => inv,
        None => {
            return (
                InvalidationStrategy::Poll { interval_secs: 30 },
                KeepAlive::Polls(2),
                scope,
            );
        }
    };

    let once = inv.get("once").and_then(|v| v.as_bool()).unwrap_or(false);
    if once {
        // Legacy `once` → pure-watch global. Run once on demand, never again (KeepAlive::Never).
        // The strategy is Watch + Global + no patterns/abs_paths = never refreshes again after
        // the first successful execute.
        return (
            InvalidationStrategy::Watch {
                patterns: vec![],
                abs_paths: vec![],
            },
            KeepAlive::Never,
            SourceScope::Global,
        );
    }

    let poll_secs = inv
        .get("poll")
        .and_then(|v| v.as_str())
        .and_then(crate::scheduler::parse_duration_secs_pub);

    let watch_patterns: Option<Vec<String>> = inv.get("watch").and_then(|v| {
        v.as_array().map(|arr| {
            arr.iter()
                .filter_map(|item| item.as_str().map(String::from))
                .collect()
        })
    });

    match (watch_patterns, poll_secs) {
        (Some(patterns), Some(secs)) => (
            InvalidationStrategy::WatchAndPoll {
                patterns,
                abs_paths: vec![],
                interval_secs: secs,
            },
            KeepAlive::Polls(2),
            scope,
        ),
        (Some(patterns), None) => {
            let (inv_strat, ka) = if scope == SourceScope::Global {
                (InvalidationStrategy::Watch { patterns: vec![], abs_paths: vec![] }, KeepAlive::Never)
            } else {
                (InvalidationStrategy::Watch { patterns, abs_paths: vec![] }, KeepAlive::Duration(120))
            };
            (inv_strat, ka, scope)
        }
        (None, Some(secs)) => (
            InvalidationStrategy::Poll { interval_secs: secs },
            KeepAlive::Polls(2),
            scope,
        ),
        (None, None) => (
            InvalidationStrategy::Poll { interval_secs: 30 },
            KeepAlive::Polls(2),
            scope,
        ),
    }
}

fn build_source_meta_from_config(name: &str, config: &ScriptProviderConfig) -> SourceMetadata {
    let poll_secs = config
        .invalidation
        .as_ref()
        .and_then(|i| i.poll.as_ref())
        .and_then(|s| crate::scheduler::parse_duration_secs_pub(s))
        .unwrap_or(30);

    let watch_patterns = config.invalidation.as_ref().and_then(|i| i.watch.clone());

    let is_global = config.scope.as_deref() != Some("path");
    let scope = if is_global {
        SourceScope::Global
    } else {
        SourceScope::PathScoped
    };

    let (invalidation, keep_alive) = match watch_patterns {
        Some(patterns) => {
            if scope == SourceScope::Global {
                (
                    InvalidationStrategy::Watch { patterns: vec![], abs_paths: vec![] },
                    KeepAlive::Never,
                )
            } else {
                (
                    InvalidationStrategy::WatchAndPoll {
                        patterns,
                        abs_paths: vec![],
                        interval_secs: poll_secs,
                    },
                    KeepAlive::Polls(2),
                )
            }
        }
        None => (
            InvalidationStrategy::Poll { interval_secs: poll_secs },
            KeepAlive::Polls(2),
        ),
    };

    let fields = config
        .fields
        .as_ref()
        .map(|f| {
            f.iter()
                .map(|(fname, spec)| FieldSchema {
                    name: fname.clone(),
                    field_type: match spec.field_type() {
                        "int" => FieldType::Int,
                        "bool" => FieldType::Bool,
                        "float" => FieldType::Float,
                        _ => FieldType::String,
                    },
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let fields = if fields.is_empty() {
        vec![FieldSchema { name: "<field>".into(), field_type: FieldType::String }]
    } else {
        fields
    };

    SourceMetadata {
        name: "main".into(),
        fields,
        scope,
        invalidation,
        keep_alive,
        failback: FailbackConfig { reattempts: 3, interval_secs: 60 },
        fsevents_reinstate: false,
    }
}

/// Expose the library metadata JSON parser for integration tests.
#[doc(hidden)]
pub fn parse_library_metadata_for_test(name: &str, json_str: &str) -> Option<ProviderMetadata> {
    let meta = parse_library_source_meta(name, json_str)?;
    Some(ProviderMetadata {
        name: name.to_string(),
        sources: vec![meta],
    })
}

fn parse_json_result(json_str: &str) -> Option<SourceResult> {
    let parsed: serde_json::Value = serde_json::from_str(json_str).ok()?;
    let obj = parsed.as_object()?;

    let mut result = SourceResult::new();
    for (key, val) in obj {
        let value = match val {
            serde_json::Value::String(s) => Value::String(s.clone()),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Value::Int(i)
                } else if let Some(f) = n.as_f64() {
                    Value::Float(f)
                } else {
                    Value::String(n.to_string())
                }
            }
            serde_json::Value::Bool(b) => Value::Bool(*b),
            other => Value::String(other.to_string()),
        };
        result.insert(key.clone(), value);
    }
    Some(result)
}

fn shellexpand(path: &str) -> String {
    if path.starts_with("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return format!("{}{}", home, &path[1..]);
    }
    path.to_string()
}
