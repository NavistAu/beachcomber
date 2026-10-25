use crate::config::{Config, parse_duration};
use crate::provider::registry::ProviderRegistry;
use std::path::PathBuf;
use std::time::Duration;
use tempfile::TempDir;

// ── Parsing the settings ─────────────────────────────────────────────────────

/// A `Config` composed from one layer that has no file, labelled
/// `config(<in memory>)`: what a test builds when it wants the checks to run.
fn in_memory(text: &str) -> Config {
    Config::from_toml(text)
}

use tracing_subscriber::filter::LevelFilter;

struct ConfigParseCase {
    name: &'static str,
    input: &'static str,
    check: fn(&Config),
}

#[test]
fn config_parse_cases() {
    let cases: &[ConfigParseCase] = &[
        ConfigParseCase {
            name: "parse_minimal_toml",
            input: "",
            check: |c| {
                assert_eq!(c.daemon().log_level, LevelFilter::INFO);
                assert_eq!(c.daemon().idle_shutdown(), None);
            },
        },
        ConfigParseCase {
            name: "parse_daemon_section",
            input: r#"
[daemon]
log_level = "debug"
socket_path = "/tmp/test.sock"
idle_shutdown = "30s"
timeout = "5s"
"#,
            check: |c| {
                assert_eq!(c.daemon().log_level, LevelFilter::DEBUG);
                assert_eq!(c.daemon().socket_path.as_deref(), Some("/tmp/test.sock"));
                assert_eq!(c.daemon().idle_shutdown(), Some(Duration::from_secs(30)));
                assert_eq!(c.daemon().timeout(), Duration::from_secs(5));
            },
        },
    ];

    for case in cases {
        let config = in_memory(case.input);
        (case.check)(&config);
        assert_eq!(config.errors(), Vec::<String>::new(), "case {}", case.name);
    }
}

#[test]
fn default_config() {
    let config = Config::default();
    assert_eq!(
        config.daemon().log_level,
        LevelFilter::INFO,
        "Default log level should be info"
    );
    assert!(
        config.daemon().socket_path.is_none(),
        "Default socket path should be None"
    );
}

#[test]
fn socket_path_resolves_xdg_default() {
    let path = Config::default_socket_path();
    assert!(
        path.to_string_lossy().contains("beachcomber"),
        "Socket path should include 'beachcomber': {path:?}",
    );
    assert!(
        path.to_string_lossy().ends_with("sock"),
        "Socket path should end with 'sock': {path:?}",
    );
}

#[test]
fn socket_path_override() {
    let config = Config::from_toml("[daemon]\nsocket_path = \"/tmp/custom.sock\"\n");
    let path = config.resolve_socket_path();
    assert_eq!(
        path.to_string_lossy(),
        "/tmp/custom.sock",
        "Explicit socket path should be used"
    );
}

#[test]
fn log_path_resolves_xdg() {
    let config = Config::default();
    let path = config.resolve_log_path();
    assert!(
        path.to_string_lossy().contains("beachcomber"),
        "Log path should include 'beachcomber': {path:?}",
    );
}

/// One grammar for every duration: a whole number with an optional unit,
/// `ms`, `s`, `m` or `h`, a bare number being seconds. Zero is zero in every
/// unit; what a consumer cannot take (a zero interval, a sub-second ttl) is
/// that consumer's to refuse.
#[test]
fn one_duration_grammar_and_zero_is_zero_in_every_unit() {
    for zero in ["0", "0s", "0ms", "0m", "0h"] {
        assert_eq!(parse_duration(zero), Some(Duration::ZERO), "{zero}");
    }
    for bad in ["1m30s", "5S", "1d", "x", "-1s", "1.5s", "ms"] {
        assert_eq!(parse_duration(bad), None, "{bad}");
    }
    let err = crate::config::duration_arg("1m30s").unwrap_err();
    assert!(err.contains(crate::config::DURATION_GRAMMAR), "{err}");
}

#[test]
fn parse_duration_variants() {
    assert_eq!(parse_duration("500ms"), Some(Duration::from_millis(500)));
    assert_eq!(parse_duration("5s"), Some(Duration::from_secs(5)));
    assert_eq!(parse_duration("2m"), Some(Duration::from_secs(120)));
    assert_eq!(parse_duration("1h"), Some(Duration::from_secs(3600)));
    assert_eq!(parse_duration("30"), Some(Duration::from_secs(30)));
    assert_eq!(parse_duration(""), None);
}

// ── Layers, facet tables, and the settings/facets split (Task 6) ─────────────

fn write(dir: &std::path::Path, rel: &str, body: &str) -> std::path::PathBuf {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, body).unwrap();
    path
}

#[test]
fn layers_are_defaults_then_the_main_file_then_conf_d_in_lexical_order() {
    let tmp = tempfile::tempdir().unwrap();
    let main = write(
        tmp.path(),
        "config.toml",
        "[daemon]\nlog_level = \"debug\"\n",
    );
    let b = write(
        tmp.path(),
        "conf.d/20-b.toml",
        "[daemon]\nlog_level = \"warn\"\n",
    );
    let a = write(
        tmp.path(),
        "conf.d/10-a.toml",
        "[daemon]\nlog_level = \"trace\"\n",
    );
    let python = write(
        tmp.path(),
        "defaults/python.toml",
        "[daemon]\nlog_level = \"error\"\n",
    );
    let aws = write(
        tmp.path(),
        "defaults/aws.toml",
        "[daemon]\nlog_level = \"info\"\n",
    );

    let read = Config::read_layers(
        Some(&tmp.path().join("defaults")),
        Some(&main),
        &tmp.path().join("conf.d"),
    );
    assert!(read.errors.is_empty(), "{:?}", read.errors);
    let layers = read.layers;
    let paths: Vec<_> = layers.iter().map(|(p, _)| p.clone()).collect();
    assert_eq!(
        paths,
        vec![aws, python, main, a, b],
        "layers compose defaults lexically, then the main file, then conf.d lexically"
    );

    // Each file is parsed on its own, not merged: an early layer still holds
    // the value the later ones override.
    assert_eq!(
        layers[0].1["daemon"]["log_level"].as_str(),
        Some("info"),
        "a layer keeps its own value; composition happens later"
    );
    assert_eq!(layers[2].1["daemon"]["log_level"].as_str(), Some("debug"));
    assert_eq!(layers[4].1["daemon"]["log_level"].as_str(), Some("warn"));
}

#[test]
fn layers_skip_a_file_that_does_not_parse() {
    let tmp = tempfile::tempdir().unwrap();
    let main = write(tmp.path(), "config.toml", "[daemon]\n");
    write(
        tmp.path(),
        "conf.d/10-broken.toml",
        "this is not [ valid toml",
    );
    let ok = write(tmp.path(), "conf.d/20-ok.toml", "[daemon]\n");

    let read = Config::read_layers(None, Some(&main), &tmp.path().join("conf.d"));
    let paths: Vec<_> = read.layers.into_iter().map(|(p, _)| p).collect();
    assert_eq!(paths, vec![main, ok], "the broken drop-in is not a layer");
    assert_eq!(
        read.errors.len(),
        1,
        "but it is an error: {:?}",
        read.errors
    );
    assert!(
        read.errors[0].starts_with("config(") && read.errors[0].contains("10-broken.toml): "),
        "{:?}",
        read.errors
    );
}

#[test]
fn layers_tolerate_a_missing_main_config_file() {
    let tmp = tempfile::tempdir().unwrap();
    let drop_in = write(tmp.path(), "conf.d/10-only.toml", "[daemon]\n");
    let read = Config::read_layers(
        Some(&tmp.path().join("defaults")),
        Some(&tmp.path().join("config.toml")),
        &tmp.path().join("conf.d"),
    );
    assert!(read.errors.is_empty(), "{:?}", read.errors);
    let paths: Vec<_> = read.layers.into_iter().map(|(p, _)| p).collect();
    assert_eq!(
        paths,
        vec![drop_in],
        "a missing main file and a missing defaults directory are both nothing"
    );
}

