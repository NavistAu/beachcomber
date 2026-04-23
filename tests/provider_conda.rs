use beachcomber::provider::FieldScope;
use beachcomber::provider::Provider;
use beachcomber::provider::conda::CondaProvider;

#[test]
fn conda_provider_is_global_scoped() {
    let meta = CondaProvider.metadata();
    assert_eq!(
        meta.inferred_scope(),
        FieldScope::Global,
        "conda reads a shell-global env var; provider must be global-scoped"
    );
}

#[test]
fn conda_execute_works_without_path() {
    // Set the env var before executing.
    unsafe {
        std::env::set_var("CONDA_DEFAULT_ENV", "my-env");
    }
    let (_, result) = CondaProvider
        .execute(None)
        .into_iter()
        .next()
        .expect("should return Some");
    let env = result.get("env").expect("env field present");
    assert_eq!(env.as_text(), "my-env");
    unsafe {
        std::env::remove_var("CONDA_DEFAULT_ENV");
    }
}
