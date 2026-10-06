use beachcomber::cli::commands::check::{CheckCommands, parse_connect_timeout, run_check};
use beachcomber::cli::commands::daemon::run_daemon;
use beachcomber::cli::commands::get::{run_get, split_keys_and_path};
use beachcomber::cli::commands::init::run_init;
use beachcomber::cli::commands::kill::run_kill;
use beachcomber::cli::commands::put::run_put;
use beachcomber::cli::commands::refresh::run_refresh;
use beachcomber::cli::commands::watch::run_watch;
use beachcomber::cli::output_format::{parse_output_format, suffix_to_format};
use beachcomber::config::Config;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

/// The `comb status` help legend, rendered with the glyph set the user will
/// actually see: `--ascii` anywhere in argv selects the ascii variant (clap
/// builds help before parsing, so the flag is pre-scanned from argv). The
/// unicode variant ends with the ascii translation line; the ascii variant
/// needs no translating and omits it.
fn status_after_help() -> String {
    let ascii = std::env::args().any(|a| a == "--ascii");
    let (star, warn, times, dot, ring, rendered) = if ascii {
        ("*", "!", "x", "-", "+", ">")
    } else {
        (
            "\u{2605}", "\u{26a0}", "\u{00d7}", "\u{2219}", "\u{2299}", "\u{25b8}",
        )
    };
    let mut help = format!(
        "Reading the output:\n\
        \n\
        \x20 PROVIDER  FIELD   SOURCE    VALUE       PATH              AGE    POLL  TTL\n\
        \x20 git       branch  head      main        ~/ws/beachcomber  14s{times}2   37s  {star}  60s{times}12 (12m) {ring}\n\
        \x20 git       branch  template  {rendered} main-dev  ~/ws/beachcomber  0s       -   ---\n\
        \n\
        \x20 Each row is one cache ENTRY, returned by a Query ie `comb get`.\n\
        \x20 - ENTRY Refreshes VALUE by SOURCE(PROVIDER.FIELD @ PATH).\n\
        \x20 - Polls and FSEvents Refresh VALUE from SOURCE.\n\
        \x20 - Queries reset AGE.PollCount.\n\
        \x20 - If AGE.PollCount >= TTL.PollCount, the ENTRY Idles.\n\
        \x20   - Idle entries halve their TTL.PollRate each TTL.PollLifetime.\n\
        \x20   - After 4 lifetimes, the ENTRY is Evicted from cache.\n\
        \n\
        \x20 SOURCE head        Which Source of the PROVIDER produced VALUE; `put` / `config:<file>` for stored data\n\
        \x20        template    Rendered here, from this shell's directory and environment — never cached\n\
        \x20 VALUE  {rendered} main-dev  A rendered VALUE, marked because it is computed, not stored (cyan with colour on)\n\
        \x20 AGE    43s         RefreshTime:  Time since VALUE Refreshed from SOURCE\n\
        \x20        {times}2          PollCount:    Polls since last Query\n\
        \x20 POLL                             Time until the next Poll / next Retry if Failing\n\
        \x20 TTL    {star} 3..0    {star} Active:       ENTRY is within first TTL.PollLifetime\n\
        \x20                 # Number:        ENTRY is Idle, Polls halve each lifetime, # lifetimes until Eviction\n\
        \x20        {warn} #3                      Failing \u{2014} 3 failures in a row; POLL shows the Retry countdown\n\
        \x20        60s{times}12      PollLifetime: Current lifetime, ie 720s with 12 VALUE Refreshes\n\
        \x20                      PollRate:   Every 60s\n\
        \x20                      PollCount:  12 times\n\
        \x20        (12m)       EvictTime:    Total time to Eviction\n\
        \x20        {dot} {ring}       {dot} WatchActive:  Watches files while Active; not during the Idle countdown\n\
        \x20                 {ring} WatchAlways:   Watches files until Evicted\n\
        \x20                 <blank>          No FSEvents, Refreshes only by Polling"
    );
    if !ascii {
        help.push_str(
            "\n\nWith --ascii: * ! x - + > replace \u{2605} \u{26a0} \u{00d7} \u{2219} \u{2299} \u{25b8}.",
        );
    }
    help
}

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
    #[command(visible_alias = "g", mut_arg("path", |a| a.short('p')))]
    Get {
        /// Provider key(s) (e.g., "hostname.name", "git.branch"). If the last positional
        /// looks like a path (starts with `.`/`~`/`/`, or contains `/`), it is used as the
        /// path context instead of a key.
        #[arg(required = true, num_args = 1..)]
        keys: Vec<String>,
        #[command(flatten)]
        path: PathArgs,
        /// Output format (text, json, sh, csv, tsv, CSV, TSV, fmt)
        #[arg(short, long, default_value = "text")]
        format: String,
        /// Re-execute every source the key demands, each at its own scope; static records survive
        #[arg(long)]
        force: bool,
        /// Block until a fresh value is available
        #[arg(long)]
        wait: bool,
    },
    /// Show daemon status
    #[command(visible_alias = "s", after_help = status_after_help())]
    Status {
        /// Output format: human (default), tsv, json, csv, table, sh
        #[arg(long, short = 'f', default_value = "", hide_default_value = true)]
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
        /// Object destructuring depth: 0 unlimited, 1 top-level fields only, 2 one level down.
        /// Default: 2 for human/table, 1 for json/tsv/csv/sh.
        #[arg(long)]
        depth: Option<usize>,
    },
    /// Put data at any node, deep-merging into what is already there
    #[command(visible_alias = "p")]
    Put {
        /// Node to write (e.g., "myapp" or "myapp.deploy.status")
        key: String,
        /// JSON data (an object at the root, any JSON below it); omit when using --null
        data: Option<String>,
        /// Remove the node's static records, dropping the entry when none remain; provider records are untouched
        #[arg(long)]
        null: bool,
        /// Replace the subtree at the node instead of merging into it
        #[arg(long)]
        replace: bool,
        /// Expected refresh interval (e.g., "30s", "5m")
        #[arg(long)]
        ttl: Option<String>,
        #[command(flatten)]
        path: PathArgs,
    },
    /// Re-execute the Sources a key demands, each at its own scope
    #[command(visible_alias = "r")]
    Refresh {
        /// Provider key (e.g., "git" or "git.branch")
        key: String,
        /// Path context as a positional, the flag's other spelling
        #[arg(value_name = "PATH", conflicts_with_all = ["path", "global"])]
        at: Option<String>,
        #[command(flatten)]
        path: PathArgs,
    },
    /// Watch a key and stream changes to stdout
    #[command(visible_alias = "w")]
    Watch {
        /// Provider key (e.g., "git.branch")
        key: String,
        /// Path context as a positional, the flag's other spelling
        #[arg(value_name = "PATH", conflicts_with_all = ["path", "global"])]
        at: Option<String>,
        #[command(flatten)]
        path: PathArgs,
        /// Output format (text, json, sh)
        #[arg(short, long, default_value = "text")]
        format: String,
    },
    /// Evaluate a value expression against cached values
    #[command(visible_alias = "e")]
    Eval {
        /// Template or expression: `{{ provider.field }}`, or text with several tags
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
        /// How long to keep trying to reach the daemon before reporting it
        /// unreachable: `0` for a single attempt, or e.g. `250ms`, `2s`
        #[arg(
            long,
            global = true,
            value_name = "DURATION",
            default_value = "1s",
            value_parser = parse_connect_timeout
        )]
        connect_timeout: std::time::Duration,
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

