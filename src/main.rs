use beachcomber::cli::format::render_fmt_template_json;
use beachcomber::config::Config;
use beachcomber::pid_check::pid_is_our_daemon;
use clap::{Parser, Subcommand};
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

#[derive(Parser)]
#[command(
    name = "comb",
    version,
    author = "NavistAu <https://beachcomber.sh>",
    about = "Centralized shell state daemon (beachcomber)",
    long_about = "beachcomber — a daemon that caches shell environment state.\n\n\
        One cache, many consumers. Every prompt, status bar, and script reads\n\
        from the same fast source instead of independently forking processes.\n\n\
        https://beachcomber.sh\n\
        MIT License — Copyright NavistAu",
    after_help = "Default output is plain text. Use suffixes for other formats: comb g.j (json), g.s (sh), g.c/.C (csv), g.t/.T (tsv), g.f (template)."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the daemon (usually auto-launched via socket activation)
    #[command(visible_alias = "d")]
    Daemon {
        /// Override socket path
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// Query one or more cached values.
    ///
    /// Positionals are keys (`provider.field`). If the last positional looks like a path
    /// (contains `/`, or starts with `.`, `~`, or `/`), it is treated as a path applied to
    /// every key. Use `--path` to set a path explicitly without ambiguity.
    /// When no path is given, the CLI's current working directory is used automatically.
    #[command(visible_alias = "g")]
    Get {
        /// Provider key(s) (e.g., "hostname.name", "git.branch"). If the last positional
        /// looks like a path (starts with `.`/`~`/`/`, or contains `/`), it is used as the
        /// path context instead of a key.
        #[arg(required = true, num_args = 1..)]
        keys: Vec<String>,
        /// Path context for directory-scoped providers (overrides any trailing positional path)
        #[arg(long, short)]
        path: Option<String>,
        /// Output format (text, json, sh, csv, tsv, CSV, TSV, fmt)
        #[arg(short, long, default_value = "text")]
        format: String,
        /// Evict cache entry, re-execute provider, return fresh value
        #[arg(long)]
        force: bool,
        /// Block until a fresh value is available
        #[arg(long)]
        wait: bool,
    },
    /// Show daemon status
    #[command(visible_alias = "s")]
    Status {
        /// Output format: human (default on TTY), tsv (default when piped), json, csv, table, sh
        #[arg(long, short = 'f', default_value = "")]
        format: String,
        /// Filter rows (e.g. provider=git, path=/home/*, stale=true); repeatable, AND semantics
        #[arg(long, value_name = "KEY=VALUE", action = clap::ArgAction::Append)]
        filter: Vec<String>,
        /// Sort by column: provider, path, field, value, age, stale (default: path)
        #[arg(long, default_value = "path")]
        sort: String,
        /// Disable value truncation
        #[arg(long)]
        no_trunc: bool,
        /// Maximum width for VALUE in human format (default 40)
        #[arg(long)]
        max_width: Option<usize>,
        /// Disable ANSI color codes
        #[arg(long, visible_alias = "no-colour")]
        no_color: bool,
    },
    /// Put data into a virtual provider
    #[command(visible_alias = "p")]
    Put {
        /// Provider name (e.g., "myapp")
        key: String,
        /// JSON data (e.g., '{"status":"healthy"}'); omit when using --null
        data: Option<String>,
        /// Clear the cached entry (removes the cache row; registry entry is kept)
        #[arg(long)]
        null: bool,
        /// Expected refresh interval (e.g., "30s", "5m")
        #[arg(long)]
        ttl: Option<String>,
        /// Path scope for directory-scoped data
        #[arg(long)]
        path: Option<String>,
    },
    /// Watch a key and stream changes to stdout
    #[command(visible_alias = "w")]
    Watch {
        /// Provider key (e.g., "git.branch")
        key: String,
        /// Path context for directory-scoped providers
        #[arg(long)]
        path: Option<String>,
        /// Output format (text, json, sh)
        #[arg(short, long, default_value = "text")]
        format: String,
    },
    /// Interpolate a template string with cached values
    #[command(visible_alias = "e")]
    Eval {
        /// Template string with {{ provider.field }} placeholders
        template: String,
        /// Path context for directory-scoped providers
        path: Option<String>,
    },
    /// Detect installed tools and show integration snippets
    #[command(visible_alias = "i")]
    Init,
    /// Run health checks
    #[command(visible_alias = "c")]
    Check {
        #[command(subcommand)]
        check_cmd: Option<CheckCommands>,
    },
    /// Stop the running daemon (it will socket-activate on the next query)
    #[command(visible_alias = "k")]
    Kill {
        /// Wait up to this many seconds for the daemon to exit
        #[arg(long, default_value = "5")]
        timeout: u64,
        /// Override socket path (derives the pid file path)
        #[arg(long)]
        socket: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum CheckCommands {
    /// Run all health checks
    All,
    /// Check daemon connectivity and stats
    Daemon,
    /// Show config path and parse status
    Config,
    /// Check provider health and backoff state
    Providers,
    /// Check cache entry counts and staleness
    Cache,
    /// Show providers currently in backoff
    Backoff,
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

/// Client-side output format — richer than the protocol Format.
/// Server-side formats (Json, Text, Sh) are passed through.
/// Client-side formats (Csv, Tsv, CsvHeader, TsvHeader, Fmt) request JSON and format locally.
enum OutputFormat {
    Json,
    Text,
    Sh,
    Csv,
    Tsv,
    CsvHeader,
    TsvHeader,
    Fmt(String),
}

impl OutputFormat {
    /// The wire format to request from the server.
    fn server_format(&self) -> &str {
        match self {
            OutputFormat::Text => "text",
            OutputFormat::Sh => "sh",
            // Client-side formats get JSON from the server
            _ => "json",
        }
    }

    /// Whether this format is handled server-side (text/sh wire format with blank-line termination).
    fn is_server_side(&self) -> bool {
        matches!(self, OutputFormat::Text | OutputFormat::Sh)
    }
}

fn parse_output_format(format_str: &str, fmt_template: Option<&str>) -> OutputFormat {
    match format_str {
        "json" => OutputFormat::Json,
        "sh" => OutputFormat::Sh,
        "csv" => OutputFormat::Csv,
        "tsv" => OutputFormat::Tsv,
        "CSV" => OutputFormat::CsvHeader,
        "TSV" => OutputFormat::TsvHeader,
        "fmt" => OutputFormat::Fmt(fmt_template.unwrap_or("").to_string()),
        // "text" and any unknown value fall through to plain text (the default).
        _ => OutputFormat::Text,
    }
}

/// Map format suffix to -f flag value. Returns None if not a recognized suffix.
fn suffix_to_format(suffix: &str) -> Option<&'static str> {
    match suffix {
        "p" => Some("text"), // plain text — now the default, but an explicit .p is still accepted
        "j" => Some("json"),
        "s" => Some("sh"),
        "c" => Some("csv"),
        "C" => Some("CSV"),
        "t" => Some("tsv"),
        "T" => Some("TSV"),
        "f" => Some("fmt"),
        _ => None,
    }
}

/// Returns true if `s` looks like a filesystem path rather than a provider key.
///
/// A positional is treated as a path when it:
/// - equals `.` literally
/// - starts with `.` (relative dot-prefix like `./subdir`)
/// - starts with `~` (home-relative)
/// - starts with `/` (absolute)
/// - contains `/` anywhere (e.g., `some/dir`)
fn is_path_like(s: &str) -> bool {
    s == "." || s.starts_with('.') || s.starts_with('~') || s.starts_with('/') || s.contains('/')
}

/// Separate keys from an optional trailing path positional.
///
/// Rules:
/// 1. If `--path` flag was already set, all positionals are keys.
/// 2. Otherwise, if the last positional is path-like (per `is_path_like`), pop it as the path.
/// 3. If still no path, return `None` — the caller should default to CWD.
fn split_keys_and_path(
    mut positionals: Vec<String>,
    explicit_path: Option<String>,
) -> (Vec<String>, Option<String>) {
    if explicit_path.is_some() {
        return (positionals, explicit_path);
    }
    if let Some(last) = positionals.last()
        && is_path_like(last)
    {
        let path = positionals.pop();
        return (positionals, path);
    }
    (positionals, None)
}

/// Pre-process argv to handle format suffix syntax (e.g., `comb g.p` → `comb g -f text`).
/// Returns the modified args and an optional fmt template string.
fn preprocess_args() -> (Vec<String>, Option<String>) {
    let mut args: Vec<String> = std::env::args().collect();
    let mut fmt_template = None;

    if args.len() > 1 {
        let first = args[1].clone();
        if let Some((cmd, suffix)) = first.split_once('.')
            && let Some(format_str) = suffix_to_format(suffix)
        {
            let is_fmt = suffix == "f";
            args[1] = cmd.to_string();

            if is_fmt && args.len() > 2 {
                // For `.f`, the next arg after the command is the template string.
                fmt_template = Some(args.remove(2));
            }

            // Insert -f <format> after the command name.
            args.insert(2, format_str.to_string());
            args.insert(2, "-f".to_string());
        }
    }

    (args, fmt_template)
}

/// Format a JSON value as a single display string.
fn value_to_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Format response data as CSV or TSV.
fn format_sv(data: &serde_json::Value, sep: &str, with_header: bool) -> String {
    match data {
        serde_json::Value::Object(map) => {
            let mut pairs: Vec<(&String, &serde_json::Value)> = map.iter().collect();
            pairs.sort_by_key(|(k, _)| *k);
            let mut out = String::new();
            if with_header {
                let keys: Vec<&str> = pairs.iter().map(|(k, _)| k.as_str()).collect();
                out.push_str(&keys.join(sep));
                out.push('\n');
            }
            let vals: Vec<String> = pairs.iter().map(|(_, v)| value_to_string(v)).collect();
            out.push_str(&vals.join(sep));
            out
        }
        other => value_to_string(other),
    }
}

fn main() -> ExitCode {
    let (args, fmt_template) = preprocess_args();
    let cli = Cli::parse_from(args);
    let config = Config::load();

    match cli.command {
        Commands::Daemon { socket } => {
            let socket_path = socket.unwrap_or_else(|| config.resolve_socket_path());
            run_daemon(socket_path, config)
        }
        Commands::Get {
            keys,
            path,
            format,
            force,
            wait,
        } => {
            let output_format = parse_output_format(&format, fmt_template.as_deref());
            let (keys, resolved_path) = split_keys_and_path(keys, path);
            let effective_path = resolved_path.or_else(|| {
                std::env::current_dir()
                    .ok()
                    .map(|p| p.to_string_lossy().into_owned())
            });
            run_get(&config, &keys, effective_path.as_deref(), output_format, force, wait)
        }
        Commands::Status {
            format,
            filter,
            sort,
            no_trunc,
            max_width,
            no_color,
        } => {
            let fmt = if format.is_empty() { None } else { Some(format.as_str()) };
            run_status(&config, fmt, &filter, &sort, no_trunc, max_width, no_color)
        }
        Commands::Put {
            key,
            data,
            null,
            ttl,
            path,
        } => run_put(&config, &key, data.as_deref(), null, ttl.as_deref(), path.as_deref()),
        Commands::Watch { key, path, format } => {
            let output_format = parse_output_format(&format, fmt_template.as_deref());
            run_watch(&config, &key, path.as_deref(), output_format)
        }
        Commands::Eval { template, path } => run_eval(&config, &template, path.as_deref()),
        Commands::Init => run_init(),
        Commands::Check { check_cmd } => run_check(&config, check_cmd),
        Commands::Kill { timeout, socket } => {
            let socket_path = socket.unwrap_or_else(|| config.resolve_socket_path());
            run_kill(&socket_path, timeout)
        }
    }
}

fn run_kill(socket_path: &std::path::Path, timeout_secs: u64) -> ExitCode {
    use beachcomber::daemon::{is_daemon_running, pid_path_for_socket};

    if !is_daemon_running(socket_path) {
        println!("Daemon is not running.");
        return ExitCode::SUCCESS;
    }

    let pid_path = pid_path_for_socket(socket_path);
    let pid = match resolve_daemon_pid(&pid_path, socket_path) {
        Ok(pid) => pid,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::from(2);
        }
    };

    // SIGTERM for a clean shutdown; the daemon's signal handler catches it.
    if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            let _ = fs::remove_file(&pid_path);
            println!("Daemon process was already stopped.");
            return ExitCode::SUCCESS;
        }
        eprintln!("Failed to signal daemon (pid {pid}): {err}");
        return ExitCode::from(2);
    }

    // Poll until the socket stops responding.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    while std::time::Instant::now() < deadline {
        if !is_daemon_running(socket_path) {
            println!("Daemon stopped (pid {pid}).");
            return ExitCode::SUCCESS;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    eprintln!(
        "Daemon did not exit within {timeout_secs}s. Send SIGKILL manually if needed: kill -9 {pid}"
    );
    ExitCode::from(1)
}

