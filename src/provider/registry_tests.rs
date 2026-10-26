use crate::provider::registry::ProviderRegistry;
use crate::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    Source, SourceMetadata, SourceResult,
};

/// A provider of one Source, `s`, of one field, `f`, that answers nothing.
struct Named(&'static str);

fn named_meta() -> SourceMetadata {
    SourceMetadata {
        name: "s".into(),
        fields: vec![FieldSchema {
            name: "f".into(),
            field_type: FieldType::String,
        }],
        invalidation: InvalidationStrategy::Poll {
            interval_secs: 60,
            count: 1,
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

struct Nothing(SourceMetadata);

impl Source for Nothing {
    fn metadata(&self) -> &SourceMetadata {
        &self.0
    }

    fn execute(&self, _: Option<&str>) -> Result<SourceResult, String> {
        Ok(SourceResult::new())
    }
}

impl Provider for Named {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: self.0.into(),
            sources: vec![named_meta()],
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![Box::new(Nothing(named_meta()))]
    }
}

#[test]
fn registry_register_and_get() {
    let mut registry = ProviderRegistry::new();
    registry.register(Box::new(Named("a"))).unwrap();
    assert!(
        registry.provider_metadata("a").is_some(),
        "Should find registered provider"
    );
}

#[test]
fn registry_get_missing() {
    let registry = ProviderRegistry::new();
    assert!(
        registry.provider_metadata("nonexistent").is_none(),
        "Should return None for unknown provider"
    );
}

#[test]
fn registry_list_providers() {
    let mut registry = ProviderRegistry::new();
    registry.register(Box::new(Named("a"))).unwrap();
    registry.register(Box::new(Named("b"))).unwrap();
    let name_strings = registry.list();
    let mut names: Vec<&str> = name_strings.iter().map(|s| s.as_str()).collect();
    names.sort();
    assert!(names.contains(&"a"), "Should contain a");
    assert!(names.contains(&"b"), "Should contain b");
}

#[test]
fn registry_with_defaults_has_builtins() {
    let registry = ProviderRegistry::shipped(&std::env::vars().collect());
    assert!(
        registry.provider_metadata("hostname").is_some(),
        "Should have hostname provider"
    );
    assert!(
        registry.provider_metadata("user").is_some(),
        "Should have user provider"
    );
}

#[test]
fn registry_source_lookup() {
    let registry = ProviderRegistry::shipped(&std::env::vars().collect());
    let source = registry.source("hostname", "host");
    assert!(
        source.is_some(),
        "hostname provider should have 'host' source"
    );
}

#[test]
fn registry_execute_via_source() {
    // The instance at `~` runs in the daemon's `HOME`.
    let home = std::env::temp_dir().to_string_lossy().into_owned();
    let registry = ProviderRegistry::shipped(&[("HOME".to_string(), home)].into());
    let source = registry.source("hostname", "host").unwrap();
    let result = source.execute(None).unwrap();
    assert!(
        !result.fields.get("name").unwrap().as_text().is_empty(),
        "Hostname source should return a non-empty name"
    );
}

#[test]
fn registry_metadata() {
    let registry = ProviderRegistry::shipped(&std::env::vars().collect());
    let meta = registry.provider_metadata("hostname").unwrap();
    assert_eq!(meta.name, "hostname");
}

/// `is_registered` no longer gates `put` — a namespace is never refused
/// wholesale, only record by record. It survives as the guard that stops
/// `register_static` shadowing a real provider name.
#[test]
fn registry_is_registered_reports_provider_names() {
    let registry = ProviderRegistry::shipped(&std::env::vars().collect());
    assert!(registry.is_registered("hostname"));
    assert!(!registry.is_registered("nonexistent"));
    assert!(
        !registry.register_static("hostname"),
        "a builtin name cannot be claimed as a static namespace"
    );
}

#[test]
fn registry_sources_for_key() {
    let registry = ProviderRegistry::shipped(&std::env::vars().collect());
    // hostname.host source declares the "name" field
    assert_eq!(
        registry.sources_for_key("hostname", "name"),
        vec!["host"],
        "field 'name' on hostname should belong to source 'host'"
    );
}

/// A reload builds the next registry whole, carrying over only what belongs to
/// the daemon's life rather than to the configuration: the failure state, the
/// static namespaces `put` made (the same set for every registry). The kept
/// roots are not carried; the integration test `a_reload_clears_kept_roots`
/// covers that.
#[test]
fn an_adopted_registry_carries_what_outlives_the_configuration() {
    use crate::cache::Cache;
    use crate::config::Config;
    use std::sync::Arc;

    let cache = Arc::new(Cache::new());
    let config = Config::from_toml(
        "[daemon.providers.p.sources.s]\ndata = \"{{ 1 }}\"\nresult.f = \"{{ data }}\"\n",
    );
    let old = ProviderRegistry::with_config(&config, &std::env::vars().collect())
        .unwrap()
        .share(&cache);
    assert!(old.register_static("kept"));

    let next = ProviderRegistry::with_config(&config, &std::env::vars().collect())
        .unwrap()
        .adopt(&old, &cache);
    assert!(Arc::ptr_eq(&old.failures, &next.failures));
    assert!(next.is_static_namespace("kept"), "a put's namespace stays");
    assert!(next.register_static("later"));
    assert!(
        old.is_static_namespace("later"),
        "and a namespace made on either registry is seen by both"
    );
    assert!(next.is_registered("p"), "and the config is read afresh");
}

/// The definition of a config Source is its body, not its schedule: an edit to
/// `data`, `result` or `root` changes it, an edit to the `return` line does not.
#[test]
fn a_definition_is_the_body_and_not_the_schedule() {
    use crate::config::Config;
    let load = |data: &str, root: &str, poll: &str| {
        let root = if root.is_empty() {
            String::new()
        } else {
            format!("root = \"{root}\"\n")
        };
        ProviderRegistry::with_config(
            &Config::from_toml(&format!(
                "[daemon.providers.p.sources.s]\n{root}data = \"{data}\"\n\
                 result.f = \"{{{{ data }}}}\"\nreturn = \"{{{{ result | poll('{poll}') }}}}\"\n"
            )),
            &std::env::vars().collect(),
        )
        .unwrap()
    };
    let base = load("{{ 1 }}", "", "5s");
    let same = base.definition("p", "s");
    assert!(same.is_some());
    assert_eq!(load("{{ 1 }}", "", "9s").definition("p", "s"), same);
    assert_ne!(load("{{ 2 }}", "", "5s").definition("p", "s"), same);
    assert_ne!(
        load("{{ 1 }}", "{{ exec('pwd') }}", "5s").definition("p", "s"),
        same,
        "a changed root is a changed definition"
    );
    assert_eq!(base.definition("git", "head"), None, "a built-in has none");
}
