//! Shared foreground/background runtime. Both binaries cancel the same agent
//! tasks and persist the final status before exiting on a shutdown event.
use crate::{client::Agent, config::Config, logging::RotatingLog, secrets};
use std::{path::Path, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

pub fn setup_logging(log_dir: Option<&Path>) -> anyhow::Result<()> {
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
async fn shutdown_signal(
    #[cfg(windows)] session: &mut crate::windows_session::SessionEnd,
) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        if let Ok(mut term) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
            return Ok(());
        }
    }
    #[cfg(windows)]
    {
        use tokio::signal::windows;
        let mut interrupt = windows::ctrl_c()?;
        let mut interrupt_break = windows::ctrl_break()?;
        let mut close = windows::ctrl_close()?;
        let mut logoff = windows::ctrl_logoff()?;
        let mut shutdown = windows::ctrl_shutdown()?;
        tokio::select! {
            _ = interrupt.recv() => {},
            _ = interrupt_break.recv() => {},
            _ = close.recv() => {},
            _ = logoff.recv() => {},
            _ = shutdown.recv() => {},
            result = session.notified() => { result?; },
        }
    }
    #[cfg(not(windows))]
    let _ = tokio::signal::ctrl_c().await;
    Ok(())
}
pub async fn run(state_dir: &Path, log_dir: Option<&Path>) -> anyhow::Result<()> {
    #[cfg(windows)]
    let mut session = crate::windows_session::SessionEnd::new()?;
    let agent = Arc::new(Agent::new(Config::load(state_dir)?, state_dir)?);
    #[cfg(windows)]
    agent.status.update(|status| {
        status.state_dir = Some(state_dir.to_owned());
        status.log_dir = log_dir.map(Path::to_owned);
    });
    #[cfg(not(windows))]
    let _ = log_dir;
    let stop = CancellationToken::new();
    let _stop_on_drop = stop.clone().drop_guard();
    let mut task = tokio::spawn(agent.clone().run(stop.clone()));
    #[cfg(windows)]
    let signal = shutdown_signal(&mut session);
    #[cfg(not(windows))]
    let signal = shutdown_signal();
    tokio::pin!(signal);
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            result = &mut signal => { result?; stop.cancel(); task.await?; break; },
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
