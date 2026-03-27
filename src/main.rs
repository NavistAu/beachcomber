use clap::{Parser, Subcommand};
use shellstate::config::Config;
use shellstate::protocol::Format;
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "shellstate", about = "Centralized shell state daemon")]
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
    Poke {
        /// Provider key
        key: String,
        /// Path context
        path: Option<String>,
    },
    /// Show daemon status
    Status,
    /// List active providers
    List,
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
        Commands::Poke { key, path } => {
            run_poke(&config, &key, path.as_deref())
        }
        Commands::Status | Commands::List => {
            eprintln!("Not yet implemented (planned for a future release)");
            ExitCode::from(2)
        }
    }
}

fn run_daemon(socket_path: PathBuf, config: Config) -> ExitCode {
    let filter = config.daemon.log_level.parse()
        .unwrap_or(tracing_subscriber::filter::LevelFilter::INFO);
    tracing_subscriber::fmt()
        .with_max_level(filter)
        .init();

    tracing::info!("Starting shellstate daemon");
    tracing::info!("Socket: {:?}", socket_path);

    let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
    rt.block_on(async {
        let handle = shellstate::daemon::start_in_process(socket_path, config);
        handle.await.ok();
    });

    ExitCode::SUCCESS
}

fn run_get(config: &Config, key: &str, path: Option<&str>, format: Format) -> ExitCode {
    let socket_path = config.resolve_socket_path();

    if let Err(e) = shellstate::daemon::ensure_daemon(&socket_path) {
        eprintln!("Failed to start daemon: {}", e);
        return ExitCode::from(2);
    }

    let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
    let result = rt.block_on(async {
        let client = shellstate::client::Client::new(socket_path);

        if format == Format::Text {
            match client.get_text(key, path).await {
                Ok(text) => {
                    print!("{}", text);
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("Error: {}", e);
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
                    eprintln!("Error: {}", e);
                    ExitCode::from(2)
                }
            }
        }
    });

    result
}

fn run_poke(config: &Config, key: &str, path: Option<&str>) -> ExitCode {
    let socket_path = config.resolve_socket_path();

    if let Err(e) = shellstate::daemon::ensure_daemon(&socket_path) {
        eprintln!("Failed to start daemon: {}", e);
        return ExitCode::from(2);
    }

    let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
    rt.block_on(async {
        let client = shellstate::client::Client::new(socket_path);
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
                eprintln!("Error: {}", e);
                ExitCode::from(2)
            }
        }
    })
}
