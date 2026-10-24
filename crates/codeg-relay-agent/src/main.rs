use anyhow::{ensure, Context};
use clap::{Args, Parser, Subcommand, ValueEnum};
use codeg_relay_agent::{
    certificate,
    config::{self, Config, TokenSource},
    pairing, runtime, secrets, service,
    status::{self, Status},
};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    version,
    about = "Standalone E2EE relay connector for the official codeg app"
)]
struct Cli {
    /// Private state directory (Windows default: %LOCALAPPDATA%\codeg-relay-agent).
    #[arg(long, global = true)]
    state_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Init(Box<Init>),
    Run {
        /// Write rotating file logs instead of stderr.
        #[arg(long)]
        log_dir: Option<PathBuf>,
    },
    Pair {
        #[arg(long)]
        name: Option<String>,
        /// Save a private QR PNG; recommended for Windows terminals.
        #[arg(long)]
        png: Option<PathBuf>,
        #[arg(long)]
        json_only: bool,
        #[arg(long)]
        json_file: Option<PathBuf>,
    },
    Status,
    /// Install a user service. Windows requires codeg-relay-agentw.exe beside this CLI.
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
    /// Read only this DB (Windows default: %APPDATA%\app.codeg\codeg.db).
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
    Windows,
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
            runtime::setup_logging(log_dir.as_deref())?;
            runtime::run(&state_dir, log_dir.as_deref()).await?;
        }
        Command::Pair {
            name,
            png,
            json_only,
            json_file,
        } => {
            let payload = pairing::create(&Config::load(&state_dir)?, &state_dir, name)?;
            let qr = if json_only || cfg!(windows) {
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
            #[cfg(windows)]
            if !json_only {
                // Never emit ANSI QR data to a legacy console that may not have
                // VT processing enabled. PNG works regardless of terminal mode.
                eprintln!("Windows terminal QR preview is disabled; use pair --png <private-path> and scan the PNG. Pairing files contain credentials.");
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
            #[cfg(windows)]
            {
                value.state_dir = Some(state_dir.clone());
                if value.log_dir.is_none() {
                    value.log_dir = Some(config::default_log_dir(&state_dir)?);
                }
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

fn manage_service(state_dir: &Path, args: ServiceArgs, install: bool) -> anyhow::Result<()> {
    ensure!(
        args.platform.is_none() || args.dry_run,
        "--platform is available only with --dry-run"
    );
    let platform = match args.platform {
        Some(ServicePlatform::Macos) => service::Platform::MacOs,
        Some(ServicePlatform::Linux) => service::Platform::Linux,
        Some(ServicePlatform::Windows) => service::Platform::Windows,
        None if cfg!(target_os = "macos") => service::Platform::MacOs,
        None if cfg!(target_os = "linux") => service::Platform::Linux,
        None if cfg!(windows) => service::Platform::Windows,
        _ => anyhow::bail!("service installation is supported on macOS, Linux and Windows"),
    };
    if install && !args.dry_run {
        Config::load(state_dir)?;
    }
    #[cfg(unix)]
    let uid = unsafe { libc::geteuid() }.to_string();
    #[cfg(not(unix))]
    let uid = "0".to_owned();
    let home = config::user_home().or_else(|_| {
        if matches!(platform, service::Platform::Windows) {
            Ok(state_dir.to_owned())
        } else {
            Err(anyhow::anyhow!("cannot determine service home directory"))
        }
    })?;
    let user = service::User {
        logs: match platform {
            service::Platform::MacOs => home.join("Library/Logs/codeg-relay-agent"),
            service::Platform::Linux => home.join(".local/state/codeg-relay-agent/logs"),
            service::Platform::Windows if cfg!(windows) => config::default_log_dir(state_dir)?,
            // A foreign-platform preview uses the analogous directory under HOME.
            service::Platform::Windows => home.join("AppData/Local/codeg-relay-agent/logs"),
        },
        home,
        id: if matches!(platform, service::Platform::Windows) {
            service::windows_user_id()?
        } else {
            uid
        },
    };
    let plan = service::plan(
        platform,
        &user,
        &std::env::current_exe()?,
        state_dir,
        install,
    )?;
    service::apply(&plan, platform, install, args.dry_run)
}
