use beachcomber::cache::Cache;
use beachcomber::provider::{ProviderResult, Value};

#[test]
fn cache_get_missing_key() {
    let cache = Cache::new();
    let entry = cache.get("hostname", None);
    assert!(entry.is_none(), "Missing key should return None");
}

#[test]
fn cache_put_and_get_global() {
    let cache = Cache::new();
    let mut result = ProviderResult::new();
    result.insert("name", Value::String("myhost".to_string()));
    cache.put("hostname", None, result.clone());

    let entry = cache.get("hostname", None).unwrap();
    assert_eq!(
        entry.result.get("name").unwrap().as_text(),
        "myhost",
        "Should retrieve cached value"
    );
}

#[test]
fn cache_put_and_get_with_path() {
    let cache = Cache::new();
    let mut result = ProviderResult::new();
    result.insert("branch", Value::String("main".to_string()));
    cache.put("git", Some("/home/user/project"), result);

    let entry = cache.get("git", Some("/home/user/project")).unwrap();
    assert_eq!(entry.result.get("branch").unwrap().as_text(), "main");
}

#[test]
fn cache_different_paths_are_separate() {
    let cache = Cache::new();

    let mut r1 = ProviderResult::new();
    r1.insert("branch", Value::String("main".to_string()));
    cache.put("git", Some("/project-a"), r1);

    let mut r2 = ProviderResult::new();
    r2.insert("branch", Value::String("develop".to_string()));
    cache.put("git", Some("/project-b"), r2);

    let a = cache.get("git", Some("/project-a")).unwrap();
    let b = cache.get("git", Some("/project-b")).unwrap();
    assert_eq!(a.result.get("branch").unwrap().as_text(), "main");
    assert_eq!(b.result.get("branch").unwrap().as_text(), "develop");
}

#[test]
fn cache_entry_age_ms() {
    let cache = Cache::new();
    let mut result = ProviderResult::new();
    result.insert("name", Value::String("test".to_string()));
    cache.put("test", None, result);

    let entry = cache.get("test", None).unwrap();
    assert!(
        entry.age_ms() < 100,
        "Freshly cached entry should have small age"
    );
}

#[test]
fn cache_overwrite() {
    let cache = Cache::new();

    let mut r1 = ProviderResult::new();
    r1.insert("name", Value::String("old".to_string()));
    cache.put("test", None, r1);

    let mut r2 = ProviderResult::new();
    r2.insert("name", Value::String("new".to_string()));
    cache.put("test", None, r2);

    let entry = cache.get("test", None).unwrap();
    assert_eq!(
        entry.result.get("name").unwrap().as_text(),
        "new",
        "Cache should return the latest value"
    );
}

#[test]
fn cache_remove() {
    let cache = Cache::new();
    let mut result = ProviderResult::new();
    result.insert("name", Value::String("test".to_string()));
    cache.put("test", None, result);

    cache.remove("test", None);
    assert!(
        cache.get("test", None).is_none(),
        "Removed entry should be gone"
    );
}

#[test]
fn cache_entry_count() {
    let cache = Cache::new();
    assert_eq!(cache.len(), 0, "Empty cache should have 0 entries");

    cache.put("a", None, ProviderResult::new());
    cache.put("b", None, ProviderResult::new());
    assert_eq!(cache.len(), 2, "Cache should have 2 entries");
}
