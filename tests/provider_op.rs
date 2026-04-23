use beachcomber::provider::Provider;
use beachcomber::provider::op::OpProvider;

#[test]
fn op_provider_metadata() {
    let p = OpProvider;
    let meta = p.metadata();
    assert_eq!(meta.name, "op");
    assert!(meta.global);
    assert_eq!(meta.fields.len(), 2);
    let field_names: Vec<&str> = meta.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(field_names.contains(&"signed_in"));
    assert!(field_names.contains(&"account"));
}

#[test]
fn op_provider_executes() {
    let p = OpProvider;
    let results = p.execute(None);
    assert!(
        !results.is_empty(),
        "op provider should always return a result"
    );
    let (_, result) = results.into_iter().next().unwrap();
    let signed_in = result.get("signed_in");
    assert!(
        signed_in.is_some(),
        "result should contain 'signed_in' field"
    );
    let account = result.get("account");
    assert!(account.is_some(), "result should contain 'account' field");
}
