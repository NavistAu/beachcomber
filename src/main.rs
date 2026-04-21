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
    /// Query a cached value
    #[command(visible_alias = "g")]
    Get {
        /// Provider key (e.g., "hostname.name", "git.branch")
        key: String,
        /// Path context for directory-scoped providers
        path: Option<String>,
        /// Output format (text, json, sh, csv, tsv, CSV, TSV, fmt)
        #[arg(short, long, default_value = "text")]
        format: String,
    },
    /// Trigger immediate recomputation of a provider
    #[command(visible_alias = "r")]
    Refresh {
        /// Provider key
        key: String,
        /// Path context
        path: Option<String>,
    },
    /// Show daemon status
    #[command(visible_alias = "s")]
    Status,
    /// List active providers
    #[command(visible_alias = "l")]
    List,
    /// Store data as a virtual provider
    #[command(visible_alias = "p")]
    Put {
        /// Provider name (e.g., "myapp")
        key: String,
        /// JSON data (e.g., '{"status":"healthy"}')
        data: String,
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
        /// Template string with {provider.field} placeholders
        template: String,
        /// Path context for directory-scoped providers
        path: Option<String>,
    },
    /// Query multiple keys in a single connection
    #[command(visible_alias = "f")]
    Fetch {
        /// Provider keys (e.g., "git.branch" "hostname.name")
        keys: Vec<String>,
        /// Path context for directory-scoped providers
        #[arg(long)]
        path: Option<String>,
        /// Output format (text, json, sh, csv, tsv, CSV, TSV, fmt)
        #[arg(short, long, default_value = "text")]
        format: String,
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
    /// Check daemon connectivity
    Daemon,
    /// Validate configuration
    Config,
    /// Check provider health and backoff state
    Providers,
    /// Check cache for stale entries
    Cache,
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

/// Format response data using a template string with {field_name} placeholders.
fn format_template(data: &serde_json::Value, template: &str) -> String {
    let mut result = template.to_string();

    // Replace escaped braces first (use temporary placeholders).
    result = result.replace("{{", "\x00LBRACE\x00");
    result = result.replace("}}", "\x00RBRACE\x00");

    if let serde_json::Value::Object(map) = data {
        for (key, val) in map {
            let placeholder = format!("{{{key}}}");
            result = result.replace(&placeholder, &value_to_string(val));
        }
    }

    // Restore escaped braces.
    result = result.replace("\x00LBRACE\x00", "{");
    result = result.replace("\x00RBRACE\x00", "}");

    result
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
        Commands::Get { key, path, format } => {
            let output_format = parse_output_format(&format, fmt_template.as_deref());
            run_get(&config, &key, path.as_deref(), output_format)
        }
        Commands::Refresh { key, path } => run_refresh(&config, &key, path.as_deref()),
        Commands::Status => run_status(&config),
        Commands::List => run_list(&config),
        Commands::Put {
            key,
            data,
            ttl,
            path,
        } => run_put(&config, &key, &data, ttl.as_deref(), path.as_deref()),
        Commands::Watch { key, path, format } => {
            let output_format = parse_output_format(&format, fmt_template.as_deref());
            run_watch(&config, &key, path.as_deref(), output_format)
        }
        Commands::Eval { template, path } => run_eval(&config, &template, path.as_deref()),
        Commands::Fetch { keys, path, format } => {
            let output_format = parse_output_format(&format, fmt_template.as_deref());
            run_fetch(&config, &keys, path.as_deref(), output_format)
        }
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

/// Open a one-shot connection to the daemon and read `pid` out of the status response.
fn query_daemon_pid(socket_path: &std::path::Path) -> Option<i32> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    let mut stream = UnixStream::connect(socket_path).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .ok()?;
    stream.write_all(b"{\"op\":\"status\"}\n").ok()?;

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

fn run_get(config: &Config, key: &str, path: Option<&str>, format: OutputFormat) -> ExitCode {
    let socket_path = config.resolve_socket_path();

    if let Err(e) = beachcomber::daemon::ensure_daemon(&socket_path) {
        eprintln!("Failed to start daemon: {e}");
        return ExitCode::from(2);
    }

    let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
    rt.block_on(async {
        let client = beachcomber::client::Client::new(socket_path);

        if format.is_server_side() {
            match client
                .get_formatted(key, path, format.server_format())
                .await
            {
                Ok(text) => {
                    print!("{text}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    ExitCode::from(2)
                }
            }
        } else {
            match client.get(key, path).await {
                Ok(response) => {
                    if !response.ok {
                        eprintln!("Error: {}", response.error.unwrap_or_default());
                        ExitCode::from(2)
                    } else if let Some(data) = &response.data {
                        match &format {
                            OutputFormat::Json => {
                                println!("{}", serde_json::to_string_pretty(&response).unwrap());
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
                                print!("{}", format_template(data, template));
                            }
                            _ => unreachable!(),
                        }
                        ExitCode::SUCCESS
                    } else {
                        ExitCode::from(1)
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

fn run_refresh(config: &Config, key: &str, path: Option<&str>) -> ExitCode {
    let socket_path = config.resolve_socket_path();

    if let Err(e) = beachcomber::daemon::ensure_daemon(&socket_path) {
        eprintln!("Failed to start daemon: {e}");
        return ExitCode::from(2);
    }

    let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
    rt.block_on(async {
        let client = beachcomber::client::Client::new(socket_path);
        match client.poke(key, path).await {
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
    })
}

fn run_status(config: &Config) -> ExitCode {
    let socket_path = config.resolve_socket_path();

    if let Err(e) = beachcomber::daemon::ensure_daemon(&socket_path) {
        eprintln!("Failed to start daemon: {e}");
        return ExitCode::from(2);
    }

    let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
    rt.block_on(async {
        let client = beachcomber::client::Client::new(socket_path);
        match client.send_raw(serde_json::json!({"op": "status"})).await {
            Ok(response) => {
                if response.ok {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(
                            &response.data.unwrap_or(serde_json::Value::Null)
                        )
                        .unwrap()
                    );
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

fn run_list(config: &Config) -> ExitCode {
    let socket_path = config.resolve_socket_path();

    if let Err(e) = beachcomber::daemon::ensure_daemon(&socket_path) {
        eprintln!("Failed to start daemon: {e}");
        return ExitCode::from(2);
    }

    let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
    rt.block_on(async {
        let client = beachcomber::client::Client::new(socket_path);
        match client.send_raw(serde_json::json!({"op": "list"})).await {
            Ok(response) => {
                if response.ok {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(
                            &response.data.unwrap_or(serde_json::Value::Null)
                        )
                        .unwrap()
                    );
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
                                    println!("{}", format_template(data, template));
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
    data_str: &str,
    ttl: Option<&str>,
    path: Option<&str>,
) -> ExitCode {
    let socket_path = config.resolve_socket_path();

    if let Err(e) = beachcomber::daemon::ensure_daemon(&socket_path) {
        eprintln!("Failed to start daemon: {e}");
        return ExitCode::from(2);
    }

    let data: serde_json::Value = match serde_json::from_str(data_str) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Invalid JSON: {e}");
            return ExitCode::from(2);
        }
    };

    if !data.is_object() {
        eprintln!("Store data must be a JSON object");
        return ExitCode::from(2);
    }

    let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
    rt.block_on(async {
        let client = beachcomber::client::Client::new(socket_path);
        match client.store(key, data, ttl, path).await {
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
    })
}

/// Extract {provider.field} placeholders from a template string.
fn extract_template_keys(template: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '{' {
            if chars.peek() == Some(&'{') {
                chars.next(); // skip escaped brace
                continue;
            }
            let mut key = String::new();
            for c in chars.by_ref() {
                if c == '}' {
                    break;
                }
                key.push(c);
            }
            if !key.is_empty() && !keys.contains(&key) {
                keys.push(key);
            }
        }
    }
    keys
}

fn run_eval(config: &Config, template: &str, path: Option<&str>) -> ExitCode {
    let socket_path = config.resolve_socket_path();

    if let Err(e) = beachcomber::daemon::ensure_daemon(&socket_path) {
        eprintln!("Failed to start daemon: {e}");
        return ExitCode::from(2);
    }

    let keys = extract_template_keys(template);
    if keys.is_empty() {
        print!("{template}");
        return ExitCode::SUCCESS;
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

        if let Some(p) = path
            && let Err(e) = session.set_context(p).await
        {
            eprintln!("Error: {e}");
            return ExitCode::from(2);
        }

        // Query all keys over a single connection.
        let mut values: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for key in &keys {
            match session.get(key, None).await {
                Ok(response) => {
                    if let Some(data) = &response.data {
                        values.insert(key.clone(), value_to_string(data));
                    }
                }
                Err(e) => {
                    eprintln!("Error querying {key}: {e}");
                    return ExitCode::from(2);
                }
            }
        }

        // Interpolate the template.
        let mut result = template.to_string();
        result = result.replace("{{", "\x00LBRACE\x00");
        result = result.replace("}}", "\x00RBRACE\x00");
        for (key, val) in &values {
            let placeholder = format!("{{{key}}}");
            result = result.replace(&placeholder, val);
        }
        // Replace unresolved placeholders with empty string.
        for key in &keys {
            let placeholder = format!("{{{key}}}");
            result = result.replace(&placeholder, "");
        }
        result = result.replace("\x00LBRACE\x00", "{");
        result = result.replace("\x00RBRACE\x00", "}");

        print!("{result}");
        ExitCode::SUCCESS
    })
}

fn run_fetch(
    config: &Config,
    keys: &[String],
    path: Option<&str>,
    format: OutputFormat,
) -> ExitCode {
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

        if let Some(p) = path
            && let Err(e) = session.set_context(p).await
        {
            eprintln!("Error: {e}");
            return ExitCode::from(2);
        }

        let mut responses = Vec::new();
        for key in keys {
            match session.get(key, None).await {
                Ok(response) => responses.push(response),
                Err(e) => {
                    eprintln!("Error querying {key}: {e}");
                    return ExitCode::from(2);
                }
            }
        }

        match &format {
            OutputFormat::Json => {
                let arr: Vec<&beachcomber::protocol::Response> = responses.iter().collect();
                println!("{}", serde_json::to_string_pretty(&arr).unwrap());
            }
            OutputFormat::Text => {
                for resp in &responses {
                    if let Some(data) = &resp.data {
                        println!("{}", value_to_string(data));
                    }
                }
            }
            OutputFormat::Sh => {
                for (i, resp) in responses.iter().enumerate() {
                    if let Some(data) = &resp.data {
                        match data {
                            serde_json::Value::Object(map) => {
                                let mut pairs: Vec<(&String, &serde_json::Value)> =
                                    map.iter().collect();
                                pairs.sort_by_key(|(k, _)| *k);
                                for (k, v) in pairs {
                                    println!("{k}={}", value_to_string(v));
                                }
                            }
                            _ => {
                                println!("{}={}", keys[i], value_to_string(data));
                            }
                        }
                    }
                }
            }
            OutputFormat::Csv | OutputFormat::CsvHeader => {
                let with_header = matches!(format, OutputFormat::CsvHeader);
                // Merge all response data into columns.
                let mut all_keys: Vec<String> = Vec::new();
                let mut all_vals: Vec<String> = Vec::new();
                for (i, resp) in responses.iter().enumerate() {
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
                                all_keys.push(keys[i].clone());
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
                for (i, resp) in responses.iter().enumerate() {
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
                                all_keys.push(keys[i].clone());
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
                // Merge all response data into a single map for template interpolation.
                let mut merged = serde_json::Map::new();
                for (i, resp) in responses.iter().enumerate() {
                    if let Some(data) = &resp.data {
                        match data {
                            serde_json::Value::Object(map) => {
                                // Prefix with key name for disambiguation.
                                let provider = keys[i].split('.').next().unwrap_or(&keys[i]);
                                for (k, v) in map {
                                    merged.insert(format!("{provider}.{k}"), v.clone());
                                    // Also insert unprefixed for convenience.
                                    merged.insert(k.clone(), v.clone());
                                }
                            }
                            _ => {
                                merged.insert(keys[i].clone(), data.clone());
                            }
                        }
                    }
                }
                print!(
                    "{}",
                    format_template(&serde_json::Value::Object(merged), template)
                );
            }
        }

        ExitCode::SUCCESS
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
    match check_cmd {
        None => {
            println!("Usage: comb check <subcommand>");
            println!();
            println!("Subcommands:");
            println!("  all        Run all health checks");
            println!("  daemon     Check daemon connectivity");
            println!("  config     Validate configuration");
            println!("  providers  Check provider health and backoff state");
            println!("  cache      Check cache for stale entries");
            println!("  procs      Snapshot process spawns to measure impact");
            ExitCode::SUCCESS
        }
        Some(CheckCommands::All) => {
            let mut worst = 0u8;
            worst = worst.max(check_daemon(config));
            worst = worst.max(check_config());
            worst = worst.max(check_providers(config));
            worst = worst.max(check_cache(config));
            ExitCode::from(worst)
        }
        Some(CheckCommands::Daemon) => ExitCode::from(check_daemon(config)),
        Some(CheckCommands::Config) => ExitCode::from(check_config()),
        Some(CheckCommands::Providers) => ExitCode::from(check_providers(config)),
        Some(CheckCommands::Cache) => ExitCode::from(check_cache(config)),
        Some(CheckCommands::Procs { duration }) => ExitCode::from(check_procs(duration)),
    }
}

fn print_check(status: &str, label: &str, detail: &str) {
    let icon = match status {
        "pass" => "PASS",
        "warn" => "WARN",
        "fail" => "FAIL",
        _ => "????",
    };
    if detail.is_empty() {
        println!("[{icon}] {label}");
    } else {
        println!("[{icon}] {label}: {detail}");
    }
}

/// Returns 0 for pass, 1 for warn, 2 for fail.
fn check_daemon(config: &Config) -> u8 {
    println!("=== Daemon ===");
    let socket_path = config.resolve_socket_path();

    if !socket_path.exists() {
        print_check(
            "fail",
            "socket",
            &format!("{} does not exist", socket_path.display()),
        );
        return 2;
    }

    // Try connecting.
    let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
    let result = rt.block_on(async {
        let client = beachcomber::client::Client::new(socket_path.clone());
        client.send_raw(serde_json::json!({"op": "status"})).await
    });

    match result {
        Ok(response) => {
            if response.ok {
                print_check("pass", "daemon", "connected and responding");
                0
            } else {
                print_check("warn", "daemon", "connected but returned error");
                1
            }
        }
        Err(e) => {
            print_check(
                "fail",
                "daemon",
                &format!("cannot connect to {}: {e}", socket_path.display()),
            );
            2
        }
    }
}

fn check_config() -> u8 {
    println!("=== Config ===");
    let config_path = beachcomber::config::Config::config_path();

    if !config_path.exists() {
        print_check("pass", "config", "no config file (using defaults)");
        return 0;
    }

    match std::fs::read_to_string(&config_path) {
        Ok(contents) => match toml::from_str::<beachcomber::config::Config>(&contents) {
            Ok(_) => {
                print_check(
                    "pass",
                    "config",
                    &format!("{} is valid", config_path.display()),
                );
                0
            }
            Err(e) => {
                print_check("fail", "config", &format!("parse error: {e}"));
                2
            }
        },
        Err(e) => {
            print_check(
                "fail",
                "config",
                &format!("cannot read {}: {e}", config_path.display()),
            );
            2
        }
    }
}

fn check_providers(config: &Config) -> u8 {
    println!("=== Providers ===");
    let socket_path = config.resolve_socket_path();

    let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
    let result = rt.block_on(async {
        let client = beachcomber::client::Client::new(socket_path);
        client.send_raw(serde_json::json!({"op": "status"})).await
    });

    match result {
        Ok(response) => {
            if let Some(data) = &response.data {
                let mut worst = 0u8;

                // Check backoff state.
                if let Some(backoff) = data.get("backoff")
                    && let Some(obj) = backoff.as_object()
                {
                    if obj.is_empty() {
                        print_check("pass", "backoff", "no providers in backoff");
                    } else {
                        for (name, info) in obj {
                            print_check("warn", "backoff", &format!("{name}: {info}"));
                            worst = worst.max(1);
                        }
                    }
                }

                // Check provider count.
                if let Some(count) = data.get("providers").and_then(|v| v.as_u64()) {
                    print_check("pass", "providers", &format!("{count} registered"));
                }

                // Check in-flight.
                if let Some(in_flight) = data.get("in_flight").and_then(|v| v.as_u64())
                    && in_flight > 0
                {
                    print_check("pass", "in-flight", &format!("{in_flight} executing"));
                }

                worst
            } else {
                print_check("warn", "providers", "no status data");
                1
            }
        }
        Err(e) => {
            print_check("fail", "providers", &format!("cannot query status: {e}"));
            2
        }
    }
}

fn check_cache(config: &Config) -> u8 {
    println!("=== Cache ===");
    let socket_path = config.resolve_socket_path();

    let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
    let result = rt.block_on(async {
        let client = beachcomber::client::Client::new(socket_path);
        client.send_raw(serde_json::json!({"op": "status"})).await
    });

    match result {
        Ok(response) => {
            if let Some(data) = &response.data {
                let mut worst = 0u8;

                if let Some(entries) = data.get("cache_entries").and_then(|v| v.as_u64()) {
                    print_check("pass", "entries", &format!("{entries} cached"));
                }

                // Check for stale entries in the cache listing.
                if let Some(cache) = data.get("cache").and_then(|v| v.as_object()) {
                    let mut stale_count = 0u64;
                    for (_key, entry) in cache {
                        if let Some(true) = entry.get("stale").and_then(|v| v.as_bool()) {
                            stale_count += 1;
                        }
                    }
                    if stale_count > 0 {
                        print_check("warn", "stale", &format!("{stale_count} stale entries"));
                        worst = worst.max(1);
                    } else {
                        print_check("pass", "stale", "no stale entries");
                    }
                }

                worst
            } else {
                print_check("warn", "cache", "no status data");
                1
            }
        }
        Err(e) => {
            print_check("fail", "cache", &format!("cannot query status: {e}"));
            2
        }
    }
}

#[cfg(target_os = "macos")]
fn check_procs(duration: u64) -> u8 {
    println!("=== Process Snapshot ({duration}s) ===");
    println!("Capturing process spawns via eslogger...");
    println!("(requires SIP to be configured for endpoint security)");
    println!();

    // eslogger exec captures process exec events on macOS.
    let output = std::process::Command::new("eslogger")
        .args(["exec"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn();

    let mut child = match output {
        Ok(c) => c,
        Err(e) => {
            print_check(
                "fail",
                "procs",
                &format!("cannot start eslogger: {e}. Try: sudo eslogger exec"),
            );
            return 2;
        }
    };

    // Let it capture for the specified duration.
    std::thread::sleep(std::time::Duration::from_secs(duration));
    let _ = child.kill();

    let output = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => {
            print_check("fail", "procs", &format!("eslogger error: {e}"));
            return 2;
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut counts: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    let mut total = 0u64;

    for line in stdout.lines() {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(line)
            && let Some(path) = json
                .pointer("/process/executable/path")
                .or(json.pointer("/event/exec/target/executable/path"))
                .and_then(|v| v.as_str())
        {
            let basename = path.rsplit('/').next().unwrap_or(path);
            *counts.entry(basename.to_string()).or_insert(0) += 1;
            total += 1;
        }
    }

    if total == 0 {
        print_check(
            "warn",
            "procs",
            "no process events captured (eslogger may need elevated privileges)",
        );
        return 1;
    }

    println!("Total process spawns: {total}");
    println!();

    // Known categories and their process names.
    let categories: &[(&str, &[&str], bool)] = &[
        ("git", &["git", "git-remote-https"], true),
        ("kubectl", &["kubectl"], true),
        ("gcloud", &["gcloud", "bq", "gsutil"], true),
        ("aws", &["aws"], true),
        ("terraform", &["terraform"], true),
        ("mise", &["mise"], true),
        ("direnv", &["direnv"], true),
        ("python", &["python", "python3", "pip", "pip3"], false),
        ("node", &["node", "npm", "npx", "yarn", "pnpm"], false),
        ("ruby", &["ruby", "gem", "bundle", "bundler"], false),
        ("shell", &["bash", "zsh", "sh", "fish"], false),
    ];

    println!("{:<20} {:>8}  Beachcomber", "Category", "Count");
    println!("{}", "-".repeat(50));

    for (name, procs, covered) in categories {
        let count: u64 = procs.iter().map(|p| counts.get(*p).unwrap_or(&0)).sum();
        if count > 0 {
            let status = if *covered { "replaces" } else { "n/a" };
            println!("{:<20} {:>8}  {}", name, count, status);
        }
    }

    // Show uncategorized top processes.
    let categorized: std::collections::HashSet<&str> = categories
        .iter()
        .flat_map(|(_, procs, _)| procs.iter().copied())
        .collect();

    let mut uncategorized: Vec<(&String, &u64)> = counts
        .iter()
        .filter(|(k, _)| !categorized.contains(k.as_str()))
        .collect();
    uncategorized.sort_by(|a, b| b.1.cmp(a.1));

    if !uncategorized.is_empty() {
        println!();
        println!("Other frequent processes:");
        for (name, count) in uncategorized.iter().take(10) {
            println!("  {:<20} {:>8}", name, count);
        }
    }

    println!();
    print_check(
        "pass",
        "procs",
        &format!("{total} spawns captured in {duration}s"),
    );
    0
}

#[cfg(not(target_os = "macos"))]
fn check_procs(duration: u64) -> u8 {
    println!("=== Process Snapshot ({duration}s) ===");
    println!("Capturing process spawns via /proc scanning...");
    println!();

    // On Linux, poll /proc for new PIDs.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut counts: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    let start = std::time::Instant::now();
    let sample_duration = std::time::Duration::from_secs(duration);

    // Initial scan to establish baseline.
    if let Ok(entries) = std::fs::read_dir("/proc") {
        for entry in entries.flatten() {
            if let Ok(name) = entry.file_name().into_string()
                && name.chars().all(|c| c.is_ascii_digit())
            {
                seen.insert(name);
            }
        }
    }

    // Poll for new processes.
    while start.elapsed() < sample_duration {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if let Ok(entries) = std::fs::read_dir("/proc") {
            for entry in entries.flatten() {
                if let Ok(pid) = entry.file_name().into_string()
                    && pid.chars().all(|c| c.is_ascii_digit())
                    && !seen.contains(&pid)
                {
                    seen.insert(pid.clone());
                    // Read the command name.
                    let comm_path = format!("/proc/{pid}/comm");
                    if let Ok(comm) = std::fs::read_to_string(&comm_path) {
                        let name = comm.trim().to_string();
                        *counts.entry(name).or_insert(0) += 1;
                    }
                }
            }
        }
    }

    let total: u64 = counts.values().sum();
    if total == 0 {
        print_check("warn", "procs", "no new processes detected");
        return 1;
    }

    println!("New processes detected: {total}");
    println!();

    let categories: &[(&str, &[&str], bool)] = &[
        ("git", &["git", "git-remote-https"], true),
        ("kubectl", &["kubectl"], true),
        ("gcloud", &["gcloud", "bq", "gsutil"], true),
        ("aws", &["aws"], true),
        ("terraform", &["terraform"], true),
        ("mise", &["mise"], true),
        ("direnv", &["direnv"], true),
        ("python", &["python", "python3", "pip", "pip3"], false),
        ("node", &["node", "npm", "npx", "yarn", "pnpm"], false),
        ("ruby", &["ruby", "gem", "bundle", "bundler"], false),
        ("shell", &["bash", "zsh", "sh", "fish"], false),
    ];

    println!("{:<20} {:>8}  Beachcomber", "Category", "Count");
    println!("{}", "-".repeat(50));

    for (name, procs, covered) in categories {
        let count: u64 = procs.iter().map(|p| counts.get(*p).unwrap_or(&0)).sum();
        if count > 0 {
            let status = if *covered { "replaces" } else { "n/a" };
            println!("{:<20} {:>8}  {}", name, count, status);
        }
    }

    let categorized: std::collections::HashSet<&str> = categories
        .iter()
        .flat_map(|(_, procs, _)| procs.iter().copied())
        .collect();

    let mut uncategorized: Vec<(&String, &u64)> = counts
        .iter()
        .filter(|(k, _)| !categorized.contains(k.as_str()))
        .collect();
    uncategorized.sort_by(|a, b| b.1.cmp(a.1));

    if !uncategorized.is_empty() {
        println!();
        println!("Other frequent processes:");
        for (name, count) in uncategorized.iter().take(10) {
            println!("  {:<20} {:>8}", name, count);
        }
    }

    println!();
    print_check(
        "pass",
        "procs",
        &format!("{total} spawns captured in {duration}s"),
    );
    0
}