#[test]
fn settings_only_strips_both_providers_roots() {
    let composed: toml::Value = toml::from_str(
        r#"
[daemon]
log_level = "debug"

[daemon.providers.git.cache._]
decay = 2

[daemon.providers.git.sources.diff]
return = "{{ result | poll('5s') }}"

[client]
bogus = 1

[client.providers.git]
template.branch = "{{ git.branch }}"
"#,
    )
    .unwrap();

    let settings = Config::settings_only(composed);
    assert!(
        settings["client"].get("providers").is_none(),
        "[client.providers] is not a setting"
    );
    assert_eq!(
        settings["client"]["bogus"].as_integer(),
        Some(1),
        "any other [client] key is, so a change to it restarts"
    );
    assert!(
        settings.get("providers").is_none(),
        "only the two roots are kept"
    );
    assert!(
        settings["daemon"].get("providers").is_none(),
        "nothing under [daemon.providers] is a setting, Sources included"
    );
    assert_eq!(settings["daemon"]["log_level"].as_str(), Some("debug"));
}

#[test]
fn config_label_writes_home_as_tilde() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().canonicalize().unwrap();
    // SAFETY: nextest gives every test its own process.
    unsafe { std::env::set_var("HOME", &home) };

    assert_eq!(
        Config::config_label(&home.join(".config/beachcomber/conf.d/10-mine.toml")),
        "config:~/.config/beachcomber/conf.d/10-mine.toml"
    );
    assert_eq!(
        Config::config_label(std::path::Path::new("/etc/beachcomber/config.toml")),
        "config:/etc/beachcomber/config.toml",
        "a path outside $HOME is written in full"
    );
}

// ── Seeded-table validation (one file at a time) ──────────────────────────────────────────────────

fn seeded_errors(toml_str: &str) -> Vec<String> {
    let layer: toml::Value = toml::from_str(toml_str).expect("valid toml");
    crate::shape::seeded::read(std::path::Path::new("/f.toml"), &layer)
        .errors
        .into_iter()
        .map(|e| {
            e.strip_prefix("config(/f.toml): ")
                .unwrap_or_else(|| panic!("an error carries its file's label: {e}"))
                .to_string()
        })
        .collect()
}

#[test]
fn plugin_path_is_a_list_of_directories() {
    use std::path::PathBuf;
    let dirs = |c: &Config, home: Option<&str>| c.plugin_path(home).unwrap();
    let config = Config::from_toml("");

    // No config file, nothing named: nothing is searched.
    assert_eq!(dirs(&config, Some("/h")), Vec::<PathBuf>::new());

    // The default is the `plugins` directory beside the config file.
    let config = Config::load_from(std::path::Path::new("/c/beachcomber/config.toml"));
    assert_eq!(
        dirs(&config, Some("/h")),
        vec![PathBuf::from("/c/beachcomber/plugins")]
    );

    // A list replaces the default, keeps its order, and expands `~`.
    let config = Config::from_toml("[daemon.plugin]\npath = [\"~/plugins\", \"/opt/p\", \"~\"]\n");
    assert_eq!(
        dirs(&config, Some("/h")),
        vec![
            PathBuf::from("/h/plugins"),
            PathBuf::from("/opt/p"),
            PathBuf::from("/h")
        ]
    );

    // An empty list searches nothing.
    let config = Config::from_toml("[daemon.plugin]\npath = []\n");
    assert_eq!(dirs(&config, Some("/h")), Vec::<PathBuf>::new());

    // `~` with no HOME to expand it from is an error, not a relative path.
    let config = Config::from_toml("[daemon.plugin]\npath = [\"~/plugins\"]\n");
    let err = config.plugin_path(None).unwrap_err();
    assert!(
        err.contains("~/plugins") && err.contains("HOME") && err.contains("[daemon.plugin] path"),
        "{err}"
    );

    // The list is checked per file: well-formed ones are accepted...
    assert_eq!(
        seeded_errors("[daemon.plugin]\npath = [\"~/plugins\", \"/opt/p\"]\n"),
        Vec::<String>::new()
    );
    // ...and an entry that is neither absolute nor under `~`, or is empty, is
    // an error.
    assert_eq!(
        seeded_errors("[daemon.plugin]\npath = [\"plugins\", \"\"]\n"),
        vec![
            "[daemon.plugin] path entry 'plugins' must be an absolute directory or start with ~/"
                .to_string(),
            "[daemon.plugin] path has an empty entry".to_string(),
        ]
    );
}

#[test]
fn an_unknown_key_under_plugin_is_an_error() {
    assert_eq!(
        seeded_errors("[daemon.plugin]\ndirs = [\"/x\"]\n"),
        vec!["unknown key 'dirs' in [daemon.plugin]".to_string()]
    );
    // The wrong type, at each level.
    assert_eq!(
        seeded_errors("[daemon]\nplugin = 3\n"),
        vec!["[daemon] plugin must be a table".to_string()]
    );
    assert_eq!(
        seeded_errors("[daemon.plugin]\npath = \"x\"\n"),
        vec!["[daemon.plugin] path must be a list of directory strings".to_string()]
    );
    assert_eq!(
        seeded_errors("[daemon.plugin]\npath = [\"/x\", 3]\n"),
        vec!["[daemon.plugin] path must be a list of directory strings".to_string()]
    );
    // A wrong type still loads (the file check reports it, not a silent
    // fall back to the default configuration): the rest of the file holds.
    let config = in_memory("[daemon]\nlog_level = \"debug\"\nplugin = 3\n");
    assert_eq!(config.daemon().log_level, LevelFilter::DEBUG);
    assert_eq!(
        config.errors(),
        vec!["config(<in memory>): [daemon] plugin must be a table".to_string()]
    );
}

#[test]
fn unknown_key_under_client_is_an_error() {
    assert_eq!(
        seeded_errors("[client]\nbogus = 1\n"),
        vec!["unknown key 'bogus' in [client]".to_string()]
    );
    assert_eq!(
        seeded_errors("[client.providers.x]\nenabled = false\n"),
        vec!["unknown key 'enabled' in [client.providers.x]".to_string()]
    );
}

/// A quoted key holding a `.` is one name, read as a quoted segment
/// (`x['a.b']`), at any depth.
#[test]
fn a_quoted_template_key_holding_a_dot_is_one_name() {
    assert!(seeded_errors("[client.providers.x]\ntemplate.\"a.b\" = \"{{ y }}\"\n").is_empty());
    assert!(
        seeded_errors("[client.providers.x.template.group]\n\"c.d\" = \"{{ y }}\"\n").is_empty()
    );
}

#[test]
fn a_non_string_template_leaf_is_an_error() {
    assert_eq!(
        seeded_errors("[client.providers.x]\ntemplate.width = 12\n"),
        vec![
            "[client.providers.x] template 'width' is integer, not a value expression or a group."
                .to_string()
        ]
    );
}

#[test]
fn enabled_must_be_a_bool() {
    for bad in ["\"no\"", "0", "[true]", "{ a = 1 }"] {
        let errors = seeded_errors(&format!("[daemon.providers.git]\nenabled = {bad}\n"));
        assert_eq!(errors.len(), 1, "{bad}: {errors:?}");
        assert!(
            errors[0].starts_with("[daemon.providers.git] enabled must be true or false, not "),
            "{bad}: {errors:?}"
        );
    }
    for good in ["true", "false"] {
        assert_eq!(
            seeded_errors(&format!("[daemon.providers.git]\nenabled = {good}\n")),
            Vec::<String>::new()
        );
    }
}

