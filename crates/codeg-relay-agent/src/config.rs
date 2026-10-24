use crate::secrets;
use anyhow::{ensure, Context};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use url::Url;

pub const AGENT_PATH: &str = "/__codeg_relay/v1/agent";

#[derive(Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "path",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum TokenSource {
    File(PathBuf),
    Database(PathBuf),
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub relay_url: String,
    pub relay_fingerprint: String,
    pub agent_key_file: PathBuf,
    pub public_url: Option<String>,
    pub upstream_url: String,
    pub upstream_fingerprint: Option<String>,
    pub token_source: TokenSource,
    pub buffer_bytes: usize,
    pub stall_timeout_secs: u64,
    pub token_refresh_secs: u64,
    pub auth_failures: u32,
    pub auth_window_secs: u64,
    pub auth_block_secs: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            relay_url: String::new(),
            relay_fingerprint: String::new(),
            agent_key_file: PathBuf::new(),
            public_url: None,
            upstream_url: "http://127.0.0.1:3080".into(),
            upstream_fingerprint: None,
            // Explicit token/db paths must work even if the default environment
            // is unavailable. validate() resolves this sentinel fallibly.
            token_source: TokenSource::Database(default_database().unwrap_or_default()),
            buffer_bytes: 8 * 1024 * 1024,
            stall_timeout_secs: 30,
            token_refresh_secs: 60,
            auth_failures: 10,
            auth_window_secs: 600,
            auth_block_secs: 900,
        }
    }
}

impl Config {
    pub fn validate(&mut self) -> anyhow::Result<()> {
        if let TokenSource::Database(path) = &mut self.token_source {
            if path.as_os_str().is_empty() {
                *path = default_database()?;
            }
        }
        self.relay_url = relay_url(&self.relay_url)?;
        self.relay_fingerprint = normalize_fingerprint(&self.relay_fingerprint)?;
        if let Some(value) = &mut self.public_url {
            *value = relay_url(value)?;
        }
        let upstream = origin(&self.upstream_url, false)?;
        ensure!(
            upstream.path() == "/",
            "upstream URL must be an origin without a path prefix"
        );
        self.upstream_url = upstream.as_str().trim_end_matches('/').into();
        if let Some(pin) = &mut self.upstream_fingerprint {
            ensure!(upstream.scheme() == "https", "upstream pin requires HTTPS");
            *pin = normalize_fingerprint(pin)?;
        }
        ensure!(
            self.agent_key_file.is_absolute(),
            "agent key file must use an absolute path"
        );
        let path = match &self.token_source {
            TokenSource::File(p) | TokenSource::Database(p) => p,
        };
        ensure!(path.is_absolute(), "token source must use an absolute path");
        ensure!(
            (1024..=128 * 1024 * 1024).contains(&self.buffer_bytes),
            "buffer_bytes must be 1024..134217728"
        );
        ensure!(
            (1..=3600).contains(&self.stall_timeout_secs),
            "stall timeout must be 1..3600 seconds"
        );
        ensure!(
            (1..=60).contains(&self.token_refresh_secs),
            "token refresh must be 1..60 seconds"
        );
        ensure!(
            self.auth_failures > 0 && self.auth_failures <= 10000,
            "auth failures must be 1..10000"
        );
        ensure!(
            (1..=86400).contains(&self.auth_window_secs)
                && (1..=86400).contains(&self.auth_block_secs),
            "auth intervals must be 1..86400 seconds"
        );
        Ok(())
    }
    pub fn load(state_dir: &Path) -> anyhow::Result<Self> {
        let json = secrets::read_private(&state_dir.join("config.json"))?;
        let mut value: Self = serde_json::from_str(&json)
            .map_err(|_| anyhow::anyhow!("invalid agent configuration JSON"))?;
        value.validate()?;
        Ok(value)
    }
    pub fn save(&mut self, state_dir: &Path) -> anyhow::Result<()> {
        self.validate()?;
        secrets::private_dir(state_dir)?;
        secrets::write_private(
            &state_dir.join("config.json"),
            &serde_json::to_vec_pretty(self)?,
            false,
        )
    }
    pub fn agent_url(&self) -> anyhow::Result<Url> {
        let mut url = Url::parse(&self.relay_url)?;
        url.set_scheme("wss")
            .map_err(|_| anyhow::anyhow!("invalid relay URL"))?;
        url.set_path(AGENT_PATH);
        Ok(url)
    }
    pub fn effective_public_url(&self) -> &str {
        self.public_url.as_deref().unwrap_or(&self.relay_url)
    }
}

