use anyhow::{ensure, Context};
use clap::{Args, Parser, Subcommand, ValueEnum};
use codeg_relay_agent::{
    certificate,
    client::Agent,
    config::{self, Config, TokenSource},
    logging::RotatingLog,
    pairing, secrets, service,
    status::{self, Status},
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[derive(Parser)]
#[command(
    version,
    about = "Standalone E2EE relay connector for the official codeg app"
)]
struct Cli {
    #[arg(long, global = true)]
    state_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Init(Box<Init>),
    Run {
        #[arg(long)]
        log_dir: Option<PathBuf>,
    },
    Pair {
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        png: Option<PathBuf>,
        #[arg(long)]
        json_only: bool,
        #[arg(long)]
        json_file: Option<PathBuf>,
    },
    Status,
    InstallService(ServiceArgs),
    UninstallService(ServiceArgs),
    RotateCert,
}
#[derive(Args)]
struct Init {
    #[arg(long)]
    relay_url: String,
    #[arg(long)]
    agent_key_file: PathBuf,
    #[arg(long)]
    relay_fingerprint: String,
    #[arg(long)]
    public_url: Option<String>,
    #[arg(long, default_value = "http://127.0.0.1:3080")]
    upstream: String,
    #[arg(long)]
    upstream_fingerprint: Option<String>,
    #[arg(long, conflicts_with = "db_path")]
    token_file: Option<PathBuf>,
    #[arg(long)]
    db_path: Option<PathBuf>,
    #[arg(long, default_value_t = 8*1024*1024)]
    buffer_bytes: usize,
    #[arg(long, default_value_t = 30)]
    stall_timeout_secs: u64,
    #[arg(long, default_value_t = 60)]
    token_refresh_secs: u64,
    #[arg(long, default_value_t = 10)]
    auth_failures: u32,
    #[arg(long, default_value_t = 600)]
    auth_window_secs: u64,
    #[arg(long, default_value_t = 900)]
    auth_block_secs: u64,
}
#[derive(Args)]
struct ServiceArgs {
    #[arg(long)]
    dry_run: bool,
    /// Preview the other platform's unit (only with --dry-run).
    #[arg(long, value_enum)]
    platform: Option<ServicePlatform>,
}
#[derive(Clone, Copy, ValueEnum)]
enum ServicePlatform {
    Macos,
    Linux,
}

#[tokio::main(worker_threads = 2)]
async fn main() -> std::process::ExitCode {
    if let Err(error) = execute(Cli::parse()).await {
        // All boundary errors are sanitized; never format requests or secrets.
        tracing::error!(error = %error, "agent command failed");
        eprintln!("codeg-relay-agent: {error}");
        std::process::ExitCode::FAILURE
    } else {
        std::process::ExitCode::SUCCESS
    }
}
async fn execute(cli: Cli) -> anyhow::Result<()> {
    let state_dir = match cli.state_dir {
        Some(path) => config::absolute_path(&path)?,
        None => config::default_state_dir()?,
    };
    match cli.command {
        Command::Init(args) => {
            ensure!(
                !state_dir.join("config.json").exists(),
                "already initialized; edit the private config.json instead"
            );
            let mut config = Config {
                relay_url: args.relay_url,
                relay_fingerprint: args.relay_fingerprint,
                agent_key_file: config::absolute_path(&args.agent_key_file)?,
                public_url: args.public_url,
                upstream_url: args.upstream,
                upstream_fingerprint: args.upstream_fingerprint,
                buffer_bytes: args.buffer_bytes,
                stall_timeout_secs: args.stall_timeout_secs,
                token_refresh_secs: args.token_refresh_secs,
                auth_failures: args.auth_failures,
                auth_window_secs: args.auth_window_secs,
                auth_block_secs: args.auth_block_secs,
                ..Default::default()
            };
            if let Some(path) = args.token_file {
                config.token_source = TokenSource::File(config::absolute_path(&path)?);
            }
            if let Some(path) = args.db_path {
                config.token_source = TokenSource::Database(config::absolute_path(&path)?);
            }
            config.validate()?;
            let key = secrets::read_private(&config.agent_key_file)?;
            let key = key.trim_end_matches(['\r', '\n']);
            ensure!(
                key.len() >= 16 && key.len() <= 4096 && !key.chars().any(char::is_whitespace),
                "agent key must contain 16..4096 non-whitespace bytes"
            );
            let identity = certificate::load(&state_dir, false)
                .map_err(|_| anyhow::anyhow!("cannot initialize home TLS identity"))?;
            config.save(&state_dir)?;
            println!(
                "Initialized {}\nHome fingerprint: {}",
                state_dir.display(),
                identity.fingerprint
            );
            println!("Run `codeg-relay-agent run` with the same --state-dir, then pair the phone.");
        }
        Command::Run { log_dir } => {
            setup_logging(log_dir.as_deref())?;
            run(&state_dir).await?;
        }
        Command::Pair {
            name,
            png,
            json_only,
            json_file,
        } => {
            let payload = pairing::create(&Config::load(&state_dir)?, &state_dir, name)?;
            let qr = if json_only {
                None
            } else {
                Some(pairing::terminal_qr(&payload)?)
            };
            if let Some(path) = png {
                pairing::write_png(&payload, &config::absolute_path(&path)?)?;
            }
            let json = serde_json::to_string(&payload)?;
            if let Some(path) = json_file {
                secrets::write_private(&config::absolute_path(&path)?, json.as_bytes(), false)?;
                println!("Pairing JSON saved privately.");
            } else {
                println!("{json}");
            }
            if let Some(qr) = qr {
                eprintln!("{qr}");
            }
        }
        Command::Status => {
            let path = state_dir.join("status.json");
            let mut value: Status = if path.exists() {
                serde_json::from_str(&secrets::read_private(&path)?)
                    .context("invalid saved status")?
            } else {
                Status::default()
            };
            if value.updated_at == 0 || status::now().saturating_sub(value.updated_at) > 5 {
                value.state = "stopped_or_stale".into();
            }
            println!("{}", serde_json::to_string_pretty(&value)?);
        }
        Command::RotateCert => {
            Config::load(&state_dir)?;
            let identity = certificate::load(&state_dir, true)
                .map_err(|_| anyhow::anyhow!("cannot rotate home TLS identity"))?;
            println!("Home fingerprint: {}\nRe-pair every phone. A running agent reloads within two seconds; existing connections finish normally.", identity.fingerprint);
        }
        Command::InstallService(args) => manage_service(&state_dir, args, true)?,
        Command::UninstallService(args) => manage_service(&state_dir, args, false)?,
    }
    Ok(())
}

