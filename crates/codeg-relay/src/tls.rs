use crate::config::Serve;
use anyhow::{Context, Result};
use rustls::pki_types::CertificateDer;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{BufReader, Write},
    path::Path,
    sync::Arc,
};
use tokio_rustls::TlsAcceptor;

fn certificates(path: &Path) -> Result<Vec<CertificateDer<'static>>> {
    let certs = rustls_pemfile::certs(&mut BufReader::new(fs::File::open(path)?))
        .collect::<Result<Vec<_>, _>>()?;
    anyhow::ensure!(!certs.is_empty(), "certificate file is empty");
    Ok(certs)
}
pub fn fingerprint(path: &Path) -> Result<String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(certificates(path)?[0].as_ref())
    ))
}

pub fn configure(options: &Serve) -> Result<(Option<TlsAcceptor>, String)> {
    if options.insecure_plaintext {
        anyhow::ensure!(
            options.listen.ip().is_loopback(),
            "plaintext requires a loopback listen address"
        );
        return Ok((None, "plaintext".into()));
    }
    let (cert_path, key_path) = if options.self_signed {
        anyhow::ensure!(
            !options.public_host.is_empty() && options.public_host.iter().all(|h| !h.is_empty()),
            "--public-host is required"
        );
        let dir = options.data_dir.join("tls");
        fs::create_dir_all(&options.data_dir)?;
        if let Ok(metadata) = fs::symlink_metadata(&dir) {
            anyhow::ensure!(
                metadata.file_type().is_dir(),
                "TLS directory must not be a symlink"
            );
        }
        let cert = dir.join("cert.pem");
        let key = dir.join("key.pem");
        if !cert.exists() && !key.exists() {
            let generated = rcgen::generate_simple_self_signed(options.public_host.clone())?;
            // Publish the entire identity with one directory rename. Interrupted
            // generation leaves only an unused temporary directory, never half a pair.
            let staging = tempfile::Builder::new()
                .prefix(".tls-")
                .tempdir_in(&options.data_dir)?;
            private_write(
                &staging.path().join("key.pem"),
                generated.key_pair.serialize_pem().as_bytes(),
            )?;
            private_write(
                &staging.path().join("cert.pem"),
                generated.cert.pem().as_bytes(),
            )?;
            fs::File::open(staging.path())?.sync_all()?;
            fs::rename(staging.path(), &dir)?;
            fs::File::open(&options.data_dir)?.sync_all()?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        }
        anyhow::ensure!(
            cert.exists() && key.exists(),
            "incomplete persisted TLS pair; restore both files before restarting"
        );
        for path in [&cert, &key] {
            anyhow::ensure!(
                fs::symlink_metadata(path)?.file_type().is_file(),
                "TLS files must be regular files"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
            }
        }
        (cert, key)
    } else {
        (
            options
                .tls_cert
                .clone()
                .context("provide --tls-cert/--tls-key or --self-signed --public-host")?,
            options.tls_key.clone().context("--tls-key is required")?,
        )
    };
    let certs = certificates(&cert_path)?;
    let fp = format!("{:x}", Sha256::digest(certs[0].as_ref()));
    let key = rustls_pemfile::private_key(&mut BufReader::new(fs::File::open(key_path)?))?
        .context("private key file is empty")?;
    let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_no_client_auth()
    .with_single_cert(certs, key)?;
    config.alpn_protocols = vec![crate::classifier::AGENT_ALPN.to_vec()];
    Ok((Some(TlsAcceptor::from(Arc::new(config))), fp))
}

fn private_write(path: &Path, content: &[u8]) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(content)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    fn options(dir: &Path) -> Serve {
        let crate::config::Command::Serve(options) = crate::config::Cli::parse_from([
            "relay",
            "serve",
            "--self-signed",
            "--public-host",
            "localhost",
            "--data-dir",
            dir.to_str().unwrap(),
        ])
        .command
        else {
            unreachable!()
        };
        options
    }
    #[test]
    fn tls_directory_symlink_does_not_write_outside_data_dir() {
        let dir = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(other.path(), dir.path().join("tls")).unwrap();
        assert!(configure(&options(dir.path())).is_err());
        assert_eq!(fs::read_dir(other.path()).unwrap().count(), 0);
    }
    #[test]
    fn partial_existing_identity_is_never_silently_rotated() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("tls")).unwrap();
        fs::write(dir.path().join("tls/key.pem"), "existing identity").unwrap();
        assert!(configure(&options(dir.path())).is_err());
        assert_eq!(
            fs::read_to_string(dir.path().join("tls/key.pem")).unwrap(),
            "existing identity"
        );
        assert!(!dir.path().join("tls/cert.pem").exists());
    }
}