#[test]
fn enabled_on_the_abstract_provider_is_an_error() {
    for value in ["false", "true", "\"no\""] {
        assert_eq!(
            seeded_errors(&format!("[daemon.providers._]\nenabled = {value}\n")),
            vec![
                "[daemon.providers._] enabled: the abstract provider applies to every provider: \
                 name the provider, [daemon.providers.<p>] enabled = false"
                    .to_string()
            ],
            "{value}"
        );
    }
}

#[test]
fn a_non_integer_decay_is_an_error() {
    for (bad, shown) in [
        ("\"4\"", "\"4\""),
        ("0", "0"),
        ("-1", "-1"),
        ("17", "17"),
        ("1.5", "1.5"),
        ("true", "true"),
    ] {
        assert_eq!(
            seeded_errors(&format!("[daemon.providers.git.cache._]\ndecay = {bad}\n")),
            vec![format!(
                "git:cache._.decay must be an integer from 1 to 16 (got {shown})"
            )],
            "decay = {bad}"
        );
    }
    assert_eq!(
        seeded_errors("[daemon.providers._.cache._]\ndecay = 0\n"),
        vec!["*:cache._.decay must be an integer from 1 to 16 (got 0)".to_string()],
        "the abstract provider is named *"
    );
    for ok in [1, 4, 16] {
        assert!(
            seeded_errors(&format!("[daemon.providers.git.cache._]\ndecay = {ok}\n")).is_empty()
        );
    }
}

#[test]
fn only_decay_is_accepted_under_a_cache_prototype() {
    assert_eq!(
        seeded_errors("[daemon.providers.git]\nenabled_ = true\n"),
        vec!["unknown key 'enabled_' in [daemon.providers.git]".to_string()]
    );
    assert_eq!(
        seeded_errors("[daemon.providers.git.cache]\npoll = 5\n"),
        vec!["unknown key 'poll' in [daemon.providers.git.cache]".to_string()]
    );
    assert_eq!(
        seeded_errors("[daemon.providers.git.cache._]\npoll = 5\n"),
        vec!["unknown key 'poll' in [daemon.providers.git.cache._]".to_string()]
    );
}

const PROVIDERS_MOVED: &str = "unknown table [providers]: a provider's settings are \
     [daemon.providers.<p>] (the daemon's) and [client.providers.<p>] (the client's); a \
     built-in Source is retuned by [daemon.providers.<p>.sources.<s>] return; templates live \
     in [client.providers.<p>] template.<leaf>, decay in [daemon.providers.<p>.cache._]";

#[test]
fn an_old_colon_table_is_an_unknown_table() {
    for name in ["x:template", "*:cache", "a:b:c", "x:put"] {
        assert_eq!(
            seeded_errors(&format!("[providers.\"{name}\"]\nn = \"1\"\n")),
            vec![PROVIDERS_MOVED.to_string()]
        );
    }
}

#[test]
fn provider_names_hold_no_colon_star_or_nothing_and_the_prototype_is_not_a_client_provider() {
    let errors = seeded_errors("[client.providers.\"a:b\"]\ntemplate.x = \"1\"\n");
    assert!(errors.iter().any(|e| e.contains("'a:b'")), "{errors:?}");
    let errors = seeded_errors("[client.providers._]\ntemplate.x = \"1\"\n");
    assert!(errors.iter().any(|e| e.contains("'_'")), "{errors:?}");
    let errors = seeded_errors("[daemon.providers.\"*\"]\n");
    assert!(errors.iter().any(|e| e.contains("'*'")), "{errors:?}");
    assert!(seeded_errors("[daemon.providers._.cache._]\ndecay = 4\n").is_empty());
}

/// `env` and `comb` mean something of their own in every key and template,
/// so neither root may name a provider after them — the same rule, in the
/// same words, as a `put` or a key.
#[test]
fn a_reserved_word_is_no_provider_name_in_either_root() {
    for (toml, name) in [
        ("[client.providers.env]\ntemplate.x = \"1\"\n", "env"),
        ("[client.providers.comb]\ntemplate.y = \"2\"\n", "comb"),
        (
            "[daemon.providers.env.sources.s]\ndata = \"{{ 1 }}\"\nresult = \"{{ {'a': data} }}\"\n",
            "env",
        ),
        ("[daemon.providers.comb]\nenabled = false\n", "comb"),
    ] {
        let errors = seeded_errors(toml);
        assert!(
            errors
                .iter()
                .any(|e| e.contains(&format!("'{name}' is reserved"))),
            "{toml}: {errors:?}"
        );
    }
}

#[test]
fn an_error_names_its_file_and_each_file_is_checked_on_its_own() {
    // The merge of these two is valid (the later string wins the leaf), but
    // seeding is per file, so the first file's integer is an error.
    let bad: toml::Value = toml::from_str("[client.providers.zz]\ntemplate.width = 12\n").unwrap();
    let good: toml::Value =
        toml::from_str("[client.providers.zz]\ntemplate.width = \"{{ 1 }}\"\n").unwrap();
    let errors = Config::validate_layers(&[
        (std::path::PathBuf::from("/a.toml"), bad.clone()),
        (std::path::PathBuf::from("/b.toml"), good),
        (std::path::PathBuf::from("/c.toml"), bad),
    ]);
    assert_eq!(
        errors,
        vec![
            "config(/a.toml): [client.providers.zz] template 'width' is integer, not a value \
             expression or a group."
                .to_string(),
            "config(/c.toml): [client.providers.zz] template 'width' is integer, not a value \
             expression or a group."
                .to_string(),
        ]
    );
}

#[test]
fn an_empty_key_in_a_template_group_names_the_group() {
    assert_eq!(
        seeded_errors("[client.providers.x.template.g]\n\"\" = \"e\"\n"),
        vec!["[client.providers.x] template.g has an empty key".to_string()]
    );
}

#[test]
fn an_empty_template_key_is_an_error() {
    assert_eq!(
        seeded_errors("[client.providers.x]\ntemplate.\"\" = \"e\"\n"),
        vec!["[client.providers.x] template has an empty key".to_string()]
    );
}

#[test]
fn a_file_seeds_only_what_validated() {
    let layer: toml::Value = toml::from_str(
        "[client.providers.bad]\ntemplate.n = 1\n\n[client.providers.ok]\ntemplate.n = \"{{ 1 }}\"\n\n\
         [daemon.providers.git.cache._]\ndecay = 0\n\n[daemon.providers._.cache._]\ndecay = 3\n",
    )
    .unwrap();
    let read = crate::shape::seeded::read(std::path::Path::new("/f.toml"), &layer);
    let mut namespaces: Vec<&str> = read.writes.iter().map(|(n, _)| n.as_str()).collect();
    namespaces.sort();
    assert_eq!(
        namespaces,
        ["*:cache"],
        "a template is the provider's template line's to seed, not the file check's"
    );
    assert_eq!(read.errors.len(), 2);
}

#[test]
fn a_client_that_is_not_a_table_is_an_error() {
    assert_eq!(
        seeded_errors("client = \"oops\"\n"),
        vec!["[client] must be a table".to_string()]
    );
}

// ── The roots, the bare [daemon] keys and what was removed ───────────────────

/// A `Config` composed from the shipped `00-comb.toml` and then `user`, as the
/// daemon composes defaults and `config.toml`: two layers, each with its own
/// label, and no `config_path` (so nothing is read or seeded from disk).
pub(crate) fn with_shipped_defaults(user: &str) -> Config {
    Config::from_layers(vec![
        crate::daemon::defaults::comb_layer(),
        (
            PathBuf::from("/user.toml"),
            toml::from_str(user).expect("valid toml"),
        ),
    ])
}

