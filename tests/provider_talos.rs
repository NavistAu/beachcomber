use beachcomber::provider::talos::TalosProvider;
use beachcomber::provider::{InvalidationStrategy, Provider, Source, SourceScope, Value};
use tempfile::TempDir;

const TC: &str =
    "context: prod\ncontexts:\n    prod:\n        endpoints:\n            - 10.0.0.1\n";

fn src() -> Box<dyn Source> {
    TalosProvider.sources().into_iter().next().unwrap()
}

#[test]
fn talos_path_scoped_watch() {
    let sm = &TalosProvider.metadata().sources[0];
    assert_eq!(sm.scope, SourceScope::PathScoped);
    assert!(matches!(
        sm.invalidation,
        InvalidationStrategy::Watch { .. }
    ));
}

#[test]
fn talos_reads_active() {
    let d = TempDir::new().unwrap();
    let p = d.path().join("config");
    std::fs::write(&p, TC).unwrap();
    assert_eq!(
        src()
            .execute(Some(p.to_str().unwrap()))
            .fields
            .get("context"),
        Some(&Value::String("prod".into()))
    );
}

#[test]
fn talos_watched_files() {
    let d = TempDir::new().unwrap();
    let p = d.path().join("config");
    std::fs::write(&p, TC).unwrap();
    assert_eq!(src().watched_files(Some(p.to_str().unwrap())), vec![p]);
}

#[test]
fn talos_no_path_empty() {
    assert!(src().execute(None).fields.is_empty());
}

#[test]
fn talos_registered() {
    assert!(
        beachcomber::provider::registry::ProviderRegistry::with_defaults()
            .provider_names()
            .iter()
            .any(|n| n == "talos")
    );
}

#[test]
fn talos_ignores_indented_context_key() {
    let d = TempDir::new().unwrap();
    let p = d.path().join("config");
    // A context: line that is indented (inside a contexts block) must not be
    // mistaken for the top-level active context selector.
    let yaml = "context: active\ncontexts:\n    active:\n        context: nested\n";
    std::fs::write(&p, yaml).unwrap();
    assert_eq!(
        src()
            .execute(Some(p.to_str().unwrap()))
            .fields
            .get("context"),
        Some(&Value::String("active".into()))
    );
}

#[test]
fn talos_empty_when_no_context_key() {
    let d = TempDir::new().unwrap();
    let p = d.path().join("config");
    std::fs::write(
        &p,
        "contexts:\n    prod:\n        endpoints:\n            - 10.0.0.1\n",
    )
    .unwrap();
    assert!(src().execute(Some(p.to_str().unwrap())).fields.is_empty());
}

#[test]
fn talos_colon_joined_paths() {
    let d = TempDir::new().unwrap();
    let p1 = d.path().join("config1");
    let p2 = d.path().join("config2");
    // p1 has no context, p2 sets it — last wins.
    std::fs::write(&p1, "contexts:\n    a:\n        endpoints: []\n").unwrap();
    std::fs::write(&p2, "context: staging\n").unwrap();
    let path = format!("{}:{}", p1.to_str().unwrap(), p2.to_str().unwrap());
    assert_eq!(
        src().execute(Some(&path)).fields.get("context"),
        Some(&Value::String("staging".into()))
    );
}
