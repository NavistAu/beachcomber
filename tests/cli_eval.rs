//! Tests for comb eval env.* injection and daemon-skip behavior.
//! These are unit-level tests on run_eval's logic; full e2e in e2e_providers.rs.

// These tests verify the CONTRACT, not the full end-to-end behavior.
// We test the helper functions directly.

use beachcomber::cli::format::find_eval_template_pairs;

#[test]
fn find_eval_template_pairs_finds_multi_ref_cascade_in_block() {
    // Guards against first-ref-only regression for block tags.
    // "{% if git.branch or user.name %}" must yield both pairs.
    let pairs = find_eval_template_pairs("{% if git.branch or user.name %}x{% endif %}");
    assert!(
        pairs.iter().any(|(p, f)| p == "git" && f == "branch"),
        "git.branch missing"
    );
    assert!(
        pairs.iter().any(|(p, f)| p == "user" && f == "name"),
        "user.name missing"
    );
}

#[test]
fn env_refs_in_template_identified() {
    // 'env' must appear as a provider name in template pairs for eval injection.
    let pairs = find_eval_template_pairs("{{ env.MY_VAR }}");
    assert!(
        pairs.iter().any(|(p, f)| p == "env" && f == "MY_VAR"),
        "env.MY_VAR must be found as a pair; got: {pairs:?}"
    );
}
