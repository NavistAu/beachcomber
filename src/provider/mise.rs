use crate::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    Source, SourceMetadata, SourceResult, Value, expand_abs_path,
};
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

pub struct MiseProvider;

/// Which of the tools `mise ls --current` reports an instance keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MiseFilter<'a> {
    /// A project root: what a config outside the global config dir sets.
    Project { root: &'a str },
    /// The instance at `~`, which is `home`: the global config's tools plus
    /// those a `mise.toml`/`.mise.toml` in `home` itself sets, the latter
    /// winning a clash. Configs in other directories belong to their own
    /// instances.
    Home { home: &'a str },
}

/// Run `mise ls --current --json` in `cwd` and return its stdout. The seam a
/// test replaces with a canned answer.
type Runner = Box<dyn Fn(&str) -> Result<String, String> + Send + Sync>;

fn run_mise(cwd: &str) -> Result<String, String> {
    let output = Command::new("mise")
        .args(["ls", "--current", "--json"])
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("mise: {e}"))?;
    if !output.status.success() {
        return Err("mise ls exited with an error".into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The tools `stdout` (the output of `mise ls --current --json`) reports that
/// `filter` keeps. `global_config_dir` is where mise's global config lives.
fn filter_tools(
    stdout: &str,
    global_config_dir: &str,
    filter: MiseFilter,
) -> Result<HashMap<String, Value>, String> {
    let parsed: serde_json::Value =
        serde_json::from_str(stdout).map_err(|e| format!("mise: unreadable output: {e}"))?;
    let obj = parsed
        .as_object()
        .ok_or_else(|| "mise: output is not an object".to_string())?;

    let global_dir = canonical(Some(Path::new(global_config_dir)));
    let mut global: HashMap<String, Value> = HashMap::new();
    let mut project: HashMap<String, Value> = HashMap::new();
    for (tool_name, versions) in obj {
        let Some(arr) = versions.as_array() else {
            continue;
        };
        for entry in arr {
            let Some(version) = entry.get("version").and_then(|v| v.as_str()) else {
                continue;
            };
            let source_path = entry
                .get("source")
                .and_then(|s| s.get("path"))
                .and_then(|p| p.as_str())
                .unwrap_or("");
            let version = Value::String(version.to_string());
            let config_dir = canonical(Path::new(source_path).parent());
            if config_dir.starts_with(&global_dir) {
                global.insert(tool_name.clone(), version);
            } else {
                let mine = match filter {
                    MiseFilter::Project { root } => {
                        let root = canonical(Some(Path::new(root)));
                        // The places mise reads a project's own config from.
                        MISE_CONFIG_DIRS
                            .iter()
                            .any(|sub| config_dir == root.join(sub))
                    }
                    MiseFilter::Home { home } => config_dir == canonical(Some(Path::new(home))),
                };
                if mine {
                    project.insert(tool_name.clone(), version);
                }
            }
        }
    }
    Ok(match filter {
        MiseFilter::Project { .. } => project,
        MiseFilter::Home { .. } => {
            global.extend(project);
            global
        }
    })
}

/// The directories under a project root that mise reads that project's own
/// config from; `""` is the root itself. A config anywhere else (an outer
/// directory's, say) belongs to the instance at that directory.
const MISE_CONFIG_DIRS: [&str; 5] = ["", ".mise", ".config", ".config/mise", "mise"];

/// `dir` with symlinks resolved, as mise may report a path through the real
/// directory where the root is named through a link (a `$HOME` of `/home`
/// pointing at `/var/home`). A directory that cannot be resolved is compared
/// as it is.
fn canonical(dir: Option<&Path>) -> std::path::PathBuf {
    let dir = dir.unwrap_or_else(|| Path::new(""));
    dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf())
}

fn global_config_dir() -> String {
    std::env::var("XDG_CONFIG_HOME")
        .or_else(|_| std::env::var("HOME").map(|h| format!("{h}/.config")))
        .map(|c| Path::new(&c).join("mise").to_string_lossy().to_string())
        .unwrap_or_default()
}

/// [`global_config_dir`], for the tests that build mise's own output.
#[cfg(test)]
pub(super) fn global_config_dir_for_tests() -> String {
    global_config_dir()
}

// ── SourceMetadata ────────────────────────────────────────────────────────────

fn tools_meta() -> SourceMetadata {
    let abs_paths = expand_abs_path("$XDG_CONFIG_HOME/mise")
        .map(|p| vec![p.to_string_lossy().to_string()])
        .unwrap_or_default();
    SourceMetadata {
        name: "tools".into(),
        fields: vec![FieldSchema {
            name: "<tool>".into(),
            field_type: FieldType::String,
        }],
        invalidation: InvalidationStrategy::Watch {
            patterns: vec!["mise.toml".into(), ".mise.toml".into()],
            abs_paths,
        },
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 60,
        },
        fsevents_reinstate: true,
        decay: None,
        budget: crate::scheduler::lifecycle::DEFAULT_BUDGET,
        timeout: crate::scheduler::lifecycle::DEFAULT_TIMEOUT,
        nearest: false,
    }
}

// ── Source ────────────────────────────────────────────────────────────────────

/// The tools mise sets: one Source, whose instance at a project root reports
/// that project's tools and whose instance at `~` reports the global ones and
/// those of a `mise.toml` in `$HOME` itself. A
/// read inside a project that misses a tool finds it at `~`.
pub(crate) struct MiseTools {
    run: Runner,
}

impl MiseTools {
    fn real() -> Self {
        Self::with_runner(Box::new(run_mise))
    }

    pub(crate) fn with_runner(run: Runner) -> Self {
        Self { run }
    }
}

impl Source for MiseTools {
    fn metadata(&self) -> &SourceMetadata {
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(tools_meta)
    }

    fn execute(&self, dir: Option<&str>) -> Result<SourceResult, String> {
        let Some(dir) = dir else {
            return Err("mise: no directory to run in".into());
        };
        // The instance at `~` runs in `$HOME`; a rooted one runs at its project.
        let filter = if crate::query::home_dir().is_some_and(|h| h.as_os_str() == dir) {
            MiseFilter::Home { home: dir }
        } else {
            MiseFilter::Project { root: dir }
        };
        let stdout = (self.run)(dir)?;
        let tools = filter_tools(&stdout, &global_config_dir(), filter)?;
        let mut result = SourceResult::new();
        for (tool, version) in tools {
            result.insert(tool, version);
        }
        Ok(result)
    }

    fn root(&self, dir: &str) -> Result<Option<String>, String> {
        Ok(find_mise_project_root(Path::new(dir)))
    }

    fn has_root(&self) -> bool {
        true
    }
}

// ── Provider ──────────────────────────────────────────────────────────────────

impl Provider for MiseProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "mise".into(),
            sources: vec![tools_meta()],
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![Box::new(MiseTools::real())]
    }
}

