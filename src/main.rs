#[cfg(not(target_os = "linux"))]
compile_error!("den supports Linux only");

mod battery;
mod daemon;
mod eta;
mod model;
mod monitor;
mod power;
mod predict;
mod storage;
mod tui;
mod usage;

use std::{env, path::PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};

use crate::storage::Store;

const SYSTEM_DATABASE: &str = "/var/lib/den/history.sqlite3";

#[derive(Parser)]
#[command(
    version,
    about,
    after_help = "Without a subcommand, den opens the live monitor. A recording daemon is optional."
)]
struct Cli {
    /// History database (the viewer prefers the system database when present)
    #[arg(long, global = true, value_name = "PATH")]
    database: Option<PathBuf>,
    /// Linux power_supply directory for live monitoring and recording
    #[arg(
        long,
        global = true,
        default_value = "/sys/class/power_supply",
        value_name = "PATH"
    )]
    sysfs: PathBuf,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Record Linux battery measurements without opening the viewer
    Daemon(DaemonArgs),
    /// Open the live terminal monitor with optional read-only history
    Tui(TuiArgs),
}

#[derive(Args, Default)]
struct TuiArgs {
    /// View recorded history without reading live battery sensors
    #[arg(long)]
    history_only: bool,
}

#[derive(Args)]
pub struct DaemonArgs {
    /// Measurement interval in seconds
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..=86400))]
    interval: u64,
    /// Number of days of measured history to retain
    #[arg(long, default_value_t = 90, value_parser = clap::value_parser!(u32).range(1..=36500))]
    retention_days: u32,
    /// Record one snapshot and exit
    #[arg(long)]
    once: bool,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("den: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let is_daemon = matches!(cli.command, Some(Command::Daemon(_)));
    let path = database_path(cli.database, is_daemon)?;
    match cli.command {
        Some(Command::Daemon(args)) => daemon::run(&args, &cli.sysfs, Store::writer(&path)?),
        Some(Command::Tui(args)) => tui::run(&path, &cli.sysfs, args.history_only),
        None => tui::run(&path, &cli.sysfs, false),
    }
}

fn database_path(explicit: Option<PathBuf>, is_daemon: bool) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path);
    }
    let system = PathBuf::from(SYSTEM_DATABASE);
    if !is_daemon && !matches!(system.try_exists(), Ok(false)) {
        // History permission errors must not prevent unprivileged live sensor
        // monitoring. The UI reports the history error separately.
        return Ok(system);
    }
    let state = if let Some(path) = env::var_os("XDG_STATE_HOME") {
        let path = PathBuf::from(path);
        if path.is_absolute() { Some(path) } else { None }
    } else {
        None
    };
    let state = match state {
        Some(path) => path,
        None => {
            let home = env::var_os("HOME")
                .map(PathBuf::from)
                .context("Set --database, XDG_STATE_HOME or HOME to choose a history location")?;
            if !home.is_absolute() {
                bail!("HOME must be absolute; alternatively, set --database");
            }
            home.join(".local/state")
        }
    };
    Ok(state.join("den/history.sqlite3"))
}