/// Find the pid of the running daemon. Asks the daemon itself via the status socket —
/// that is the only source that cannot go stale. Falls back to the pid file only if
/// the status query doesn't return a pid (older daemons pre-dating the `pid` field).
fn resolve_daemon_pid(
    pid_path: &std::path::Path,
    socket_path: &std::path::Path,
) -> Result<i32, String> {
    // Authoritative: ask the daemon.
    if let Some(pid) = query_daemon_pid(socket_path) {
        return Ok(pid);
    }

    // Fallback: pid file, but only if the process actually looks like our daemon.
    if let Ok(contents) = fs::read_to_string(pid_path)
        && let Ok(pid) = contents.trim().parse::<i32>()
        && pid > 0
        && pid_is_our_daemon(pid)
    {
        return Ok(pid);
    }

    Err(format!(
        "Daemon is reachable but its pid could not be determined.\n\
         The daemon may predate the `kill` command; upgrade it or restart it with a\n\
         newer binary, then try again. (Checked pid file: {})",
        pid_path.display()
    ))
}

/// Open a one-shot connection to the daemon and read `pid` out of the introspect{daemon} response.
fn query_daemon_pid(socket_path: &std::path::Path) -> Option<i32> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    let mut stream = UnixStream::connect(socket_path).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .ok()?;
    stream
        .write_all(b"{\"op\":\"introspect\",\"subject\":\"daemon\"}\n")
        .ok()?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;

    let parsed: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    parsed.get("data")?.get("pid")?.as_i64().map(|n| n as i32)
}

