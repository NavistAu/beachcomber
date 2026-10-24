//! Handler for the `check` subcommand.
//!
//! Moved from `src/main.rs` in Task 2.7.
//!
//! # CheckCommands placement (Option B)
//!
//! The `CheckCommands` clap enum is defined here (in the library crate) rather
//! than in `src/main.rs`.  Clap's derive macros work fine in library code; the
//! binary simply re-exports the type via a `use` import so the `Commands::Check`
//! variant in `main.rs` can reference it.  This keeps all check-related code in
//! one module and avoids a cross-binary-to-library dependency inversion.

use crate::cli::introspect_types::{DaemonIntrospect, Verdict};
use crate::config::Config;
use clap::Subcommand;
use std::process::ExitCode;
use std::time::Duration;

/// Parse `--connect-timeout`: `0`, `250ms`, `2s`, or a bare number of seconds.
pub fn parse_connect_timeout(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    let (num, unit_ms) = match s.strip_suffix("ms") {
        Some(n) => (n, 1u64),
        None => (s.strip_suffix('s').unwrap_or(s), 1000),
    };
    num.trim()
        .parse::<u64>()
        .map(|n| Duration::from_millis(n * unit_ms))
        .map_err(|_| format!("expected a duration like `0`, `250ms` or `2s`, got `{s}`"))
}

/// Subcommands for `comb check`.
#[derive(Subcommand)]
pub enum CheckCommands {
    /// Run all health checks
    All,
    /// Check daemon connectivity and stats
    Daemon,
    /// Show config path and parse status
    Config,
    /// Check provider health and lifecycle state
    Providers,
    /// Check cache entry counts and staleness
    Cache,
    /// Show providers currently in a decay lifecycle state
    Lifecycle,
    /// Show active filesystem watches
    Watches,
    /// Show active poll timers
    Timers,
    /// Show demand-tracked keys
    Demand,
    /// Snapshot process spawns to measure beachcomber impact
    Procs {
        /// Sample duration in seconds
        #[arg(short, long, default_value = "60")]
        duration: u64,
    },
}

pub fn run_check(
    config: &Config,
    check_cmd: Option<CheckCommands>,
    connect_timeout: Duration,
) -> ExitCode {
    const ALL_SUBJECTS: &[&str] = &[
        "daemon",
        "config",
        "providers",
        "cache",
        "watches",
        "lifecycle",
        "timers",
        "demand",
        "procs",
    ];

    let (subjects, procs_duration): (Vec<&str>, Option<u64>) = match &check_cmd {
        None | Some(CheckCommands::All) => (ALL_SUBJECTS.to_vec(), None),
        Some(CheckCommands::Daemon) => (vec!["daemon"], None),
        Some(CheckCommands::Config) => (vec!["config"], None),
        Some(CheckCommands::Providers) => (vec!["providers"], None),
        Some(CheckCommands::Cache) => (vec!["cache"], None),
        Some(CheckCommands::Lifecycle) => (vec!["lifecycle"], None),
        Some(CheckCommands::Watches) => (vec!["watches"], None),
        Some(CheckCommands::Timers) => (vec!["timers"], None),
        Some(CheckCommands::Demand) => (vec!["demand"], None),
        Some(CheckCommands::Procs { duration }) => (vec!["procs"], Some(*duration)),
    };

    let worst = run_check_subjects(config, &subjects, procs_duration, connect_timeout);
    ExitCode::from(worst)
}

