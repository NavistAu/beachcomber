mod common;

/// The test process's own git — the git provider runs in it, through
/// `RealGitExecutor` — reads no host configuration, as the fixtures' git
/// (`common::git::git_cmd`) does not: what a test sees must not depend on the
/// developer's `~/.gitconfig` or the system's.
#[ctor::ctor]
fn isolate_git_config() {
    // SAFETY: a constructor runs before `main`, while the process has one
    // thread.
    unsafe {
        std::env::set_var("GIT_CONFIG_GLOBAL", "/dev/null");
        std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
    }
}

mod absent_fields;
mod arrays;
mod auto_shutdown;
mod binary_self_watch;
mod boundaries_http_real;
mod budget;
mod cache_facet;
mod cache_lifecycle;
mod check_subjects;
mod cli_durations;
mod cli_eval;
mod cli_exec;
mod cli_exit;
mod cli_ffi_equivalence;
mod cli_formats;
mod cli_get_multikey_force;
mod cli_get_path;
mod cli_get_sh_escape;
mod cli_get_variadic;
mod cli_get_wait_propagation;
mod cli_golden;
mod cli_paths;
mod cli_refresh;
mod cli_report;
mod cli_resolve;
mod cli_socket;
mod cli_status_rendered;
mod cli_style;
mod cli_virtual_fields;
mod cli_virtual_fields_kinded;
mod cli_watch;
mod client;
mod config_layers;
mod config_origins;
mod config_self_watch;
mod connection_context;
mod daemon;
mod daemon_exit_with_parent;
mod daemon_lifecycle;
mod daemon_singleton;
mod defaults;
mod demand;
mod e2e;
mod e2e_git;
mod e2e_providers;
mod exec_cache;
mod facets;
mod get_force;
mod get_template;
mod get_wait;
mod hello_op;
mod integration_scheduler;
mod json_value_conversion;
mod key_addressing;
mod kill_introspect_pid;
mod memo_facet;
mod memo_lifecycle;
mod no_sleep_in_tests;
mod path_chain;
mod plugin_real_ffi;
mod provider_conformance;
mod provider_git;
mod put_node;
mod put_remove;
mod put_ttl;
mod refresh;
mod scheduler;
mod scheduler_dedup;
mod scheduler_failure;
mod scheduler_timeout;
mod seed;
mod server;
mod shipped;
mod shutdown;
mod status_shape;
mod store;
mod test_isolation;
mod validity;
mod watch;
mod watch_registration;