fn run_daemon(socket_path: PathBuf, config: Config) -> ExitCode {
    let log_path = config.resolve_log_path();

    // Ensure log directory exists.
    if let Some(parent) = log_path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    // Open log file (append mode).
    let log_file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path);

    let filter: tracing_subscriber::filter::LevelFilter = config
        .daemon
        .log_level
        .parse()
        .unwrap_or(tracing_subscriber::filter::LevelFilter::INFO);
    let env_filter = EnvFilter::from_default_env().add_directive(filter.into());

    match log_file {
        Ok(file) => {
            // Log to both stderr and file.
            let stderr_layer = fmt::layer().with_target(true).with_writer(std::io::stderr);
            let file_layer = fmt::layer()
                .with_target(true)
                .with_ansi(false)
                .with_writer(std::sync::Mutex::new(file));

            tracing_subscriber::registry()
                .with(env_filter)
                .with(stderr_layer)
                .with(file_layer)
                .init();
        }
        Err(_) => {
            // Fall back to stderr only.
            tracing_subscriber::fmt().with_max_level(filter).init();
        }
    }

    tracing::info!("Starting beachcomber daemon");
    tracing::info!("Socket: {:?}", socket_path);
    tracing::info!("Log file: {:?}", log_path);

    let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
    rt.block_on(async {
        let cancel = tokio_util::sync::CancellationToken::new();
        let cancel_clone = cancel.clone();

        tokio::spawn(async move {
            use tokio::signal::unix::{SignalKind, signal};
            let mut sigint = match signal(SignalKind::interrupt()) {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("Failed to install SIGINT handler: {e}");
                    return;
                }
            };
            let mut sigterm = match signal(SignalKind::terminate()) {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("Failed to install SIGTERM handler: {e}");
                    return;
                }
            };
            tokio::select! {
                _ = sigint.recv() => tracing::info!("Received SIGINT, shutting down..."),
                _ = sigterm.recv() => tracing::info!("Received SIGTERM, shutting down..."),
            }
            cancel_clone.cancel();
        });

        let handle = beachcomber::daemon::start_in_process_with_cancel(socket_path, config, cancel);
        handle.await.ok();
    });

    ExitCode::SUCCESS
}