/// `connect_timeout` bounds the whole search for a daemon: a check exists to
/// report reachability, so it should not inherit the client's full restart-
/// window retry budget unless asked to.
pub fn run_check_subjects(
    config: &Config,
    subjects: &[&str],
    procs_duration: Option<u64>,
    connect_timeout: Duration,
) -> u8 {
    let socket_path = config.resolve_socket_path();
    let client = crate::client::Client::with_connect_timeout(socket_path, connect_timeout);
    let mut worst = 0u8;

    for &subject in subjects {
        let duration = if subject == "procs" {
            procs_duration
        } else {
            None
        };

        // The config file is validated here as well as by the daemon: a config
        // the daemon refuses to start on is exactly the one there is no daemon
        // left to ask about. The two views join one verdict list, under one
        // count, so a disagreement reads as information rather than as two
        // unrelated blocks.
        let local = if subject == "config" {
            local_config_verdicts(config)
        } else {
            Vec::new()
        };
        let (local_lines, local_worst) = render_verdicts(&local, Some(LOCAL_VIEW));
        worst = worst.max(local_worst);

        match client.introspect(subject, duration) {
            Ok(resp) if resp.ok => {
                let payload = resp
                    .data
                    .as_ref()
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                let (text, code) = render_subject(subject, &payload, &local);
                print!("{text}");
                worst = worst.max(code);
            }
            Ok(resp) => {
                let title = subject_title(subject);
                let err = resp.error.as_deref().unwrap_or("unknown error");
                let line = verdict_line("FAIL", daemon_view(&local), err);
                print!("\n{title}\n{local_lines}{line}");
                worst = worst.max(2);
            }
            Err(e) => {
                let title = subject_title(subject);
                let line = verdict_line(
                    "FAIL",
                    daemon_view(&local),
                    &format!("daemon not responding: {e}"),
                );
                print!("\n{title}\n{local_lines}{line}");
                worst = worst.max(2);
                // No point continuing if daemon is unreachable.
                break;
            }
        }
    }

    worst
}

/// Validate the config file against the built-in providers, as
/// `run_daemon_with_cancel` does at startup. Returns the verdict lines and the
/// worst exit code they imply.
///
/// The provider set is `ProviderRegistry::load`'s. A Source's `plugin()` calls
/// are linted, not run, so nothing is `dlopen`ed in the checking process.
fn local_config_verdicts(config: &Config) -> Vec<serde_json::Value> {
    // The loader the daemon and the config watch use; the checking process's
    // own variables stand in for the daemon's.
    let vars = std::env::vars().collect();
    let load = crate::provider::registry::ProviderRegistry::load(config, &vars);
    let mut out: Vec<serde_json::Value> = load
        .errors
        .into_iter()
        .map(|e| serde_json::json!({"level": "FAIL", "message": e}))
        .collect();
    // Silence would be ambiguous — a reader could not tell a clean config from
    // a check that never ran.
    if out.is_empty() {
        out.push(serde_json::json!({"level": "PASS", "message": "config file valid"}));
    }
    out.extend(defaults_verdicts(config));
    out
}

/// The managed-defaults directory (spec §3.3), read without writing anything:
/// whether every shipped file is present at this binary's version, whether
/// the directory holds files comb did not write, and whether the daemon could
/// write it — the one thing that stops it starting.
fn defaults_verdicts(config: &Config) -> Vec<serde_json::Value> {
    use crate::daemon::defaults::{SHIPPED, audit};
    let version = env!("BEACHCOMBER_VERSION");
    let dir = Config::defaults_dir_for(config.config_file().unwrap_or(&Config::config_path()));
    let shown = dir.display();
    let found = audit(&dir, version);

    let mut out = Vec::new();
    if found.stale.is_empty() {
        out.push(serde_json::json!({
            "level": "PASS",
            "message": format!("defaults {shown} ({} files, comb {version})", SHIPPED.len()),
        }));
    } else {
        out.push(serde_json::json!({
            "level": "WARN",
            "message": format!(
                "defaults {shown}: {} shipped file(s) missing or stale — the daemon writes \
                 them on its next start",
                found.stale.len()
            ),
        }));
    }
    if !found.unmanaged.is_empty() {
        out.push(serde_json::json!({
            "level": "WARN",
            "message": format!(
                "defaults {shown} holds {} file(s) comb did not write",
                found.unmanaged.len()
            ),
        }));
    }
    // A dry probe of whether the daemon could write here: open one file for
    // writing and remove it, in the directory if it exists, else in the nearest
    // ancestor that does — the one `create_dir_all` would have to write into.
    // A check leaves nothing behind, so it never creates the directory itself.
    let mut target = dir.as_path();
    while !target.exists() {
        match target.parent() {
            Some(parent) => target = parent,
            None => break,
        }
    }
    // Named per process so two concurrent checks never remove each other's probe.
    let probe = target.join(format!(".comb-probe.{}", std::process::id()));
    let writable = std::fs::File::create(&probe).and_then(|_| std::fs::remove_file(&probe));
    if let Err(e) = writable {
        out.push(serde_json::json!({
            "level": "FAIL",
            "message": format!(
                "defaults {shown} not writable: {e} — the daemon cannot rewrite them on upgrade"
            ),
        }));
    }
    out
}

