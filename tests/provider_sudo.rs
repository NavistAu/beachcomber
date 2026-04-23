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
    let results = p.execute(None);
    assert!(
        !results.is_empty(),
        "sudo provider should always return a result"
    );
    let (_, result) = results.into_iter().next().unwrap();
    let active = result.get("active");
    assert!(active.is_some(), "result should contain 'active' field");
}