fn run_get(
    config: &Config,
    keys: &[String],
    path: Option<&str>,
    format: OutputFormat,
    force: bool,
    wait: bool,
) -> ExitCode {
    let socket_path = config.resolve_socket_path();

    if let Err(e) = beachcomber::daemon::ensure_daemon(&socket_path) {
        eprintln!("Failed to start daemon: {e}");
        return ExitCode::from(2);
    }

    let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
    rt.block_on(async {
        let client = beachcomber::client::Client::new(socket_path.clone());

        // Single-key shortcut for server-side formats (text / sh): delegate directly to the
        // client helper so the daemon renders the value consistently.
        if keys.len() == 1 && format.is_server_side() {
            let key = &keys[0];
            match client
                .get_formatted_with_flags(key, path, format.server_format(), force, wait)
                .await
            {
                Ok(text) => {
                    print!("{text}");
                    return ExitCode::SUCCESS;
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    return ExitCode::from(2);
                }
            }
        }

        // Multi-key (or single-key with client-side format): open one session and issue one
        // Request::Get per key.  Results are aggregated before rendering so that formats like
        // JSON / CSV / TSV can produce a single coherent output document.
        let mut session = match client.connect().await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Error: {e}");
                return ExitCode::from(2);
            }
        };

        if let Some(p) = path
            && let Err(e) = session.set_context(p).await
        {
            eprintln!("Error: {e}");
            return ExitCode::from(2);
        }

        // Server-side formats (text / sh) for multi-key: emit each key's output on its own line.
        if format.is_server_side() {
            let wire_fmt = format.server_format();
            let mut any_error = false;
            for key in keys {
                match session
                    .get_formatted_with_flags(key, None, wire_fmt, force, wait)
                    .await
                {
                    Ok(text) => {
                        if !text.is_empty() {
                            println!("{text}");
                        }
                    }
                    Err(e) => {
                        eprintln!("Error querying {key}: {e}");
                        any_error = true;
                    }
                }
            }
            return if any_error {
                ExitCode::from(2)
            } else {
                ExitCode::SUCCESS
            };
        }

        // Client-side formats: collect all responses (preserving per-key association), then
        // render.  Errors are recorded but do not prevent successful keys from being emitted.
        let mut responses: Vec<(String, beachcomber::protocol::Response)> = Vec::new();
        let mut any_error = false;
        for key in keys {
            match session.get_with_flags(key, None, force, wait).await {
                Ok(response) => {
                    if !response.ok {
                        eprintln!(
                            "Error querying {key}: {}",
                            response.error.as_deref().unwrap_or("unknown error")
                        );
                        any_error = true;
                    } else {
                        responses.push((key.clone(), response));
                    }
                }
                Err(e) => {
                    eprintln!("Error querying {key}: {e}");
                    any_error = true;
                }
            }
        }

        // Single-key client-side rendering: preserve the original single-key output shape.
        if keys.len() == 1 {
            if let Some((_, response)) = responses.first() {
                if let Some(data) = &response.data {
                    match &format {
                        OutputFormat::Json => {
                            println!("{}", serde_json::to_string_pretty(response).unwrap());
                        }
                        OutputFormat::Csv => {
                            print!("{}", format_sv(data, ",", false));
                        }
                        OutputFormat::Tsv => {
                            print!("{}", format_sv(data, "\t", false));
                        }
                        OutputFormat::CsvHeader => {
                            print!("{}", format_sv(data, ",", true));
                        }
                        OutputFormat::TsvHeader => {
                            print!("{}", format_sv(data, "\t", true));
                        }
                        OutputFormat::Fmt(template) => {
                            match render_fmt_template_json(template, data) {
                                Ok(rendered) => print!("{}", rendered),
                                Err(e) => {
                                    eprintln!("Template error: {e}");
                                    return ExitCode::from(2);
                                }
                            }
                        }
                        _ => unreachable!(),
                    }
                } else {
                    any_error = true;
                }
            }
            return if any_error {
                ExitCode::from(2)
            } else {
                ExitCode::SUCCESS
            };
        }

        // Multi-key client-side aggregation.
        match &format {
            OutputFormat::Json => {
                let arr: Vec<&beachcomber::protocol::Response> =
                    responses.iter().map(|(_, r)| r).collect();
                println!("{}", serde_json::to_string_pretty(&arr).unwrap());
            }
            OutputFormat::Csv | OutputFormat::CsvHeader => {
                let with_header = matches!(format, OutputFormat::CsvHeader);
                let mut all_keys: Vec<String> = Vec::new();
                let mut all_vals: Vec<String> = Vec::new();
                for (key, resp) in &responses {
                    if let Some(data) = &resp.data {
                        match data {
                            serde_json::Value::Object(map) => {
                                let mut pairs: Vec<(&String, &serde_json::Value)> =
                                    map.iter().collect();
                                pairs.sort_by_key(|(k, _)| *k);
                                for (k, v) in pairs {
                                    all_keys.push(k.clone());
                                    all_vals.push(value_to_string(v));
                                }
                            }
                            _ => {
                                all_keys.push(key.clone());
                                all_vals.push(value_to_string(data));
                            }
                        }
                    }
                }
                if with_header {
                    println!("{}", all_keys.join(","));
                }
                println!("{}", all_vals.join(","));
            }
            OutputFormat::Tsv | OutputFormat::TsvHeader => {
                let with_header = matches!(format, OutputFormat::TsvHeader);
                let mut all_keys: Vec<String> = Vec::new();
                let mut all_vals: Vec<String> = Vec::new();
                for (key, resp) in &responses {
                    if let Some(data) = &resp.data {
                        match data {
                            serde_json::Value::Object(map) => {
                                let mut pairs: Vec<(&String, &serde_json::Value)> =
                                    map.iter().collect();
                                pairs.sort_by_key(|(k, _)| *k);
                                for (k, v) in pairs {
                                    all_keys.push(k.clone());
                                    all_vals.push(value_to_string(v));
                                }
                            }
                            _ => {
                                all_keys.push(key.clone());
                                all_vals.push(value_to_string(data));
                            }
                        }
                    }
                }
                if with_header {
                    println!("{}", all_keys.join("\t"));
                }
                println!("{}", all_vals.join("\t"));
            }
            OutputFormat::Fmt(template) => {
                let mut merged = serde_json::Map::new();
                for (key, resp) in &responses {
                    if let Some(data) = &resp.data {
                        match data {
                            serde_json::Value::Object(map) => {
                                let provider = key.split('.').next().unwrap_or(key);
                                for (k, v) in map {
                                    merged.insert(format!("{provider}.{k}"), v.clone());
                                    merged.insert(k.clone(), v.clone());
                                }
                            }
                            _ => {
                                merged.insert(key.clone(), data.clone());
                            }
                        }
                    }
                }
                match render_fmt_template_json(template, &serde_json::Value::Object(merged)) {
                    Ok(rendered) => print!("{}", rendered),
                    Err(e) => {
                        eprintln!("Template error: {e}");
                        return ExitCode::from(2);
                    }
                }
            }
            // Text and Sh multi-key handled above via server-side path.
            OutputFormat::Text | OutputFormat::Sh => unreachable!(),
        }

        if any_error {
            ExitCode::from(2)
        } else {
            ExitCode::SUCCESS
        }
    })
}