fn subject_title(subject: &str) -> &'static str {
    match subject {
        "daemon" => "Daemon",
        "config" => "Config",
        "providers" => "Providers",
        "cache" => "Cache",
        "lifecycle" => "Lifecycle",
        "watches" => "Watches",
        "timers" => "Timers",
        "demand" => "Demand",
        "procs" => "Procs",
        _ => "Unknown",
    }
}

/// Render a subject's introspect payload into a formatted block.
/// Returns (text, worst_exit_code): PASS/INFO=0, WARN=1, FAIL=2.
///
/// `local` carries verdicts the client produced itself rather than reading off
/// the daemon — only `config` has any. They join the block's own verdict list
/// and the count beneath it, so one mistake is reported once.
fn render_subject(
    subject: &str,
    payload: &serde_json::Value,
    local: &[serde_json::Value],
) -> (String, u8) {
    let verdicts = payload
        .get("verdicts")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut worst: u8 = 0;
    let mut lines = String::new();

    // Build per-subject header and detail lines before verdict lines.
    match subject {
        "daemon" => {
            // Parse into DaemonIntrospect once so all field accesses are typed.
            // This prevents the class of bug (e.g. `comb kill` reading a pid
            // out of a Status response that changed shape) by failing loudly at
            // the serde boundary rather than silently returning a wrong default.
            match serde_json::from_value::<DaemonIntrospect>(payload.clone()) {
                Ok(d) => {
                    let uptime_fmt = format_uptime(d.uptime_secs);
                    let (vlines, vworst) = render_typed_verdicts(&d.verdicts);
                    worst = worst.max(vworst);

                    lines.push_str("Daemon\n");
                    lines.push_str(&format!(
                        "  [PASS] beachcomber {} — pid {} — uptime {uptime_fmt}\n",
                        d.version, d.pid
                    ));
                    lines.push_str(&format!("  [PASS] socket   {}\n", d.socket_path));
                    if let Some(cp) = &d.config_path {
                        lines.push_str(&format!("  [PASS] config   {cp}\n"));
                    } else {
                        lines.push_str("  [INFO] config   (none — using defaults)\n");
                    }
                    lines.push_str(&format!(
                        "  [PASS] requests_total={}  in_flight={}  active_watchers={}  cache_entries={}\n",
                        d.requests_total, d.in_flight, d.active_watchers, d.cache_entries
                    ));
                    lines.push_str(&vlines);
                }
                Err(e) => {
                    // Deserialisation failure means the protocol schema has
                    // changed incompatibly.  Surface it as a FAIL so `comb
                    // check daemon` exits 2 and the operator knows to upgrade.
                    lines.push_str("Daemon\n");
                    lines.push_str(&format!(
                        "  [FAIL] could not parse introspect{{daemon}} response: {e}\n"
                    ));
                    worst = worst.max(2);
                }
            }
        }
        "providers" => {
            let providers = payload
                .get("providers")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let count = providers.len();

            let (vlines, vworst) = render_verdicts(&verdicts, None);
            worst = worst.max(vworst);

            lines.push_str(&format!("Providers ({count} registered)\n"));
            for p in &providers {
                let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                let source = p.get("source").and_then(|v| v.as_str()).unwrap_or("?");
                let fields: Vec<&str> = p
                    .get("fields")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|f| f.get("name").and_then(|n| n.as_str()))
                            .collect()
                    })
                    .unwrap_or_default();
                let fields_str = if fields.is_empty() {
                    "—".to_string()
                } else {
                    fields.join(",")
                };
                let invalidation = p
                    .get("invalidation")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                let in_backoff = !p.get("in_backoff").map(|v| v.is_null()).unwrap_or(true);

                if in_backoff {
                    let stage = p
                        .get("in_backoff")
                        .and_then(|b| b.get("stage"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    let elapsed = p
                        .get("in_backoff")
                        .and_then(|b| b.get("elapsed_secs"))
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    lines.push_str(&format!(
                        "  [WARN] {source:<8} {name:<12} fields={fields_str:<30} {invalidation} — in backoff {elapsed}s (stage={stage})\n"
                    ));
                    worst = worst.max(1);
                } else {
                    lines.push_str(&format!(
                        "  [PASS] {source:<8} {name:<12} fields={fields_str:<30} {invalidation}\n"
                    ));
                }
            }
            lines.push_str(&vlines);
        }
        "config" => {
            let path = payload.get("path").and_then(|v| v.as_str());
            let parsed = payload
                .get("parsed")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let errors: Vec<&str> = payload
                .get("errors")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|e| e.as_str()).collect())
                .unwrap_or_default();
            let source_count = payload
                .get("config_source_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);

            let (vlines, vworst) = render_verdicts(&verdicts, Some(DAEMON_VIEW));
            worst = worst.max(vworst);
            let (local_lines, local_worst) = render_verdicts(local, Some(LOCAL_VIEW));
            worst = worst.max(local_worst);

            // Two views in one block: the config the *daemon* loaded at startup,
            // which may predate the file on disk, and the client's own reading
            // of that file. Tagged so a disagreement reads as information.
            lines.push_str("Config\n");
            let daemon = Some(DAEMON_VIEW);
            match path {
                Some(p) => lines.push_str(&verdict_line("PASS", daemon, &format!("path {p}"))),
                None => lines.push_str(&verdict_line(
                    "INFO",
                    daemon,
                    "path (none — using defaults)",
                )),
            }
            if parsed {
                lines.push_str(&verdict_line(
                    "PASS",
                    daemon,
                    &format!("parsed ok — {source_count} config Sources"),
                ));
            } else {
                lines.push_str(&verdict_line("FAIL", daemon, "parsed FAILED"));
                worst = worst.max(2);
            }
            for err in &errors {
                lines.push_str(&verdict_line("FAIL", daemon, err));
                worst = worst.max(2);
            }
            lines.push_str(&vlines);
            lines.push_str(&local_lines);
        }
        "cache" => {
            let total = payload
                .get("total_entries")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let stale = payload
                .get("stale_entries")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);

            let (vlines, vworst) = render_verdicts(&verdicts, None);
            worst = worst.max(vworst);

            lines.push_str("Cache\n");
            lines.push_str(&format!("  [PASS] entries   {total} total\n"));
            if stale > 0 {
                lines.push_str(&format!("  [WARN] stale     {stale} stale entries\n"));
                worst = worst.max(1);
            } else {
                lines.push_str("  [PASS] stale     none\n");
            }
            lines.push_str(&vlines);
        }
        "lifecycle" => {
            let entries = payload
                .get("lifecycle")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

            let (vlines, vworst) = render_verdicts(&verdicts, None);
            worst = worst.max(vworst);

            lines.push_str("Lifecycle\n");
            if entries.is_empty() {
                lines.push_str("  [PASS] no providers in decay\n");
            } else {
                for entry in &entries {
                    let provider = entry
                        .get("provider")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    let stage = entry.get("stage").and_then(|v| v.as_str()).unwrap_or("?");
                    let elapsed = entry
                        .get("elapsed_secs")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let path = entry.get("path").and_then(|v| v.as_str());
                    let label = match path {
                        Some(p) => format!("{provider} ({p})"),
                        None => provider.to_string(),
                    };
                    lines.push_str(&format!(
                        "  [WARN] {label}   stage={stage} elapsed={elapsed}s\n"
                    ));
                    worst = worst.max(1);
                }
            }
            lines.push_str(&vlines);
        }
        "watches" => {
            let paths = payload
                .get("paths")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

            let (vlines, vworst) = render_verdicts(&verdicts, None);
            worst = worst.max(vworst);

            lines.push_str(&format!("Watches ({} paths)\n", paths.len()));
            if paths.is_empty() {
                lines.push_str("  [WARN] not watching any paths\n");
                worst = worst.max(1);
            } else {
                for path in &paths {
                    let p = path.as_str().unwrap_or("?");
                    lines.push_str(&format!("  [PASS] {p}\n"));
                }
            }
            lines.push_str(&vlines);
        }
        "timers" => {
            let timers = payload
                .get("timers")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

            let (vlines, vworst) = render_verdicts(&verdicts, None);
            worst = worst.max(vworst);

            lines.push_str(&format!("Timers ({} poll timers)\n", timers.len()));
            for timer in &timers {
                let provider = timer
                    .get("provider")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                let interval = timer
                    .get("interval_secs")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let last = timer
                    .get("last_run_secs_ago")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let path = timer.get("path").and_then(|v| v.as_str());
                let label = match path {
                    Some(p) => format!("{provider} ({p})"),
                    None => provider.to_string(),
                };
                let overdue = interval > 0 && last > interval * 2;
                if overdue {
                    lines.push_str(&format!(
                        "  [WARN] {label}   interval={interval}s  last={last}s ago  OVERDUE\n"
                    ));
                    worst = worst.max(1);
                } else {
                    lines.push_str(&format!(
                        "  [PASS] {label}   interval={interval}s  last={last}s ago\n"
                    ));
                }
            }
            if timers.is_empty() {
                lines.push_str("  [INFO] no active poll timers\n");
            }
            lines.push_str(&vlines);
        }
        "demand" => {
            let keys = payload
                .get("demand")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

            let (vlines, vworst) = render_verdicts(&verdicts, None);
            worst = worst.max(vworst);

            lines.push_str(&format!("Demand ({} active keys)\n", keys.len()));
            for key in &keys {
                let k = key.get("key").and_then(|v| v.as_str()).unwrap_or("?");
                let state = key.get("state").and_then(|v| v.as_str()).unwrap_or("?");
                let queries = key.get("query_count").and_then(|v| v.as_u64()).unwrap_or(0);
                lines.push_str(&format!(
                    "  [INFO] {k}   state={state}  queries={queries}\n"
                ));
            }
            if keys.is_empty() {
                lines.push_str("  [INFO] no keys currently tracked by demand\n");
            }
            lines.push_str(&vlines);
        }
        "procs" => {
            let duration = payload
                .get("duration_secs")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let samples = payload
                .get("samples")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let suggestions = payload
                .get("replacement_suggestions")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

            let (vlines, vworst) = render_verdicts(&verdicts, None);
            worst = worst.max(vworst);

            let total: u64 = samples
                .iter()
                .map(|s| s.get("count").and_then(|v| v.as_u64()).unwrap_or(0))
                .sum();
            lines.push_str(&format!(
                "Procs ({duration}s sample — {total} exec events)\n"
            ));

            for s in &samples {
                let cmd = s.get("command").and_then(|v| v.as_str()).unwrap_or("?");
                let count = s.get("count").and_then(|v| v.as_u64()).unwrap_or(0);
                let category = s.get("category").and_then(|v| v.as_str());
                let covered = category.map(|cat| {
                    suggestions.iter().any(|r| {
                        r.get("command_pattern")
                            .and_then(|v| v.as_str())
                            .map(|p| p == cat)
                            .unwrap_or(false)
                    })
                });
                match covered {
                    Some(true) => {
                        lines.push_str(&format!(
                            "  [WARN] {cmd:<20} {count:>8}  beachcomber can replace\n"
                        ));
                        worst = worst.max(1);
                    }
                    Some(false) => {
                        lines.push_str(&format!("  [INFO] {cmd:<20} {count:>8}\n"));
                    }
                    None => {
                        lines.push_str(&format!("  [INFO] {cmd:<20} {count:>8}\n"));
                    }
                }
            }
            if !suggestions.is_empty() {
                lines.push_str(&format!(
                    "  [WARN] {} command(s) could be replaced by `comb get`\n",
                    suggestions.len()
                ));
                worst = worst.max(1);
            }
            lines.push_str(&vlines);
        }
        other => {
            lines.push_str(&format!("Unknown subject: {other}\n"));
            lines.push_str("  [FAIL] no renderer for this subject\n");
            worst = 2;
        }
    }

    // Append aggregate summary if there are warnings or failures. `local` is
    // empty for every subject but `config`, where its verdicts are part of the
    // same list and so of the same count.
    let counted = || verdicts.iter().chain(local.iter());
    let warn_count = counted()
        .filter(|v| v.get("level").and_then(|l| l.as_str()) == Some("WARN"))
        .count();
    let fail_count = counted()
        .filter(|v| v.get("level").and_then(|l| l.as_str()) == Some("FAIL"))
        .count();
    if worst >= 1 {
        lines.push('\n');
        match (fail_count, warn_count) {
            (f, _) if f > 0 => lines.push_str(&format!("({f} failure(s))\n")),
            (_, w) if w > 0 => lines.push_str(&format!("({w} warning(s))\n")),
            _ => {}
        }
    }

    lines.push('\n');
    (lines, worst)
}

