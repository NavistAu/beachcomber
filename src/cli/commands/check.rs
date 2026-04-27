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

pub fn run_check(config: &Config, check_cmd: Option<CheckCommands>) -> ExitCode {
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

    let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
    let worst = rt.block_on(run_check_subjects(config, &subjects, procs_duration));
    ExitCode::from(worst)
}

pub async fn run_check_subjects(
    config: &Config,
    subjects: &[&str],
    procs_duration: Option<u64>,
) -> u8 {
    let socket_path = config.resolve_socket_path();
    let client = crate::client::Client::new(socket_path);
    let mut worst = 0u8;

    for &subject in subjects {
        let mut req = serde_json::json!({"op": "introspect", "subject": subject});
        if subject == "procs"
            && let Some(d) = procs_duration
        {
            req["duration_secs"] = serde_json::json!(d);
        }

        match client.send_raw(req).await {
            Ok(resp) if resp.ok => {
                let payload = resp
                    .data
                    .as_ref()
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                let (text, code) = render_subject(subject, &payload);
                print!("{text}");
                worst = worst.max(code);
            }
            Ok(resp) => {
                let title = subject_title(subject);
                let err = resp.error.as_deref().unwrap_or("unknown error");
                println!("\n{title}\n  [FAIL] {err}");
                worst = worst.max(2);
            }
            Err(e) => {
                let title = subject_title(subject);
                println!("\n{title}\n  [FAIL] daemon not responding: {e}");
                worst = worst.max(2);
                // No point continuing if daemon is unreachable.
                break;
            }
        }
    }

    worst
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
fn render_subject(subject: &str, payload: &serde_json::Value) -> (String, u8) {
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

            let (vlines, vworst) = render_verdicts(&verdicts);
            worst = worst.max(vworst);

            lines.push_str(&format!("Providers ({count} registered)\n"));
            for p in &providers {
                let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                let source = p.get("source").and_then(|v| v.as_str()).unwrap_or("?");
                let scope = p.get("scope").and_then(|v| v.as_str()).unwrap_or("?");
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
                        "  [WARN] {source:<8} {name:<12} {scope:<7} fields={fields_str:<30} {invalidation} — in backoff {elapsed}s (stage={stage})\n"
                    ));
                    worst = worst.max(1);
                } else {
                    lines.push_str(&format!(
                        "  [PASS] {source:<8} {name:<12} {scope:<7} fields={fields_str:<30} {invalidation}\n"
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
            let provider_count = payload
                .get("provider_count_from_config")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);

            let (vlines, vworst) = render_verdicts(&verdicts);
            worst = worst.max(vworst);

            lines.push_str("Config\n");
            if let Some(p) = path {
                lines.push_str(&format!("  [PASS] path   {p}\n"));
            } else {
                lines.push_str("  [INFO] path   (none — using defaults)\n");
            }
            if parsed {
                lines.push_str(&format!(
                    "  [PASS] parsed   ok — {provider_count} provider definitions\n"
                ));
            } else {
                lines.push_str("  [FAIL] parsed   FAILED\n");
                worst = worst.max(2);
            }
            for err in &errors {
                lines.push_str(&format!("  [FAIL] error   {err}\n"));
                worst = worst.max(2);
            }
            lines.push_str(&vlines);
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

            let (vlines, vworst) = render_verdicts(&verdicts);
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

            let (vlines, vworst) = render_verdicts(&verdicts);
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

            let (vlines, vworst) = render_verdicts(&verdicts);
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

            let (vlines, vworst) = render_verdicts(&verdicts);
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

            let (vlines, vworst) = render_verdicts(&verdicts);
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

            let (vlines, vworst) = render_verdicts(&verdicts);
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

    // Append aggregate summary if there are warnings or failures.
    let warn_count = verdicts
        .iter()
        .filter(|v| v.get("level").and_then(|l| l.as_str()) == Some("WARN"))
        .count();
    let fail_count = verdicts
        .iter()
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
/// PASS/INFO = 0, WARN = 1, FAIL = 2.
fn render_verdicts(verdicts: &[serde_json::Value]) -> (String, u8) {
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
        out.push_str(&format!("  [{level}] {msg}\n"));
    }
    (out, worst)
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