fn run_status(
    config: &Config,
    format: Option<&str>,
    filters: &[String],
    sort_col: &str,
    no_trunc: bool,
    max_width: Option<usize>,
    no_color_flag: bool,
) -> ExitCode {
    use beachcomber::cli::status_format::{RenderOpts, apply_filters, apply_sort, render_preset};
    use std::io::IsTerminal;

    let socket_path = config.resolve_socket_path();

    if let Err(e) = beachcomber::daemon::ensure_daemon(&socket_path) {
        eprintln!("Failed to start daemon: {e}");
        return ExitCode::from(2);
    }

    let is_tty = std::io::stdout().is_terminal();
    let no_color =
        no_color_flag || std::env::var("NO_COLOR").is_ok() || !is_tty;

    let preset = format.unwrap_or(if is_tty { "human" } else { "tsv" });
    let opts = RenderOpts {
        is_tty,
        no_color,
        max_width: if no_trunc { None } else { Some(max_width.unwrap_or(40)) },
        no_trunc,
    };

    let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
    rt.block_on(async {
        let client = beachcomber::client::Client::new(socket_path);
        match client.send_raw(serde_json::json!({"op": "status"})).await {
            Ok(response) => {
                if response.ok {
                    let rows: Vec<beachcomber::cache::CacheRow> = response
                        .data
                        .as_ref()
                        .and_then(|d| serde_json::from_value(d.clone()).ok())
                        .unwrap_or_default();
                    let rows = apply_filters(rows, filters).unwrap_or_else(|e| {
                        eprintln!("filter error: {e}");
                        std::process::exit(2);
                    });
                    let rows = apply_sort(rows, sort_col).unwrap_or_else(|e| {
                        eprintln!("sort error: {e}");
                        std::process::exit(2);
                    });
                    let out = render_preset(preset, &rows, &opts);
                    print!("{out}");
                    ExitCode::SUCCESS
                } else {
                    eprintln!("Error: {}", response.error.unwrap_or_default());
                    ExitCode::from(2)
                }
            }
            Err(e) => {
                eprintln!("Error: {e}");
                ExitCode::from(2)
            }
        }
    })
}

fn run_watch(config: &Config, key: &str, path: Option<&str>, format: OutputFormat) -> ExitCode {
    let socket_path = config.resolve_socket_path();

    if let Err(e) = beachcomber::daemon::ensure_daemon(&socket_path) {
        eprintln!("Failed to start daemon: {e}");
        return ExitCode::from(2);
    }

    let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
    rt.block_on(async {
        let client = beachcomber::client::Client::new(socket_path);
        let mut session = match client.connect().await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Error: {e}");
                return ExitCode::from(2);
            }
        };

        let server_fmt = match &format {
            OutputFormat::Text => Some("text"),
            OutputFormat::Sh => Some("sh"),
            _ => None,
        };
        if let Err(e) = session.watch(key, path, server_fmt).await {
            eprintln!("Error: {e}");
            return ExitCode::from(2);
        }

        // For server-side formats, stream lines directly.
        // For client-side formats, each watch line is a JSON response we need to reformat.
        loop {
            match session.read_watch_line().await {
                Ok(Some(line)) => match &format {
                    OutputFormat::Json | OutputFormat::Text | OutputFormat::Sh => {
                        print!("{line}");
                    }
                    _ => {
                        if let Ok(response) =
                            serde_json::from_str::<beachcomber::protocol::Response>(&line)
                            && let Some(data) = &response.data
                        {
                            match &format {
                                OutputFormat::Csv => {
                                    println!("{}", format_sv(data, ",", false));
                                }
                                OutputFormat::Tsv => {
                                    println!("{}", format_sv(data, "\t", false));
                                }
                                OutputFormat::CsvHeader => {
                                    println!("{}", format_sv(data, ",", true));
                                }
                                OutputFormat::TsvHeader => {
                                    println!("{}", format_sv(data, "\t", true));
                                }
                                OutputFormat::Fmt(template) => {
                                    match render_fmt_template_json(template, data) {
                                        Ok(rendered) => println!("{}", rendered),
                                        Err(e) => {
                                            eprintln!("Template error: {e}");
                                            return ExitCode::from(2);
                                        }
                                    }
                                }
                                _ => unreachable!(),
                            }
                        }
                    }
                },
                Ok(None) => break,
                Err(e) => {
                    eprintln!("Error: {e}");
                    return ExitCode::from(2);
                }
            }
        }

        ExitCode::SUCCESS
    })
}