/// The path a `get`, `put`, `refresh`, `watch` or `eval` addresses: `Ok(None)` for `--global` (the
/// daemon's global entry, which is `$HOME`), otherwise the requested path made
/// absolute, otherwise the current working directory.
///
/// A relative path is the caller's, not the daemon's: the daemon canonicalises
/// what it receives against *its own* working directory, so `comb g git.branch .`
/// would otherwise answer for wherever the daemon happened to be started. The
/// cwd is read only when it is needed — an absolute path must keep working from
/// a directory that no longer exists — and when it is needed and unreadable the
/// answer is an error, never a silent read of the global entry.
fn cli_path(global: bool, requested: Option<String>) -> Result<Option<String>, ExitCode> {
    if global {
        return Ok(None);
    }
    let cwd = || {
        std::env::current_dir().map_err(|e| {
            eprintln!("cannot determine the current directory: {e}");
            ExitCode::from(2)
        })
    };
    Ok(Some(match requested {
        Some(p) if std::path::Path::new(&p).is_relative() => {
            cwd()?.join(p).to_string_lossy().into_owned()
        }
        Some(p) => p,
        None => cwd()?.to_string_lossy().into_owned(),
    }))
}

/// `--path` / `--global`, shared by get, put, refresh and watch.
#[derive(clap::Args)]
struct PathArgs {
    /// Path context for directory-scoped providers (defaults to the current working directory)
    #[arg(long, value_name = "PATH")]
    path: Option<String>,
    /// Use the global entry ($HOME) instead of a directory
    #[arg(long, conflicts_with = "path")]
    global: bool,
}