/// The load errors of `user` composed over the shipped defaults, against the
/// built-in providers.
fn source_errors(user: &str) -> Vec<String> {
    let config = with_shipped_defaults(user);
    ProviderRegistry::with_config(&config, &Default::default())
        .err()
        .unwrap_or_default()
}

/// Every load error names the file that wrote what it is about, then its
/// table, in one form: a Source's lint and its calls as much as the file
/// check.
#[test]
fn a_load_error_names_its_file_and_its_table() {
    assert_eq!(
        source_errors(
            "[daemon.providers.p.sources.s]\ndata = \"{{ nope }}\"\nresult.x = \"{{ data }}\"\n"
        ),
        vec![
            "config(/user.toml): [daemon.providers.p.sources.s] data: unknown name 'nope' (this \
             line sees env, comb, sources, daemon, _)"
                .to_string()
        ]
    );
    assert_eq!(
        source_errors("[daemon.providers.p.sources.s]\nresult.x = \"{{ 1 }}\"\n"),
        vec!["config(/user.toml): [daemon.providers.p.sources.s] has no data line".to_string()]
    );
    assert_eq!(
        source_errors(
            "[daemon.providers.p.sources.s]\ndata = \"{{ daemon.providers.q.sources.t() }}\"\n\
             result.x = \"{{ data }}\"\n"
        ),
        vec![
            "config(/user.toml): [daemon.providers.p.sources.s] data: daemon.providers.q: no \
             such provider"
                .to_string()
        ]
    );
}

/// The daemon's refusal and `comb check config` list everything wrong at
/// once: a mistyped setting hides no Source, and a broken Source hides no
/// other.
#[test]
fn every_stage_reports_at_once() {
    let errors = source_errors(
        "[daemon]\nsocket_path = 3\n\
         [daemon.providers.p.sources.s]\nresult.x = \"{{ 1 }}\"\n\
         [daemon.providers.q.sources.t]\ndata = \"{{ nope }}\"\nresult.x = \"{{ data }}\"\n",
    );
    for want in [
        "config(/user.toml): [daemon] socket_path must be a string",
        "config(/user.toml): [daemon.providers.p.sources.s] has no data line",
        "config(/user.toml): [daemon.providers.q.sources.t] data: unknown name 'nope'",
    ] {
        assert!(
            errors.iter().any(|e| e.starts_with(want)),
            "{want}: {errors:#?}"
        );
    }
}

fn shipped_registry(user: &str) -> ProviderRegistry {
    let config = with_shipped_defaults(user);
    ProviderRegistry::with_config(&config, &Default::default())
        .unwrap_or_else(|e| panic!("loads: {e:?}"))
}

#[test]
fn fences_accept_a_user_name_in_every_fenced_position() {
    // A provider named `poll`, a Source named `return`, a field named
    // `sources`, a template leaf named `providers`: names are the user's.
    let user = r#"
[daemon.providers.poll.sources.return]
data = "{{ 1 }}"
result.sources = "{{ data }}"

[client.providers.poll]
template.providers = "{{ poll.sources }}"
"#;
    assert_eq!(seeded_errors(user), Vec::<String>::new());
    let registry = shipped_registry(user);
    let sources = registry.provider_sources("poll").expect("provider poll");
    assert_eq!(sources[0].name, "return");
    assert_eq!(sources[0].fields[0].name, "sources");
}

/// Two Sources of `demo`, both declaring `x`.
const SHARED_FIELD: &str = r#"
[daemon.providers.demo.sources.a]
data = "{{ 1 }}"
result.x = "{{ data }}"

[daemon.providers.demo.sources.b]
data = "{{ 2 }}"
result.x = "{{ data }}"
"#;

#[test]
fn the_default_result_forbids_two_sources_declaring_one_field() {
    let errors = source_errors(SHARED_FIELD);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].contains("field 'x' declared by both source 'a' and source 'b'"),
        "{errors:?}"
    );
}

#[test]
fn an_explicit_merge_has_chosen_an_order_and_may_share_a_field() {
    let user = format!(
        "[daemon.providers.demo]\nresult = \"{{{{ merge(sources.a(), sources.b()) }}}}\"\n{SHARED_FIELD}"
    );
    assert_eq!(source_errors(&user), Vec::<String>::new());
    let registry = shipped_registry(&user);
    assert_eq!(registry.sources_for_key("demo", "x"), vec!["a", "b"]);
}

#[test]
fn a_schedule_decorator_on_a_provider_line_is_a_load_error() {
    for (key, line) in [
        ("return", "{{ result | poll('5s') }}"),
        ("return", "{{ result | watch('/etc/hosts') }}"),
        ("result", "{{ merge(sources) | ttl(5) }}"),
    ] {
        let user = format!("[daemon.providers.demo]\n{key} = \"{line}\"\n");
        let errors = source_errors(&user);
        assert_eq!(errors.len(), 1, "{line}: {errors:?}");
        assert!(
            errors[0].starts_with(&format!(
                "config(/user.toml): [daemon.providers.demo] {key}: "
            )) && errors[0].contains("schedule decorator"),
            "{line}: {errors:?}"
        );
    }
}

#[test]
fn a_provider_line_must_be_a_string() {
    for key in ["template", "result", "return"] {
        assert_eq!(
            seeded_errors(&format!("[daemon.providers.demo]\n{key} = 3\n")),
            vec![format!(
                "[daemon.providers.demo] {key} must be a string (a jinja line), not integer"
            )]
        );
    }
}

#[test]
fn a_comb_key_added_later_cannot_collide() {
    // `budget` is a bare [daemon] key; a provider of that name is
    // [daemon.providers.budget], a different table.
    let user = r#"
[daemon]
budget = "20ms"

[daemon.providers.budget.sources.s]
data = "{{ 1 }}"
result.x = "{{ data }}"
"#;
    assert_eq!(seeded_errors(user), Vec::<String>::new());
    let config = with_shipped_defaults(user);
    assert_eq!(config.daemon().budget(), Duration::from_millis(20));
    let registry = ProviderRegistry::with_config(&config, &Default::default()).unwrap();
    let source = &registry.provider_sources("budget").unwrap()[0];
    assert_eq!(source.budget_execute, Duration::from_millis(20));
}

/// Every spelling the one-tree config removed, at every level it could appear.
#[test]
fn every_removed_form_is_an_error() {
    // Whole tables and the old flat provider level.
    let tables: &[(&str, &str, &str)] = &[
        ("[providers.git]", "enabled = true", PROVIDERS_MOVED),
        (
            "[providers.git.diff]",
            "failback_count = 3",
            PROVIDERS_MOVED,
        ),
        (
            "[lifecycle]",
            "idle_shutdown_secs = 5",
            "unknown table [lifecycle]: idle_shutdown_secs is [daemon] idle_shutdown",
        ),
        (
            "[failback]",
            "count = 3",
            "unknown table [failback]: a Source sets failback on its return line",
        ),
        ("[tree]", "x = 1", "unknown table [tree]"),
    ];
    for (header, body, want) in tables {
        let errors = seeded_errors(&format!("{header}\n{body}\n"));
        assert_eq!(errors.len(), 1, "{header}: {errors:?}");
        assert!(errors[0].starts_with(want), "{header}: {errors:?}");
    }

    // Every removed key, wherever it is written.
    let keys = [
        "scope",
        "marker",
        "poll_interval",
        "poll_count",
        "fsevent_patterns",
        "fsevent_abs_paths",
        "budget_execute",
        "budget_stat",
        "failback_count",
        "failback_interval",
        "enabled",
    ];
    for key in keys {
        for (header, label) in [
            ("[daemon]", "[daemon]"),
            ("[daemon.providers.git]", "[daemon.providers.git]"),
            (
                "[daemon.providers.git.sources.diff]",
                "[daemon.providers.git.sources.diff]",
            ),
            (
                "[daemon.providers.demo.sources.s]",
                "[daemon.providers.demo.sources.s]",
            ),
            ("[client.providers.x]", "[client.providers.x]"),
        ] {
            // `enabled` is a provider's own key now; where it is not, it is
            // still the removed one.
            if key == "enabled" && header == "[daemon.providers.git]" {
                continue;
            }
            assert_eq!(
                seeded_errors(&format!("{header}\n{key} = 1\n")),
                vec![format!("unknown key '{key}' in {label}")],
                "{key} under {header}"
            );
        }
        assert_eq!(
            seeded_errors(&format!("{key} = 1\n")),
            vec![format!("unknown key '{key}' at the top level")],
            "{key} at the top level"
        );
        let errors = seeded_errors(&format!("[providers.git]\n{key} = 1\n"));
        assert_eq!(errors, vec![PROVIDERS_MOVED.to_string()], "{key}");
    }
}