fn run_put(
    config: &Config,
    key: &str,
    data_str: Option<&str>,
    null: bool,
    ttl: Option<&str>,
    path: Option<&str>,
) -> ExitCode {
    // Validate argument combinations.
    if null && data_str.is_some() {
        eprintln!("cannot combine --null with a data argument");
        return ExitCode::from(2);
    }
    if !null && data_str.is_none() {
        eprintln!("put requires either a data argument or --null");
        return ExitCode::from(2);
    }

    let socket_path = config.resolve_socket_path();

    if let Err(e) = beachcomber::daemon::ensure_daemon(&socket_path) {
        eprintln!("Failed to start daemon: {e}");
        return ExitCode::from(2);
    }

    let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
    rt.block_on(async {
        let client = beachcomber::client::Client::new(socket_path);

        if null {
            match client.put_null(key, ttl, path).await {
                Ok(response) => {
                    if response.ok {
                        ExitCode::SUCCESS
                    } else {
                        eprintln!("Error: {}", response.error.unwrap_or_default());
                        ExitCode::from(2)
                    }
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    ExitCode::from(2)
                }
            }
        } else {
            let data_str = data_str.unwrap();
            let data: serde_json::Value = match serde_json::from_str(data_str) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("Invalid JSON: {e}");
                    return ExitCode::from(2);
                }
            };

            if !data.is_object() {
                eprintln!("put data must be a JSON object");
                return ExitCode::from(2);
            }

            match client.put(key, data, ttl, path).await {
                Ok(response) => {
                    if response.ok {
                        ExitCode::SUCCESS
                    } else {
                        eprintln!("Error: {}", response.error.unwrap_or_default());
                        ExitCode::from(2)
                    }
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    ExitCode::from(2)
                }
            }
        }
    })
}

fn run_eval(config: &Config, template: &str, path: Option<&str>) -> ExitCode {
    use beachcomber::cli::format::{find_eval_template_pairs, render_eval_template};

    let socket_path = config.resolve_socket_path();

    if let Err(e) = beachcomber::daemon::ensure_daemon(&socket_path) {
        eprintln!("Failed to start daemon: {e}");
        return ExitCode::from(2);
    }

    // Discover (provider, field) pairs referenced in the template.
    let pairs = find_eval_template_pairs(template);

    // If the template has no provider.field references, render it directly
    // (it may still contain jinja conditionals, literals, etc.).
    if pairs.is_empty() {
        return match render_eval_template(template, &serde_json::Value::Object(Default::default())) {
            Ok(s) => { print!("{s}"); ExitCode::SUCCESS }
            Err(e) => { eprintln!("template render error: {e}"); ExitCode::from(2) }
        };
    }

    // Deduplicate provider.field pairs before querying.
    let mut seen = std::collections::HashSet::new();
    let unique_pairs: Vec<(String, String)> = pairs
        .into_iter()
        .filter(|pair| seen.insert(pair.clone()))
        .collect();

    let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
    rt.block_on(async {
        let client = beachcomber::client::Client::new(socket_path);
        let mut session = match client.connect().await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Error: {e}");
                return ExitCode::from(2);
            }
        };

        if let Some(p) = path
            && let Err(e) = session.set_context(p).await
        {
            eprintln!("Error: {e}");
            return ExitCode::from(2);
        }

        // Fetch each provider.field and assemble a nested JSON context.
        let mut ctx: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
        for (provider, field) in &unique_pairs {
            let key = format!("{provider}.{field}");
            match session.get(&key, None).await {
                Ok(response) => {
                    if let Some(data) = &response.data {
                        let provider_entry = ctx
                            .entry(provider.clone())
                            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
                        if let serde_json::Value::Object(m) = provider_entry {
                            m.insert(field.clone(), data.clone());
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Error querying {key}: {e}");
                    return ExitCode::from(2);
                }
            }
        }

        // Render the template with the nested context.
        match render_eval_template(template, &serde_json::Value::Object(ctx)) {
            Ok(s) => { print!("{s}"); ExitCode::SUCCESS }
            Err(e) => { eprintln!("template render error: {e}"); ExitCode::from(2) }
        }
    })
}

struct DetectedTool {
    name: &'static str,
    snippet: &'static str,
}

