use beachcomber::provider::Provider;
use beachcomber::provider::op::OpProvider;
use beachcomber::provider::SourceScope;

#[test]
fn op_provider_metadata() {
    let p = OpProvider;
    let meta = p.metadata();
    assert_eq!(meta.name, "op");
    assert_eq!(meta.sources.len(), 1);
    let src = &meta.sources[0];
    assert_eq!(src.name, "vault");
    assert_eq!(src.scope, SourceScope::Global);
    let field_names: Vec<&str> = src.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(field_names.contains(&"signed_in"));
    assert!(field_names.contains(&"account"));
}

#[test]
fn op_provider_executes() {
    let p = OpProvider;
    let sources = p.sources();
    let result = sources[0].execute(None);
    assert!(
        result.fields.contains_key("signed_in"),
        "result should contain 'signed_in' field"
    );
    assert!(
        result.fields.contains_key("account"),
        "result should contain 'account' field"
    );
}
