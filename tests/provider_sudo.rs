use beachcomber::provider::Provider;
use beachcomber::provider::sudo::SudoProvider;

#[test]
fn sudo_provider_metadata() {
    let p = SudoProvider;
    let meta = p.metadata();
    assert_eq!(meta.name, "sudo");
    assert!(meta.global);
    assert_eq!(meta.fields.len(), 1);
    assert_eq!(meta.fields[0].name, "active");
}

#[test]
fn sudo_provider_executes() {
    let p = SudoProvider;
    let result = p.execute(None);
    assert!(
        result.is_some(),
        "sudo provider should always return a result"
    );
    let result = result.unwrap();
    let active = result.get("active");
    assert!(active.is_some(), "result should contain 'active' field");
}
