use beachcomber::config::Config;
use beachcomber::protocol::Format;
use clap::{Parser, Subcommand};
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

#[derive(Parser)]
#[command(
    name = "comb",
    version,
    about = "Centralized shell state daemon (beachcomber)"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the daemon (usually auto-launched via socket activation)
    Daemon {
        /// Override socket path
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// Query a cached value
    Get {
        /// Provider key (e.g., "hostname.name", "git.branch")
        key: String,
        /// Path context for directory-scoped providers
        path: Option<String>,
        /// Output format
        #[arg(short, long, default_value = "json")]
        format: String,
    },
    /// Trigger immediate recomputation of a provider
    Refresh {
        /// Provider key
        key: String,
        /// Path context
        path: Option<String>,
    },
    /// Show daemon status
    Status,
    /// List active providers
    List,
    /// Store data as a virtual provider
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
    Watch {
        /// Provider key (e.g., "git.branch")
        key: String,
        /// Path context for directory-scoped providers
        #[arg(long)]
        path: Option<String>,
        /// Output format (json or text)
        #[arg(short, long, default_value = "json")]
        format: String,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let config = Config::load();

    match cli.command {
        Commands::Daemon { socket } => {
            let socket_path = socket.unwrap_or_else(|| config.resolve_socket_path());
            run_daemon(socket_path, config)
        }
        Commands::Get { key, path, format } => {
            let format = match format.as_str() {
                "text" => Format::Text,
                _ => Format::Json,
            };
            run_get(&config, &key, path.as_deref(), format)
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
        Commands::Watch { key, path, format } => run_watch(&config, &key, path.as_deref(), &format),
    }
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
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("Received SIGINT, shutting down...");
            cancel_clone.cancel();
        });

        let handle = beachcomber::daemon::start_in_process_with_cancel(socket_path, config, cancel);
        handle.await.ok();
    });

    ExitCode::SUCCESS
}

fn run_get(config: &Config, key: &str, path: Option<&str>, format: Format) -> ExitCode {
    let socket_path = config.resolve_socket_path();

    if let Err(e) = beachcomber::daemon::ensure_daemon(&socket_path) {
        eprintln!("Failed to start daemon: {e}");
        return ExitCode::from(2);
    }

    let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
    rt.block_on(async {
        let client = beachcomber::client::Client::new(socket_path);

        if format == Format::Text {
            match client.get_text(key, path).await {
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
                    if response.ok {
                        if response.data.is_none() {
                            ExitCode::from(1)
                        } else {
                            println!("{}", serde_json::to_string_pretty(&response).unwrap());
                            ExitCode::SUCCESS
                        }
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

fn run_watch(config: &Config, key: &str, path: Option<&str>, format: &str) -> ExitCode {
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

        let fmt = if format == "text" { Some("text") } else { None };
        if let Err(e) = session.watch(key, path, fmt).await {
            eprintln!("Error: {e}");
            return ExitCode::from(2);
        }

        loop {
            match session.read_watch_line().await {
                Ok(Some(line)) => {
                    print!("{line}");
                }
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
