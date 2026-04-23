use beachcomber::provider::uname::UnameProvider;
use beachcomber::provider::{InvalidationStrategy, Provider};

#[test]
fn uname_provider_metadata() {
    let p = UnameProvider;
    let meta = p.metadata();
    assert_eq!(meta.name, "uname");
    assert!(meta.global);
    assert!(matches!(meta.invalidation, InvalidationStrategy::Once));
    let field_names: Vec<&str> = meta.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(field_names.contains(&"sysname"));
    assert!(field_names.contains(&"release"));
    assert!(field_names.contains(&"version"));
    assert!(field_names.contains(&"machine"));
}

#[test]
fn uname_provider_executes() {
    let p = UnameProvider;
    let (_, result) = p
        .execute(None)
        .into_iter()
        .next()
        .expect("uname provider should return a result");
    let sysname = result.get("sysname").expect("should have sysname field");
    let sysname_text = sysname.as_text();
    assert!(
        sysname_text == "Darwin" || sysname_text == "Linux",
        "sysname should be Darwin or Linux, got: {}",
        sysname_text
    );

    let machine = result.get("machine").expect("should have machine field");
    let machine_text = machine.as_text();
    assert!(!machine_text.is_empty(), "machine should not be empty");

    let release = result.get("release").expect("should have release field");
    assert!(!release.as_text().is_empty(), "release should not be empty");

    let version = result.get("version").expect("should have version field");
    assert!(!version.as_text().is_empty(), "version should not be empty");
}
