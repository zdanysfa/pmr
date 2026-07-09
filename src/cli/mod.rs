//! CLI definition (clap) and dispatch.

pub mod commands;
pub mod logs;
pub mod startup;
pub mod table;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "pmr",
    version,
    about = "pmr — production process manager (pm2 rewritten in Rust, fork mode)"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Cmd,
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)] // Cmd lives once on the stack at startup
pub enum Cmd {
    /// Start an app (script path or ecosystem .json/.yaml/.toml file)
    Start {
        /// Script, binary, config file, or an existing (stopped) name/id
        target: String,
        /// Process name
        #[arg(long)]
        name: Option<String>,
        /// Number of instances
        #[arg(short = 'i', long)]
        instances: Option<u32>,
        /// Interpreter (default: auto-detect by extension; "none" = run directly)
        #[arg(long)]
        interpreter: Option<String>,
        /// Working directory
        #[arg(long)]
        cwd: Option<String>,
        /// Env profile to apply (env_<profile> section of the config file)
        #[arg(long)]
        env: Option<String>,
        /// Disable automatic restart
        #[arg(long)]
        no_autorestart: bool,
        /// Max unstable restarts before giving up
        #[arg(long)]
        max_restarts: Option<u32>,
        /// Restart above this memory usage (e.g. 200M, 1G)
        #[arg(long)]
        max_memory_restart: Option<String>,
        /// Fixed delay between restarts (ms)
        #[arg(long)]
        restart_delay: Option<u64>,
        /// Exponential backoff restart delay base (ms)
        #[arg(long)]
        exp_backoff_restart_delay: Option<u64>,
        /// Cron pattern for scheduled restart
        #[arg(long)]
        cron_restart: Option<String>,
        /// Watch working dir and restart on change
        #[arg(long)]
        watch: bool,
        /// Prefix logs with timestamp
        #[arg(long)]
        time: bool,
        /// Milliseconds before SIGKILL after the stop signal
        #[arg(long)]
        kill_timeout: Option<u64>,
        /// Arguments passed to the script (after --)
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Stop process(es): id | name | all
    Stop { target: String },
    /// Restart process(es): id | name | all
    Restart { target: String },
    /// Alias for restart (pmr is fork-only)
    Reload { target: String },
    /// Stop and remove process(es): id | name | all
    #[command(alias = "del")]
    Delete { target: String },
    /// Reset restart counters
    Reset { target: String },
    /// List processes
    #[command(alias = "list", alias = "l", alias = "ps", alias = "status")]
    Ls,
    /// List processes as JSON
    Jlist,
    /// Show details for a process
    #[command(alias = "show", alias = "info", alias = "desc")]
    Describe { target: String },
    /// Show environment of a process
    Env { target: String },
    /// Print process id(s) by name
    Id { name: String },
    /// Print OS pid(s) of a process
    Pid { target: Option<String> },
    /// Scale an app to N instances
    Scale { name: String, instances: u32 },
    /// Send a signal to process(es)
    #[command(name = "sendSignal", alias = "send-signal")]
    SendSignal { signal: String, target: String },
    /// Stream logs (default: all processes)
    Logs {
        target: Option<String>,
        /// Number of lines to tail from files first
        #[arg(long, default_value_t = 15)]
        lines: usize,
        /// Only stderr
        #[arg(long)]
        err: bool,
        /// Only stdout
        #[arg(long)]
        out: bool,
        /// Print tail and exit (no live stream)
        #[arg(long)]
        nostream: bool,
        /// Prefix each line with its arrival timestamp
        #[arg(long)]
        timestamp: bool,
        /// Raw output (no pm_id|name gutter)
        #[arg(long)]
        raw: bool,
    },
    /// Truncate log files
    Flush { target: Option<String> },
    /// Reopen all log files (for logrotate)
    #[command(name = "reloadLogs", alias = "reload-logs")]
    ReloadLogs,
    /// Save the process list for resurrect
    #[command(alias = "dump")]
    Save,
    /// Restore processes from the last save
    Resurrect,
    /// Ping the daemon
    Ping,
    /// Stop the daemon and all managed processes
    Kill,
    /// Live monitoring dashboard
    Monit,
    /// Generate a systemd unit so pmr resurrects on boot
    Startup {
        /// Only print the unit file, don't install
        #[arg(long)]
        print_only: bool,
    },
    /// Remove the systemd unit
    Unstartup,
    /// Write a sample ecosystem.yaml
    Init,
    /// Run the daemon in the foreground (internal; spawned automatically)
    #[command(hide = true)]
    Daemon,
}
