mod common;

/// The test process's own git — the git plugin runs in it, through
/// `RealGitExecutor` — reads no host configuration, as the fixtures' git
/// (`common::git::git_cmd`) does not: what a test sees must not depend on the
/// developer's `~/.gitconfig` or the system's.
#[ctor::ctor(unsafe)]
fn isolate_git_config() {
    // SAFETY: a constructor runs before `main`, while the process has one
    // thread.
    unsafe {
        std::env::set_var("GIT_CONFIG_GLOBAL", "/dev/null");
        std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
    }
}

/// A test process keeps its state and cache (`$XDG_STATE_HOME`,
/// `$XDG_CACHE_HOME`) in a directory of its own, removed when it exits: never
/// the developer's. A config load that migrates a file locks `migrate.lock`
/// and rewrites `migrated.json` in the state directory; shared, every test
/// process queued on the one lock and grew the developer's record by a
/// temp path each. A test about those directories sets its own.
#[ctor::ctor(unsafe)]
fn isolate_state_dirs() {
    let dir = std::env::temp_dir().join(format!("beachcomber-test-{}", std::process::id()));
    // SAFETY: a constructor runs before `main`, while the process has one
    // thread; the exit handler only removes the directory.
    unsafe {
        std::env::set_var("XDG_STATE_HOME", dir.join("state"));
        std::env::set_var("XDG_CACHE_HOME", dir.join("cache"));
        libc::atexit(remove_state_dirs);
    }
}

extern "C" fn remove_state_dirs() {
    let _ = std::fs::remove_dir_all(
        std::env::temp_dir().join(format!("beachcomber-test-{}", std::process::id())),
    );
}

/// A test process closes the pipes it was born holding. macOS has no `pipe2`:
/// nextest makes a test's output pipes and marks them close-on-exec in two
/// steps, and a test spawned between them inherits the other's ends. That
/// test is then reported leaky (LEAK-FAIL) until this one exits, seconds
/// later for a long test. Nothing else hands a test process a pipe.
#[cfg(target_os = "macos")]
#[ctor::ctor(unsafe)]
fn close_inherited_pipes() {
    let fds: Vec<i32> = std::fs::read_dir("/dev/fd")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse().ok())
        .filter(|&fd| fd > 2)
        .collect();
    for fd in fds {
        // SAFETY: a constructor runs before `main`, while nothing in this
        // process owns a descriptor above stderr but what it inherited; the
        // listing's own is closed by now, and `fstat` fails on it.
        unsafe {
            let mut stat: libc::stat = std::mem::zeroed();
            if libc::fstat(fd, &mut stat) == 0 && stat.st_mode & libc::S_IFMT == libc::S_IFIFO {
                libc::close(fd);
            }
        }
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
mod cli_get_verbose;
mod cli_get_wait_propagation;
mod cli_golden;
mod cli_migrate;
mod cli_paths;
mod cli_refresh;
mod cli_report;
mod cli_resolve;
mod cli_restart_older;
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
mod put_node;
mod put_persist;
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
mod template_log;
mod test_isolation;
mod underscore_field;
mod validity;
mod watch;
mod watch_registration;