pub fn normalize_fingerprint(value: &str) -> anyhow::Result<String> {
    let value = value.trim().to_ascii_lowercase();
    let value = value
        .strip_prefix("sha256/")
        .or_else(|| value.strip_prefix("sha256:"))
        .unwrap_or(&value);
    let value: String = value
        .chars()
        .filter(|c| *c != ':' && !c.is_ascii_whitespace())
        .collect();
    ensure!(
        value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()),
        "fingerprint must be 64 hexadecimal characters"
    );
    Ok(value)
}
fn origin(value: &str, relay: bool) -> anyhow::Result<Url> {
    let value = value.trim();
    let value = if value.contains("://") {
        value.into()
    } else {
        format!("https://{value}")
    };
    let mut url = Url::parse(&value).map_err(|_| anyhow::anyhow!("invalid URL"))?;
    if relay && url.scheme() == "wss" {
        let _ = url.set_scheme("https");
    }
    ensure!(
        url.scheme() == "https" || (!relay && url.scheme() == "http"),
        "relay requires HTTPS; upstream requires HTTP or HTTPS"
    );
    ensure!(
        url.host().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && url.port() != Some(0),
        "URL must have a host and no credentials, query or fragment"
    );
    Ok(url)
}
pub fn relay_url(value: &str) -> anyhow::Result<String> {
    let mut url = origin(value, true)?;
    ensure!(
        url.path() == "/" || url.path() == AGENT_PATH,
        "relay URL must be an origin or agent endpoint"
    );
    url.set_path("");
    Ok(url.as_str().trim_end_matches('/').into())
}
pub fn absolute_path(path: &Path) -> anyhow::Result<PathBuf> {
    Ok(if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    })
}
pub fn user_home() -> anyhow::Result<PathBuf> {
    #[cfg(windows)]
    let variable = "USERPROFILE";
    #[cfg(not(windows))]
    let variable = "HOME";
    std::env::var_os(variable)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .with_context(|| format!("{variable} must name an absolute directory; supply --state-dir"))
}
pub fn default_state_dir() -> anyhow::Result<PathBuf> {
    #[cfg(windows)]
    {
        Ok(windows_app_data("LOCALAPPDATA", "--state-dir")?.join("codeg-relay-agent"))
    }
    #[cfg(target_os = "macos")]
    {
        Ok(user_home()?.join("Library/Application Support/codeg-relay-agent"))
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let home = user_home()?;
        Ok(std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| home.join(".local/state"))
            .join("codeg-relay-agent"))
    }
}
pub fn default_database() -> anyhow::Result<PathBuf> {
    #[cfg(windows)]
    {
        Ok(
            windows_app_data("APPDATA", "init --db-path or init --token-file")?
                .join("app.codeg")
                .join("codeg.db"),
        )
    }
    #[cfg(not(windows))]
    let home = user_home().unwrap_or_default();
    #[cfg(target_os = "macos")]
    {
        Ok(home.join("Library/Application Support/app.codeg/codeg.db"))
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        Ok(home.join(".local/share/app.codeg/codeg.db"))
    }
}

#[cfg(windows)]
fn windows_app_data(variable: &str, override_hint: &str) -> anyhow::Result<PathBuf> {
    app_data_path(std::env::var_os(variable), variable, override_hint)
}

#[cfg(any(windows, test))]
fn app_data_path(
    value: Option<std::ffi::OsString>,
    variable: &str,
    override_hint: &str,
) -> anyhow::Result<PathBuf> {
    value
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .with_context(|| {
            format!("{variable} must name an absolute directory; supply {override_hint}")
        })
}

