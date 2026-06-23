use beachcomber::cli::path_expr::evaluate_path;
use std::collections::HashMap;

fn env(p: &[(&str, &str)]) -> HashMap<String, String> {
    p.iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn empty_is_global() {
    assert_eq!(evaluate_path("\"\"", "/work", &env(&[])), None);
}

#[test]
fn cwd_returns_cwd() {
    assert_eq!(
        evaluate_path("cwd", "/work/p", &env(&[])),
        Some("/work/p".to_string())
    );
}

#[test]
fn env_default_when_unset() {
    assert_eq!(
        evaluate_path("env.KUBECONFIG or '/home/u/.kube/config'", "/w", &env(&[])),
        Some("/home/u/.kube/config".to_string())
    );
}

#[test]
fn env_used_when_set() {
    assert_eq!(
        evaluate_path(
            "env.KUBECONFIG or '/home/u/.kube/config'",
            "/w",
            &env(&[("KUBECONFIG", "/a:/b")])
        ),
        Some("/a:/b".to_string())
    );
}

#[test]
fn cascade_to_global() {
    assert_eq!(evaluate_path("env.NOPE or ''", "/w", &env(&[])), None);
}

#[test]
fn tilde_expansion_with_home() {
    // env.KUBECONFIG is unset → falls back to '~/.kube/config' → tilde expanded with HOME
    assert_eq!(
        evaluate_path(
            "env.KUBECONFIG or '~/.kube/config'",
            "/w",
            &env(&[("HOME", "/home/u")])
        ),
        Some("/home/u/.kube/config".to_string())
    );
}
