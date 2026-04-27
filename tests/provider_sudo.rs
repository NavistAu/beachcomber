use beachcomber::provider::Provider;
use beachcomber::provider::SourceScope;
use beachcomber::provider::sudo::SudoProvider;

#[test]
fn sudo_provider_metadata() {
    let p = SudoProvider;
    let meta = p.metadata();
    assert_eq!(meta.name, "sudo");
    assert_eq!(meta.sources.len(), 1);
    let src = &meta.sources[0];
    assert_eq!(src.name, "state");
    assert_eq!(src.scope, SourceScope::Global);
    assert_eq!(src.fields.len(), 1);
    assert_eq!(src.fields[0].name, "active");
}

#[test]
fn sudo_provider_executes() {
    let p = SudoProvider;
    let sources = p.sources();
    let result = sources[0].execute(None);
    // sudo state always returns the active field (true or false)
    assert!(
        result.fields.contains_key("active"),
        "result should contain 'active' field"
    );
}
