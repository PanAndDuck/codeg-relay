use serde::{Deserialize, Serialize};
use std::{
    sync::Mutex,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Status {
    #[cfg(windows)]
    #[serde(default)]
    pub state_dir: Option<std::path::PathBuf>,
    #[cfg(windows)]
    #[serde(default)]
    pub log_dir: Option<std::path::PathBuf>,
    pub state: String,
    pub pid: u32,
    pub updated_at: u64,
    pub connected_since: Option<u64>,
    pub home_fingerprint: String,
    pub relay_fingerprint: String,
    pub active_streams: usize,
    pub total_streams: u64,
    pub last_rtt_ms: Option<u64>,
    pub last_error: Option<String>,
    pub next_retry_in_ms: Option<u64>,
    pub stalled_connections: u64,
}

#[derive(Default)]
pub struct StatusStore(Mutex<Status>);
impl StatusStore {
    pub fn snapshot(&self) -> Status {
        self.0.lock().unwrap().clone()
    }
    pub fn update(&self, f: impl FnOnce(&mut Status)) {
        let mut status = self.0.lock().unwrap();
        f(&mut status);
        status.updated_at = now();
        status.pid = std::process::id();
    }
    pub fn transition(&self, state: &str, error: Option<&str>, retry: Option<Duration>) {
        self.update(|s| {
            s.state = state.into();
            s.last_error = error.map(str::to_owned);
            s.next_retry_in_ms = retry.map(|v| v.as_millis() as u64);
            s.connected_since = (state == "connected").then(now);
            if state != "connected" {
                s.active_streams = 0;
                s.last_rtt_ms = None;
            }
        });
        tracing::info!(
            state,
            reason = error.unwrap_or("none"),
            "relay state changed"
        );
    }
}
