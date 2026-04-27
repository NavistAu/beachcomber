use crate::config::{ExternalSourceConfig, ScriptProviderConfig};
use crate::provider::script::build_source_meta_from_external;
use crate::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, KeepAlive, Provider,
    ProviderMetadata, Source, SourceMetadata, SourceResult, SourceScope, Value,
};
use libloading::{Library, Symbol};
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::path::Path;
use std::sync::{Arc, OnceLock};
use tracing::{debug, warn};

// `size_t` in C corresponds to `usize` in Rust.
#[allow(non_camel_case_types)]
type c_size_t = usize;

// ── Legacy single-entry-point C ABI ──────────────────────────────────────────
//
// Libraries must export these symbols (original ABI):
//
//   const char* beachcomber_provider_metadata(void);
//   const char* beachcomber_provider_execute(const char* path);
//   void        beachcomber_provider_free(char* ptr);
//
// Libraries that export the Phase 4 multi-source ABI instead export:
//
//   size_t              bc_source_count(void);
//   const char*         bc_source_metadata(size_t idx);   // JSON per source
//   const char*         bc_source_execute(size_t idx, const char* path);
//   void                bc_source_free(char* ptr);        // free any bc_* string
//
// If `bc_source_count` is found, the Phase 4 ABI is used. Otherwise the
// library falls back to the legacy ABI, and the library's single source is
// mapped to source index 0.

type MetadataFn = unsafe extern "C" fn() -> *const c_char;
type ExecuteFn = unsafe extern "C" fn(*const c_char) -> *const c_char;
type FreeFn = unsafe extern "C" fn(*mut c_char);

// Phase 4 multi-source ABI
type BcSourceCountFn = unsafe extern "C" fn() -> c_size_t;
type BcSourceMetadataFn = unsafe extern "C" fn(c_size_t) -> *const c_char;
type BcSourceExecuteFn = unsafe extern "C" fn(c_size_t, *const c_char) -> *const c_char;
type BcSourceFreeFn = unsafe extern "C" fn(*mut c_char);

/// Whether a library uses the Phase 4 multi-source ABI or the legacy ABI.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LibraryAbi {
    /// Phase 4: `bc_source_count / bc_source_metadata / bc_source_execute / bc_source_free`.
    MultiSource,
    /// Legacy: `beachcomber_provider_metadata / execute / free`.
    Legacy,
}

// ── LibraryHandle: Arc-shareable library wrapper ──────────────────────────────

struct LibraryHandle {
    inner: std::mem::ManuallyDrop<Library>,
}

// SAFETY: Shared-library functions are required to be thread-safe per the provider contract.
unsafe impl Send for LibraryHandle {}
unsafe impl Sync for LibraryHandle {}

// ── LibraryProvider ───────────────────────────────────────────────────────────

pub struct LibraryProvider {
    name: String,
    library: Library,
    config: ScriptProviderConfig,
    /// User-supplied knob overrides from TOML sub-tables (Phase 4).
    source_overrides: Vec<ExternalSourceConfig>,
}

// SAFETY: see LibraryHandle.
unsafe impl Send for LibraryProvider {}
unsafe impl Sync for LibraryProvider {}

impl LibraryProvider {
    /// Single-source constructor (legacy `type = "library"` TOML path).
    pub fn new(name: &str, config: ScriptProviderConfig) -> Option<Self> {
        let lib = load_library(name, config.library_path.as_deref()?)?;
        Some(Self {
            name: name.to_string(),
            library: lib,
            config,
            source_overrides: vec![],
        })
    }

    /// Phase 4 constructor: `backend = "library"` with a `library_path` and optional
    /// per-source override sub-tables.
    pub fn with_sources(
        name: &str,
        library_path: &str,
        source_overrides: Vec<ExternalSourceConfig>,
    ) -> Option<Self> {
        let cfg = ScriptProviderConfig {
            library_path: Some(library_path.to_string()),
            ..Default::default()
        };
        let lib = load_library(name, library_path)?;
        Some(Self {
            name: name.to_string(),
            library: lib,
            config: cfg,
            source_overrides,
        })
    }

    fn detect_abi(&self) -> LibraryAbi {
        // Probe for `bc_source_count` to decide which ABI to use.
        let found: Result<Symbol<BcSourceCountFn>, _> =
            unsafe { self.library.get(b"bc_source_count\0") };
        if found.is_ok() {
            LibraryAbi::MultiSource
        } else {
            LibraryAbi::Legacy
        }
    }

    /// Build SourceMetadata list. For Phase 4 ABI, calls `bc_source_count` and
    /// `bc_source_metadata(idx)`. For legacy ABI, calls `beachcomber_provider_metadata`.
    fn build_source_metas(&self) -> Vec<SourceMetadata> {
        match self.detect_abi() {
            LibraryAbi::MultiSource => self.build_multi_source_metas(),
            LibraryAbi::Legacy => vec![self.build_legacy_source_meta()],
        }
    }

