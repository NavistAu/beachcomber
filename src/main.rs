use beachcomber::cli::commands::check::{CheckCommands, run_check};
use beachcomber::cli::commands::get::{run_get, split_keys_and_path};
use beachcomber::cli::commands::kill::run_kill;
use beachcomber::cli::commands::put::run_put;
use beachcomber::cli::commands::watch::run_watch;
use beachcomber::cli::output_format::{parse_output_format, suffix_to_format};
use beachcomber::config::Config;
use clap::{Parser, Subcommand};
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

#[derive(Parser)]
#[command(
    name = "comb",
    version = env!("BEACHCOMBER_VERSION"),
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
        /// Output format: human (default), tsv, json, csv, table, sh
        #[arg(long, short = 'f', default_value = "")]
        format: String,
        /// Filter rows (e.g. provider=git, path=/home/*, stale=true); repeatable, AND semantics
        #[arg(long, value_name = "KEY=VALUE", action = clap::ArgAction::Append)]
        filter: Vec<String>,
        /// Sort by column: default, provider, path, field, value, age, stale (default: default)
        #[arg(long, default_value = "default")]
        sort: String,
        /// Disable value truncation
        #[arg(long)]
        no_trunc: bool,
        /// Maximum width for VALUE in human format: integer or 'auto' (terminal width). Default 120.
        #[arg(long)]
        max_width: Option<String>,
        /// Colorize output: auto (default), always, never
        #[arg(long, value_parser = ["auto", "always", "never"], default_value = "auto")]
        color: String,
        /// Use ASCII-only output (no Unicode box-drawing or ellipsis)
        #[arg(long)]
        ascii: bool,
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
            let effective_path: Option<String> = resolved_path.or_else(|| {
                std::env::current_dir()
                    .ok()
                    .map(|p| p.to_string_lossy().into_owned())
            });
            run_get(
                &config,
                &keys,
                effective_path.as_deref(),
                output_format,
                force,
                wait,
            )
        }
        Commands::Status {
            format,
            filter,
            sort,
            no_trunc,
            max_width,
            color,
            ascii,
        } => {
            let fmt = if format.is_empty() {
                None
            } else {
                Some(format.as_str())
            };
            beachcomber::cli::commands::status::run_status(
                &config,
                fmt,
                &filter,
                &sort,
                no_trunc,
                max_width.as_deref(),
                color.as_str(),
                ascii,
            )
        }
        Commands::Put {
            key,
            data,
            null,
            ttl,
            path,
        } => run_put(
            &config,
            &key,
            data.as_deref(),
            null,
            ttl.as_deref(),
            path.as_deref(),
        ),
        Commands::Watch { key, path, format } => {
            let output_format = parse_output_format(&format, fmt_template.as_deref());
            run_watch(&config, &key, path.as_deref(), output_format)
        }
        Commands::Eval { template, path } => {
            beachcomber::cli::commands::eval::run_eval(&config, &template, path.as_deref())
        }
        Commands::Init => run_init(),
        Commands::Check { check_cmd } => run_check(&config, check_cmd),
        Commands::Kill { timeout, socket } => {
            let socket_path = socket.unwrap_or_else(|| config.resolve_socket_path());
            run_kill(&socket_path, timeout)
        }
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

    let pid_path = socket_path.with_file_name("pid");
    let binary_hash = match beachcomber::singleton::hash_current_binary() {
        Ok(h) => h,
        Err(e) => {
            tracing::error!("failed to hash current binary for singleton identity: {e}");
            return ExitCode::FAILURE;
        }
    };
    let _singleton_lock = match beachcomber::singleton::acquire_or_supersede(
        &pid_path,
        env!("BEACHCOMBER_VERSION"),
        &binary_hash,
    ) {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            tracing::info!(
                "another daemon with the same binary is already running; exiting silently"
            );
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            tracing::error!("failed to acquire singleton lock: {e}");
            return ExitCode::FAILURE;
        }
    };

    if let Ok(exe) = std::env::current_exe() {
        let reaped = beachcomber::singleton::reap_orphans(&exe);
        if reaped > 0 {
            tracing::info!("reaped {reaped} orphan daemon(s) on startup");
        }
    }

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

        let cancel_for_self_watch = cancel.clone();
        if let Err(e) = beachcomber::singleton::spawn_binary_self_watch(move || {
            cancel_for_self_watch.cancel();
        }) {
            tracing::warn!(
                "failed to start binary self-watch: {e} (binary updates won't trigger restart)"
            );
        }

        let our_start_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        if let Ok(exe) = std::env::current_exe() {
            match beachcomber::singleton::binary_newer_than(&exe, our_start_ms) {
                Ok(true) => {
                    tracing::warn!(
                        "binary was modified between exec and watch registration; shutting down for restart"
                    );
                    cancel.cancel();
                }
                Ok(false) => {}  // common case — proceed
                Err(e) => {
                    tracing::warn!("could not stat binary for race-check: {e}");
                }
            }
        }

        let handle = beachcomber::daemon::start_in_process_with_cancel(socket_path, config, cancel);
        handle.await.ok();
    });

    ExitCode::SUCCESS
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