#[test]
fn an_unknown_key_in_bare_daemon_is_an_error() {
    assert_eq!(
        seeded_errors("[daemon]\nbogus = 1\n"),
        vec!["unknown key 'bogus' in [daemon]".to_string()]
    );
    // The list is closed and typed.
    assert!(
        seeded_errors(
            "[daemon]\nsocket_path = \"/s\"\nlog_level = \"info\"\ntimeout = \"5s\"\n\
             env_file = \"/e\"\nwatchdog_interval = \"30s\"\nwatchdog_threshold = \"90s\"\n\
             idle_shutdown = \"60s\"\nplugin.path = [\"/p\"]\nbudget = \"50ms\"\n\
             providers._.cache._.decay = 4\n"
        )
        .is_empty()
    );
    for (bad, want) in [
        (
            "timeout = \"x\"",
            "[daemon] timeout must be a duration of at least 1ms (\"10s\", \"500ms\"), got \"x\"",
        ),
        (
            "timeout = 0",
            "[daemon] timeout must be a duration of at least 1ms (\"10s\", \"500ms\"), got 0",
        ),
        (
            "idle_shutdown = -1",
            "[daemon] idle_shutdown must be a duration of whole seconds, at least 1s (\"30s\", \
             \"2m\"), got -1",
        ),
        (
            "idle_shutdown = \"0s\"",
            "[daemon] idle_shutdown must be a duration of whole seconds, at least 1s (\"30s\", \
             \"2m\"), got \"0s\"",
        ),
        (
            "provider_timeout_secs = 5",
            "[daemon] provider_timeout_secs is removed: write [daemon] timeout = \"10s\" (a \
             duration)",
        ),
        (
            "idle_shutdown_secs = 60",
            "[daemon] idle_shutdown_secs is removed: write [daemon] idle_shutdown = \"5m\" (a \
             duration)",
        ),
        (
            "budget.execute = \"50ms\"",
            "[daemon.budget] is removed: write [daemon] budget = \"50ms\" (a duration)",
        ),
        (
            "log_level = 3",
            "[daemon] log_level must be one of off, error, warn, info, debug, trace, got 3",
        ),
        (
            "log_level = \"loud\"",
            "[daemon] log_level must be one of off, error, warn, info, debug, trace, got \"loud\"",
        ),
        ("env_file = \"\"", "[daemon] env_file must not be empty"),
    ] {
        assert_eq!(
            seeded_errors(&format!("[daemon]\n{bad}\n")),
            vec![want.to_string()]
        );
    }
    assert_eq!(
        seeded_errors("daemon = 3\n"),
        vec!["[daemon] must be a table".to_string()]
    );
}

/// A mistyped setting is an error the user sees, not a silent default: at
/// load, through the layers the `Config` was composed from.
#[test]
fn a_type_error_is_a_load_error_not_the_default_config() {
    let tmp = tempfile::tempdir().unwrap();
    let main = write(
        tmp.path(),
        "config.toml",
        "[daemon]\ntimeout = \"x\"\nlog_level = \"debug\"\n",
    );
    let config = Config::load_from(&main);
    let errors = config.errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].contains("config.toml): [daemon] timeout must be"),
        "{errors:?}"
    );
    assert_eq!(
        ProviderRegistry::load(&config, &Default::default()).errors,
        errors,
        "the loader every caller shares reports it"
    );

    // A [daemon] that is not a table cannot deserialize at all.
    write(tmp.path(), "config.toml", "daemon = 3\n");
    let errors = Config::load_from(&main).errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].ends_with("[daemon] must be a table"),
        "{errors:?}"
    );

    // The check is of the files the Config was composed from: fixing the file
    // after the load does not make the loaded Config valid.
    write(tmp.path(), "config.toml", "[daemon]\ntimeout = \"x\"\n");
    let loaded = Config::load_from(&main);
    write(tmp.path(), "config.toml", "[daemon]\n");
    assert_eq!(loaded.errors().len(), 1);
    assert_eq!(Config::load_from(&main).errors().len(), 0);
}

#[test]
fn a_wrongly_typed_source_in_an_in_memory_config_is_an_error() {
    let config =
        in_memory("[daemon.providers.demo.sources.s]\ndata = 5\nresult = \"{{ data }}\"\n");
    let errors = ProviderRegistry::load(&config, &Default::default()).errors;
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].starts_with("config(<in memory>): ")
            && errors[0].contains("data must be a string"),
        "{errors:?}"
    );
}

#[test]
fn budget_defaults_to_50ms_and_takes_ms() {
    assert_eq!(
        Config::default().daemon().budget(),
        Duration::from_millis(50)
    );
    for (text, want) in [
        ("50ms", Duration::from_millis(50)),
        ("250ms", Duration::from_millis(250)),
        ("1ms", Duration::from_millis(1)),
        ("2s", Duration::from_secs(2)),
    ] {
        let config = with_shipped_defaults(&format!("[daemon]\nbudget = \"{text}\"\n"));
        assert_eq!(config.errors(), Vec::<String>::new());
        assert_eq!(config.daemon().budget(), want, "{text}");
    }
    // A bare number is seconds, as in every duration.
    let config = with_shipped_defaults("[daemon]\nbudget = 5\n");
    assert_eq!(config.errors(), Vec::<String>::new());
    assert_eq!(config.daemon().budget(), Duration::from_secs(5));
    for bad in ["\"0ms\"", "0", "\"soon\"", "\"\""] {
        let errors = seeded_errors(&format!("[daemon]\nbudget = {bad}\n"));
        assert_eq!(errors.len(), 1, "{bad}: {errors:?}");
        assert!(
            errors[0].starts_with("[daemon] budget must be a duration of at least 1ms"),
            "{bad}: {errors:?}"
        );
    }
    assert_eq!(
        seeded_errors("[daemon.budget]\nstat = \"5ms\"\n"),
        vec![
            "[daemon.budget] is removed: write [daemon] budget = \"50ms\" (a duration)".to_string()
        ]
    );
}

/// `[daemon] timeout` is how long one run of a Source may take before it is
/// cut off, for every Source whose `return` does not say: ten seconds unless
/// the files say otherwise.
#[test]
fn timeout_defaults_to_10s_and_takes_any_duration() {
    assert_eq!(
        Config::default().daemon().timeout(),
        Duration::from_secs(10)
    );
    for (text, want) in [
        ("\"250ms\"", Duration::from_millis(250)),
        ("\"2m\"", Duration::from_secs(120)),
        ("30", Duration::from_secs(30)),
    ] {
        let config = with_shipped_defaults(&format!("[daemon]\ntimeout = {text}\n"));
        assert_eq!(config.errors(), Vec::<String>::new());
        assert_eq!(config.daemon().timeout(), want, "{text}");
    }
}

