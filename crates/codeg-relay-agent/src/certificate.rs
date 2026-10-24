//! Ported from src-tauri/src/relay/certificate.rs (E2EE-a). No Tauri/DB dependencies.
use chrono::Datelike;
use std::{
    fs,
    io::{BufReader, Write},
    path::Path,
    sync::{Arc, Mutex},
};
use tokio_rustls::{rustls, TlsAcceptor};

#[derive(Clone)]
pub struct HomeTls {
    pub acceptor: TlsAcceptor,
    pub fingerprint: String,
}

type CertResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;
static IDENTITY_LOCK: Mutex<()> = Mutex::new(());

pub fn load(data_dir: &Path, regenerate: bool) -> CertResult<HomeTls> {
    let _lock = IDENTITY_LOCK.lock().unwrap();
    crate::secrets::private_dir(data_dir)?;
    let lock = crate::secrets::open_private(&data_dir.join("identity.lock"), true)?;
    fs2::FileExt::lock_exclusive(&lock)?;
    let parent = data_dir.join("relay");
    private_dir(&parent)?;
    let dir = parent.join("home-tls");
    let previous = parent.join("home-tls.previous");
    // Recover the last complete identity if interrupted between the two renames.
    if !dir.exists() && previous.exists() {
        private_dir(&previous)?;
        fs::rename(&previous, &dir)?;
    }
    if dir.exists() {
        private_dir(&dir)?;
    }
    if regenerate || !dir.exists() {
        let staging = tempfile::Builder::new()
            .prefix(".home-tls-")
            .tempdir_in(&parent)?;
        let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)?;
        let mut params = rcgen::CertificateParams::new(vec!["codeg-home".into()])?;
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "codeg-home");
        let today = chrono::Utc::now().date_naive();
        let expires = today
            .checked_add_months(chrono::Months::new(120))
            .ok_or("Invalid certificate date")?;
        params.not_before =
            rcgen::date_time_ymd(today.year(), today.month() as u8, today.day() as u8);
        params.not_after =
            rcgen::date_time_ymd(expires.year(), expires.month() as u8, expires.day() as u8);
        let cert = params.self_signed(&key)?;
        private_write(
            &staging.path().join("key.pem"),
            key.serialize_pem().as_bytes(),
        )?;
        private_write(&staging.path().join("cert.pem"), cert.pem().as_bytes())?;
        let identity = read(staging.path())?;
        #[cfg(unix)]
        fs::File::open(staging.path())?.sync_all()?;
        if previous.exists() {
            private_dir(&previous)?;
            fs::remove_dir_all(&previous)?;
        }
        if dir.exists() {
            fs::rename(&dir, &previous)?;
        }
        if let Err(error) = fs::rename(staging.path(), &dir) {
            if previous.exists() {
                let _ = fs::rename(&previous, &dir);
            }
            return Err(error.into());
        }
        #[cfg(unix)]
        fs::File::open(&parent)?.sync_all()?;
        if previous.exists() {
            fs::remove_dir_all(&previous)?;
        }
        Ok(identity)
    } else {
        let identity = read(&dir)?;
        if previous.exists() {
            private_dir(&previous)?;
            fs::remove_dir_all(previous)?;
        }
        Ok(identity)
    }
}

fn private_dir(dir: &Path) -> CertResult<()> {
    match fs::symlink_metadata(dir) {
        Ok(meta) if !meta.is_dir() || crate::secrets::is_redirect(&meta) => {
            return Err("TLS directory must be a directory, not a symlink or reparse point".into())
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let builder = fs::DirBuilder::new();
            #[cfg(unix)]
            let mut builder = builder;
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(dir)?;
        }
        Err(error) => return Err(error.into()),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn private_write(path: &Path, bytes: &[u8]) -> CertResult<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn read(dir: &Path) -> CertResult<HomeTls> {
    for name in ["cert.pem", "key.pem"] {
        let path = dir.join(name);
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.is_file() || crate::secrets::is_redirect(&metadata) {
            return Err("TLS identity requires regular files".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        }
    }
    let certs = rustls_pemfile::certs(&mut BufReader::new(identity_file(&dir.join("cert.pem"))?))
        .collect::<Result<Vec<_>, _>>()?;
    let leaf = certs.first().ok_or("Empty home certificate")?;
    let fingerprint = crate::tls::fingerprint_der(leaf.as_ref());
    let key =
        rustls_pemfile::private_key(&mut BufReader::new(identity_file(&dir.join("key.pem"))?))?
            .ok_or("Empty home key")?;
    let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_no_client_auth()
    .with_single_cert(certs, key)?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(HomeTls {
        acceptor: TlsAcceptor::from(Arc::new(config)),
        fingerprint,
    })
}

fn identity_file(path: &Path) -> CertResult<fs::File> {
    #[cfg(windows)]
    {
        // Use the same no-reparse handle policy as other credential readers.
        crate::secrets::open_private(path, false).map_err(Into::into)
    }
    #[cfg(not(windows))]
    {
        Ok(fs::File::open(path)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn interrupted_identity_rename_recovers_last_complete_pair() {
        let dir = tempfile::tempdir().unwrap();
        let first = load(dir.path(), false).unwrap();
        fs::rename(
            dir.path().join("relay/home-tls"),
            dir.path().join("relay/home-tls.previous"),
        )
        .unwrap();
        assert_eq!(
            load(dir.path(), false).unwrap().fingerprint,
            first.fingerprint
        );
        assert!(!dir.path().join("relay/home-tls.previous").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for path in [
                dir.path().to_owned(),
                dir.path().join("relay"),
                dir.path().join("relay/home-tls"),
            ] {
                assert_eq!(
                    fs::metadata(path).unwrap().permissions().mode() & 0o777,
                    0o700
                );
            }
        }
    }

    #[test]
    fn generates_persists_reuses_and_regenerates_home_identity() {
        let dir = tempfile::tempdir().unwrap();
        let first = load(dir.path(), false).unwrap();
        assert_eq!(first.fingerprint.len(), 64);
        assert!(first
            .fingerprint
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        assert_eq!(
            load(dir.path(), false).unwrap().fingerprint,
            first.fingerprint
        );
        for name in ["cert.pem", "key.pem"] {
            let path = dir.path().join("relay/home-tls").join(name);
            assert!(path.is_file());
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }
        }
        let rotated = load(dir.path(), true).unwrap();
        assert_ne!(rotated.fingerprint, first.fingerprint);
        assert_eq!(
            load(dir.path(), false).unwrap().fingerprint,
            rotated.fingerprint
        );
    }

    #[test]
    fn incomplete_identity_is_not_silently_replaced() {
        let dir = tempfile::tempdir().unwrap();
        load(dir.path(), false).unwrap();
        std::fs::remove_file(dir.path().join("relay/home-tls/key.pem")).unwrap();
        assert!(load(dir.path(), false).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinks_outside_data_directory() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("relay")).unwrap();
        assert!(load(dir.path(), false).is_err());
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    }
}