fn main() -> ExitCode {
    // Rust's runtime ignores SIGPIPE, which turns `comb status | head` into
    // a broken-pipe panic on the first write after the reader exits. Restore
    // the default disposition so the process dies silently like any CLI.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    let (args, fmt_template) = preprocess_args();
    let cli = Cli::parse_from(args);

    // Only the daemon writes the managed defaults, and it does so before
    // `Config::load()` reads the directory, so the defaults it seeds are this
    // binary's. A directory it cannot write is a refusal to start (spec §3.3).
    if matches!(cli.command, Commands::Daemon { .. }) {
        let dir = Config::defaults_dir_for(&Config::config_path());
        if let Err(e) =
            beachcomber::daemon::defaults::write_managed_defaults(&dir, env!("BEACHCOMBER_VERSION"))
        {
            eprintln!(
                "error: could not write managed defaults to {}: {e}",
                dir.display()
            );
            return ExitCode::from(2);
        }
    }
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
            path: PathArgs { path, global },
            format,
            force,
            wait,
        } => {
            let output_format = parse_output_format(&format, fmt_template.as_deref());
            let (keys, resolved_path) = split_keys_and_path(keys, path);
            // `--global` addresses $HOME, which the daemon keys as the pathless
            // global entry, so it is sent as "no path" — and it cannot be
            // combined with a path, whether that came from `--path` (clap
            // refuses it) or from a trailing positional.
            if global && resolved_path.is_some() {
                eprintln!("cannot combine --global with a path");
                return ExitCode::from(2);
            }
            let effective_path = match cli_path(global, resolved_path) {
                Ok(p) => p,
                Err(code) => return code,
            };
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
            depth,
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
                depth,
            )
        }
        Commands::Put {
            key,
            data,
            null,
            replace,
            ttl,
            path: PathArgs { path, global },
        } => {
            let effective_path = match cli_path(global, path) {
                Ok(p) => p,
                Err(code) => return code,
            };
            run_put(
                &config,
                &key,
                data.as_deref(),
                null,
                replace,
                ttl.as_deref(),
                effective_path.as_deref(),
            )
        }
        Commands::Refresh {
            key,
            at,
            path: PathArgs { path, global },
        } => {
            let effective_path = match cli_path(global, path.or(at)) {
                Ok(p) => p,
                Err(code) => return code,
            };
            run_refresh(&config, &key, effective_path.as_deref())
        }
        Commands::Watch {
            key,
            at,
            path: PathArgs { path, global },
            format,
        } => {
            let output_format = parse_output_format(&format, fmt_template.as_deref());
            let effective_path = match cli_path(global, path.or(at)) {
                Ok(p) => p,
                Err(code) => return code,
            };
            run_watch(&config, &key, effective_path.as_deref(), output_format)
        }
        Commands::Eval { template, path } => {
            // `eval` is a query like `get`, so it addresses the same path: the
            // one given, else the caller's directory; it has no `--global`.
            let path = match cli_path(false, path) {
                Ok(Some(p)) => p,
                Ok(None) => unreachable!("eval never asks for the global entry"),
                Err(code) => return code,
            };
            beachcomber::cli::commands::eval::run_eval(&config, &template, &path)
        }
        Commands::Init => run_init(),
        Commands::Check {
            connect_timeout,
            check_cmd,
        } => run_check(&config, check_cmd, connect_timeout),
        Commands::Kill { timeout, socket } => {
            let socket_path = socket.unwrap_or_else(|| config.resolve_socket_path());
            run_kill(&socket_path, timeout)
        }
    }
}
