use beachcomber::provider::Provider;
use beachcomber::provider::conda::CondaProvider;

#[test]
fn conda_provider_is_global_scoped() {
    let meta = CondaProvider.metadata();
    assert!(
        meta.global,
        "conda reads a shell-global env var; provider must be global-scoped"
    );
}

#[test]
fn conda_execute_works_without_path() {
    // Set the env var before executing.
    unsafe {
        std::env::set_var("CONDA_DEFAULT_ENV", "my-env");
    }
    let result = CondaProvider.execute(None).expect("should return Some");
    let env = result.get("env").expect("env field present");
    assert_eq!(env.as_text(), "my-env");
    unsafe {
        std::env::remove_var("CONDA_DEFAULT_ENV");
    }
}
