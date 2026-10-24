use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

pub async fn copy<A, B>(client: &mut A, home: &mut B, prefix: &[u8]) -> std::io::Result<(u64, u64)>
where
    A: AsyncRead + AsyncWrite + Unpin,
    B: AsyncRead + AsyncWrite + Unpin,
{
    home.write_all(prefix).await?;
    tokio::io::copy_bidirectional(client, home).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io,
        pin::Pin,
        sync::{Arc, Mutex},
        task::{Context, Poll},
    };
    use tokio::io::{AsyncReadExt, ReadBuf};

    // Test-only tap at the exact splice IO boundary. Capture both directions,
    // including the buffered ClientHello, never the TLS endpoint plaintext.
    struct Tap<S> {
        stream: S,
        read: Arc<Mutex<Vec<u8>>>,
        written: Arc<Mutex<Vec<u8>>>,
    }
    impl<S: AsyncRead + Unpin> AsyncRead for Tap<S> {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            let start = buf.filled().len();
            let result = Pin::new(&mut self.stream).poll_read(cx, buf);
            self.read
                .lock()
                .unwrap()
                .extend_from_slice(&buf.filled()[start..]);
            result
        }
    }
    impl<S: AsyncWrite + Unpin> AsyncWrite for Tap<S> {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<io::Result<usize>> {
            let result = Pin::new(&mut self.stream).poll_write(cx, bytes);
            if let Poll::Ready(Ok(n)) = result {
                self.written.lock().unwrap().extend_from_slice(&bytes[..n]);
            }
            result
        }
        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.stream).poll_flush(cx)
        }
        fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.stream).poll_shutdown(cx)
        }
    }

    #[tokio::test]
    async fn five_mib_tls_echo_is_exact_and_splice_cannot_see_secret_in_either_direction() {
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let server = rustls::ServerConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.cert.der().clone()],
                rustls::pki_types::PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der()).into(),
            )
            .unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(cert.cert.der().clone()).unwrap();
        let client = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let (phone, relay_client) = tokio::io::duplex(8192);
        let (mut relay_home, home) = tokio::io::duplex(8192);
        let up = Arc::new(Mutex::new(Vec::new()));
        let down = Arc::new(Mutex::new(Vec::new()));
        let mut tap = Tap {
            stream: relay_client,
            read: up.clone(),
            written: down.clone(),
        };
        let relay = tokio::spawn(async move {
            let (kind, prefix) = crate::classifier::classify(&mut tap).await.unwrap();
            assert_eq!(kind, crate::classifier::Kind::Client);
            copy(&mut tap, &mut relay_home, &prefix).await.unwrap()
        });
        let marker = b"E2EE-secret-marker-must-never-reach-the-relay";
        let mut payload: Vec<u8> = (0..5 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
        payload[12345..12345 + marker.len()].copy_from_slice(marker);
        let expected = payload.clone();
        let home = tokio::spawn(async move {
            let mut tls = tokio_rustls::TlsAcceptor::from(Arc::new(server))
                .accept(home)
                .await
                .unwrap();
            let mut received = vec![0; expected.len()];
            tls.read_exact(&mut received).await.unwrap();
            assert_eq!(received, expected);
            tls.write_all(&received).await.unwrap();
            tls.shutdown().await.unwrap();
            tls.read_to_end(&mut Vec::new()).await.unwrap();
        });
        let mut phone = tokio_rustls::TlsConnector::from(Arc::new(client))
            .connect(
                rustls::pki_types::ServerName::try_from("localhost").unwrap(),
                phone,
            )
            .await
            .unwrap();
        phone.write_all(&payload).await.unwrap();
        phone.flush().await.unwrap();
        let mut received = vec![0; payload.len()];
        phone.read_exact(&mut received).await.unwrap();
        assert_eq!(received, payload);
        phone.shutdown().await.unwrap();
        phone.read_to_end(&mut Vec::new()).await.unwrap();
        drop(phone);
        home.await.unwrap();
        relay.await.unwrap();
        for bytes in [up, down] {
            let bytes = bytes.lock().unwrap();
            assert!(bytes.len() > payload.len());
            assert!(!bytes.windows(marker.len()).any(|w| w == marker));
        }
    }
}
