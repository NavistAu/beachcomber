use crate::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    Source, SourceMetadata, SourceResult, Value,
};
use std::path::Path;

pub struct AsdfProvider;

impl Provider for AsdfProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "asdf".into(),
            sources: vec![tools_source_metadata()],
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![Box::new(AsdfTools)]
    }
}

fn tools_source_metadata() -> SourceMetadata {
    SourceMetadata {
        name: "tools".into(),
        fields: vec![FieldSchema {
            name: "<tool>".into(),
            field_type: FieldType::String,
        }],
        invalidation: InvalidationStrategy::Watch {
            patterns: vec![".tool-versions".into()],
            abs_paths: vec![],
        },
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 60,
        },
        decay: None,
        budget: crate::scheduler::lifecycle::DEFAULT_BUDGET,
        timeout: crate::scheduler::lifecycle::DEFAULT_TIMEOUT,
        nearest: false,
    }
}

struct AsdfTools;

impl Source for AsdfTools {
    fn metadata(&self) -> &SourceMetadata {
        use std::sync::OnceLock;
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(tools_source_metadata)
    }

    fn execute(&self, path: Option<&str>) -> Result<SourceResult, String> {
        let Some(path) = path else {
            return Err("no directory to run in".into());
        };
        let dir = Path::new(path);

        // Try to find a .tool-versions file: first walk from dir, then global fallback.
        let Some(content) = find_tool_versions_content(dir) else {
            return Ok(SourceResult::new());
        };

        let mut result = SourceResult::new();
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                // Emit primary version as flat field.
                // NOTE: multi-version fallback lists (e.g. "node 20.11.0 18.19.0") are
                // documented as a known limitation — only the primary version is emitted.
                result.insert(parts[0], Value::String(parts[1].to_string()));
            }
        }
        Ok(result)
    }

    fn root(&self, dir: &str) -> Result<Option<String>, String> {
        Ok(find_tool_versions_root(Path::new(dir)))
    }

    fn has_root(&self) -> bool {
        true
    }
}

/// Walk up from `start` looking for .tool-versions: the directory containing
/// it, or `None` if there is none (the global file is the `~` instance's, which
/// `find_tool_versions_content` falls back to).
fn find_tool_versions_root(start: &Path) -> Option<String> {
    let mut cur: Option<&Path> = Some(start);
    while let Some(dir) = cur {
        if dir.join(".tool-versions").exists() {
            return Some(dir.to_string_lossy().to_string());
        }
        cur = dir.parent();
    }
    None
}

/// Read the content of the first .tool-versions found, walking up from start.
/// Falls back to ~/.tool-versions and ~/.config/asdf/tool-versions if no local file found.
fn find_tool_versions_content(start: &Path) -> Option<String> {
    // Walk up from start.
    let mut cur: Option<&Path> = Some(start);
    while let Some(dir) = cur {
        let candidate = dir.join(".tool-versions");
        if candidate.exists() {
            return std::fs::read_to_string(&candidate).ok();
        }
        cur = dir.parent();
    }
    // Global fallback: ~/.tool-versions
    if let Ok(home) = std::env::var("HOME") {
        let global = Path::new(&home).join(".tool-versions");
        if global.exists()
            && let Ok(c) = std::fs::read_to_string(&global)
        {
            return Some(c);
        }
        // XDG alternative: ~/.config/asdf/tool-versions
        let xdg_global = Path::new(&home)
            .join(".config")
            .join("asdf")
            .join("tool-versions");
        if xdg_global.exists() {
            return std::fs::read_to_string(&xdg_global).ok();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::AsdfProvider;
    use crate::provider::{FieldType, Provider};

    #[test]
    fn asdf_tools_field_type_is_object() {
        let meta = AsdfProvider.metadata();
        // The sentinel field <tool> is declared in the source metadata
        let src = &meta.sources[0];
        // asdf uses a single String-typed sentinel field named "<tool>"
        assert_eq!(src.fields.len(), 1);
        assert!(matches!(src.fields[0].field_type, FieldType::String));
    }

    #[test]
    fn asdf_tools_emitted_as_flat_fields_not_object() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join(".tool-versions"),
            "node 20.11.0\npython 3.12.1\n",
        )
        .unwrap();
        let sources = AsdfProvider.sources();
        let result = sources[0]
            .execute(Some(d.path().to_str().unwrap()))
            .unwrap();

        // Fields should be flat: result.fields["node"] = "20.11.0"
        assert!(
            !result.fields.contains_key("tools"),
            "must not emit a nested 'tools' object; got keys: {:?}",
            result.fields.keys().collect::<Vec<_>>()
        );
        assert_eq!(
            result.fields.get("node").unwrap().as_text(),
            "20.11.0",
            "node should be a flat top-level field"
        );
        assert_eq!(result.fields.get("python").unwrap().as_text(), "3.12.1");
    }

    #[test]
    fn asdf_global_fallback_from_home_tool_versions() {
        // Simulate a directory with no .tool-versions and a fake HOME with one.
        let home_dir = tempfile::tempdir().unwrap();
        std::fs::write(home_dir.path().join(".tool-versions"), "ruby 3.2.0\n").unwrap();

        // Project dir: no .tool-versions, walk exhausts, falls back to HOME.
        let project_dir = tempfile::tempdir().unwrap();

        let result = temp_env::with_var("HOME", Some(home_dir.path().to_str().unwrap()), || {
            let sources = AsdfProvider.sources();
            sources[0]
                .execute(Some(project_dir.path().to_str().unwrap()))
                .unwrap()
        });

        assert_eq!(
            result.fields.get("ruby").unwrap().as_text(),
            "3.2.0",
            "global ~/.tool-versions should be used when no local .tool-versions found"
        );
    }

    #[test]
    fn asdf_existing_object_test_updated() {
        // After the fix, Value::Object must NOT appear — tools are flat fields.
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join(".tool-versions"),
            "node 20.11.0\npython 3.12.1\n",
        )
        .unwrap();
        let sources = AsdfProvider.sources();
        let result = sources[0]
            .execute(Some(d.path().to_str().unwrap()))
            .unwrap();
        assert!(!result.fields.contains_key("tools"), "no 'tools' object");
        assert!(result.fields.contains_key("node"));
        assert!(result.fields.contains_key("python"));
    }
}
