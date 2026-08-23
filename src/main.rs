use beachcomber::cli::commands::check::{CheckCommands, run_check};
use beachcomber::cli::commands::daemon::run_daemon;
use beachcomber::cli::commands::get::{run_get, split_keys_and_path};
use beachcomber::cli::commands::init::run_init;
use beachcomber::cli::commands::kill::run_kill;
use beachcomber::cli::commands::put::run_put;
use beachcomber::cli::commands::watch::run_watch;
use beachcomber::cli::output_format::{parse_output_format, suffix_to_format};
use beachcomber::config::Config;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

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
        /// Exit automatically when the spawning parent process dies. Used by the
        /// test harness so a daemon never outlives the test that spawned it —
        /// even when the test process is SIGKILLed (which skips normal cleanup).
        #[arg(long)]
        exit_with_parent: bool,
        /// Exempt this daemon from orphan reaping by the canonical daemon.
        /// For deliberate, supervised daemons on non-canonical sockets (a
        /// supervisor parents the process to PID 1, which otherwise marks it
        /// orphaned). The flag only needs to be visible in argv.
        #[arg(long)]
        no_reap: bool,
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
    #[command(
        visible_alias = "s",
        after_help = "Columns (human/table, in order): PROVIDER PATH FIELD VALUE AGE POLL TTL\n\
            \n\
            PATH   Paths under $HOME render as ~ in the human preset (json/csv/tsv/sh keep the real path).\n\
            AGE    \"{age}\u{00d7}N\" — N = polls elapsed in the current lifecycle step.\n\
            POLL   Seconds until the next poll. While a source is failing, this is the retry countdown instead.\n\
            \x20      \"-\" = no poll path (pure watch, or a once/virtual/transient row).\n\
            TTL    \"<lead> Ps\u{00d7}K (total) <indicator>\":\n\
            \x20        lead      \u{2605} active | 3 2 1 0 decay countdown to eviction | \u{26a0} failing (cell becomes \"\u{26a0} #N\", N = failed attempts)\n\
            \x20        P\u{00d7}K       poll interval \u{00d7} keep-alive count\n\
            \x20        (total)   total time-to-eviction at the current rate, humanized\n\
            \x20        indicator \u{2219} watches files | \u{2299} watches + fsevents-reinstate | blank = poll-only\n\
            \n\
            --ascii swaps: \u{2605}\u{2192}* \u{26a0}\u{2192}! \u{00d7}\u{2192}x \u{2219}\u{2192}- \u{2299}\u{2192}+"
    )]
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
    Init {
        /// Append built-in virtual field defaults to ~/.config/beachcomber/config.toml
        #[arg(long)]
        write_config: bool,
    },
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
        Commands::Daemon {
            socket,
            exit_with_parent,
            // Read by the reaping canonical daemon from our argv, not by us.
            no_reap: _,
        } => {
            let socket_path = socket.unwrap_or_else(|| config.resolve_socket_path());
            run_daemon(socket_path, config, exit_with_parent)
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
        Commands::Init { write_config } => run_init(write_config),
        Commands::Check { check_cmd } => run_check(&config, check_cmd),
        Commands::Kill { timeout, socket } => {
            let socket_path = socket.unwrap_or_else(|| config.resolve_socket_path());
            run_kill(&socket_path, timeout)
        }
    }
}
