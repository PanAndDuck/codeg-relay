//! Agent lifecycle ported from src-tauri/src/relay/client.rs.
use crate::{
    certificate::{self, HomeTls},
    config::Config,
    proxy::Gateway,
    secrets,
    serve::{self, StreamLimits},
    status::StatusStore,
    token::TokenStore,
    transport::{self, Observations},
};
use codeg_tunnel::mux::{self, MuxConfig, MuxRole};
use rand::Rng;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};
use tokio::{task::JoinSet, time::Instant};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
pub struct Backoff {
    failures: usize,
}
// mux::start owns a separate task. Dropping its JoinHandle does not stop it,
// so cancellation of this future must explicitly request mux shutdown too.
struct SessionCloser(mux::MuxHandle);
impl Drop for SessionCloser {
    fn drop(&mut self) {
        self.0.close();
    }
}
impl Backoff {
    pub fn next_delay(&mut self, jitter: f64) -> Duration {
        let seconds = [1, 2, 4, 8, 16, 30][self.failures.min(5)];
        self.failures = self.failures.saturating_add(1);
        Duration::from_secs_f64((seconds as f64 * (1.0 + jitter.clamp(-0.2, 0.2))).min(30.0))
    }
    pub fn connected_for(&mut self, duration: Duration) {
        if duration >= Duration::from_secs(60) {
            self.failures = 0;
        }
    }
}

