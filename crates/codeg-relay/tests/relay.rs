use codeg_tunnel::mux::{self, MuxConfig, MuxHandle, MuxMessage, MuxRole, TunnelStream};
use futures::{future, SinkExt, StreamExt};
use rustls::{pki_types::ServerName, ClientConfig, RootCertStore};
use std::{
    net::SocketAddr,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
    process::{Child, Command},
    sync::oneshot,
    task::JoinHandle,
    time::timeout,
};
use tokio_rustls::{client::TlsStream, TlsAcceptor, TlsConnector};
use tokio_tungstenite::{
    client_async,
    tungstenite::{client::IntoClientRequest, protocol::Role, Message},
    WebSocketStream,
};

const KEY: &str = "c6-test-only-agent-key-32-characters";
const AGENT: &str = "/__codeg_relay/v1/agent";
const HEALTH: &str = "/__codeg_relay/v1/health";

struct Relay {
    child: Child,
    dir: tempfile::TempDir,
    addr: SocketAddr,
    tls: TlsConnector,
    client_tls: TlsConnector,
    home_tls: TlsAcceptor,
    fingerprint: String,
    logs: Arc<Mutex<String>>,
    log_task: Option<JoinHandle<()>>,
}
impl Relay {
    async fn start(cap: usize) -> Self {
        Self::in_dir(tempfile::tempdir().unwrap(), cap).await
    }
    async fn in_dir(dir: tempfile::TempDir, cap: usize) -> Self {
        Self::with_options(dir, cap, 512, "info").await
    }
    async fn with_options(dir: tempfile::TempDir, cap: usize, total: usize, level: &str) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_codeg-relay"))
            .args([
                "serve",
                "--listen",
                "127.0.0.1:0",
                "--self-signed",
                "--public-host",
                "localhost,127.0.0.1",
                "--data-dir",
            ])
            .arg(dir.path())
            .args(["--max-conns-per-ip", &cap.to_string()])
            .args(["--max-conns", &total.to_string()])
            .env("CODEG_RELAY_AGENT_KEY", KEY)
            .env("CODEG_DATA_DIR", dir.path().join("unused-data"))
            .env("CODEG_HOME", dir.path().join("unused-home"))
            .env("RUST_LOG", level)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let line = timeout(Duration::from_secs(10), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .expect("startup banner");
        let field = |name: &str| {
            line.split_whitespace()
                .find_map(|s| s.strip_prefix(name))
                .unwrap()
                .to_owned()
        };
        let addr = field("listen=").parse().unwrap();
        let fingerprint = field("fingerprint=");
        let logs = Arc::new(Mutex::new(String::new()));
        let captured = logs.clone();
        let log_task = tokio::spawn(async move {
            while let Some(line) = lines.next_line().await.ok().flatten() {
                captured.lock().unwrap().push_str(&format!("{line}\n"));
            }
        });
        let pem = std::fs::read(dir.path().join("tls/cert.pem")).unwrap();
        let cert = rustls_pemfile::certs(&mut &pem[..])
            .next()
            .unwrap()
            .unwrap();
        let mut roots = RootCertStore::empty();
        roots.add(cert).unwrap();
        let mut config =
            ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_root_certificates(roots)
                .with_no_client_auth();
        config.alpn_protocols = vec![b"codeg-relay-agent/1".to_vec(), b"http/1.1".to_vec()];
        let home = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let mut roots = RootCertStore::empty();
        roots.add(home.cert.der().clone()).unwrap();
        let mut client =
            ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_root_certificates(roots)
                .with_no_client_auth();
        client.alpn_protocols = vec![b"http/1.1".to_vec()];
        let mut home_config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![home.cert.der().clone()],
            rustls::pki_types::PrivatePkcs8KeyDer::from(home.key_pair.serialize_der()).into(),
        )
        .unwrap();
        home_config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Self {
            child,
            dir,
            addr,
            tls: TlsConnector::from(Arc::new(config)),
            client_tls: TlsConnector::from(Arc::new(client)),
            home_tls: TlsAcceptor::from(Arc::new(home_config)),
            fingerprint,
            logs,
            log_task: Some(log_task),
        }
    }
    async fn agent_connect(&self) -> TlsStream<TcpStream> {
        let tls = self
            .tls
            .connect(
                ServerName::try_from("localhost").unwrap(),
                TcpStream::connect(self.addr).await.unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            tls.get_ref().1.alpn_protocol(),
            Some(&b"codeg-relay-agent/1"[..])
        );
        tls
    }
    async fn try_agent_connect(&self) -> std::io::Result<TlsStream<TcpStream>> {
        self.tls
            .connect(
                ServerName::try_from("localhost").unwrap(),
                TcpStream::connect(self.addr).await?,
            )
            .await
    }
    async fn try_connect(&self) -> std::io::Result<TlsStream<TcpStream>> {
        self.client_tls
            .connect(
                ServerName::try_from("localhost").unwrap(),
                TcpStream::connect(self.addr).await?,
            )
            .await
    }
    async fn connect(&self) -> TlsStream<TcpStream> {
        self.try_connect().await.unwrap()
    }
    async fn request(&self, path: &str, key: Option<&str>) -> String {
        trait Io: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin {}
        impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin> Io for T {}
        let mut stream: Box<dyn Io> = if path.starts_with("/__codeg_relay/") {
            Box::new(TcpStream::connect(self.addr).await.unwrap())
        } else {
            Box::new(self.connect().await)
        };
        let auth = key
            .map(|k| format!("Authorization: Bearer {k}\r\n"))
            .unwrap_or_default();
        stream
            .write_all(
                format!(
                    "GET {path} HTTP/1.1\r\nHost: localhost\r\n{auth}Connection: close\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let mut response = String::new();
        timeout(Duration::from_secs(5), stream.read_to_string(&mut response))
            .await
            .unwrap()
            .unwrap();
        response
    }
    async fn ws(
        &self,
        key: &str,
    ) -> Result<WebSocketStream<TlsStream<TcpStream>>, Box<tokio_tungstenite::tungstenite::Error>>
    {
        let mut req = format!("wss://localhost:{}{AGENT}", self.addr.port())
            .into_client_request()
            .unwrap();
        req.headers_mut()
            .insert("Authorization", format!("Bearer {key}").parse().unwrap());
        req.headers_mut()
            .insert("Sec-WebSocket-Protocol", "codeg-relay.v1".parse().unwrap());
        let (ws, response) = client_async(req, self.agent_connect().await).await?;
        assert_eq!(
            response.headers()["Sec-WebSocket-Protocol"],
            "codeg-relay.v1"
        );
        Ok(ws)
    }
    async fn stop(&mut self) {
        // This PID is the child spawned above from CARGO_BIN_EXE_codeg-relay.
        let status = Command::new("/bin/kill")
            .args(["-TERM", &self.child.id().unwrap().to_string()])
            .status()
            .await
            .unwrap();
        assert!(status.success());
        assert!(timeout(Duration::from_secs(6), self.child.wait())
            .await
            .unwrap()
            .unwrap()
            .success());
        self.log_task.take().unwrap().await.unwrap();
    }
}

struct Agent {
    handle: MuxHandle,
    task: JoinHandle<codeg_tunnel::mux::MuxEnd>,
    serving: JoinHandle<()>,
    close: oneshot::Receiver<u16>,
}
impl Agent {
    async fn start(relay: &Relay) -> Self {
        let ws = relay.ws(KEY).await.unwrap();
        let (tx, close) = oneshot::channel();
        let mut tx = Some(tx);
        let transport = ws
            .with(|msg: MuxMessage| {
                future::ready(Ok::<_, tokio_tungstenite::tungstenite::Error>(match msg {
                    MuxMessage::Binary(b) => Message::Binary(b.into()),
                    MuxMessage::Ping(b) => Message::Ping(b.into()),
                    MuxMessage::Pong(b) => Message::Pong(b.into()),
                    MuxMessage::Close => Message::Close(None),
                }))
            })
            .map(move |msg| -> std::io::Result<MuxMessage> {
                Ok(match msg.map_err(std::io::Error::other)? {
                    Message::Binary(b) => MuxMessage::Binary(b.to_vec()),
                    Message::Ping(b) => MuxMessage::Ping(b.to_vec()),
                    Message::Pong(b) => MuxMessage::Pong(b.to_vec()),
                    Message::Close(frame) => {
                        if let Some(tx) = tx.take() {
                            let _ = tx.send(frame.map(|f| f.code.into()).unwrap_or(1005));
                        }
                        MuxMessage::Close
                    }
                    _ => return Err(std::io::Error::other("unexpected text")),
                })
            });
        let (handle, mut incoming, task) = mux::start(
            transport,
            MuxConfig {
                role: MuxRole::Acceptor,
                ..Default::default()
            },
        );
        let tls = relay.home_tls.clone();
        let serving = tokio::spawn(async move {
            let mut tasks = tokio::task::JoinSet::new();
            while let Some(request) = incoming.accept().await {
                assert_eq!(request.host(), "127.0.0.1");
                assert_ne!(request.port(), 0);
                let tls = tls.clone();
                tasks.spawn(async move {
                    if let Ok(stream) = tls.accept(request.accept().await.unwrap()).await {
                        serve_fake(stream).await;
                    }
                });
                while tasks.try_join_next().is_some() {}
            }
            tasks.shutdown().await;
        });
        Self {
            handle,
            task,
            serving,
            close,
        }
    }
    async fn stop(self) {
        self.handle.close();
        let _ = self.task.await;
        let _ = self.serving.await;
    }
}
async fn read_head<S: tokio::io::AsyncRead + Unpin>(stream: &mut S) -> String {
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        head.push(stream.read_u8().await.unwrap());
    }
    String::from_utf8(head).unwrap()
}
async fn serve_fake(mut stream: tokio_rustls::server::TlsStream<TunnelStream>) {
    let head = read_head(&mut stream).await;
    if head.starts_with("GET /ws ") {
        let key = head
            .lines()
            .find_map(|s| {
                s.split_once(':')
                    .filter(|(k, _)| k.eq_ignore_ascii_case("sec-websocket-key"))
                    .map(|(_, v)| v.trim())
            })
            .unwrap();
        let accept = tokio_tungstenite::tungstenite::handshake::derive_accept_key(key.as_bytes());
        stream.write_all(format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n").as_bytes()).await.unwrap();
        let mut ws = WebSocketStream::from_raw_socket(stream, Role::Server, None).await;
        while let Some(Ok(msg)) = ws.next().await {
            if msg.is_close() {
                break;
            }
            if ws.send(msg).await.is_err() {
                break;
            }
        }
    } else if head.starts_with("GET /hold ") {
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000000\r\n\r\n")
            .await
            .unwrap();
        let _ = stream.read_u8().await;
    } else {
        let len = head
            .lines()
            .find_map(|s| s.strip_prefix("Content-Length: "))
            .map(|s| s.parse::<usize>().unwrap())
            .unwrap_or(0);
        let body = if len > 0 {
            let mut body = vec![0; len];
            stream.read_exact(&mut body).await.unwrap();
            body
        } else {
            b"hello through relay".to_vec()
        };
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        stream.write_all(&body).await.unwrap();
        stream.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn offline_closes_fast_and_health_is_public_plaintext() {
    let mut relay = Relay::start(64).await;
    assert!(timeout(Duration::from_secs(2), relay.try_connect())
        .await
        .unwrap()
        .is_err());
    let public = relay.request(HEALTH, None).await;
    assert!(public.starts_with("HTTP/1.1 200"));
    assert!(public.contains("\"mode\":\"e2ee\""));
    assert!(public.contains("\"agent_connected\":false"));
    let agent = Agent::start(&relay).await;
    assert!(relay
        .request(HEALTH, None)
        .await
        .contains("\"agent_connected\":true"));
    let mut plain = TcpStream::connect(relay.addr).await.unwrap();
    plain
        .write_all(b"GET /api/private HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    assert!(read_head(&mut plain).await.starts_with("HTTP/1.1 404"));
    agent.stop().await;
    relay.stop().await;
}

#[tokio::test]
async fn https_and_websocket_are_forwarded() {
    let mut relay = Relay::start(64).await;
    let agent = Agent::start(&relay).await;
    assert!(relay
        .request("/hello", None)
        .await
        .ends_with("hello through relay"));
    let (mut ws, _) = client_async(
        format!("wss://localhost:{}/ws", relay.addr.port()),
        relay.connect().await,
    )
    .await
    .unwrap();
    ws.send(Message::Text("echo through relay".into()))
        .await
        .unwrap();
    assert_eq!(
        ws.next().await.unwrap().unwrap(),
        Message::Text("echo through relay".into())
    );
    ws.close(None).await.unwrap();
    drop(ws);
    agent.stop().await;
    relay.stop().await;
}

#[tokio::test]
async fn agent_auth_is_rate_limited() {
    let mut relay = Relay::start(64).await;
    for attempt in 0..12 {
        match *relay.ws("wrong-key").await.unwrap_err() {
            tokio_tungstenite::tungstenite::Error::Http(response) => assert_eq!(
                response.status().as_u16(),
                if attempt < 10 { 401 } else { 429 }
            ),
            e => panic!("{e}"),
        }
    }
    relay.stop().await;
}

#[tokio::test]
async fn replacement_closes_old_agent_with_4001_and_preserves_new_one() {
    let mut relay = Relay::start(64).await;
    let mut old = Agent::start(&relay).await;
    let mut old_client = relay.connect().await;
    old_client
        .write_all(b"GET /hold HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    assert!(read_head(&mut old_client).await.starts_with("HTTP/1.1 200"));
    let new = Agent::start(&relay).await;
    assert_eq!(
        timeout(Duration::from_secs(3), &mut old.close)
            .await
            .unwrap()
            .unwrap(),
        4001
    );
    old.stop().await;
    assert!(timeout(Duration::from_secs(2), old_client.read_u8())
        .await
        .unwrap()
        .is_err());
    assert!(relay
        .request("/hello", None)
        .await
        .ends_with("hello through relay"));
    new.stop().await;
    relay.stop().await;
}

#[tokio::test]
async fn per_ip_cap_closes_and_recovers() {
    let mut relay = Relay::start(2).await;
    let a = relay.agent_connect().await;
    let b = relay.agent_connect().await;
    let mut rejected = TcpStream::connect(relay.addr).await.unwrap();
    assert!(timeout(Duration::from_secs(2), rejected.read_u8())
        .await
        .unwrap()
        .is_err());
    drop(a);
    drop(b);
    timeout(Duration::from_secs(3), async {
        loop {
            if relay.try_agent_connect().await.is_ok() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    relay.stop().await;
}

#[tokio::test]
async fn fingerprint_matches_leaf_and_cli_and_survives_restart() {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::PermissionsExt;
    let mut relay = Relay::start(64).await;
    let stream = relay.agent_connect().await;
    let der = &stream.get_ref().1.peer_certificates().unwrap()[0];
    assert_eq!(
        relay.fingerprint,
        format!("{:x}", Sha256::digest(der.as_ref()))
    );
    drop(stream);
    for f in ["tls/key.pem", "tls/cert.pem"] {
        assert_eq!(
            std::fs::metadata(relay.dir.path().join(f))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let output = Command::new(env!("CARGO_BIN_EXE_codeg-relay"))
        .args(["fingerprint", "--data-dir"])
        .arg(relay.dir.path())
        .output()
        .await
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .next()
            .unwrap(),
        relay.fingerprint
    );
    relay.stop().await;
    let fingerprint = relay.fingerprint.clone();
    let mut relay = Relay::in_dir(relay.dir, 64).await;
    assert_eq!(fingerprint, relay.fingerprint);
    relay.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn twenty_mib_in_both_directions_is_intact() {
    let mut relay = Relay::start(64).await;
    let agent = Agent::start(&relay).await;
    let mut client = relay.connect().await;
    let body: Vec<u8> = (0..20 * 1024 * 1024)
        .map(|i| ((i * 73 + i / 251) % 256) as u8)
        .collect();
    let mut phase = "upload";
    let mut response = Vec::new();
    let result = timeout(Duration::from_secs(30), async {
        client
            .write_all(
                format!(
                    "POST /echo HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        client.write_all(&body).await.unwrap();
        client.flush().await.unwrap();
        phase = "response head";
        assert!(read_head(&mut client).await.starts_with("HTTP/1.1 200"));
        phase = "response body/EOF";
        client.read_to_end(&mut response).await.unwrap();
        assert_eq!(response, body);
    })
    .await;
    assert!(
        result.is_ok(),
        "timed out during {phase}, received {} bytes",
        response.len()
    );
    agent.stop().await;
    relay.stop().await;
}

#[tokio::test]
async fn disconnect_midstream_closes_client_promptly() {
    let mut relay = Relay::start(64).await;
    let agent = Agent::start(&relay).await;
    let mut client = relay.connect().await;
    client
        .write_all(b"GET /hold HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    assert!(read_head(&mut client).await.starts_with("HTTP/1.1 200"));
    assert_eq!(agent.handle.stats().active_streams, 1);
    agent.stop().await;
    let result = timeout(Duration::from_secs(2), client.read_u8())
        .await
        .unwrap();
    assert!(result.is_err());
    relay.stop().await;
}

#[tokio::test]
async fn trace_logging_never_exposes_auth_headers_queries_or_payloads() {
    let mut relay = Relay::with_options(tempfile::tempdir().unwrap(), 64, 512, "trace").await;
    let agent = Agent::start(&relay).await;
    assert!(relay
        .request("/safe/SECRET-PATH?token=SECRET-QUERY", Some("SECRET-TOKEN"))
        .await
        .starts_with("HTTP/1.1 200"));
    agent.stop().await;
    relay.stop().await;
    let logs = relay.logs.lock().unwrap();
    for secret in [
        KEY,
        "SECRET-PATH",
        "SECRET-QUERY",
        "SECRET-TOKEN",
        "hello through relay",
        "Authorization:",
    ] {
        assert!(!logs.contains(secret), "leaked secret class: {secret}");
    }
    assert!(!logs.contains("path="));
    assert!(!logs.contains("method="));
    assert!(logs.contains("classification=\"client\""));
    assert!(logs.contains("bytes_up="));
}

#[tokio::test]
async fn total_cap_and_oversized_or_malformed_head() {
    let mut relay = Relay::with_options(tempfile::tempdir().unwrap(), 64, 2, "info").await;
    let a = relay.agent_connect().await;
    let b = relay.agent_connect().await;
    assert!(relay.try_connect().await.is_err());
    drop(a);
    drop(b);
    relay.stop().await;
    let mut relay = Relay::start(64).await;
    for (request, status) in [
        (
            format!("GET / HTTP/1.1\r\nX-Long: {}", "x".repeat(65536)),
            431,
        ),
        ("INVALID\r\n\r\n".to_owned(), 400),
    ] {
        let mut client = relay.agent_connect().await;
        let _ = client.write_all(request.as_bytes()).await;
        assert!(read_head(&mut client)
            .await
            .starts_with(&format!("HTTP/1.1 {status}")));
    }
    assert!(relay
        .request(HEALTH, None)
        .await
        .starts_with("HTTP/1.1 200"));
    relay.stop().await;
}

#[tokio::test]
async fn refused_mux_open_closes_without_client_http() {
    let mut relay = Relay::start(64).await;
    let mut ws = relay.ws(KEY).await.unwrap();
    let request = relay.try_connect();
    let reject = async {
        while let Some(Ok(message)) = ws.next().await {
            if let Message::Binary(bytes) = message {
                assert_eq!(bytes[0], 1);
                let mut close = vec![6];
                close.extend_from_slice(&bytes[1..5]);
                close.push(1);
                ws.send(Message::Binary(close.into())).await.unwrap();
                break;
            }
        }
    };
    let (response, _) = tokio::join!(request, reject);
    assert!(response.is_err());
    drop(ws);
    relay.stop().await;
}

#[tokio::test]
async fn sigterm_closes_live_agent_and_partial_requests_within_five_seconds() {
    let mut relay = Relay::start(64).await;
    let agent = Agent::start(&relay).await;
    let _partial = relay.connect().await;
    let started = std::time::Instant::now();
    relay.stop().await;
    assert!(started.elapsed() < Duration::from_millis(5800));
    agent.stop().await;
}

#[tokio::test]
async fn head_and_body_in_one_write_are_replayed_intact() {
    let mut relay = Relay::start(64).await;
    let agent = Agent::start(&relay).await;
    let mut client = relay.connect().await;
    client
        .write_all(
            b"POST /echo HTTP/1.1\r\nHost: localhost\r\nContent-Length: 13\r\n\r\nreplayed-body",
        )
        .await
        .unwrap();
    let mut response = String::new();
    client.read_to_string(&mut response).await.unwrap();
    assert!(response.ends_with("replayed-body"));
    agent.stop().await;
    relay.stop().await;
}

#[tokio::test]
async fn tls_and_partial_head_have_independent_ten_second_deadlines() {
    let mut relay = Relay::start(64).await;
    let mut tls_stall = TcpStream::connect(relay.addr).await.unwrap();
    let mut head_stall = relay.agent_connect().await;
    head_stall.write_all(b"GET / HTTP/1.1\r\n").await.unwrap();
    let started = std::time::Instant::now();
    timeout(Duration::from_secs(12), async {
        let (tls_end, head) = tokio::join!(tls_stall.read_u8(), read_head(&mut head_stall));
        assert!(tls_end.is_err());
        assert!(head.starts_with("HTTP/1.1 408"));
    })
    .await
    .unwrap();
    assert!(started.elapsed() >= Duration::from_secs(9));
    assert!(relay
        .request(HEALTH, None)
        .await
        .starts_with("HTTP/1.1 200"));
    relay.stop().await;
}

#[tokio::test]
async fn text_or_oversized_agent_messages_terminate_session() {
    let mut relay = Relay::start(64).await;
    for message in [
        Message::Text("invalid text".into()),
        Message::Binary(vec![0; 32774].into()),
    ] {
        let mut ws = relay.ws(KEY).await.unwrap();
        ws.send(message).await.unwrap();
        timeout(Duration::from_secs(3), async {
            while let Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) = ws.next().await {}
        })
        .await
        .unwrap();
    }
    relay.stop().await;
}
