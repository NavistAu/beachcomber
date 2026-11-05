use beachcomber::cli::commands::check::{CheckCommands, run_check};
use beachcomber::cli::commands::daemon::run_daemon;
use beachcomber::cli::commands::get::{run_get, split_keys_and_path};
use beachcomber::cli::commands::init::run_init;
use beachcomber::cli::commands::kill::run_kill;
use beachcomber::cli::commands::put::run_put;
use beachcomber::cli::commands::refresh::run_refresh;
use beachcomber::cli::commands::watch::run_watch;
use beachcomber::cli::exit::Outcome;
use beachcomber::cli::output_format::{
    EVAL, Format, GET, STATUS, WATCH, eval_format, get_format, rewrite_suffix, status_format,
    watch_format,
};
use beachcomber::cli::style::{ColorChoice, Glyphs, Style};
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
    let g = Glyphs::of(ascii);
    let (star, warn, times, dot, rendered) = (g.star, g.warn, g.times, g.dot, g.rendered);
    let mut help = format!(
        "Reading the output:\n\
        \n\
        \x20 PROVIDER  FIELD   SOURCE        VALUE       PATH              AGE    POLL  TTL\n\
        \x20 git       branch  source(head)  main        ~/ws/beachcomber  14s{times}2   37s  {star}  60s{times}12 (12m) {dot}\n\
        \x20 git       branch  rendered      {rendered} main-dev  ~/ws/beachcomber  0s       -   ---\n\
        \n\
        \x20 Each row is one cache ENTRY, returned by a Query ie `comb get`.\n\
        \x20 - ENTRY Refreshes VALUE by SOURCE(PROVIDER.FIELD @ PATH).\n\
        \x20 - Polls and FSEvents Refresh VALUE from SOURCE.\n\
        \x20 - Queries reset AGE.PollCount.\n\
        \x20 - If AGE.PollCount >= TTL.PollCount, the ENTRY Idles.\n\
        \x20   - Idle entries halve their TTL.PollRate each TTL.PollLifetime.\n\
        \x20   - After 4 lifetimes, the ENTRY is Evicted from cache.\n\
        \n\
        \x20 SOURCE source(head)  Which Source of the PROVIDER produced VALUE; `put` / `config(<file>)` for stored data,\n\
        \x20                      `computed(return)` for a leaf a provider line computed\n\
        \x20        rendered      Rendered here, from this shell's directory and environment — never cached\n\
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
        \x20        {dot}         {dot} Watch:        Watches files while Active; not during the Idle countdown,\n\
        \x20                                  when a Query checks them instead\n\
        \x20                 <blank>          No FSEvents, Refreshes only by Polling"
    );
    if !ascii {
        help.push_str(
            "\n\nWith --ascii: * ! x - > replace \u{2605} \u{26a0} \u{00d7} \u{2219} \u{25b8}.",
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
    after_help = "Format suffixes are the short spelling of -f on get, watch and status: comb g.j (json), \
        g.s (sh), g.c / g.C (csv, CSV with a header row), g.t / g.T (tsv, TSV with a header row), \
        g.f TEMPLATE (a template)."
)]
struct Cli {
    /// A read that finds nothing exits 1 rather than 0 (`get`, `eval`)
    #[arg(
        long,
        global = true,
        env = "BEACHCOMBER_STRICT",
        value_parser = clap::builder::FalseyValueParser::new(),
        hide_env_values = true
    )]
    strict: bool,
    /// Colour: auto (a terminal, unless NO_COLOR is set), always or never.
    /// Wins over NO_COLOR, for comb's own output and the template filters alike
    #[arg(long, global = true, value_enum, value_name = "WHEN", default_value_t = ColorChoice::Auto)]
    color: ColorChoice,
    /// Render templates with `exec()` off: a template that calls it fails,
    /// naming why, and nothing is spawned
    #[arg(long, global = true)]
    no_exec: bool,
    /// ASCII glyphs only (no Unicode box drawing, marks or ellipsis)
    #[arg(long, global = true)]
    ascii: bool,
    /// The daemon's socket. Beats `[daemon] socket_path` and
    /// `$BEACHCOMBER_SOCKET`, which it matches
    #[arg(long, global = true, value_name = "PATH")]
    socket: Option<PathBuf>,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the daemon (usually auto-launched via socket activation)
    #[command(visible_alias = "d")]
    Daemon {
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
        /// Write a newline to this inherited file descriptor once the socket
        /// is served (or a daemon of this build already serves it), then close
        /// it. A daemon that exits first closes it unwritten. How comb and
        /// every SDK know a daemon they started is up.
        #[arg(long, value_name = "FD", value_parser = clap::value_parser!(i32).range(3..))]
        ready_fd: Option<i32>,
    },
    /// Query one or more cached values.
    ///
    /// Positionals are keys (`provider.field`). After the first, a last positional that starts
    /// with `/`, `~` or `.` is the directory to address, the same as --path; no key starts so.
    #[command(visible_alias = "g")]
    Get {
        /// Keys to read (e.g., "hostname.name", "git.branch"); after the first, a last one
        /// starting with /, ~ or . is the directory to address, the same as --path
        #[arg(required = true, num_args = 1..)]
        keys: Vec<String>,
        #[command(flatten)]
        path: PathArgs,
        #[arg(short, long, default_value = "text", value_parser = get_format, help = GET.help())]
        format: Format,
        /// Re-execute every source the key demands, each at the instance that answers for the directory; static records survive
        #[arg(long)]
        force: bool,
        /// How long to wait for a run the read needs before answering what is stored: a duration
        /// (`0` answers at once) or `forever`. Without it, a stored record waits its Source's
        /// budget, and a cold read or --force waits for the run
        #[arg(long, value_name = "DURATION|forever", value_parser = str::parse::<beachcomber::protocol::Wait>)]
        wait: Option<beachcomber::protocol::Wait>,
    },
    /// Show daemon status
    #[command(visible_alias = "s", after_help = status_after_help())]
    Status {
        #[arg(long, short = 'f', default_value = "human", value_parser = status_format, help = STATUS.help())]
        format: Format,
        /// Filter rows: KEY=VALUE or KEY!=VALUE, repeatable, all must match. Keys: provider (base
        /// name), facet, path, field, source, value, shadowed, lifecycle;
        /// `*` globs, and `-` means none for facet, path and value. Without a
        /// filter on that key, facet=-, value!=- and shadowed=false apply: facet rows, absent values
        /// and records a read here does not take are hidden.
        #[arg(long, value_name = "KEY=VALUE", action = clap::ArgAction::Append)]
        filter: Vec<String>,
        /// Show every row: drop the default facet=-, value!=- and shadowed=false filters
        #[arg(long, short = 'a')]
        all: bool,
        /// Sort by column: default (provider, then its facets, field, path), provider, path, field,
        /// value, age, lifecycle, poll_interval
        #[arg(long, default_value = "default")]
        sort: String,
        /// Disable value truncation
        #[arg(long)]
        no_trunc: bool,
        /// Maximum width for VALUE in human format: integer or 'auto' (terminal width). Default 120.
        #[arg(long)]
        max_width: Option<String>,
        /// Object destructuring depth: 0 unlimited, 1 top-level fields only, 2 one level down.
        /// Default: 2 for human/table, 1 for json/tsv/csv/sh; 0 with --tree.
        #[arg(long)]
        depth: Option<usize>,
        /// Under each value, the layers a read from here consults — templates, then put,
        /// config and Source records, nearest directory first — the one that answered drawn
        /// plainly and the rest dimmed. human, table and json only.
        #[arg(long)]
        tree: bool,
    },
    /// Put data at any node, replacing what is there; null data removes the node
    #[command(visible_alias = "p")]
    Put {
        /// Node to write (e.g., "myapp" or "myapp.deploy.status")
        key: String,
        /// JSON data (an object at the root, any JSON below it); `null` removes the node's put, dropping the entry when none remain; provider records are untouched
        data: String,
        /// Directory to address, the same as --path; it starts with /, ~ or .
        #[arg(
            value_name = "PATH",
            conflicts_with_all = ["path", "global"],
            value_parser = trailing_dir
        )]
        at: Option<String>,
        /// Expire the node: valid for this long (e.g., "30s", "5m"), stale for one decay step, then removed; the next put renews it. Whole seconds, at least 1s; not with null data
        #[arg(long, value_name = "DURATION", value_parser = beachcomber::config::duration_arg)]
        ttl: Option<std::time::Duration>,
        #[command(flatten)]
        path: PathArgs,
    },
    /// Re-execute the Sources a key demands, each at the instance that answers for the directory
    #[command(visible_alias = "r")]
    Refresh {
        /// Provider key (e.g., "git" or "git.branch")
        key: String,
        /// Directory to address, the same as --path; it starts with /, ~ or .
        #[arg(
            value_name = "PATH",
            conflicts_with_all = ["path", "global"],
            value_parser = trailing_dir
        )]
        at: Option<String>,
        #[command(flatten)]
        path: PathArgs,
    },
    /// Watch a key and stream changes to stdout
    #[command(visible_alias = "w")]
    Watch {
        /// Provider key (e.g., "git.branch")
        key: String,
        /// Directory to address, the same as --path; it starts with /, ~ or .
        #[arg(
            value_name = "PATH",
            conflicts_with_all = ["path", "global"],
            value_parser = trailing_dir
        )]
        at: Option<String>,
        #[command(flatten)]
        path: PathArgs,
        #[arg(short, long, default_value = "text", value_parser = watch_format, help = WATCH.help())]
        format: Format,
    },
    /// Evaluate a value expression against cached values
    #[command(visible_alias = "e")]
    Eval {
        /// Template or expression: `{{ provider.field }}`, or text with several tags
        template: String,
        /// Directory to address, the same as --path; it starts with /, ~ or .
        #[arg(
            value_name = "PATH",
            conflicts_with_all = ["path", "global"],
            value_parser = trailing_dir
        )]
        at: Option<String>,
        #[command(flatten)]
        path: PathArgs,
        #[arg(short, long, default_value = "text", value_parser = eval_format, help = EVAL.help())]
        format: Format,
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
            value_parser = beachcomber::config::duration_arg
        )]
        connect_timeout: std::time::Duration,
        #[command(subcommand)]
        check_cmd: Option<CheckCommands>,
    },
    /// Stop the running daemon (it will socket-activate on the next query)
    #[command(visible_alias = "k")]
    Kill {
        /// How long to wait for the daemon to exit, e.g. `5s` or `500ms`
        #[arg(
            long,
            value_name = "DURATION",
            default_value = "5s",
            value_parser = beachcomber::config::duration_arg
        )]
        timeout: std::time::Duration,
    },
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
fn cli_path(global: bool, requested: Option<String>) -> Result<Option<String>, Outcome> {
    if global {
        return Ok(None);
    }
    // A `~` the shell left alone (it was quoted) means what it would have
    // expanded to; a relative path is the caller's.
    libbeachcomber::path::normalise_here(requested.as_deref())
        .map(Some)
        .map_err(|e| {
            beachcomber::cli::report::error_named(e);
            Outcome::Error
        })
}