pub struct Agent {
    pub gateway: Arc<Gateway>,
    pub status: Arc<StatusStore>,
    home_tls: Arc<RwLock<HomeTls>>,
    state_dir: PathBuf,
    _run_lock: std::fs::File,
}
impl Agent {
    pub fn new(mut config: Config, state_dir: &Path) -> anyhow::Result<Self> {
        config.validate()?;
        secrets::private_dir(state_dir)?;
        let run_lock = secrets::open_private(&state_dir.join("run.lock"), true)?;
        fs2::FileExt::try_lock_exclusive(&run_lock)
            .map_err(|_| anyhow::anyhow!("agent already running in this state directory"))?;
        let tls = certificate::load(state_dir, false)
            .map_err(|_| anyhow::anyhow!("cannot load home TLS identity"))?;
        let tokens = Arc::new(TokenStore::load(config.token_source.clone())?);
        let status = Arc::new(StatusStore::default());
        status.update(|s| {
            s.home_fingerprint = tls.fingerprint.clone();
            s.relay_fingerprint = config.relay_fingerprint.clone();
        });
        Ok(Self {
            gateway: Arc::new(Gateway::new(Arc::new(config), tokens)),
            status,
            home_tls: Arc::new(RwLock::new(tls)),
            state_dir: state_dir.to_owned(),
            _run_lock: run_lock,
        })
    }
    pub async fn run(self: Arc<Self>, cancel: CancellationToken) {
        let local = cancel.child_token();
        let _cancel_on_drop = local.clone().drop_guard();
        let mut tasks = JoinSet::new();
        tasks.spawn(self.gateway.tokens.clone().watch(
            Duration::from_secs(self.gateway.config.token_refresh_secs),
            local.clone(),
        ));
        let agent = self.clone();
        let watch_cancel = local.clone();
        tasks.spawn(async move {
            agent.watch_identity(watch_cancel).await;
        });
        self.link(local.clone()).await;
        local.cancel();
        while tasks.join_next().await.is_some() {}
        self.status.transition("stopped", None, None);
    }
    async fn watch_identity(&self, cancel: CancellationToken) {
        let mut tick = tokio::time::interval(Duration::from_secs(2));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = tick.tick() => {
                    let dir = self.state_dir.clone();
                    let identity = tokio::task::spawn_blocking(move || certificate::load(&dir, false)).await;
                    if let Ok(Ok(tls)) = identity {
                        self.status.update(|s| s.home_fingerprint = tls.fingerprint.clone());
                        *self.home_tls.write().unwrap() = tls;
                    } else { tracing::warn!("cannot reload home identity; retaining current identity"); }
                }
            }
        }
    }
    async fn link(&self, cancel: CancellationToken) {
        let config = &self.gateway.config;
        let mut backoff = Backoff::default();
        let mut total: u64 = 0;
        loop {
            if cancel.is_cancelled() {
                return;
            }
            self.status.transition("connecting", None, None);
            let connection = tokio::select! { biased; _ = cancel.cancelled() => return, value = transport::connect(config) => value };
            let (error, auth_rejected) = match connection {
                Err(error) => (error.message, error.code == "auth_rejected"),
                Ok(socket) => {
                    self.status.transition("connected", None, None);
                    let started = Instant::now();
                    let observed = Arc::new(Mutex::new(Observations::default()));
                    let (handle, mut incoming, mut task) = mux::start(
                        transport::adapter(socket, observed.clone()),
                        MuxConfig {
                            role: MuxRole::Acceptor,
                            ..Default::default()
                        },
                    );
                    let _close_mux_on_drop = SessionCloser(handle.clone());
                    let session_cancel = cancel.child_token();
                    let _session_guard = session_cancel.clone().drop_guard();
                    let limits = Arc::new(StreamLimits::default());
                    let mut streams = JoinSet::new();
                    let mut tick = tokio::time::interval(Duration::from_secs(1));
                    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                    let mut joined = false;
                    loop {
                        tokio::select! {
                            biased;
                            _ = cancel.cancelled() => break,
                            _ = &mut task => { joined = true; break; },
                            _ = streams.join_next(), if !streams.is_empty() => {},
                            request = incoming.accept() => match request {
                                Some(request) => { streams.spawn(serve::serve(request, self.gateway.clone(), limits.clone(), self.home_tls.read().unwrap().clone(), session_cancel.child_token())); },
                                None => break,
                            },
                            _ = tick.tick() => {
                                let stats = handle.stats();
                                self.status.update(|s| { s.active_streams = stats.active_streams; s.total_streams = total.saturating_add(stats.total_streams); s.last_rtt_ms = observed.lock().unwrap().rtt_ms; s.stalled_connections = self.gateway.metrics.stalls.load(std::sync::atomic::Ordering::Relaxed); });
                            }
                        }
                    }
                    total = total.saturating_add(handle.stats().total_streams);
                    session_cancel.cancel();
                    handle.close();
                    if !joined {
                        let _ = task.await;
                    }
                    streams.shutdown().await;
                    if cancel.is_cancelled() {
                        return;
                    }
                    backoff.connected_for(started.elapsed());
                    let replaced = observed.lock().unwrap().replaced;
                    (
                        if replaced {
                            "relay connection replaced (4001)"
                        } else {
                            "relay connection closed"
                        },
                        false,
                    )
                }
            };
            let delay = if auth_rejected {
                Duration::from_secs(300)
            } else {
                backoff.next_delay(rand::thread_rng().gen_range(-0.2..=0.2))
            };
            self.status.transition(
                if auth_rejected { "error" } else { "backoff" },
                Some(error),
                Some(delay),
            );
            tokio::select! { _ = cancel.cancelled() => return, _ = tokio::time::sleep(delay) => {} }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exponential_backoff_has_hard_cap_and_only_stable_connections_reset() {
        let mut b = Backoff::default();
        for seconds in [1, 2, 4, 8, 16, 30, 30] {
            assert_eq!(b.next_delay(0.0), Duration::from_secs(seconds));
        }
        assert_eq!(b.next_delay(0.2), Duration::from_secs(30));
        b.connected_for(Duration::from_secs(59));
        assert_eq!(b.next_delay(-0.2), Duration::from_secs(24));
        b.connected_for(Duration::from_secs(60));
        assert_eq!(b.next_delay(0.2), Duration::from_millis(1200));
    }
}
