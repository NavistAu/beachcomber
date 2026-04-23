use beachcomber::provider::FieldScope;
use beachcomber::provider::Provider;
use beachcomber::provider::aws::AwsProvider;
use beachcomber::provider::gcloud::GcloudProvider;
use beachcomber::provider::kubecontext::KubecontextProvider;

#[test]
fn kubecontext_provider_metadata() {
    let p = KubecontextProvider;
    let meta = p.metadata();
    assert_eq!(meta.name, "kubecontext");
    assert_eq!(meta.inferred_scope(), FieldScope::Global);
    let fields: Vec<&str> = meta.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(fields.contains(&"context"));
    assert!(fields.contains(&"namespace"));
}

#[test]
fn kubecontext_executes_without_panic() {
    let p = KubecontextProvider;
    let _ = p.execute(None); // May return None if kubectl not installed
}

#[test]
fn aws_provider_metadata() {
    let p = AwsProvider;
    let meta = p.metadata();
    assert_eq!(meta.name, "aws");
    assert_eq!(meta.inferred_scope(), FieldScope::Global);
    let fields: Vec<&str> = meta.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(fields.contains(&"profile"));
    assert!(fields.contains(&"region"));
}

#[test]
fn aws_executes_without_panic() {
    let p = AwsProvider;
    let _ = p.execute(None);
}

#[test]
fn gcloud_provider_metadata() {
    let p = GcloudProvider;
    let meta = p.metadata();
    assert_eq!(meta.name, "gcloud");
    assert_eq!(meta.inferred_scope(), FieldScope::Global);
    let fields: Vec<&str> = meta.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(fields.contains(&"project"));
    assert!(fields.contains(&"account"));
}

#[test]
fn gcloud_executes_without_panic() {
    let p = GcloudProvider;
    let _ = p.execute(None);
}