fn run_init() -> ExitCode {
    let home = std::env::var("HOME").unwrap_or_default();
    let xdg_config = std::env::var("XDG_CONFIG_HOME").unwrap_or(format!("{home}/.config"));

    let mut detected: Vec<DetectedTool> = Vec::new();

    // Powerlevel10k
    if PathBuf::from(format!("{home}/.p10k.zsh")).exists() {
        detected.push(DetectedTool {
            name: "Powerlevel10k",
            snippet: r#"# Add to your .p10k.zsh — replace native git segment with beachcomber:
# In prompt_git(), replace git status calls with:
#   local branch=$(comb g git.branch .)
#   local dirty=$(comb g git.dirty .)"#,
        });
    }

    // Starship
    if PathBuf::from(format!("{xdg_config}/starship.toml")).exists()
        || std::env::var("STARSHIP_CONFIG").is_ok()
    {
        detected.push(DetectedTool {
            name: "Starship",
            snippet: r#"# Add to starship.toml:
[custom.git_branch]
command = "comb g git.branch ."
when = true
shell = ["sh"]"#,
        });
    }

    // oh-my-tmux
    if PathBuf::from(format!("{home}/.tmux.conf.local")).exists() {
        detected.push(DetectedTool {
            name: "oh-my-tmux",
            snippet: r##"# Add to .tmux.conf.local:
tmux_conf_theme_status_right="#(comb g git.branch .) | #(comb g load.one) | %R""##,
        });
    }

    // tmux (generic)
    if PathBuf::from(format!("{home}/.tmux.conf")).exists() {
        detected.push(DetectedTool {
            name: "tmux",
            snippet: r##"# Add to .tmux.conf:
set -g status-right "#(comb g git.branch .) #(comb g load.one)""##,
        });
    }

    // Neovim
    if PathBuf::from(format!("{xdg_config}/nvim/init.lua")).exists()
        || PathBuf::from(format!("{xdg_config}/nvim/init.vim")).exists()
    {
        detected.push(DetectedTool {
            name: "Neovim",
            snippet: r#"-- Lua statusline integration (lualine, heirline, etc.):
-- local beachcomber = require('libbeachcomber')
-- local client = beachcomber.connect()
-- local branch = client:get_text('git.branch', vim.fn.getcwd())"#,
        });
    }

    // Polybar
    if PathBuf::from(format!("{xdg_config}/polybar/config.ini")).exists()
        || PathBuf::from(format!("{xdg_config}/polybar/config")).exists()
    {
        detected.push(DetectedTool {
            name: "Polybar",
            snippet: r#"# Add to polybar config:
[module/beachcomber-git]
type = custom/script
exec = comb g git.branch .
interval = 2"#,
        });
    }

    // Waybar
    if PathBuf::from(format!("{xdg_config}/waybar/config")).exists()
        || PathBuf::from(format!("{xdg_config}/waybar/config.jsonc")).exists()
    {
        detected.push(DetectedTool {
            name: "Waybar",
            snippet: r#"// Add to waybar config:
"custom/git": {
    "exec": "comb g git.branch .",
    "interval": 2
}"#,
        });
    }

    // Sketchybar
    if PathBuf::from(format!("{xdg_config}/sketchybar/sketchybarrc")).exists()
        || PathBuf::from(format!("{home}/.config/sketchybar/sketchybarrc")).exists()
    {
        detected.push(DetectedTool {
            name: "Sketchybar",
            snippet: r#"# Add to sketchybarrc:
sketchybar --add item git left \
           --set git script="comb g git.branch ." \
           update_freq=2"#,
        });
    }

    // oh-my-zsh
    if PathBuf::from(format!("{home}/.oh-my-zsh")).exists() || std::env::var("ZSH").is_ok() {
        detected.push(DetectedTool {
            name: "Oh My Zsh",
            snippet: r#"# Source the chpwd hook for faster directory switching:
source <(curl -fsSL https://beachcomber.sh/scripts/chpwd.sh)
# Or download and source from a local path."#,
        });
    }

    if detected.is_empty() {
        println!("No supported tools detected.");
        println!();
        println!("beachcomber integrates with: starship, powerlevel10k, oh-my-tmux,");
        println!("tmux, neovim, polybar, waybar, sketchybar, oh-my-zsh, and more.");
        println!();
        println!("See https://beachcomber.sh for integration guides.");
    } else {
        println!(
            "Detected {} tool(s) with beachcomber integration support:",
            detected.len()
        );
        println!();
        for tool in &detected {
            println!("--- {} ---", tool.name);
            println!();
            println!("{}", tool.snippet);
            println!();
        }
        println!("Full integration guides: https://beachcomber.sh");
    }

    ExitCode::SUCCESS
}

fn run_check(config: &Config, check_cmd: Option<CheckCommands>) -> ExitCode {
    const ALL_SUBJECTS: &[&str] = &[
        "daemon", "config", "providers", "cache", "watches", "backoff", "timers", "demand",
        "procs",
    ];

    let (subjects, procs_duration): (Vec<&str>, Option<u64>) = match &check_cmd {
        None | Some(CheckCommands::All) => (ALL_SUBJECTS.to_vec(), None),
        Some(CheckCommands::Daemon) => (vec!["daemon"], None),
        Some(CheckCommands::Config) => (vec!["config"], None),
        Some(CheckCommands::Providers) => (vec!["providers"], None),
        Some(CheckCommands::Cache) => (vec!["cache"], None),
        Some(CheckCommands::Backoff) => (vec!["backoff"], None),
        Some(CheckCommands::Watches) => (vec!["watches"], None),
        Some(CheckCommands::Timers) => (vec!["timers"], None),
        Some(CheckCommands::Demand) => (vec!["demand"], None),
        Some(CheckCommands::Procs { duration }) => (vec!["procs"], Some(*duration)),
    };

    let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
    let worst = rt.block_on(run_check_subjects(config, &subjects, procs_duration));
    ExitCode::from(worst)
}

async fn run_check_subjects(
    config: &Config,
    subjects: &[&str],
    procs_duration: Option<u64>,
) -> u8 {
    let socket_path = config.resolve_socket_path();
    let client = beachcomber::client::Client::new(socket_path);
    let mut worst = 0u8;

    for &subject in subjects {
        let mut req = serde_json::json!({"op": "introspect", "subject": subject});
        if subject == "procs" && let Some(d) = procs_duration {
            req["duration_secs"] = serde_json::json!(d);
        }

        match client.send_raw(req).await {
            Ok(resp) if resp.ok => {
                let payload = resp.data.as_ref().cloned().unwrap_or(serde_json::Value::Null);
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
        "backoff" => "Backoff",
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
            let version = payload
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let pid = payload
                .get("pid")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let uptime = payload
                .get("uptime_secs")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let uptime_fmt = format_uptime(uptime);
            let socket = payload
                .get("socket_path")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let config_path = payload.get("config_path").and_then(|v| v.as_str());
            let requests = payload
                .get("requests_total")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let in_flight = payload
                .get("in_flight")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let watchers = payload
                .get("active_watchers")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let cache = payload
                .get("cache_entries")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);

            let (vlines, vworst) = render_verdicts(&verdicts);
            worst = worst.max(vworst);

            lines.push_str("Daemon\n");
            lines.push_str(&format!(
                "  [PASS] beachcomber {version} — pid {pid} — uptime {uptime_fmt}\n"
            ));
            lines.push_str(&format!("  [PASS] socket   {socket}\n"));
            if let Some(cp) = config_path {
                lines.push_str(&format!("  [PASS] config   {cp}\n"));
            } else {
                lines.push_str("  [INFO] config   (none — using defaults)\n");
            }
            lines.push_str(&format!(
                "  [PASS] requests_total={requests}  in_flight={in_flight}  active_watchers={watchers}  cache_entries={cache}\n"
            ));
            lines.push_str(&vlines);
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
        "backoff" => {
            let entries = payload
                .get("backoff")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

            let (vlines, vworst) = render_verdicts(&verdicts);
            worst = worst.max(vworst);

            lines.push_str("Backoff\n");
            if entries.is_empty() {
                lines.push_str("  [PASS] no providers in backoff\n");
            } else {
                for entry in &entries {
                    let provider = entry
                        .get("provider")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    let stage = entry
                        .get("stage")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
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
                let queries = key
                    .get("query_count")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
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

            let total: u64 = samples.iter().map(|s| s.get("count").and_then(|v| v.as_u64()).unwrap_or(0)).sum();
            lines.push_str(&format!("Procs ({duration}s sample — {total} exec events)\n"));

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
                        lines.push_str(&format!("  [WARN] {cmd:<20} {count:>8}  beachcomber can replace\n"));
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
    let warn_count = verdicts.iter().filter(|v| v.get("level").and_then(|l| l.as_str()) == Some("WARN")).count();
    let fail_count = verdicts.iter().filter(|v| v.get("level").and_then(|l| l.as_str()) == Some("FAIL")).count();
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

#[cfg(test)]
mod path_disambiguation_tests {
    use super::{is_path_like, split_keys_and_path};

    // --- is_path_like ---

    #[test]
    fn dot_alone_is_path_like() {
        assert!(is_path_like("."));
    }

    #[test]
    fn dot_prefix_is_path_like() {
        assert!(is_path_like("./subdir"));
        assert!(is_path_like(".hidden"));
    }

    #[test]
    fn tilde_prefix_is_path_like() {
        assert!(is_path_like("~/repos"));
        assert!(is_path_like("~"));
    }

    #[test]
    fn absolute_path_is_path_like() {
        assert!(is_path_like("/home/me/repo"));
        assert!(is_path_like("/"));
    }

    #[test]
    fn slash_containing_is_path_like() {
        assert!(is_path_like("some/dir"));
        assert!(is_path_like("a/b/c"));
    }

    #[test]
    fn provider_key_is_not_path_like() {
        // Keys contain `.` but not `/` or any path prefix.
        assert!(!is_path_like("git.branch"));
        assert!(!is_path_like("hostname.name"));
        assert!(!is_path_like("git"));
        assert!(!is_path_like("user"));
    }

    // --- split_keys_and_path ---

    fn keys(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn single_key_trailing_dot_is_path() {
        // `comb get git.branch .` → key=git.branch, path=.
        let (ks, path) = split_keys_and_path(keys(&["git.branch", "."]), None);
        assert_eq!(ks, vec!["git.branch"]);
        assert_eq!(path.as_deref(), Some("."));
    }

    #[test]
    fn single_key_trailing_absolute_path_is_path() {
        // `comb get git.branch /home/me/repo`
        let (ks, path) = split_keys_and_path(keys(&["git.branch", "/home/me/repo"]), None);
        assert_eq!(ks, vec!["git.branch"]);
        assert_eq!(path.as_deref(), Some("/home/me/repo"));
    }

    #[test]
    fn single_key_trailing_relative_dot_prefix_is_path() {
        // `comb get git.branch ./subdir`
        let (ks, path) = split_keys_and_path(keys(&["git.branch", "./subdir"]), None);
        assert_eq!(ks, vec!["git.branch"]);
        assert_eq!(path.as_deref(), Some("./subdir"));
    }

    #[test]
    fn two_keys_no_path_returns_no_path() {
        // `comb get git.branch git.sha` → no path extracted; caller defaults to CWD
        let (ks, path) = split_keys_and_path(keys(&["git.branch", "git.sha"]), None);
        assert_eq!(ks, vec!["git.branch", "git.sha"]);
        assert!(path.is_none());
    }

    #[test]
    fn two_keys_trailing_dot_pops_path() {
        // `comb get git.branch user.name .`
        let (ks, path) =
            split_keys_and_path(keys(&["git.branch", "user.name", "."]), None);
        assert_eq!(ks, vec!["git.branch", "user.name"]);
        assert_eq!(path.as_deref(), Some("."));
    }

    #[test]
    fn explicit_flag_overrides_positional_dot() {
        // `--path /other` is set; the positional `.` is NOT treated as path — flag wins.
        let (ks, path) =
            split_keys_and_path(keys(&["git.branch", "."]), Some("/other".to_string()));
        // With explicit flag, no positional popping occurs — `.` stays as a key.
        assert_eq!(ks, vec!["git.branch", "."]);
        assert_eq!(path.as_deref(), Some("/other"));
    }

    #[test]
    fn single_key_that_looks_like_key_returns_no_path() {
        // `comb get git.branch` → key only, no path extracted
        let (ks, path) = split_keys_and_path(keys(&["git.branch"]), None);
        assert_eq!(ks, vec!["git.branch"]);
        assert!(path.is_none());
    }

    #[test]
    fn provider_only_key_is_not_path_like() {
        // `comb get git` — bare provider name, no dot, no slash
        let (ks, path) = split_keys_and_path(keys(&["git"]), None);
        assert_eq!(ks, vec!["git"]);
        assert!(path.is_none());
    }

    #[test]
    fn tilde_path_is_popped() {
        let (ks, path) = split_keys_and_path(keys(&["git.branch", "~/myrepo"]), None);
        assert_eq!(ks, vec!["git.branch"]);
        assert_eq!(path.as_deref(), Some("~/myrepo"));
    }
}