/// Render verdicts array into formatted lines and the worst exit code.
/// PASS/INFO = 0, WARN = 1, FAIL = 2. `tag` names the view that produced them,
/// for a subject whose block carries more than one.
fn render_verdicts(verdicts: &[serde_json::Value], tag: Option<&str>) -> (String, u8) {
    let mut out = String::new();
    let mut worst: u8 = 0;
    for v in verdicts {
        let level = v.get("level").and_then(|l| l.as_str()).unwrap_or("INFO");
        let msg = v.get("message").and_then(|m| m.as_str()).unwrap_or("");
        let code = match level {
            "FAIL" => 2,
            "WARN" => 1,
            _ => 0,
        };
        worst = worst.max(code);
        // Only emit verdicts that aren't already captured by the per-subject rendering above.
        // We include them for completeness but callers may suppress if they prefer.
        out.push_str(&verdict_line(level, tag, msg));
    }
    (out, worst)
}

/// Tag a daemon-sourced line only when the block also carries client-sourced
/// ones — a subject with a single view needs no column telling the reader which
/// one it is.
fn daemon_view(local: &[serde_json::Value]) -> Option<&'static str> {
    (!local.is_empty()).then_some(DAEMON_VIEW)
}

/// Which view of a subject produced a line. Padded to one width so the message
/// column lines up whichever view it was.
const DAEMON_VIEW: &str = "daemon";
const LOCAL_VIEW: &str = "local";
const VIEW_WIDTH: usize = 6;

/// One `  [LEVEL] <view>   message` line, or `  [LEVEL] message` when the
/// subject has only one view.
fn verdict_line(level: &str, tag: Option<&str>, msg: &str) -> String {
    match tag {
        Some(t) => format!("  [{level}] {t:<VIEW_WIDTH$}   {msg}\n"),
        None => format!("  [{level}] {msg}\n"),
    }
}

/// Typed variant of render_verdicts for subjects parsed into structured types.
/// Accepts `&[Verdict]` directly so the caller never needs to touch raw JSON.
fn render_typed_verdicts(verdicts: &[Verdict]) -> (String, u8) {
    let mut out = String::new();
    let mut worst: u8 = 0;
    for v in verdicts {
        let level = &v.level;
        let msg = &v.message;
        worst = worst.max(v.severity());
        out.push_str(&format!("  [{level}] {msg}\n"));
    }
    (out, worst)
}

fn format_uptime(secs: u64) -> String {
    if secs < 60 {
        return format!("{secs}s");
    }
    let minutes = secs / 60;
    if minutes < 60 {
        let s = secs % 60;
        return format!("{minutes}m {s}s");
    }
    let hours = minutes / 60;
    let m = minutes % 60;
    format!("{hours}h {m}m")
}