    fn build_multi_source_metas(&self) -> Vec<SourceMetadata> {
        let count = unsafe {
            let f: Symbol<BcSourceCountFn> = self
                .library
                .get(b"bc_source_count\0")
                .expect("symbol validated by detect_abi");
            f()
        };

        let mut metas = Vec::new();
        for idx in 0..count {
            let raw = self.call_bc_source_metadata(idx);
            let meta = raw
                .as_deref()
                .and_then(|s| parse_library_source_meta(&self.name, s))
                .unwrap_or_else(|| fallback_source_meta(&self.name, idx));

            // Apply user overrides if a matching block exists.
            let meta = self.apply_override(meta);
            metas.push(meta);
        }
        metas
    }

    fn build_legacy_source_meta(&self) -> SourceMetadata {
        if let Some(json_str) = self.call_legacy_metadata_raw() {
            if let Some(meta) = parse_library_source_meta(&self.name, &json_str) {
                return self.apply_override(meta);
            }
            debug!(
                "Library provider '{}': failed to parse metadata JSON, using config fallback",
                self.name
            );
        }
        self.apply_override(build_source_meta_from_config(&self.name, &self.config))
    }

    /// Apply per-source override if `source_overrides` has a matching source name.
    fn apply_override(&self, meta: SourceMetadata) -> SourceMetadata {
        if let Some(ov) = self.source_overrides.iter().find(|o| o.name == meta.name) {
            // Build a new SourceMetadata from the override config; the override
            // config may not have all fields set, so we merge.
            let merged = build_source_meta_from_external(ov);
            // Keep the original name from the library's declaration.
            SourceMetadata {
                name: meta.name.clone(),
                ..merged
            }
        } else {
            meta
        }
    }

    // ── Raw call helpers ──────────────────────────────────────────────────────

    fn call_legacy_metadata_raw(&self) -> Option<String> {
        unsafe {
            let f: Symbol<MetadataFn> =
                self.library.get(b"beachcomber_provider_metadata\0").ok()?;
            let ptr = f();
            if ptr.is_null() {
                return None;
            }
            let cstr = CStr::from_ptr(ptr);
            let result = cstr.to_string_lossy().into_owned();
            if let Ok(free_fn) = self.library.get::<FreeFn>(b"beachcomber_provider_free\0") {
                free_fn(ptr as *mut c_char);
            }
            Some(result)
        }
    }

    fn call_bc_source_metadata(&self, idx: c_size_t) -> Option<String> {
        unsafe {
            let f: Symbol<BcSourceMetadataFn> = self
                .library
                .get(b"bc_source_metadata\0")
                .expect("symbol validated by detect_abi");
            let ptr = f(idx);
            if ptr.is_null() {
                return None;
            }
            let cstr = CStr::from_ptr(ptr);
            let result = cstr.to_string_lossy().into_owned();
            if let Ok(free_fn) = self.library.get::<BcSourceFreeFn>(b"bc_source_free\0") {
                free_fn(ptr as *mut c_char);
            }
            Some(result)
        }
    }
}

impl Provider for LibraryProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: self.name.clone(),
            sources: self.build_source_metas(),
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        let metas = self.build_source_metas();
        let abi = self.detect_abi();

        // Re-load the library into an Arc-wrapped handle so each source can use it.
        let lib_path = self.config.library_path.clone().unwrap_or_default();
        let lib_arc = match unsafe { Library::new(shellexpand(&lib_path)) } {
            Ok(lib) => Arc::new(LibraryHandle {
                inner: std::mem::ManuallyDrop::new(lib),
            }),
            Err(e) => {
                warn!(
                    "Library provider '{}': failed to reload library for sources: {}",
                    self.name, e
                );
                return vec![];
            }
        };

        metas
            .into_iter()
            .enumerate()
            .map(|(idx, meta)| {
                Box::new(LibrarySource {
                    source_idx: idx,
                    abi,
                    library: lib_arc.clone(),
                    meta: OnceLock::new(),
                    meta_value: meta,
                }) as Box<dyn Source>
            })
            .collect()
    }
}

// ── LibrarySource ─────────────────────────────────────────────────────────────

struct LibrarySource {
    source_idx: usize,
    abi: LibraryAbi,
    library: Arc<LibraryHandle>,
    meta: OnceLock<SourceMetadata>,
    meta_value: SourceMetadata,
}

// SAFETY: LibraryHandle is Send+Sync.
unsafe impl Send for LibrarySource {}
unsafe impl Sync for LibrarySource {}

impl Source for LibrarySource {
    fn metadata(&self) -> &SourceMetadata {
        self.meta.get_or_init(|| self.meta_value.clone())
    }

    fn execute(&self, path: Option<&str>) -> SourceResult {
        let json_str = match self.abi {
            LibraryAbi::MultiSource => self.call_bc_execute(path),
            LibraryAbi::Legacy => self.call_legacy_execute(path),
        };
        let Some(s) = json_str else {
            return SourceResult::new();
        };
        parse_json_result(&s).unwrap_or_default()
    }
}

