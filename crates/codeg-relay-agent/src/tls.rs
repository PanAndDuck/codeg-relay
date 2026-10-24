use rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, ServerName, UnixTime},
    ClientConfig, DigitallySignedStruct, SignatureScheme,
};
use sha2::{Digest, Sha256};
use std::{io, sync::Arc, time::Duration};
use subtle::ConstantTimeEq;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpStream,
};

pub trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}
pub type Socket = Box<dyn Io>;

pub fn fingerprint_der(der: &[u8]) -> String {
    format!("{:x}", Sha256::digest(der))
}

#[derive(Debug)]
struct PinnedLeaf {
    pin: String,
    algorithms: rustls::crypto::WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for PinnedLeaf {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if bool::from(
            fingerprint_der(end_entity.as_ref())
                .as_bytes()
                .ct_eq(self.pin.as_bytes()),
        ) {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(
                "certificate fingerprint mismatch".into(),
            ))
        }
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algorithms)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

pub fn client_config(pin: Option<&str>, alpn: &[&[u8]]) -> anyhow::Result<Arc<ClientConfig>> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()?;
    let mut config = if let Some(pin) = pin {
        let pin = crate::config::normalize_fingerprint(pin)?;
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(PinnedLeaf {
                pin,
                algorithms: provider.signature_verification_algorithms,
            }))
            .with_no_client_auth()
    } else {
        let roots = rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        builder.with_root_certificates(roots).with_no_client_auth()
    };
    config.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
    Ok(Arc::new(config))
}

pub fn configure_tcp(stream: &TcpStream) -> io::Result<()> {
    stream.set_nodelay(true)?;
    socket2::SockRef::from(stream).set_tcp_keepalive(
        &socket2::TcpKeepalive::new()
            .with_time(Duration::from_secs(60))
            .with_interval(Duration::from_secs(15)),
    )
}

pub async fn connect(url: &url::Url, pin: Option<&str>, alpn: &[&[u8]]) -> anyhow::Result<Socket> {
    use anyhow::Context;
    let host = url
        .host_str()
        .context("URL needs a host")?
        .trim_start_matches('[')
        .trim_end_matches(']');
    let port = url.port_or_known_default().context("URL needs a port")?;
    let tcp = TcpStream::connect((host, port))
        .await
        .map_err(|_| anyhow::anyhow!("TCP connection failed"))?;
    configure_tcp(&tcp)?;
    if matches!(url.scheme(), "https" | "wss") {
        let connector = tokio_rustls::TlsConnector::from(client_config(pin, alpn)?);
        let name = ServerName::try_from(host.to_owned())
            .map_err(|_| anyhow::anyhow!("invalid TLS server name"))?;
        let tls = connector
            .connect(name, tcp)
            .await
            .map_err(|_| anyhow::anyhow!("TLS handshake or certificate verification failed"))?;
        Ok(Box::new(tls))
    } else {
        Ok(Box::new(tcp))
    }
}