/// A trailing `[PATH]`: a directory only when it starts with `/`, `~` or
/// `.`, the one rule `get` reads its last argument by
/// ([`is_path_like`](beachcomber::cli::commands::get::is_path_like)). Where the
/// last argument can only be a directory, anything else is refused with the
/// spelling that would have worked.
fn trailing_dir(arg: &str) -> Result<String, String> {
    if beachcomber::cli::commands::get::is_path_like(arg) {
        Ok(arg.to_string())
    } else {
        Err(format!(
            "{arg} is not a directory: write ./{arg} or --path {arg}"
        ))
    }
}

/// `-p/--path` / `--global`, shared by every command that addresses a
/// directory — get, put, refresh, watch and eval — beside the trailing
/// `[PATH]` each takes, so all five say the same thing in the same words.
#[derive(clap::Args)]
struct PathArgs {
    /// Directory to address (default: the current directory)
    #[arg(short = 'p', long, value_name = "PATH")]
    path: Option<String>,
    /// Address the global entry ($HOME) instead of a directory
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

    let args = match rewrite_suffix(std::env::args().collect()) {
        Ok(args) => args,
        Err(e) => {
            beachcomber::cli::report::error_named(e);
            return Outcome::Error.code(false);
        }
    };
    let cli = Cli::parse_from(args);
    cli.color.apply_to_env();
    beachcomber::cli::set_no_exec(cli.no_exec);
    let strict = cli.strict;
    run(cli).code(strict)
}