#[test]
fn the_watchdog_keys_are_whole_seconds_of_at_least_one() {
    for (key, bad) in [
        ("watchdog_interval", "\"0s\""),
        ("watchdog_interval", "\"500ms\""),
        ("watchdog_interval", "\"soon\""),
        ("watchdog_interval", "0"),
        ("watchdog_threshold", "\"0s\""),
        ("watchdog_threshold", "\"1500ms\""),
    ] {
        let errors = seeded_errors(&format!("[daemon]\n{key} = {bad}\n"));
        assert_eq!(errors.len(), 1, "{key} = {bad}: {errors:?}");
        assert!(
            errors[0].starts_with(&format!(
                "[daemon] {key} must be a duration of whole seconds, at least 1s"
            )),
            "{errors:?}"
        );
    }
    // A bare number is seconds, in a TOML integer as in a string.
    for ok in ["\"1s\"", "\"2000ms\"", "\"2m\"", "30", "\"30\""] {
        assert!(
            seeded_errors(&format!("[daemon]\nwatchdog_interval = {ok}\n")).is_empty(),
            "{ok}"
        );
    }

    // A threshold below the interval would restart a healthy scheduler.
    let config = with_shipped_defaults(
        "[daemon]\nwatchdog_interval = \"30s\"\nwatchdog_threshold = \"10s\"\n",
    );
    let errors = config.errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].contains(
            "config(/user.toml): [daemon] watchdog_threshold (10s) is below watchdog_interval (30s, set in \
             config(/user.toml))"
        ),
        "{errors:?}"
    );
    // The two may come from different files, and the error says which.
    let config = Config::from_layers(vec![
        (
            PathBuf::from("/a.toml"),
            toml::from_str("[daemon]\nwatchdog_interval = \"30s\"\n").unwrap(),
        ),
        (
            PathBuf::from("/b.toml"),
            toml::from_str("[daemon]\nwatchdog_threshold = \"10s\"\n").unwrap(),
        ),
    ]);
    let errors = config.errors();
    assert!(
        errors[0].starts_with("config(/b.toml): [daemon] watchdog_threshold (10s)")
            && errors[0].contains("(30s, set in config(/a.toml))"),
        "{errors:?}"
    );
    // A threshold with no interval has nothing to check against.
    let errors = in_memory("[daemon]\nwatchdog_threshold = \"90s\"\n").errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].contains("watchdog_threshold (90s) is set without watchdog_interval"),
        "{errors:?}"
    );
    // In memory the same checks run: a zero interval cannot reach the timer.
    let errors = in_memory("[daemon]\nwatchdog_interval = \"0s\"\n").errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    let config = with_shipped_defaults(
        "[daemon]\nwatchdog_interval = \"30s\"\nwatchdog_threshold = \"30s\"\n",
    );
    assert!(config.errors().is_empty());
}

#[test]
fn a_bad_plugin_path_does_not_hide_the_sources_errors() {
    let config = Config::from_toml(
        "[daemon.plugin]\npath = [\"~/p\"]\n\
         [daemon.providers.demo.sources.s]\ndata = \"{{ nope_name }}\"\nresult = \"{{ data }}\"\n",
    );
    let errors = ProviderRegistry::load(&config, &Default::default()).errors;
    assert!(
        errors.iter().any(|e| e.contains("[daemon.plugin] path")),
        "{errors:?}"
    );
    assert!(errors.iter().any(|e| e.contains("nope_name")), "{errors:?}");
}

#[test]
fn an_unknown_key_in_a_source_table_is_an_error() {
    let errors = seeded_errors(
        "[daemon.providers.demo.sources.s]\ndata = \"{{ 1 }}\"\ncommand = \"echo\"\npoll = 5\n",
    );
    assert_eq!(
        errors,
        vec![
            "unknown key 'command' in [daemon.providers.demo.sources.s]".to_string(),
            "unknown key 'poll' in [daemon.providers.demo.sources.s]".to_string(),
        ]
    );
    // The provider table fences too.
    assert_eq!(
        seeded_errors("[daemon.providers.demo]\ncommand = \"x\"\n"),
        vec!["unknown key 'command' in [daemon.providers.demo]".to_string()]
    );
}

#[test]
fn a_source_line_that_is_not_a_string_is_an_error() {
    let errors = seeded_errors(
        "[daemon.providers.demo.sources.s]\ndata = 5\nresult.a = true\nreturn = []\n",
    );
    assert_eq!(errors.len(), 3, "{errors:?}");
    assert!(errors[0].contains("data must be a string"), "{errors:?}");
}

#[test]
fn a_builtin_source_accepts_only_return() {
    // root, data and result stay refused, with one error naming the Source.
    let errors = source_errors(
        "[daemon.providers.git.sources.diff]\ndata = \"{{ 1 }}\"\nresult.x = \"{{ 1 }}\"\n",
    );
    assert_eq!(
        errors,
        vec![
            "config(/user.toml): [daemon.providers.git.sources.diff] is a built-in Source: only return can \
             be set"
                .to_string()
        ]
    );
    // return alone loads.
    assert_eq!(
        source_errors(
            "[daemon.providers.git.sources.diff]\nreturn = \"{{ result | poll('5s') }}\"\n"
        ),
        Vec::<String>::new()
    );
    // Anything else is the file check's unknown key.
    assert_eq!(
        source_errors("[daemon.providers.git.sources.diff]\nfoo = \"x\"\n"),
        vec![
            "config(/user.toml): unknown key 'foo' in [daemon.providers.git.sources.diff]"
                .to_string()
        ]
    );
    // A return that does not load names its table.
    let errors = source_errors(
        "[daemon.providers.git.sources.diff]\nreturn = \"{{ result | poll('banana') }}\"\n",
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0]
            .starts_with("config(/user.toml): [daemon.providers.git.sources.diff] return: poll()"),
        "{errors:?}"
    );
}

#[test]
fn duplicate_fields_under_the_default_merge_are_a_load_error() {
    let errors = source_errors(
        r#"
[daemon.providers.demo.sources.a]
data = "{{ 1 }}"
result.same = "{{ data }}"
[daemon.providers.demo.sources.b]
data = "{{ 2 }}"
result.same = "{{ data }}"
"#,
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
    for named in ["demo", "same", "'a'", "'b'"] {
        assert!(errors[0].contains(named), "names {named}: {errors:?}");
    }
    // A new Source of a built-in provider may not take one of its fields.
    let errors = source_errors(
        "[daemon.providers.git.sources.mine]\ndata = \"{{ 1 }}\"\nresult.branch = \"{{ data }}\"\n",
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].contains("'branch'") && errors[0].contains("'mine'"),
        "{errors:?}"
    );
}

#[test]
fn a_broken_prototype_return_line_is_a_load_error_naming_its_table() {
    // No Source uses it, and no `| super` reaches it.
    let errors = source_errors(
        "[daemon.providers._.sources._]\nreturn = \"{{ result | poll('banana') }}\"\n",
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].starts_with("config(/user.toml): [daemon.providers._.sources._] return: poll()"),
        "{errors:?}"
    );
}

#[test]
fn a_return_failure_names_the_table_that_failed_not_the_sources() {
    let errors = source_errors(
        r#"
[daemon.providers.demo.sources._]
return = "{{ result | poll('banana') }}"
[daemon.providers.demo.sources.s]
data = "{{ 1 }}"
result = "{{ data }}"
return = "{{ result | super }}"
"#,
    );
    assert_eq!(
        errors.len(),
        1,
        "the prototype and the Source agree: {errors:?}"
    );
    assert!(
        errors[0]
            .starts_with("config(/user.toml): [daemon.providers.demo.sources._] return: poll()"),
        "{errors:?}"
    );
}