/// Service log location. An explicit state directory also provides a fallback
/// when LOCALAPPDATA is unavailable (e.g. a deliberately minimal environment).
pub fn default_log_dir(state_dir: &Path) -> anyhow::Result<PathBuf> {
    #[cfg(windows)]
    {
        Ok(match std::env::var_os("LOCALAPPDATA") {
            Some(value) => app_data_path(Some(value), "LOCALAPPDATA", "run --log-dir")?
                .join("codeg-relay-agent")
                .join("logs"),
            None => state_dir.join("logs"),
        })
    }
    #[cfg(not(windows))]
    {
        let _ = state_dir;
        #[cfg(target_os = "macos")]
        return Ok(user_home()?.join("Library/Logs/codeg-relay-agent"));
        #[cfg(not(target_os = "macos"))]
        return Ok(user_home()?.join(".local/state/codeg-relay-agent/logs"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn app_data_requires_an_absolute_directory_and_names_the_override() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            app_data_path(
                Some(root.path().as_os_str().into()),
                "LOCALAPPDATA",
                "--state-dir"
            )
            .unwrap(),
            root.path()
        );
        for value in [None, Some("".into()), Some("relative".into())] {
            let error = app_data_path(value, "APPDATA", "init --db-path")
                .unwrap_err()
                .to_string();
            assert!(error.contains("APPDATA"));
            assert!(error.contains("init --db-path"));
        }
    }
    #[test]
    fn config_roundtrips_privately_and_rejects_unsafe_limits_or_unknown_fields() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("state");
        let mut config = Config {
            relay_url: "relay.example:18443".into(),
            relay_fingerprint: "ab".repeat(32),
            agent_key_file: root.path().join("agent.key"),
            token_source: TokenSource::File(root.path().join("web.token")),
            ..Default::default()
        };
        config.save(&dir).unwrap();
        let stored = Config::load(&dir).unwrap();
        assert_eq!(stored.relay_url, "https://relay.example:18443");
        assert_eq!(stored.buffer_bytes, 8 * 1024 * 1024);
        assert_eq!(stored.token_refresh_secs, 60);
        assert!(config.save(&dir).is_err());
        let mut invalid = stored.clone();
        invalid.buffer_bytes = 0;
        assert!(invalid.validate().is_err());
        let mut invalid = stored.clone();
        invalid.stall_timeout_secs = 0;
        assert!(invalid.validate().is_err());
        let mut invalid = stored.clone();
        invalid.auth_failures = 0;
        assert!(invalid.validate().is_err());
        let mut invalid = stored.clone();
        invalid.token_refresh_secs = 61;
        assert!(invalid.validate().is_err());
        let mut invalid = stored.clone();
        invalid.upstream_fingerprint = Some("ab".repeat(32));
        assert!(invalid.validate().is_err());
        let mut value = serde_json::to_value(stored).unwrap();
        value["unknown_secret"] = "do-not-display-this".into();
        secrets::write_private(
            &dir.join("config.json"),
            &serde_json::to_vec(&value).unwrap(),
            true,
        )
        .unwrap();
        let error = Config::load(&dir).err().unwrap();
        assert!(!error.to_string().contains("do-not-display-this"));
    }
    #[test]
    fn canonical_pins_and_secure_relay_origins() {
        assert_eq!(
            normalize_fingerprint(&format!("SHA256: {}", ["AB"; 32].join(":"))).unwrap(),
            "ab".repeat(32)
        );
        assert_eq!(
            relay_url("wss://relay.test:18443/__codeg_relay/v1/agent").unwrap(),
            "https://relay.test:18443"
        );
        assert_eq!(
            relay_url("relay.test:18443").unwrap(),
            "https://relay.test:18443"
        );
        for value in ["", "ab", &"gg".repeat(32)] {
            assert!(normalize_fingerprint(value).is_err());
        }
        for value in [
            "http://127.0.0.1",
            "https://a:b@relay.test",
            "https://relay.test?key=bad",
            "https://relay.test/#x",
            "https://relay.test/other",
            "https://relay.test:0",
        ] {
            assert!(relay_url(value).is_err());
        }
    }
}
