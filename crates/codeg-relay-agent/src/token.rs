use crate::config::TokenSource;
use anyhow::{ensure, Context};
use std::{
    sync::{Arc, RwLock},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

pub fn read_source(source: &TokenSource) -> anyhow::Result<String> {
    let value = match source {
        TokenSource::File(path) => crate::secrets::read_private(path)?
            .trim_end_matches(['\r', '\n'])
            .to_owned(),
        TokenSource::Database(path) => {
            let mut uri = url::Url::from_file_path(path)
                .map_err(|_| anyhow::anyhow!("database path must be absolute"))?;
            uri.set_query(Some("mode=ro"));
            let db = rusqlite::Connection::open_with_flags(
                uri.as_str(),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                    | rusqlite::OpenFlags::SQLITE_OPEN_URI
                    | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .map_err(|_| anyhow::anyhow!("cannot open token database read-only"))?;
            db.busy_timeout(Duration::from_secs(2))?;
            db.query_row(
                "SELECT value FROM app_metadata WHERE key = 'web_service_token'",
                [],
                |row| row.get::<_, String>(0),
            )
            .map_err(|_| {
                anyhow::anyhow!("web_service_token is unavailable in read-only database")
            })?
        }
    };
    ensure!(
        !value.is_empty() && value.len() <= 4096 && !value.contains(['\r', '\n']),
        "web token is empty, oversized or malformed"
    );
    Ok(value)
}

// No Debug: credential values never belong in diagnostics.
pub struct TokenStore {
    source: TokenSource,
    current: RwLock<String>,
}
impl TokenStore {
    pub fn load(source: TokenSource) -> anyhow::Result<Self> {
        let current = RwLock::new(read_source(&source)?);
        Ok(Self { source, current })
    }
    pub fn current(&self) -> String {
        self.current.read().unwrap().clone()
    }
    pub fn authorized(&self, headers: &http::HeaderMap) -> bool {
        crate::auth::authorized(headers, &self.current.read().unwrap())
    }
    pub async fn refresh(&self) -> anyhow::Result<()> {
        let source = self.source.clone();
        let result = tokio::task::spawn_blocking(move || read_source(&source))
            .await
            .context("token reader failed")?;
        let mut current = self.current.write().unwrap();
        match result {
            Ok(value) => {
                *current = value;
                Ok(())
            }
            Err(error) => {
                current.clear();
                Err(error)
            }
        }
    }
    pub async fn watch(self: Arc<Self>, interval: Duration, cancel: CancellationToken) {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        ticker.tick().await;
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = ticker.tick() => {
                    if self.refresh().await.is_err() {
                        tracing::warn!("token refresh failed; protected requests now fail closed");
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn refresh_failure_revokes_cached_token_and_later_success_recovers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        crate::secrets::write_private(&path, b"test-only-before", false).unwrap();
        let store = TokenStore::load(TokenSource::File(path.clone())).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert!(store.refresh().await.is_err());
        assert!(store.current().is_empty());
        crate::secrets::write_private(&path, b"test-only-after", false).unwrap();
        store.refresh().await.unwrap();
        assert!(!store.current().is_empty());
        let mut headers = http::HeaderMap::new();
        headers.insert("authorization", "Bearer test-only-before".parse().unwrap());
        assert!(!store.authorized(&headers));
        headers.insert("authorization", "Bearer test-only-after".parse().unwrap());
        assert!(store.authorized(&headers));
    }
    #[test]
    fn sqlite_read_only_sees_rotation_without_creating_a_missing_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db #? test.sqlite");
        let source = TokenSource::Database(path.clone());
        assert!(read_source(&source).is_err());
        assert!(!path.exists());
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE app_metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL); INSERT INTO app_metadata VALUES ('web_service_token','example-one');").unwrap();
        let original = read_source(&source).unwrap();
        db.execute(
            "UPDATE app_metadata SET value=?1 WHERE key='web_service_token'",
            ["example-two"],
        )
        .unwrap();
        assert!(original != read_source(&source).unwrap());
        db.execute("UPDATE app_metadata SET value=''", []).unwrap();
        assert!(read_source(&source).is_err());
        db.execute("DELETE FROM app_metadata", []).unwrap();
        assert!(read_source(&source).is_err());
    }
    #[test]
    fn token_file_rereads_rotation_and_refuses_unconfigured_values() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        crate::secrets::write_private(&path, b"example-one\n", false).unwrap();
        let source = TokenSource::File(path.clone());
        let original = read_source(&source).unwrap();
        assert!(!original.ends_with('\n'));
        crate::secrets::write_private(&path, b"example-two\r\n", true).unwrap();
        assert!(original != read_source(&source).unwrap());
        crate::secrets::write_private(&path, b"\n", true).unwrap();
        assert!(read_source(&source).is_err());
    }
}
