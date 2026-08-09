use crate::boundaries::library::{LibloadingLoader, LibraryLoader, LoadedLibrary};
use crate::config::{ExternalSourceConfig, ScriptProviderConfig};
use crate::provider::script::build_source_meta_from_external;
use crate::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, KeepAlive, Provider,
    ProviderMetadata, Source, SourceMetadata, SourceResult, SourceScope,
};
use std::sync::{Arc, OnceLock};
use tracing::{debug, warn};

/// Whether a library uses the Phase 4 multi-source ABI or the legacy ABI.
///
/// Determined at runtime by whether `bc_source_count` is present.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LibraryAbi {
    /// Phase 4: `bc_source_count / bc_source_metadata / bc_source_execute`.
    MultiSource,
    /// Legacy: `beachcomber_provider_metadata / execute / free`.
    Legacy,
}

// ── LibraryProvider ───────────────────────────────────────────────────────────

pub struct LibraryProvider {
    name: String,
    config: ScriptProviderConfig,
    /// User-supplied knob overrides from TOML sub-tables (Phase 4).
    source_overrides: Vec<ExternalSourceConfig>,
    /// The loader used to open the shared library.  Stored so `sources()` can
    /// re-load the library when creating `LibrarySource` objects.
    loader: Arc<dyn LibraryLoader>,
    /// A loaded handle used *only* at metadata-query time (inside `metadata()`
    /// and `sources()`).  The per-source `LibrarySource` objects hold their own
    /// clone of a freshly loaded handle so each source is independent.
    loaded: Arc<dyn LoadedLibrary>,
}

impl LibraryProvider {
    /// Single-source constructor (legacy `type = "library"` TOML path).
    /// Wires the real `LibloadingLoader`.
    pub fn new(name: &str, config: ScriptProviderConfig) -> Option<Self> {
        Self::with_loader(name, config, Arc::new(LibloadingLoader))
    }

    /// Single-source constructor with injected loader (for tests).
    pub fn with_loader(
        name: &str,
        config: ScriptProviderConfig,
        loader: Arc<dyn LibraryLoader>,
    ) -> Option<Self> {
        let lib_path = config.library_path.as_deref()?.to_string();
        let loaded = loader
            .load(lib_path)
            .map_err(|e| {
                warn!("Library provider '{}': failed to load: {}", name, e);
            })
            .ok()?;
        Some(Self {
            name: name.to_string(),
            config,
            source_overrides: vec![],
            loader,
            loaded: Arc::from(loaded),
        })
    }

    /// Phase 4 constructor: `backend = "library"` with a `library_path` and optional
    /// per-source override sub-tables.  Wires the real `LibloadingLoader`.
    pub fn with_sources(
        name: &str,
        library_path: &str,
        source_overrides: Vec<ExternalSourceConfig>,
    ) -> Option<Self> {
        Self::with_sources_and_loader(
            name,
            library_path,
            source_overrides,
            Arc::new(LibloadingLoader),
        )
    }

    /// Phase 4 constructor with injected loader (for tests).
    pub fn with_sources_and_loader(
        name: &str,
        library_path: &str,
        source_overrides: Vec<ExternalSourceConfig>,
        loader: Arc<dyn LibraryLoader>,
    ) -> Option<Self> {
        let cfg = ScriptProviderConfig {
            library_path: Some(library_path.to_string()),
            ..Default::default()
        };
        let loaded = loader
            .load(library_path.to_string())
            .map_err(|e| {
                warn!("Library provider '{}': failed to load: {}", name, e);
            })
            .ok()?;
        Some(Self {
            name: name.to_string(),
            config: cfg,
            source_overrides,
            loader,
            loaded: Arc::from(loaded),
        })
    }

    fn detect_abi(&self) -> LibraryAbi {
        if self.loaded.source_count() > 0 {
            LibraryAbi::MultiSource
        } else {
            LibraryAbi::Legacy
        }
    }

    /// Build SourceMetadata list.
    fn build_source_metas(&self) -> Vec<SourceMetadata> {
        match self.detect_abi() {
            LibraryAbi::MultiSource => self.build_multi_source_metas(),
            LibraryAbi::Legacy => vec![self.build_legacy_source_meta()],
        }
    }

    fn build_multi_source_metas(&self) -> Vec<SourceMetadata> {
        let count = self.loaded.source_count();
        let mut metas = Vec::new();
        for idx in 0..count {
            let raw = self.loaded.call_source_metadata(idx);
            let meta = raw
                .as_deref()
                .and_then(|s| parse_library_source_meta(&self.name, s))
                .unwrap_or_else(|| fallback_source_meta(&self.name, idx));
            let meta = self.apply_override(meta);
            metas.push(meta);
        }
        metas
    }

    fn build_legacy_source_meta(&self) -> SourceMetadata {
        if let Some(json_str) = self
            .loaded
            .call_metadata("beachcomber_provider_metadata".to_string())
        {
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
            let merged = build_source_meta_from_external(ov);
            SourceMetadata {
                name: meta.name.clone(),
                ..merged
            }
        } else {
            meta
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

        // Re-load the library into a fresh handle so each source has independent ownership.
        let lib_path = self.config.library_path.clone().unwrap_or_default();
        let loaded_arc: Arc<dyn LoadedLibrary> = match self.loader.load(lib_path) {
            Ok(lib) => Arc::from(lib),
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
                    loaded: Arc::clone(&loaded_arc),
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
    loaded: Arc<dyn LoadedLibrary>,
    meta: OnceLock<SourceMetadata>,
    meta_value: SourceMetadata,
}

impl Source for LibrarySource {
    fn metadata(&self) -> &SourceMetadata {
        self.meta.get_or_init(|| self.meta_value.clone())
    }

    fn execute(&self, path: Option<&str>) -> SourceResult {
        let path_str = path.map(|p| p.to_string());
        let json_str = match self.abi {
            LibraryAbi::MultiSource => self.loaded.call_source_execute(self.source_idx, path_str),
            LibraryAbi::Legacy => self
                .loaded
                .call_execute("beachcomber_provider_execute".to_string(), path_str),
        };
        let Some(s) = json_str else {
            return SourceResult::new();
        };
        parse_json_result(&s).unwrap_or_default()
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

fn fallback_source_meta(provider_name: &str, idx: usize) -> SourceMetadata {
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

fn parse_json_result(json_str: &str) -> Option<SourceResult> {
    let parsed: serde_json::Value = serde_json::from_str(json_str).ok()?;
    let obj = parsed.as_object()?;

    Some(SourceResult::from_json_object(obj))
}
