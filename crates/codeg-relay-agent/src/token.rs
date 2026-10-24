use crate::config::TokenSource;
use anyhow::{ensure, Context};
use std::{
    path::Path,
    sync::{Arc, RwLock},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

pub fn database_uri(path: &Path) -> anyhow::Result<String> {
    // Recognize Windows syntax independently of the host OS so URI handling is
    // tested by every CI platform. SQLite expects UTF-8 and /C:/ drive paths.
    // https://sqlite.org/uri.html
    if let Some(value) = path.to_str() {
        if value.starts_with(r"\\") || value.as_bytes().get(1) == Some(&b':') {
            ensure!(!value.contains('\0'), "database path must not contain NUL");
            let verbatim = value.starts_with(r"\\?\");
            let normalized = value.replace('\\', "/");
            let normalized = normalized.strip_prefix("//?/").unwrap_or(&normalized);
            let normalized = if let Some(unc) = normalized.strip_prefix("UNC/") {
                format!("//{unc}")
            } else {
                normalized.to_owned()
            };
            let bytes = normalized.as_bytes();
            let drive = bytes.len() >= 3
                && bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && bytes[2] == b'/';
            let unc = normalized.strip_prefix("//").is_some_and(|p| {
                let mut parts = p.split('/');
                parts.next().is_some_and(|s| !s.is_empty() && s != ".")
                    && parts.next().is_some_and(|s| !s.is_empty())
            });
            ensure!(
                drive || unc,
                "database path must be an absolute Windows drive or UNC path"
            );
            // Preserve extended-length syntax, including trailing dots/spaces.
            // Bundled SQLite's win32-longpath VFS accepts /\\?\ after URI
            // decoding and removes only the leading URI slash.
            let bytes = if verbatim { value.as_bytes() } else { bytes };
            let mut uri = if drive || verbatim {
                "file:///"
            } else {
                "file://"
            }
            .to_owned();
            for &byte in bytes {
                if byte.is_ascii_alphanumeric() || b"/-._~:".contains(&byte) {
                    uri.push(byte as char);
                } else {
                    use std::fmt::Write;
                    write!(uri, "%{byte:02X}")?;
                }
            }
            uri.push_str("?mode=ro");
            if verbatim {
                uri.push_str("&vfs=win32-longpath");
            }
            return Ok(uri);
        }
    }
    let mut uri = url::Url::from_file_path(path)
        .map_err(|_| anyhow::anyhow!("database path must be absolute"))?;
    uri.set_query(Some("mode=ro"));
    Ok(uri.into())
}

fn open_database(path: &Path) -> anyhow::Result<rusqlite::Connection> {
    let db = rusqlite::Connection::open_with_flags(
        database_uri(path)?,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
            | rusqlite::OpenFlags::SQLITE_OPEN_URI
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| anyhow::anyhow!("cannot open token database read-only"))?;
    db.busy_timeout(Duration::from_secs(2))?;
    Ok(db)
}

pub fn read_source(source: &TokenSource) -> anyhow::Result<String> {
    let value = match source {
        TokenSource::File(path) => crate::secrets::read_private(path)?
            .trim_end_matches(['\r', '\n'])
            .to_owned(),
        TokenSource::Database(path) => {
            let db = open_database(path)?;
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
    #[test]
    fn extended_windows_database_uri_preserves_verbatim_path_semantics() {
        assert_eq!(
            database_uri(Path::new(r"\\?\C:\directory.\codeg.db")).unwrap(),
            "file:///%5C%5C%3F%5CC:%5Cdirectory.%5Ccodeg.db?mode=ro&vfs=win32-longpath"
        );
    }

    #[test]
    fn windows_database_uris_preserve_drive_unicode_spaces_and_reserved_bytes() {
        for (path, expected) in [
            (r"C:\Users\Example\AppData\Roaming\app.codeg\codeg.db", "file:///C:/Users/Example/AppData/Roaming/app.codeg/codeg.db?mode=ro"),
            (r"D:\Users\测试 用户\db #100%.sqlite", "file:///D:/Users/%E6%B5%8B%E8%AF%95%20%E7%94%A8%E6%88%B7/db%20%23100%25.sqlite?mode=ro"),
            (r"\\?\C:\Users\Example\codeg.db", "file:///%5C%5C%3F%5CC:%5CUsers%5CExample%5Ccodeg.db?mode=ro&vfs=win32-longpath"),
            ("C:/Users/Example/codeg.db", "file:///C:/Users/Example/codeg.db?mode=ro"),
        ] {
            assert_eq!(database_uri(Path::new(path)).unwrap(), expected);
        }
    }

    #[test]
    fn windows_database_uris_reject_relative_drives_and_keep_unc_authority_empty() {
        for value in [
            r"C:relative.db",
            r"\\.\pipe\db",
            r"\\?\relative.db",
            r"\\server",
            "C:\\db\0ignored",
        ] {
            assert!(database_uri(Path::new(value)).is_err(), "{value}");
        }
        assert_eq!(
            database_uri(Path::new(r"\\server\share\测试.db")).unwrap(),
            "file:////server/share/%E6%B5%8B%E8%AF%95.db?mode=ro"
        );
        assert_eq!(database_uri(Path::new(r"\\?\UNC\server\share\测试.db")).unwrap(), "file:///%5C%5C%3F%5CUNC%5Cserver%5Cshare%5C%E6%B5%8B%E8%AF%95.db?mode=ro&vfs=win32-longpath");
    }

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

    #[cfg(windows)]
    #[test]
    fn extended_windows_database_paths_longer_than_max_path_are_read_only() {
        let root = tempfile::tempdir().unwrap();
        let mut directory = std::fs::canonicalize(root.path()).unwrap();
        for _ in 0..6 {
            directory.push("long directory 测试 012345678901234567890123456789");
        }
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("codeg.db");
        assert!(path.to_str().unwrap().len() > 260);
        let db = rusqlite::Connection::open_with_flags_and_vfs(
            &path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_CREATE,
            "win32-longpath",
        )
        .unwrap();
        db.execute_batch("CREATE TABLE app_metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL); INSERT INTO app_metadata VALUES ('web_service_token','test-only-long-path');").unwrap();
        assert_eq!(
            read_source(&TokenSource::Database(path.clone())).unwrap(),
            "test-only-long-path"
        );
        assert!(open_database(&path)
            .unwrap()
            .is_readonly(rusqlite::DatabaseName::Main)
            .unwrap());
    }

    #[test]
    fn sqlite_read_only_sees_rotation_without_creating_a_missing_database() {
        let dir = tempfile::tempdir().unwrap();
        // '?' is a legal Unix filename but not a legal Windows filename.
        let path = dir.path().join("db # 测试 %.sqlite");
        let source = TokenSource::Database(path.clone());
        assert!(read_source(&source).is_err());
        assert!(!path.exists());
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE app_metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL); INSERT INTO app_metadata VALUES ('web_service_token','example-one');").unwrap();
        let original = read_source(&source).unwrap();
        let reader = open_database(&path).unwrap();
        assert!(reader.is_readonly(rusqlite::DatabaseName::Main).unwrap());
        assert!(reader
            .execute("UPDATE app_metadata SET value='must-not-write'", [])
            .is_err());
        // canonicalize() returns the extended-length \\?\ form on Windows.
        let canonical_source = TokenSource::Database(std::fs::canonicalize(&path).unwrap());
        assert_eq!(read_source(&canonical_source).unwrap(), original);
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