/// Walk upwards from `start` looking for a directory that contains either
/// `mise.toml` or `.mise.toml`. Returns the containing directory's path,
/// or `None` if no mise config is found before reaching the filesystem root.
fn find_mise_project_root(start: &Path) -> Option<String> {
    let mut cur: Option<&Path> = Some(start);
    while let Some(dir) = cur {
        if dir.join("mise.toml").exists() || dir.join(".mise.toml").exists() {
            return Some(dir.to_string_lossy().to_string());
        }
        cur = dir.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ls(entries: &[(&str, &str)]) -> String {
        let mut obj = serde_json::Map::new();
        for (tool, path) in entries {
            obj.insert(
                tool.to_string(),
                serde_json::json!([{"version": "1", "source": {"path": path}}]),
            );
        }
        serde_json::Value::Object(obj).to_string()
    }

    fn tools(stdout: &str, filter: MiseFilter) -> Vec<String> {
        let mut names: Vec<String> = filter_tools(stdout, "/nowhere/mise", filter)
            .unwrap()
            .into_keys()
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_project_instance_keeps_only_the_tools_its_own_files_set() {
        let out = ls(&[
            ("node", "/work/outer/mise.toml"),
            ("go", "/work/outer/proj/mise.toml"),
            ("ruby", "/work/outer/proj/.config/mise.toml"),
            ("bun", "/work/outer/proj/.mise/config.toml"),
            ("deno", "/work/outer/proj/sub/mise.toml"),
        ]);
        assert_eq!(
            tools(
                &out,
                MiseFilter::Project {
                    root: "/work/outer/proj"
                }
            ),
            vec!["bun", "go", "ruby"],
            "an outer project's tools come from the outer instance, up the chain"
        );
    }

    #[test]
    fn a_symlinked_home_still_owns_the_files_in_it() {
        let base = tempfile::tempdir().unwrap();
        let real = base.path().join("var-home");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("mise.toml"), "").unwrap();
        let link = base.path().join("home");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let canonical = real.canonicalize().unwrap();
        let out = ls(&[("node", canonical.join("mise.toml").to_str().unwrap())]);

        assert_eq!(
            tools(
                &out,
                MiseFilter::Home {
                    home: link.to_str().unwrap()
                }
            ),
            vec!["node"],
            "mise reports the resolved path; $HOME is the link"
        );
        assert_eq!(
            tools(
                &out,
                MiseFilter::Project {
                    root: link.to_str().unwrap()
                }
            ),
            vec!["node"]
        );
    }

    #[test]
    fn a_global_config_named_through_a_symlink_is_still_global() {
        let base = tempfile::tempdir().unwrap();
        let real = base.path().join("private-config");
        std::fs::create_dir_all(real.join("mise")).unwrap();
        std::fs::write(real.join("mise/config.toml"), "").unwrap();
        let link = base.path().join("config");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let reported = real.canonicalize().unwrap().join("mise/config.toml");
        let out = ls(&[("go", reported.to_str().unwrap())]);
        let global = link.join("mise");

        let names = |filter| -> Vec<String> {
            filter_tools(&out, global.to_str().unwrap(), filter)
                .unwrap()
                .into_keys()
                .collect()
        };
        assert_eq!(
            names(MiseFilter::Home { home: "/nowhere" }),
            vec!["go"],
            "mise reports the resolved path; XDG_CONFIG_HOME is the link"
        );
        assert!(names(MiseFilter::Project { root: "/nowhere" }).is_empty());
    }
}
