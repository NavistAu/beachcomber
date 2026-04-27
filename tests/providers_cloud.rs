use beachcomber::provider::Provider;
use beachcomber::provider::SourceScope;
use beachcomber::provider::aws::AwsProvider;
use beachcomber::provider::gcloud::GcloudProvider;
use beachcomber::provider::kubecontext::KubecontextProvider;

#[test]
fn kubecontext_provider_metadata() {
    let p = KubecontextProvider;
    let meta = p.metadata();
    assert_eq!(meta.name, "kubecontext");
    assert_eq!(meta.sources.len(), 1);
    let src = &meta.sources[0];
    assert_eq!(src.name, "context");
    assert_eq!(src.scope, SourceScope::Global);
    let fields: Vec<&str> = src.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(fields.contains(&"context"));
    assert!(fields.contains(&"namespace"));
}

#[test]
fn kubecontext_executes_without_panic() {
    let p = KubecontextProvider;
    let sources = p.sources();
    let _ = sources[0].execute(None); // May return empty if kubectl not installed
}

#[test]
fn aws_provider_metadata() {
    let p = AwsProvider;
    let meta = p.metadata();
    assert_eq!(meta.name, "aws");
    assert_eq!(meta.sources.len(), 1);
    let src = &meta.sources[0];
    assert_eq!(src.name, "profile");
    assert_eq!(src.scope, SourceScope::Global);
    let fields: Vec<&str> = src.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(fields.contains(&"profile"));
    assert!(fields.contains(&"region"));
}

#[test]
fn aws_executes_without_panic() {
    let p = AwsProvider;
    let sources = p.sources();
    let _ = sources[0].execute(None);
}

#[test]
fn gcloud_provider_metadata() {
    let p = GcloudProvider;
    let meta = p.metadata();
    assert_eq!(meta.name, "gcloud");
    assert_eq!(meta.sources.len(), 1);
    let src = &meta.sources[0];
    assert_eq!(src.name, "config");
    assert_eq!(src.scope, SourceScope::Global);
    let fields: Vec<&str> = src.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(fields.contains(&"project"));
    assert!(fields.contains(&"account"));
}

#[test]
fn gcloud_executes_without_panic() {
    let p = GcloudProvider;
    let sources = p.sources();
    let _ = sources[0].execute(None);
}
