// Task Scheduler starts this GUI-subsystem binary in the user's session without
// allocating a console. Keep the normal CLI as a console app for init/pair/status.
#![cfg_attr(windows, windows_subsystem = "windows")]

use clap::Parser;
use codeg_relay_agent::{config, runtime};
use std::{path::PathBuf, process::ExitCode};

#[derive(Parser)]
#[command(
    version,
    about = "Background relay agent with rotating file logs (no Windows console)"
)]
struct Args {
    #[arg(long)]
    state_dir: Option<PathBuf>,
    #[arg(long)]
    log_dir: Option<PathBuf>,
}

#[tokio::main(worker_threads = 2)]
async fn main() -> ExitCode {
    if let Err(error) = execute(Args::parse()).await {
        tracing::error!(error = %error, "background agent failed");
        #[cfg(not(windows))]
        eprintln!("codeg-relay-agentw: {error}");
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

async fn execute(args: Args) -> anyhow::Result<()> {
    let state_dir = match args.state_dir {
        Some(path) => config::absolute_path(&path)?,
        None => config::default_state_dir()?,
    };
    let log_dir = match args.log_dir {
        Some(path) => config::absolute_path(&path)?,
        None => config::default_log_dir(&state_dir)?,
    };
    runtime::setup_logging(Some(&log_dir))?;
    runtime::run(&state_dir, Some(&log_dir)).await
}
