use beachcomber::provider::Provider;
use beachcomber::provider::SourceScope;
use beachcomber::provider::conda::CondaProvider;

#[test]
fn conda_provider_is_global_scoped() {
    let meta = CondaProvider.metadata();
    assert_eq!(
        meta.sources[0].scope,
        SourceScope::Global,
        "conda reads a shell-global env var; source must be global-scoped"
    );
}

#[test]
fn conda_execute_works_without_path() {
    // Scope CONDA_DEFAULT_ENV to this test body; temp_env restores via Drop.
    temp_env::with_var("CONDA_DEFAULT_ENV", Some("my-env"), || {
        let sources = CondaProvider.sources();
        let result = sources[0].execute(None);
        let env = result.fields.get("env").expect("env field present");
        assert_eq!(env.as_text(), "my-env");
    });
}