fn setup_logging(log_dir: Option<&Path>) -> anyhow::Result<()> {
    use tracing_subscriber::{fmt::writer::BoxMakeWriter, prelude::*};
    let writer = match log_dir {
        Some(dir) => BoxMakeWriter::new(RotatingLog::open(dir, 5 * 1024 * 1024, 3)?),
        None => BoxMakeWriter::new(std::io::stderr),
    };
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_target(false)
                .with_writer(writer)
                .with_filter(tracing_subscriber::filter::filter_fn(|metadata| {
                    metadata.target().starts_with("codeg_relay_agent")
                        && *metadata.level() <= tracing::Level::INFO
                })),
        )
        .try_init()
        .map_err(|_| anyhow::anyhow!("cannot initialize logging"))?;
    Ok(())
}
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        if let Ok(mut term) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}
async fn run(state_dir: &Path) -> anyhow::Result<()> {
    let agent = Arc::new(Agent::new(Config::load(state_dir)?, state_dir)?);
    let stop = CancellationToken::new();
    let _stop_on_drop = stop.clone().drop_guard();
    let mut task = tokio::spawn(agent.clone().run(stop.clone()));
    let signal = shutdown_signal();
    tokio::pin!(signal);
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = &mut signal => { stop.cancel(); task.await?; break; },
            result = &mut task => { result?; break; },
            _ = tick.tick() => {
                agent.status.update(|_| {});
                if secrets::write_private(&state_dir.join("status.json"), &serde_json::to_vec(&agent.status.snapshot())?, true).is_err() { tracing::warn!("cannot persist agent status"); }
            }
        }
    }
    secrets::write_private(
        &state_dir.join("status.json"),
        &serde_json::to_vec(&agent.status.snapshot())?,
        true,
    )?;
    Ok(())
}
fn manage_service(state_dir: &Path, args: ServiceArgs, install: bool) -> anyhow::Result<()> {
    ensure!(
        args.platform.is_none() || args.dry_run,
        "--platform is available only with --dry-run"
    );
    let platform = match args.platform {
        Some(ServicePlatform::Macos) => service::Platform::MacOs,
        Some(ServicePlatform::Linux) => service::Platform::Linux,
        None if cfg!(target_os = "macos") => service::Platform::MacOs,
        None if cfg!(target_os = "linux") => service::Platform::Linux,
        _ => anyhow::bail!("service installation is supported on macOS and Linux"),
    };
    if install && !args.dry_run {
        Config::load(state_dir)?;
    }
    #[cfg(unix)]
    let uid = unsafe { libc::geteuid() };
    #[cfg(not(unix))]
    let uid = 0;
    let plan = service::plan(
        platform,
        &config::user_home()?,
        &std::env::current_exe()?,
        state_dir,
        uid,
        install,
    )?;
    service::apply(&plan, platform, install, args.dry_run)
}