impl LibrarySource {
    fn call_legacy_execute(&self, path: Option<&str>) -> Option<String> {
        unsafe {
            let f: Symbol<ExecuteFn> = self
                .library
                .inner
                .get(b"beachcomber_provider_execute\0")
                .ok()?;
            let c_path = path.and_then(|p| CString::new(p).ok());
            let path_ptr = c_path.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
            let ptr = f(path_ptr);
            if ptr.is_null() {
                return None;
            }
            let cstr = CStr::from_ptr(ptr);
            let result = cstr.to_string_lossy().into_owned();
            if let Ok(free_fn) = self
                .library
                .inner
                .get::<FreeFn>(b"beachcomber_provider_free\0")
            {
                free_fn(ptr as *mut c_char);
            }
            Some(result)
        }
    }

    fn call_bc_execute(&self, path: Option<&str>) -> Option<String> {
        unsafe {
            let f: Symbol<BcSourceExecuteFn> =
                self.library.inner.get(b"bc_source_execute\0").ok()?;
            let c_path = path.and_then(|p| CString::new(p).ok());
            let path_ptr = c_path.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
            let ptr = f(self.source_idx, path_ptr);
            if ptr.is_null() {
                return None;
            }
            let cstr = CStr::from_ptr(ptr);
            let result = cstr.to_string_lossy().into_owned();
            if let Ok(free_fn) = self
                .library
                .inner
                .get::<BcSourceFreeFn>(b"bc_source_free\0")
            {
                free_fn(ptr as *mut c_char);
            }
            Some(result)
        }
    }
}

// ── Metadata parsing ──────────────────────────────────────────────────────────

/// Parse library metadata JSON (single-source legacy format) into a SourceMetadata.
fn parse_library_source_meta(_name: &str, json_str: &str) -> Option<SourceMetadata> {
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

    let fields = if fields.is_empty() {
        vec![FieldSchema {
            name: "<field>".into(),
            field_type: FieldType::String,
        }]
    } else {
        fields
    };

    // Source name from JSON or default "main".
    let source_name = obj
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("main")
        .to_string();

    Some(SourceMetadata {
        name: source_name,
        fields,
        scope,
        invalidation,
        keep_alive,
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 60,
        },
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
                (
                    InvalidationStrategy::Watch {
                        patterns: vec![],
                        abs_paths: vec![],
                    },
                    KeepAlive::Never,
                )
            } else {
                (
                    InvalidationStrategy::Watch {
                        patterns,
                        abs_paths: vec![],
                    },
                    KeepAlive::Duration(120),
                )
            };
            (inv_strat, ka, scope)
        }
        (None, Some(secs)) => (
            InvalidationStrategy::Poll {
                interval_secs: secs,
            },
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

fn build_source_meta_from_config(_name: &str, config: &ScriptProviderConfig) -> SourceMetadata {
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
                    InvalidationStrategy::Watch {
                        patterns: vec![],
                        abs_paths: vec![],
                    },
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
            InvalidationStrategy::Poll {
                interval_secs: poll_secs,
            },
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
        vec![FieldSchema {
            name: "<field>".into(),
            field_type: FieldType::String,
        }]
    } else {
        fields
    };

    SourceMetadata {
        name: "main".into(),
        fields,
        scope,
        invalidation,
        keep_alive,
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 60,
        },
        fsevents_reinstate: false,
    }
}

fn fallback_source_meta(provider_name: &str, idx: c_size_t) -> SourceMetadata {
    let name = if idx == 0 {
        "main".to_string()
    } else {
        format!("source{}", idx)
    };
    warn!(
        "Library provider '{}': failed to parse bc_source_metadata({}), using fallback",
        provider_name, idx
    );
    SourceMetadata {
        name,
        fields: vec![FieldSchema {
            name: "<field>".into(),
            field_type: FieldType::String,
        }],
        scope: SourceScope::Global,
        invalidation: InvalidationStrategy::Poll { interval_secs: 30 },
        keep_alive: KeepAlive::Polls(2),
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 60,
        },
        fsevents_reinstate: false,
    }
}

// ── Test helper ───────────────────────────────────────────────────────────────

/// Expose the library metadata JSON parser for integration tests.
#[doc(hidden)]
pub fn parse_library_metadata_for_test(name: &str, json_str: &str) -> Option<ProviderMetadata> {
    let meta = parse_library_source_meta(name, json_str)?;
    Some(ProviderMetadata {
        name: name.to_string(),
        sources: vec![meta],
    })
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn load_library(name: &str, lib_path: &str) -> Option<Library> {
    let expanded = shellexpand(lib_path);
    let path = Path::new(&expanded);

    if !path.exists() {
        warn!(
            "Library provider '{}': path does not exist: {}",
            name, expanded
        );
        return None;
    }

    // SAFETY: We trust the user-configured library path.
    match unsafe { Library::new(path) } {
        Ok(lib) => Some(lib),
        Err(e) => {
            warn!(
                "Library provider '{}': failed to load {}: {}",
                name, expanded, e
            );
            None
        }
    }
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
