use beachcomber::provider::hostname::HostnameProvider;
use beachcomber::provider::user::UserProvider;
use beachcomber::provider::{FieldScope, InvalidationStrategy, Provider};

#[test]
fn hostname_provider_metadata() {
    let p = HostnameProvider;
    let meta = p.metadata();
    assert_eq!(meta.name, "hostname");
    assert_eq!(
        meta.inferred_scope(),
        FieldScope::Global,
        "hostname should be global"
    );
    assert!(
        matches!(meta.invalidation, InvalidationStrategy::Once),
        "hostname should use Once invalidation"
    );
    let field_names: Vec<&str> = meta.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(field_names.contains(&"name"), "Should have 'name' field");
    assert!(field_names.contains(&"short"), "Should have 'short' field");
}

#[test]
fn hostname_provider_executes() {
    let p = HostnameProvider;
    let (_, result) = p
        .execute(None)
        .into_iter()
        .next()
        .expect("hostname should always succeed");
    let name = result.get("name").expect("Should have 'name' field");
    assert!(!name.as_text().is_empty(), "Hostname should not be empty");
}

#[test]
fn hostname_provider_short_is_prefix() {
    let p = HostnameProvider;
    let (_, result) = p.execute(None).into_iter().next().unwrap();
    let name = result.get("name").unwrap().as_text();
    let short = result.get("short").unwrap().as_text();
    assert!(
        name.starts_with(&short),
        "Short hostname '{short}' should be a prefix of full hostname '{name}'",
    );
}

#[test]
fn hostname_provider_ignores_path() {
    let p = HostnameProvider;
    let (_, r1) = p.execute(None).into_iter().next().unwrap();
    let (_, r2) = p.execute(Some("/some/path")).into_iter().next().unwrap();
    assert_eq!(
        r1.get("name").unwrap().as_text(),
        r2.get("name").unwrap().as_text(),
        "Hostname should be the same regardless of path"
    );
}

#[test]
fn user_provider_metadata() {
    let p = UserProvider;
    let meta = p.metadata();
    assert_eq!(meta.name, "user");
    assert_eq!(
        meta.inferred_scope(),
        FieldScope::Global,
        "user should be global"
    );
    assert!(
        matches!(meta.invalidation, InvalidationStrategy::Once),
        "user should use Once invalidation"
    );
    let field_names: Vec<&str> = meta.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(field_names.contains(&"name"), "Should have 'name' field");
    assert!(field_names.contains(&"uid"), "Should have 'uid' field");
}

#[test]
fn user_provider_executes() {
    let p = UserProvider;
    let (_, result) = p
        .execute(None)
        .into_iter()
        .next()
        .expect("user should always succeed");
    let name = result.get("name").expect("Should have 'name' field");
    assert!(!name.as_text().is_empty(), "Username should not be empty");
    let uid = result.get("uid").expect("Should have 'uid' field");
    let uid_text = uid.as_text();
    let uid_num: i64 = uid_text.parse().expect("UID should be a number");
    assert!(uid_num >= 0, "UID should be non-negative");
}
