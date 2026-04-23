use crate::config::ScriptProviderConfig;
use crate::provider::{
    FieldSchema, FieldScope, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    ProviderResult, Value,
};
use libloading::{Library, Symbol};
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::path::Path;
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

    /// Call the library's free function on a pointer returned by metadata or execute.
    fn free_str(&self, ptr: *mut c_char) {
        // SAFETY: ptr was returned by the library's metadata/execute functions
        // and must be freed by the library's free function.
        unsafe {
            let free_fn: Symbol<FreeFn> = self
                .library
                .get(b"beachcomber_provider_free\0")
                .expect("symbol validated at load");
            free_fn(ptr);
        }
    }

    /// Call beachcomber_provider_metadata and return the raw JSON string.
    fn call_metadata_raw(&self) -> Option<String> {
        // SAFETY: Symbol was validated at load time. The function returns a
        // pointer to a C string that we must free via beachcomber_provider_free.
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
            self.free_str(ptr as *mut c_char);
            Some(result)
        }
    }

    /// Call beachcomber_provider_execute and return the raw JSON string.
    fn call_execute_raw(&self, path: Option<&str>) -> Option<String> {
        // SAFETY: Symbol was validated at load time. We pass a valid C string
        // (or null) and the function returns a pointer we must free.
        unsafe {
            let execute_fn: Symbol<ExecuteFn> = self
                .library
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
            self.free_str(ptr as *mut c_char);
            Some(result)
        }
    }
}

impl Provider for LibraryProvider {
    fn metadata(&self) -> ProviderMetadata {
        // Try to get metadata from the library itself.
        if let Some(json_str) = self.call_metadata_raw() {
            if let Some(meta) = parse_library_metadata(&self.name, &json_str) {
                return meta;
            }
            debug!(
                "Library provider '{}': failed to parse metadata JSON, using config fallback",
                self.name
            );
        }

        // Fall back to config-based metadata (same as script provider).
        let invalidation = build_invalidation(&self.config);

        let fields = self
            .config
            .fields
            .as_ref()
            .map(|f| {
                f.iter()
                    .map(|(name, spec)| FieldSchema {
                        name: name.clone(),
                        field_type: match spec.field_type() {
                            "int" => FieldType::Int,
                            "bool" => FieldType::Bool,
                            "float" => FieldType::Float,
                            _ => FieldType::String,
                        },
                        scope: crate::config::resolve_field_scope(&self.config, name),
                    })
                    .collect()
            })
            .unwrap_or_default();

        ProviderMetadata {
            name: self.name.clone(),
            fields,
            invalidation,
        }
    }

    fn execute(&self, path: Option<&str>) -> Vec<(Option<String>, ProviderResult)> {
        let Some(json_str) = self.call_execute_raw(path) else {
            return Vec::new();
        };
        let Some(result) = parse_json_result(&json_str) else {
            return Vec::new();
        };
        let is_global = self.config.scope.as_deref() != Some("path");
        let scope_path = if is_global {
            None
        } else {
            path.map(|p| p.to_string())
        };
        vec![(scope_path, result)]
    }
}