#[test]
fn a_source_with_no_return_anywhere_is_watch_only_with_the_engine_defaults() {
    // No shipped defaults here, so no `_` above the Source sets a return:
    // poll and watch are opt-in, failback and budget are always on.
    let config = Config::from_toml(
        "[daemon.providers.demo.sources.s]\ndata = \"{{ 1 }}\"\nresult = \"{{ data }}\"\n",
    );
    let registry = ProviderRegistry::with_config(&config, &Default::default()).unwrap();
    let meta = &registry.provider_sources("demo").unwrap()[0];
    assert_eq!(
        meta.invalidation,
        crate::provider::InvalidationStrategy::Watch {
            patterns: vec![],
            abs_paths: vec![]
        }
    );
    assert_eq!(
        (meta.failback.reattempts, meta.failback.interval_secs),
        (
            crate::scheduler::lifecycle::DEFAULT_FAILBACK_COUNT,
            crate::scheduler::lifecycle::DEFAULT_FAILBACK_INTERVAL_SECS
        )
    );
    assert_eq!(
        meta.budget_execute,
        crate::scheduler::lifecycle::DEFAULT_BUDGET_EXECUTE
    );
}

#[test]
fn a_wrong_type_in_a_prototype_is_the_file_checks_alone() {
    let user = "[daemon.providers.demo.sources._]\ndata = 5\n\
                [daemon.providers.demo.sources.s]\nresult = \"{{ data }}\"\n";
    let file = seeded_errors(user);
    assert_eq!(file.len(), 1, "{file:?}");
    assert!(
        file[0].contains("[daemon.providers.demo.sources._]"),
        "{file:?}"
    );
    assert_eq!(
        source_errors(user),
        vec![format!("config(/user.toml): {}", file[0])],
        "reported once, by the file check: the build does not report it a second time, \
         naming the Source's table"
    );
}

#[test]
fn a_prototypes_lines_are_checked_at_load() {
    let errors = source_errors("[daemon.providers.git.sources._]\ndata = \"{{ nope_name }}\"\n");
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].contains("nope_name") && errors[0].contains("[daemon.providers.git.sources._]"),
        "{errors:?}"
    );
}

// ── Built-ins in the files ───────────────────────────────────────────────────

#[test]
fn all_providers_registered_by_default() {
    let config = Config::default();
    let registry = ProviderRegistry::with_config(&config, &Default::default()).unwrap();
    assert!(registry.provider_sources("hostname").is_some());
    assert!(registry.provider_sources("user").is_some());
    assert!(registry.provider_sources("git").is_some());
    assert!(registry.provider_sources("battery").is_some());
}

// ── from tests/suite/config_parse_duration.rs ──────────

#[test]
fn accepts_whole_seconds() {
    assert_eq!(parse_duration("30s"), Some(Duration::from_secs(30)));
    assert_eq!(parse_duration("5m"), Some(Duration::from_secs(300)));
    assert_eq!(parse_duration("1h"), Some(Duration::from_secs(3600)));
}

#[test]
fn accepts_ms_that_are_whole_seconds() {
    assert_eq!(parse_duration("1000ms"), Some(Duration::from_secs(1)));
    assert_eq!(parse_duration("2000ms"), Some(Duration::from_secs(2)));
}

#[test]
fn parse_duration_accepts_milliseconds() {
    assert_eq!(parse_duration("50ms"), Some(Duration::from_millis(50)));
    assert_eq!(parse_duration("1ms"), Some(Duration::from_millis(1)));
    assert_eq!(parse_duration("1500ms"), Some(Duration::from_millis(1500)));
    assert_eq!(parse_duration("2000ms"), Some(Duration::from_secs(2)));
    assert_eq!(parse_duration("0ms"), Some(Duration::ZERO));
}

#[test]
fn parse_duration_overflow_is_none_not_a_panic() {
    assert_eq!(parse_duration("999999999999999999h"), None);
    assert_eq!(parse_duration("18446744073709551615m"), None);
}

#[test]
fn whole_second_consumers_reject_sub_second_values() {
    use crate::config::parse_whole_seconds as secs;
    assert_eq!(secs("500ms"), None);
    assert_eq!(secs("1500ms"), None);
    assert_eq!(secs("2000ms"), Some(Duration::from_secs(2)));
    assert_eq!(secs("5s"), Some(Duration::from_secs(5)));
}

#[test]
fn empty_or_garbage_returns_none() {
    assert_eq!(parse_duration(""), None);
    assert_eq!(parse_duration("abc"), None);
    assert_eq!(parse_duration("5x"), None);
}

// ── from tests/suite/env_file.rs ──────────

#[test]
fn loads_env_file() {
    let tmp = TempDir::new().unwrap();
    let env_path = tmp.path().join("env");
    std::fs::write(
        &env_path,
        r#"
# This is a comment
BEACHCOMBER_TEST_KEY=hello_world
BEACHCOMBER_TEST_QUOTED="quoted value"
BEACHCOMBER_TEST_SINGLE='single quoted'

# Blank lines are fine
BEACHCOMBER_TEST_NUM=42
"#,
    )
    .unwrap();

    let config = Config::from_toml(&format!(
        "[daemon]\nenv_file = {}\n",
        toml::Value::String(env_path.to_string_lossy().to_string())
    ));

    // Scope the test so load_env_file's set_var calls are rolled back via Drop,
    // regardless of assertion panics. All four vars start and end as unset.
    temp_env::with_vars(
        [
            ("BEACHCOMBER_TEST_KEY", None::<&str>),
            ("BEACHCOMBER_TEST_QUOTED", None::<&str>),
            ("BEACHCOMBER_TEST_SINGLE", None::<&str>),
            ("BEACHCOMBER_TEST_NUM", None::<&str>),
        ],
        || {
            let count = config.load_env_file();
            assert_eq!(count, 4, "Should load 4 variables");
            assert_eq!(
                std::env::var("BEACHCOMBER_TEST_KEY").unwrap(),
                "hello_world"
            );
            assert_eq!(
                std::env::var("BEACHCOMBER_TEST_QUOTED").unwrap(),
                "quoted value"
            );
            assert_eq!(
                std::env::var("BEACHCOMBER_TEST_SINGLE").unwrap(),
                "single quoted"
            );
            assert_eq!(std::env::var("BEACHCOMBER_TEST_NUM").unwrap(), "42");
        },
    );
}

#[test]
fn missing_env_file_returns_zero() {
    let config = Config::from_toml("[daemon]\nenv_file = \"/nonexistent/path/to/env\"\n");

    let count = config.load_env_file();
    assert_eq!(count, 0, "Missing file should return 0");
}

#[test]
fn no_env_file_configured_returns_zero() {
    // Config with env_file pointing to a guaranteed-nonexistent path so we
    // never fall through to ~/.config/beachcomber/env on the developer's machine.
    let tmp = TempDir::new().unwrap();
    let missing = tmp.path().join("nonexistent_env_file");
    let config = Config::from_toml(&format!(
        "[daemon]\nenv_file = {}\n",
        toml::Value::String(missing.to_string_lossy().to_string())
    ));
    let count = config.load_env_file();
    assert_eq!(count, 0, "nonexistent env_file should return 0");
}

// ── Where the config is read from ────────────────────────────────────────────

#[test]
fn an_empty_socket_path_is_an_error() {
    assert_eq!(
        seeded_errors("[daemon]\nsocket_path = \"\"\n"),
        vec!["[daemon] socket_path must not be empty".to_string()]
    );
    assert!(seeded_errors("[daemon]\nsocket_path = \"/s\"\n").is_empty());
}