/// Run the command `cli` names.
fn run(cli: Cli) -> Outcome {
    let style = Style::detect(cli.color, cli.ascii);
    // Only the daemon writes the managed defaults, and it does so before
    // `Config::load()` reads the directory, so the defaults it seeds are this
    // binary's. A directory it cannot write is a refusal to start (spec §3.3).
    if matches!(cli.command, Commands::Daemon { .. }) {
        let dir = Config::defaults_dir_for(&Config::config_path());
        if let Err(e) =
            beachcomber::daemon::defaults::write_managed_defaults(&dir, env!("BEACHCOMBER_VERSION"))
        {
            beachcomber::cli::report::error(
                "daemon",
                format_args!("could not write managed defaults to {}: {e}", dir.display()),
            );
            return Outcome::Error;
        }
    }
    let mut config = Config::load();
    config.set_socket_flag(cli.socket);

    match cli.command {
        Commands::Daemon {
            exit_with_parent,
            // Read by the reaping canonical daemon from our argv, not by us.
            no_reap: _,
            ready_fd,
        } => {
            let socket_path = config.resolve_socket_path();
            run_daemon(socket_path, config, exit_with_parent, ready_fd)
        }
        Commands::Get {
            keys,
            path: PathArgs { path, global },
            format,
            force,
            wait,
        } => {
            let (keys, resolved_path) = split_keys_and_path(keys, path);
            // `--global` addresses $HOME, which the daemon keys as the pathless
            // global entry, so it is sent as "no path" — and it cannot be
            // combined with a path, whether that came from `--path` (clap
            // refuses it) or from a trailing positional.
            if global && resolved_path.is_some() {
                beachcomber::cli::report::error("--global", "cannot be combined with a path");
                return Outcome::Error;
            }
            let effective_path = match cli_path(global, resolved_path) {
                Ok(p) => p,
                Err(code) => return code,
            };
            run_get(
                &config,
                &keys,
                effective_path.as_deref(),
                format,
                force,
                wait.unwrap_or_default(),
            )
        }
        Commands::Status {
            format,
            filter,
            all,
            sort,
            no_trunc,
            max_width,
            depth,
            tree,
        } => beachcomber::cli::commands::status::run_status(
            &config,
            &format,
            &filter,
            all,
            &sort,
            no_trunc,
            max_width.as_deref(),
            style,
            depth,
            tree,
        ),
        Commands::Put {
            key,
            data,
            at,
            ttl,
            path: PathArgs { path, global },
        } => {
            let effective_path = match cli_path(global, path.or(at)) {
                Ok(p) => p,
                Err(code) => return code,
            };
            run_put(&config, &key, &data, ttl, effective_path.as_deref())
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
            let effective_path = match cli_path(global, path.or(at)) {
                Ok(p) => p,
                Err(code) => return code,
            };
            run_watch(&config, &key, effective_path.as_deref(), format)
        }
        Commands::Eval {
            template,
            at,
            path: PathArgs { path, global },
            format,
        } => {
            // `eval` is a query like `get`, so it addresses the same path: the
            // one given, the global entry, else the caller's directory.
            let path = match cli_path(global, path.or(at)) {
                Ok(p) => p,
                Err(code) => return code,
            };
            beachcomber::cli::commands::eval::run_eval(&config, &template, path.as_deref(), format)
        }
        Commands::Init => run_init(),
        Commands::Check {
            connect_timeout,
            check_cmd,
        } => run_check(&config, check_cmd, connect_timeout, style),
        Commands::Kill { timeout } => run_kill(&config.resolve_socket_path(), timeout),
    }
}
