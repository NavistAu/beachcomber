use beachcomber::provider::uname::UnameProvider;
use beachcomber::provider::{InvalidationStrategy, Provider, SourceScope};

#[test]
fn uname_provider_metadata() {
    let p = UnameProvider;
    let meta = p.metadata();
    assert_eq!(meta.name, "uname");
    assert_eq!(meta.sources.len(), 1);
    let src = &meta.sources[0];
    assert_eq!(src.name, "system");
    assert_eq!(src.scope, SourceScope::Global);
    assert!(
        matches!(src.invalidation, InvalidationStrategy::Watch { .. }),
        "uname system source should use Watch invalidation"
    );
    let field_names: Vec<&str> = src.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(field_names.contains(&"sysname"));
    assert!(field_names.contains(&"release"));
    assert!(field_names.contains(&"version"));
    assert!(field_names.contains(&"machine"));
}

#[test]
fn uname_provider_executes() {
    let p = UnameProvider;
    let sources = p.sources();
    assert_eq!(sources.len(), 1);
    let result = sources[0].execute(None);
    let sysname = result.fields.get("sysname").expect("should have sysname field");
    let sysname_text = sysname.as_text();
    assert!(
        sysname_text == "Darwin" || sysname_text == "Linux",
        "sysname should be Darwin or Linux, got: {}",
        sysname_text
    );

    let machine = result.fields.get("machine").expect("should have machine field");
    let machine_text = machine.as_text();
    assert!(!machine_text.is_empty(), "machine should not be empty");

    let release = result.fields.get("release").expect("should have release field");
    assert!(!release.as_text().is_empty(), "release should not be empty");

    let version = result.fields.get("version").expect("should have version field");
    assert!(!version.as_text().is_empty(), "version should not be empty");
}