/// The config is the config home's: a `config.toml` or a variables file under
/// `$XDG_CONFIG_DIRS` is never read, as it was never watched or reloaded.
#[test]
fn xdg_config_dirs_is_not_searched() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let sys = tmp.path().join("sys");
    write(
        &sys,
        "beachcomber/config.toml",
        "[daemon]\nlog_level = \"trace\"\n",
    );
    write(&sys, "beachcomber/env", "BEACHCOMBER_SYS_VAR=1\n");
    temp_env::with_vars(
        [
            ("XDG_CONFIG_HOME", Some(home.as_os_str())),
            ("XDG_CONFIG_DIRS", Some(sys.as_os_str())),
            ("BEACHCOMBER_SYS_VAR", None),
        ],
        || {
            let config = Config::load();
            assert_eq!(config.daemon().log_level, LevelFilter::INFO);
            assert_eq!(config.loaded_path(), None, "no file at the config home");
            assert_eq!(config.load_env_file(), 0);
            assert!(std::env::var("BEACHCOMBER_SYS_VAR").is_err());

            let main = write(&home, "beachcomber/config.toml", "[daemon]\n");
            let config = Config::load();
            assert_eq!(config.loaded_path(), Some(main.as_path()));
        },
    );
}

/// The default variables file is `env` beside the config file the `Config`
/// was loaded from; one with no file behind it reads nothing ambient.
#[test]
fn the_default_variables_file_is_beside_the_loaded_config() {
    let tmp = tempfile::tempdir().unwrap();
    let main = write(tmp.path(), "config.toml", "");
    write(tmp.path(), "env", "BEACHCOMBER_BESIDE_VAR=1\n");
    temp_env::with_vars([("BEACHCOMBER_BESIDE_VAR", None::<&str>)], || {
        assert_eq!(Config::default().load_env_file(), 0);
        assert_eq!(Config::load_from(&main).load_env_file(), 1);
        assert_eq!(std::env::var("BEACHCOMBER_BESIDE_VAR").unwrap(), "1");
    });
}

#[test]
fn the_watchdog_accessors_read_whole_seconds_and_default_the_threshold() {
    let c = in_memory("[daemon]\nwatchdog_interval = \"2s\"\nwatchdog_threshold = \"2000ms\"\n");
    assert_eq!(
        c.daemon().watchdog_interval_duration(),
        Some(Duration::from_secs(2))
    );
    assert_eq!(
        c.daemon().watchdog_threshold_duration(),
        Some(Duration::from_secs(2))
    );
    let c = in_memory("[daemon]\nwatchdog_interval = \"10s\"\n");
    assert_eq!(
        c.daemon().watchdog_threshold_duration(),
        Some(Duration::from_secs(30))
    );
    let c = in_memory("");
    assert_eq!(c.daemon().watchdog_interval_duration(), None);
    assert_eq!(c.daemon().watchdog_threshold_duration(), None);
    // A sub-second value is not accepted, by the check or the accessor.
    let c = in_memory("[daemon]\nwatchdog_interval = \"500ms\"\n");
    assert_eq!(c.errors().len(), 1);
    assert_eq!(c.daemon().watchdog_interval_duration(), None);
}

#[test]
fn a_misspelled_builtin_source_says_what_the_provider_has() {
    let errors = source_errors(
        "[daemon.providers.git.sources.dif]\nreturn = \"{{ result | poll('5s') }}\"\n",
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].starts_with(
            "config(/user.toml): [daemon.providers.git.sources.dif]: git has no Source 'dif' (its \
             Sources: "
        ) && errors[0].contains("diff")
            && errors[0].ends_with("a new Source needs data and result"),
        "{errors:?}"
    );
}

/// `nearest` chooses between instances; a Source with one instance has no
/// choice to make, so the line that says it is a load error, whether the Source
/// is a built-in retuned or a config Source with no root.
#[test]
fn nearest_on_a_source_with_one_instance_is_a_load_error() {
    let errors = source_errors(
        "[daemon.providers.hostname.sources.host]\nreturn = \"{{ result | nearest }}\"\n",
    );
    assert_eq!(
        errors,
        vec![
            "config(/user.toml): [daemon.providers.hostname.sources.host] return: hostname.host has one \
             instance; nearest has no effect"
                .to_string()
        ]
    );
    let errors = source_errors(
        "[daemon.providers.demo.sources.s]\ndata = \"{{ 1 }}\"\nresult.x = \"{{ data }}\"\n\
         return = \"{{ result | nearest }}\"\n",
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].contains("nearest has no effect") && errors[0].contains("demo.s"),
        "{errors:?}"
    );
}

/// A Source with one instance has no directory to resolve a relative watch
/// path against, so a retune to one is a load error rather than a Source
/// that quietly never refreshes.
#[test]
fn a_retune_that_watches_nothing_is_a_load_error() {
    let errors = source_errors(
        "[daemon.providers.hostname.sources.host]\nreturn = \"{{ result | watch('etc/hosts') }}\"\n",
    );
    assert_eq!(
        errors,
        vec![
            "config(/user.toml): [daemon.providers.hostname.sources.host] return: watch('etc/hosts') is \
             relative, but hostname.host has one instance; use an absolute path"
                .to_string()
        ]
    );
    // With a poll backstop too.
    let errors = source_errors(
        "[daemon.providers.hostname.sources.host]\n\
         return = \"{{ result | watch('etc/hosts') | poll('60s') }}\"\n",
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
    // An absolute path, or any path on a path-scoped Source, loads.
    assert!(
        source_errors(
            "[daemon.providers.hostname.sources.host]\n\
             return = \"{{ result | watch('/etc/hosts') | poll('60s') }}\"\n\
             [daemon.providers.git.sources.diff]\n\
             return = \"{{ result | watch('.git/index') }}\"\n"
        )
        .is_empty()
    );
}

#[test]
fn a_new_source_of_a_builtin_provider_may_take_its_lines_from_a_prototype() {
    let reg = shipped_registry(
        r#"
[daemon.providers.git.sources._]
data = "{{ 1 }}"
result.extra = "{{ data }}"

[daemon.providers.git.sources.extra]
return = "{{ result | poll('5s', 2) }}"
"#,
    );
    assert!(reg.source("git", "extra").is_some());
    assert!(reg.source("git", "refs").is_some(), "the built-ins stay");
}

#[test]
fn from_toml_reports_text_that_is_not_toml_as_an_error() {
    let errors = Config::from_toml(
        "[daemon
log_level = ",
    )
    .errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].starts_with("config(<in memory>): "), "{errors:?}");
    assert!(!errors[0].contains('\n'), "one line per error: {errors:?}");
}

#[test]
fn log_level_takes_the_six_names_in_any_case_and_nothing_else() {
    for (text, want) in [
        ("info", LevelFilter::INFO),
        ("DEBUG", LevelFilter::DEBUG),
        ("Warn", LevelFilter::WARN),
        ("off", LevelFilter::OFF),
        ("trace", LevelFilter::TRACE),
        ("error", LevelFilter::ERROR),
    ] {
        let config = Config::from_toml(&format!("[daemon]\nlog_level = \"{text}\"\n"));
        assert_eq!(config.errors(), Vec::<String>::new(), "{text}");
        assert_eq!(config.daemon().log_level, want, "{text}");
    }
    for bad in ["\"\"", "\"0\"", "\"5\"", "\"loud\"", "3"] {
        assert_eq!(
            Config::from_toml(&format!("[daemon]\nlog_level = {bad}\n"))
                .errors()
                .len(),
            1,
            "{bad}"
        );
    }
}

/// A TOML parse error keeps its reason: the toml crate puts the line and
/// column on its first line and the reason further down, and both belong in
/// the one line a verdict list shows.
#[test]
fn a_toml_parse_error_keeps_its_reason() {
    // A dotted key may not add to a table a `[header]` already created.
    let text = "[a.b.c]\nx = 1\n[a.b]\nc.y = 2\n";
    let reason = toml::from_str::<toml::Value>(text)
        .unwrap_err()
        .message()
        .to_string();
    assert!(!reason.is_empty());
    let errors = Config::from_toml(text).errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("line 4"), "{errors:?}");
    assert!(errors[0].contains(&reason), "{errors:?}");
    assert!(!errors[0].contains('\n'), "one line: {errors:?}");
}