/// Parse the JSON metadata returned by a library's beachcomber_provider_metadata().
///
/// Expected format (legacy):
/// ```json
/// {
///   "name": "myprovider",
///   "fields": {"field1": "string", "field2": "int"},
///   "invalidation": {"poll": "30s"},
///   "global": true
/// }
/// ```
///
/// Per-field scope format:
/// ```json
/// {
///   "name": "myprovider",
///   "fields": {
///     "field1": {"type": "string", "scope": "path"},
///     "field2": {"type": "int", "scope": "global"}
///   },
///   "invalidation": {"poll": "30s"}
/// }
/// ```
///
/// The top-level `global` bool acts as a fallback default scope for fields that
/// do not declare their own scope. Missing `global` defaults to `true` (Global).
fn parse_library_metadata(name: &str, json_str: &str) -> Option<ProviderMetadata> {
    let parsed: serde_json::Value = serde_json::from_str(json_str).ok()?;
    let obj = parsed.as_object()?;

    // Legacy default: top-level "global" bool acts as the provider-level
    // fallback scope for fields that don't declare their own.
    // Missing → default to Global (preserves legacy behaviour).
    let legacy_global = obj.get("global").and_then(|v| v.as_bool()).unwrap_or(true);
    let default_scope = if legacy_global {
        FieldScope::Global
    } else {
        FieldScope::PathScoped
    };

    let fields: Vec<FieldSchema> = obj
        .get("fields")
        .and_then(|f| f.as_object())
        .map(|f| {
            f.iter()
                .map(|(fname, ftype)| {
                    let (type_str, field_scope) = match ftype {
                        serde_json::Value::String(s) => (s.as_str(), default_scope),
                        serde_json::Value::Object(o) => {
                            let type_str =
                                o.get("type").and_then(|v| v.as_str()).unwrap_or("string");
                            let scope = o
                                .get("scope")
                                .and_then(|v| v.as_str())
                                .and_then(|s| match s {
                                    "global" => Some(FieldScope::Global),
                                    "path" | "pathscoped" => Some(FieldScope::PathScoped),
                                    _ => None,
                                })
                                .unwrap_or(default_scope);
                            (type_str, scope)
                        }
                        _ => ("string", default_scope),
                    };
                    FieldSchema {
                        name: fname.clone(),
                        field_type: match type_str {
                            "int" => FieldType::Int,
                            "bool" => FieldType::Bool,
                            "float" => FieldType::Float,
                            _ => FieldType::String,
                        },
                        scope: field_scope,
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    let invalidation = parse_invalidation_from_json(obj);

    Some(ProviderMetadata {
        name: name.to_string(),
        fields,
        invalidation,
    })
}

/// Expose the library metadata JSON parser for integration tests.
/// Hidden from docs; not part of the public API.
#[doc(hidden)]
pub fn parse_library_metadata_for_test(name: &str, json_str: &str) -> Option<ProviderMetadata> {
    parse_library_metadata(name, json_str)
}

fn parse_invalidation_from_json(
    obj: &serde_json::Map<String, serde_json::Value>,
) -> InvalidationStrategy {
    let inv = match obj.get("invalidation").and_then(|v| v.as_object()) {
        Some(inv) => inv,
        None => {
            return InvalidationStrategy::Poll {
                interval_secs: 30,
                floor_secs: 1,
            };
        }
    };

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

    let once = inv.get("once").and_then(|v| v.as_bool()).unwrap_or(false);

    if once {
        return InvalidationStrategy::Once;
    }

    match (watch_patterns, poll_secs) {
        (Some(patterns), Some(secs)) => InvalidationStrategy::WatchAndPoll {
            patterns,
            interval_secs: secs,
            floor_secs: 1,
        },
        (Some(patterns), None) => InvalidationStrategy::Watch {
            patterns,
            fallback_poll_secs: Some(60),
        },
        (None, Some(secs)) => InvalidationStrategy::Poll {
            interval_secs: secs,
            floor_secs: 1,
        },
        (None, None) => InvalidationStrategy::Poll {
            interval_secs: 30,
            floor_secs: 1,
        },
    }
}

/// Parse JSON output from beachcomber_provider_execute into a ProviderResult.
fn parse_json_result(json_str: &str) -> Option<ProviderResult> {
    let parsed: serde_json::Value = serde_json::from_str(json_str).ok()?;
    let obj = parsed.as_object()?;

    let mut result = ProviderResult::new();
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

fn build_invalidation(config: &ScriptProviderConfig) -> InvalidationStrategy {
    let poll_secs = config
        .invalidation
        .as_ref()
        .and_then(|i| i.poll.as_ref())
        .and_then(|s| crate::scheduler::parse_duration_secs_pub(s));

    let watch_patterns = config.invalidation.as_ref().and_then(|i| i.watch.clone());

    match (watch_patterns, poll_secs) {
        (Some(patterns), Some(secs)) => InvalidationStrategy::WatchAndPoll {
            patterns,
            interval_secs: secs,
            floor_secs: 1,
        },
        (Some(patterns), None) => InvalidationStrategy::Watch {
            patterns,
            fallback_poll_secs: Some(60),
        },
        (None, Some(secs)) => InvalidationStrategy::Poll {
            interval_secs: secs,
            floor_secs: 1,
        },
        (None, None) => InvalidationStrategy::Poll {
            interval_secs: 30,
            floor_secs: 1,
        },
    }
}

/// Expand ~ to $HOME in a path string.
fn shellexpand(path: &str) -> String {
    if path.starts_with("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return format!("{}{}", home, &path[1..]);
    }
    path.to_string()
}
