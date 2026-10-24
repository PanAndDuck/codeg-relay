use axum::{
    body::Body as AxumBody,
    extract::{
        ws::{Message, WebSocket},
        State, WebSocketUpgrade,
    },
    response::IntoResponse,
    routing::get,
    Router,
};
use bytes::Bytes;
use codeg_relay::config::Serve;
use codeg_relay_agent::{
    client::Agent,
    config::{Config, TokenSource},
    proxy::{Body, BoxError},
    secrets, tls,
};
use futures_util::{SinkExt, StreamExt};
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::{
    body::{Frame, Incoming},
    client::conn::http1::SendRequest,
    Request, Response,
};
use hyper_util::rt::TokioIo;
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::{
    net::TcpListener,
    task::JoinHandle,
    time::{timeout, Instant},
};
use tokio_util::sync::CancellationToken;

const TEST_TOKEN: &str = "test-only-web-value-01";
const LARGE: usize = 12 * 1024 * 1024;

#[derive(Default)]
struct Upstream {
    hits: AtomicUsize,
    produced: AtomicUsize,
    finished: AtomicUsize,
}
struct ProducerDone(Arc<Upstream>);
impl Drop for ProducerDone {
    fn drop(&mut self) {
        self.0.finished.fetch_add(1, Ordering::Relaxed);
    }
}

async fn fake(
    State(state): State<Arc<Upstream>>,
    request: Request<AxumBody>,
) -> Response<AxumBody> {
    state.hits.fetch_add(1, Ordering::Relaxed);
    match request.uri().path() {
        "/api/echo" => {
            let chunked = request
                .headers()
                .get("transfer-encoding")
                .is_some_and(|v| v == "chunked");
            let body = axum::body::to_bytes(request.into_body(), LARGE * 2)
                .await
                .unwrap();
            Response::builder()
                .header("x-was-chunked", chunked.to_string())
                .body(AxumBody::from(body))
                .unwrap()
        }
        "/api/large" => {
            let stream = futures_util::stream::iter(
                (0..LARGE / 16384)
                    .map(|i| Ok::<_, std::io::Error>(Bytes::from(vec![(i % 251) as u8; 16384]))),
            );
            Response::new(AxumBody::from_stream(stream))
        }
        "/api/stream" => {
            let stream = futures_util::stream::unfold(0, |i| async move {
                if i == 5 {
                    return None;
                }
                if i > 0 {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Some((Ok::<_, std::io::Error>(format!("data: {i}\n\n")), i + 1))
            });
            Response::builder()
                .header("content-type", "text/event-stream")
                .body(AxumBody::from_stream(stream))
                .unwrap()
        }
        "/api/flood" => {
            let done = ProducerDone(state.clone());
            let stream =
                futures_util::stream::unfold((0, state, done), |(i, state, done)| async move {
                    if i == 32768 {
                        return None;
                    }
                    state.produced.fetch_add(32 * 1024, Ordering::Relaxed);
                    Some((
                        Ok::<_, std::io::Error>(Bytes::from_static(&[37; 32 * 1024])),
                        (i + 1, state, done),
                    ))
                });
            Response::new(AxumBody::from_stream(stream))
        }
        "/api/headers" => {
            let h = request.headers();
            let get = |name| {
                h.get(name)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default()
            };
            let body = serde_json::json!({"host":get("host"), "xff":get("x-forwarded-for"), "xfp":get("x-forwarded-proto"), "keep":get("x-keep"), "removed":h.contains_key("x-remove"), "proxy_auth":h.contains_key("proxy-authorization")});
            Response::builder()
                .header("connection", "keep-alive, x-remove-response")
                .header("x-remove-response", "gone")
                .header("x-keep-response", "kept")
                .body(AxumBody::from(body.to_string()))
                .unwrap()
        }
        _ => Response::new(AxumBody::from("fake-upstream-ok")),
    }
}

async fn websocket(
    State(state): State<Arc<Upstream>>,
    uri: http::Uri,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    state.hits.fetch_add(1, Ordering::Relaxed);
    ws.protocols(["codeg-events"])
        .on_upgrade(move |mut socket: WebSocket| async move {
            let _done = ProducerDone(state.clone());
            if socket
                .send(Message::Text("server-push".into()))
                .await
                .is_err()
            {
                return;
            }
            if uri.query() == Some("flood=1") {
                for _ in 0..32768 {
                    if socket
                        .send(Message::Binary(Bytes::from_static(&[37; 32 * 1024])))
                        .await
                        .is_err()
                    {
                        break;
                    }
                    state.produced.fetch_add(32 * 1024, Ordering::Relaxed);
                }
                return;
            }
            while let Some(Ok(message)) = socket.recv().await {
                if matches!(message, Message::Close(_)) {
                    break;
                }
                if socket.send(message).await.is_err() {
                    break;
                }
            }
        })
}

struct Fixture {
    _temp: tempfile::TempDir,
    relay_dir: PathBuf,
    key_file: PathBuf,
    token_file: PathBuf,
    relay_addr: SocketAddr,
    relay_stop: CancellationToken,
    relay_task: Option<JoinHandle<anyhow::Result<()>>>,
    stop: CancellationToken,
    agent_task: JoinHandle<()>,
    upstream_task: JoinHandle<()>,
    agent: Arc<Agent>,
    upstream: Arc<Upstream>,
}

fn relay_options(addr: SocketAddr, dir: PathBuf, key: PathBuf) -> Serve {
    Serve {
        listen: addr,
        agent_key_file: Some(key),
        tls_cert: None,
        tls_key: None,
        self_signed: true,
        public_host: vec!["localhost".into(), "127.0.0.1".into()],
        data_dir: dir,
        insecure_plaintext: false,
        max_conns: 512,
        max_conns_per_ip: 64,
    }
}

async fn wait_for(mut predicate: impl FnMut() -> bool) {
    timeout(Duration::from_secs(10), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("readiness deadline");
}

impl Fixture {
    async fn start(edit: impl FnOnce(&mut Config)) -> Self {
        Self::start_inner(edit, false).await
    }
    async fn start_inner(edit: impl FnOnce(&mut Config), https: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let relay_dir = temp.path().join("relay");
        let key_file = temp.path().join("agent.key");
        let token_file = temp.path().join("web.token");
        secrets::write_private(&key_file, b"test-only-agent-value-123456", false).unwrap();
        secrets::write_private(&token_file, TEST_TOKEN.as_bytes(), false).unwrap();
        let upstream = Arc::new(Upstream::default());
        let app = Router::new()
            .route("/ws/events", get(websocket))
            .route("/ws/browser-tunnel", get(websocket))
            .fallback(fake)
            .with_state(upstream.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_addr = listener.local_addr().unwrap();
        let stop = CancellationToken::new();
        let upstream_stop = stop.clone();
        let identity = https.then(|| {
            codeg_relay_agent::certificate::load(&temp.path().join("upstream-identity"), false)
                .unwrap()
        });
        let upstream_fingerprint = identity.as_ref().map(|tls| tls.fingerprint.clone());
        let upstream_task = if let Some(tls) = identity {
            tokio::spawn(async move {
                use tower::ServiceExt;
                let mut connections = tokio::task::JoinSet::new();
                loop {
                    tokio::select! {
                        _ = upstream_stop.cancelled() => break,
                        _ = connections.join_next(), if !connections.is_empty() => {},
                        accepted = listener.accept() => {
                            let (tcp, _) = accepted.unwrap();
                            let app = app.clone(); let tls = tls.clone();
                            connections.spawn(async move {
                                let Ok(tls) = tls.acceptor.accept(tcp).await else { return; };
                                let service = hyper::service::service_fn(move |req| app.clone().oneshot(req));
                                let _ = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(tls),service).with_upgrades().await;
                            });
                        }
                    }
                }
                connections.shutdown().await;
            })
        } else {
            tokio::spawn(async move {
                axum::serve(listener, app)
                    .with_graceful_shutdown(upstream_stop.cancelled_owned())
                    .await
                    .unwrap();
            })
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let relay_addr = listener.local_addr().unwrap();
        let options = relay_options(relay_addr, relay_dir.clone(), key_file.clone());
        let (_, relay_pin) = codeg_relay::tls::configure(&options).unwrap();
        let relay_stop = CancellationToken::new();
        let stopped = relay_stop.clone();
        let relay_task = tokio::spawn(codeg_relay::server::serve_with_listener(
            options,
            listener,
            stopped.cancelled_owned(),
        ));
        let mut config = Config {
            relay_url: format!("https://{relay_addr}"),
            relay_fingerprint: relay_pin,
            agent_key_file: key_file.clone(),
            token_source: TokenSource::File(token_file.clone()),
            upstream_url: format!("{}://{upstream_addr}", if https { "https" } else { "http" }),
            upstream_fingerprint,
            ..Default::default()
        };
        edit(&mut config);
        let agent = Arc::new(Agent::new(config, &temp.path().join("agent")).unwrap());
        let agent_task = tokio::spawn(agent.clone().run(stop.clone()));
        wait_for(|| agent.status.snapshot().state == "connected").await;
        Self {
            _temp: temp,
            relay_dir,
            key_file,
            token_file,
            relay_addr,
            relay_stop,
            relay_task: Some(relay_task),
            stop,
            agent_task,
            upstream_task,
            agent,
            upstream,
        }
    }
    async fn tls(&self) -> tls::Socket {
        let url = url::Url::parse(&format!("https://{}", self.relay_addr)).unwrap();
        timeout(
            Duration::from_secs(5),
            tls::connect(
                &url,
                Some(&self.agent.status.snapshot().home_fingerprint),
                &[b"http/1.1"],
            ),
        )
        .await
        .unwrap()
        .unwrap()
    }
    async fn slow_tls(&self) -> tls::Socket {
        let tcp = tokio::net::TcpStream::connect(self.relay_addr)
            .await
            .unwrap();
        tls::configure_tcp(&tcp).unwrap();
        socket2::SockRef::from(&tcp)
            .set_recv_buffer_size(4096)
            .unwrap();
        let connector = tokio_rustls::TlsConnector::from(
            tls::client_config(
                Some(&self.agent.status.snapshot().home_fingerprint),
                &[b"http/1.1"],
            )
            .unwrap(),
        );
        Box::new(
            connector
                .connect(
                    rustls::pki_types::ServerName::try_from("codeg-home").unwrap(),
                    tcp,
                )
                .await
                .unwrap(),
        )
    }
    async fn phone(&self) -> Phone {
        let (sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(self.tls().await))
            .await
            .unwrap();
        let task = tokio::spawn(async move {
            let _ = conn.with_upgrades().await;
        });
        Phone { sender, task }
    }
    async fn ws(&self) -> tokio_tungstenite::WebSocketStream<tls::Socket> {
        use base64::Engine;
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let mut request = format!("wss://{}/ws/events", self.relay_addr)
            .into_client_request()
            .unwrap();
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(TEST_TOKEN);
        request.headers_mut().insert(
            "sec-websocket-protocol",
            format!("codeg-events, codeg-token.{encoded}")
                .parse()
                .unwrap(),
        );
        tokio_tungstenite::client_async(request, self.tls().await)
            .await
            .unwrap()
            .0
    }
    async fn shutdown(mut self) {
        self.stop.cancel();
        self.relay_stop.cancel();
        timeout(Duration::from_secs(10), &mut self.agent_task)
            .await
            .unwrap()
            .unwrap();
        if let Some(task) = self.relay_task.take() {
            timeout(Duration::from_secs(10), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        }
        timeout(Duration::from_secs(5), &mut self.upstream_task)
            .await
            .unwrap()
            .unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.cancel();
        self.relay_stop.cancel();
        self.agent_task.abort();
        self.upstream_task.abort();
        if let Some(task) = &self.relay_task {
            task.abort();
        }
    }
}
struct Phone {
    sender: SendRequest<Body>,
    task: JoinHandle<()>,
}
impl Drop for Phone {
    fn drop(&mut self) {
        self.task.abort();
    }
}
fn full(bytes: impl Into<Bytes>) -> Body {
    Full::new(bytes.into())
        .map_err(|never| match never {})
        .boxed_unsync()
}
fn request(path: &str, token: Option<&str>, body: Body) -> Request<Body> {
    let mut b = Request::builder().uri(path).header("host", "phone.example");
    if let Some(token) = token {
        b = b.header("authorization", format!("Bearer {token}"));
    }
    b.body(body).unwrap()
}
async fn bytes(response: Response<Incoming>) -> Bytes {
    timeout(Duration::from_secs(10), response.into_body().collect())
        .await
        .unwrap()
        .unwrap()
        .to_bytes()
}

#[tokio::test]
async fn http_keepalive_chunked_upload_and_twelve_mib_download() {
    let f = Fixture::start(|_| {}).await;
    let mut phone = f.phone().await;
    let response = phone
        .sender
        .send_request(request("/api/health", Some(TEST_TOKEN), full("")))
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(bytes(response).await, "fake-upstream-ok");
    let chunks = futures_util::stream::iter(
        (0..64).map(|i| Ok::<_, BoxError>(Frame::data(Bytes::from(vec![i; 1024])))),
    );
    let mut upload = request(
        "/api/echo",
        Some(TEST_TOKEN),
        StreamBody::new(chunks).boxed_unsync(),
    );
    *upload.method_mut() = http::Method::POST;
    let response = phone.sender.send_request(upload).await.unwrap();
    assert_eq!(response.headers()["x-was-chunked"], "true");
    let body = bytes(response).await;
    assert_eq!(body.len(), 64 * 1024);
    for (i, chunk) in body.chunks(1024).enumerate() {
        assert!(chunk.iter().all(|b| *b == i as u8));
    }
    let response = phone
        .sender
        .send_request(request("/api/large", Some(TEST_TOKEN), full("")))
        .await
        .unwrap();
    let body = bytes(response).await;
    assert_eq!(body.len(), LARGE);
    for (i, chunk) in body.chunks(16384).enumerate() {
        assert!(chunk.iter().all(|b| *b == (i % 251) as u8));
    }
    assert_eq!(
        f.agent.gateway.metrics.opened.load(Ordering::Relaxed),
        1,
        "upstream keepalive was not reused"
    );
    drop(phone);
    f.shutdown().await;
}

#[tokio::test]
async fn streaming_delivers_first_event_before_upstream_finishes() {
    let f = Fixture::start(|_| {}).await;
    let mut phone = f.phone().await;
    let started = Instant::now();
    let response = phone
        .sender
        .send_request(request("/api/stream", Some(TEST_TOKEN), full("")))
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let mut body = response.into_body();
    let first = timeout(Duration::from_millis(250), body.frame())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .into_data()
        .unwrap();
    assert_eq!(first, "data: 0\n\n");
    assert!(started.elapsed() < Duration::from_millis(350));
    let rest = body.collect().await.unwrap().to_bytes();
    assert_eq!(rest, "data: 1\n\ndata: 2\n\ndata: 3\n\ndata: 4\n\n");
    assert!(started.elapsed() >= Duration::from_millis(400));
    drop(phone);
    f.shutdown().await;
}

#[tokio::test]
async fn websocket_protocol_auth_server_push_and_echo() {
    let f = Fixture::start(|_| {}).await;
    let mut ws = f.ws().await;
    assert_eq!(
        ws.next().await.unwrap().unwrap().into_text().unwrap(),
        "server-push"
    );
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        "echo-me".into(),
    ))
    .await
    .unwrap();
    assert_eq!(
        ws.next().await.unwrap().unwrap().into_text().unwrap(),
        "echo-me"
    );
    ws.send(tokio_tungstenite::tungstenite::Message::Binary(
        vec![19; 1024 * 1024].into(),
    ))
    .await
    .unwrap();
    let data = ws.next().await.unwrap().unwrap().into_data();
    assert_eq!(data.len(), 1024 * 1024);
    assert!(data.iter().all(|b| *b == 19));
    ws.close(None).await.unwrap();
    drop(ws);
    f.shutdown().await;
}

#[tokio::test]
async fn unauthorized_and_rate_limited_requests_never_reach_upstream() {
    let f = Fixture::start(|c| c.auth_failures = 2).await;
    let mut phone = f.phone().await;
    let response = phone
        .sender
        .send_request(request("/api/health", None, full("")))
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    assert_eq!(
        response.headers()["content-type"],
        "text/plain; charset=utf-8"
    );
    assert_eq!(
        response.headers()["www-authenticate"],
        "Bearer realm=\"codeg\""
    );
    assert_eq!(bytes(response).await, "Invalid or missing token");
    let response = phone
        .sender
        .send_request(request("/api/health", Some("wrong"), full("")))
        .await
        .unwrap();
    assert_eq!(response.status(), 429);
    assert!(response.headers().contains_key("retry-after"));
    assert_eq!(bytes(response).await, "Too many authentication failures");
    assert_eq!(f.upstream.hits.load(Ordering::Relaxed), 0);
    assert_eq!(f.agent.gateway.metrics.opened.load(Ordering::Relaxed), 0);
    let response = phone
        .sender
        .send_request(request("/api/health", Some(TEST_TOKEN), full("")))
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    bytes(response).await;
    let response = phone
        .sender
        .send_request(request("/api/health", Some("wrong"), full("")))
        .await
        .unwrap();
    assert_eq!(response.status(), 429);
    assert_eq!(
        response.headers()["content-type"],
        "text/plain; charset=utf-8"
    );
    let retry_after: u64 = response.headers()["retry-after"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((1..=900).contains(&retry_after));
    assert_eq!(bytes(response).await, "Too many authentication failures");
    assert_eq!(f.upstream.hits.load(Ordering::Relaxed), 1);
    let response = phone
        .sender
        .send_request(request("/api/get_system_language_settings", None, full("")))
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    bytes(response).await;
    let response = phone
        .sender
        .send_request(request("/api/health", Some("wrong"), full("")))
        .await
        .unwrap();
    assert_eq!(response.status(), 429);
    assert_eq!(bytes(response).await, "Too many authentication failures");
    assert_eq!(f.upstream.hits.load(Ordering::Relaxed), 2);
    drop(phone);
    f.shutdown().await;
}

#[tokio::test]
async fn token_rotation_is_picked_up_on_existing_keepalive_connection() {
    let f = Fixture::start(|c| c.token_refresh_secs = 1).await;
    let mut phone = f.phone().await;
    bytes(
        phone
            .sender
            .send_request(request("/api/health", Some(TEST_TOKEN), full("")))
            .await
            .unwrap(),
    )
    .await;
    secrets::write_private(&f.token_file, b"test-only-rotated-value-02", true).unwrap();
    timeout(Duration::from_secs(3), async {
        while f.agent.gateway.tokens.current() == TEST_TOKEN {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let response = phone
        .sender
        .send_request(request("/api/health", Some(TEST_TOKEN), full("")))
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    bytes(response).await;
    let response = phone
        .sender
        .send_request(request(
            "/api/health",
            Some("test-only-rotated-value-02"),
            full(""),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    bytes(response).await;
    drop(phone);
    f.shutdown().await;
}

#[tokio::test]
async fn hop_headers_are_removed_and_forwarded_peer_cannot_be_spoofed() {
    let f = Fixture::start(|_| {}).await;
    let mut phone = f.phone().await;
    let mut req = request("/api/headers", Some(TEST_TOKEN), full(""));
    for (name, value) in [
        ("connection", "keep-alive, x-remove"),
        ("x-remove", "gone"),
        ("proxy-authorization", "example"),
        ("x-keep", "kept"),
        ("x-forwarded-for", "198.51.100.99"),
        ("x-forwarded-proto", "http"),
    ] {
        req.headers_mut().insert(name, value.parse().unwrap());
    }
    let response = phone.sender.send_request(req).await.unwrap();
    assert_eq!(response.status(), 200);
    assert!(!response.headers().contains_key("x-remove-response"));
    assert_eq!(response.headers()["x-keep-response"], "kept");
    let value: serde_json::Value = serde_json::from_slice(&bytes(response).await).unwrap();
    assert_eq!(
        value,
        serde_json::json!({"host":"phone.example","xff":"127.0.0.1","xfp":"https","keep":"kept","removed":false,"proxy_auth":false})
    );
    drop(phone);
    f.shutdown().await;
}

#[tokio::test]
async fn reconnects_after_same_relay_is_restarted() {
    let mut f = Fixture::start(|_| {}).await;
    f.relay_stop.cancel();
    timeout(Duration::from_secs(10), f.relay_task.take().unwrap())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    wait_for(|| f.agent.status.snapshot().state != "connected").await;
    let started = Instant::now();
    let listener = TcpListener::bind(f.relay_addr).await.unwrap();
    f.relay_stop = CancellationToken::new();
    f.relay_task = Some(tokio::spawn(codeg_relay::server::serve_with_listener(
        relay_options(f.relay_addr, f.relay_dir.clone(), f.key_file.clone()),
        listener,
        f.relay_stop.clone().cancelled_owned(),
    )));
    wait_for(|| f.agent.status.snapshot().state == "connected").await;
    let mut phone = f.phone().await;
    let response = phone
        .sender
        .send_request(request("/api/health", Some(TEST_TOKEN), full("")))
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    bytes(response).await;
    assert!(started.elapsed() < Duration::from_secs(5));
    drop(phone);
    f.shutdown().await;
}

async fn prove_other_stream_and_stall(f: &Fixture) {
    wait_for(|| f.upstream.produced.load(Ordering::Relaxed) > 128 * 1024).await;
    let started = Instant::now();
    let mut healthy = f.phone().await;
    let response = timeout(
        Duration::from_millis(750),
        healthy
            .sender
            .send_request(request("/api/health", Some(TEST_TOKEN), full(""))),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(response.status(), 200);
    bytes(response).await;
    // The caller still owns the unpolled slow socket. Confirm timeout and
    // upstream release before allowing it to drain any already-buffered bytes.
    wait_for(|| f.agent.gateway.metrics.stalls.load(Ordering::Relaxed) == 1).await;
    wait_for(|| f.upstream.finished.load(Ordering::Relaxed) == 1).await;
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "upstream remained blocked past bounded stall deadline"
    );
    assert!(
        f.upstream.produced.load(Ordering::Relaxed) < 16 * 1024 * 1024,
        "unbounded production while phone stopped reading"
    );
    let response = healthy
        .sender
        .send_request(request("/api/health", Some(TEST_TOKEN), full("")))
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    bytes(response).await;
    drop(healthy);
    wait_for(|| f.agent.gateway.metrics.active.load(Ordering::Relaxed) == 0).await;
}

#[tokio::test]
async fn slow_http_phone_is_closed_upstream_is_released_and_other_streams_survive() {
    let f = Fixture::start(|c| {
        c.buffer_bytes = 64 * 1024;
        c.stall_timeout_secs = 1;
    })
    .await;
    let mut stalled = f.slow_tls().await;
    stalled.write_all(format!("GET /api/flood HTTP/1.1\r\nHost: phone.example\r\nAuthorization: Bearer {TEST_TOKEN}\r\n\r\n").as_bytes()).await.unwrap();
    stalled.flush().await.unwrap();
    // No response bytes are read until the saturation timer has fired.
    prove_other_stream_and_stall(&f).await;
    let mut scratch = [0; 32 * 1024];
    timeout(Duration::from_secs(4), async {
        loop {
            match stalled.read(&mut scratch).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
    })
    .await
    .unwrap();
    drop(stalled);
    f.shutdown().await;
}

#[tokio::test]
async fn slow_websocket_phone_is_closed_without_stalling_other_streams() {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let f = Fixture::start(|c| {
        c.buffer_bytes = 64 * 1024;
        c.stall_timeout_secs = 1;
    })
    .await;
    let mut req = format!("wss://{}/ws/events?flood=1", f.relay_addr)
        .into_client_request()
        .unwrap();
    req.headers_mut().insert(
        "authorization",
        format!("Bearer {TEST_TOKEN}").parse().unwrap(),
    );
    let (mut stalled, _) = tokio_tungstenite::client_async(req, f.slow_tls().await)
        .await
        .unwrap();
    // Stop polling the WebSocket entirely after the upgrade.
    prove_other_stream_and_stall(&f).await;
    timeout(Duration::from_secs(4), async {
        while let Some(Ok(_)) = stalled.next().await {}
    })
    .await
    .unwrap();
    drop(stalled);
    f.shutdown().await;
}

#[tokio::test]
async fn wrong_home_pin_fails_before_any_upstream_request() {
    let f = Fixture::start(|_| {}).await;
    let url = url::Url::parse(&format!("https://{}", f.relay_addr)).unwrap();
    assert!(tls::connect(&url, Some(&"00".repeat(32)), &[b"http/1.1"])
        .await
        .is_err());
    assert_eq!(f.upstream.hits.load(Ordering::Relaxed), 0);
    f.shutdown().await;
}

#[tokio::test]
async fn https_upstream_with_matching_pin_is_supported() {
    let f = Fixture::start_inner(|_| {}, true).await;
    let mut phone = f.phone().await;
    let response = phone
        .sender
        .send_request(request("/api/health", Some(TEST_TOKEN), full("")))
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(bytes(response).await, "fake-upstream-ok");
    drop(phone);
    f.shutdown().await;
}

#[tokio::test]
async fn https_upstream_rejects_bad_pin_and_untrusted_webpki_certificate() {
    for pin in [Some("00".repeat(32)), None] {
        let f = Fixture::start_inner(|c| c.upstream_fingerprint = pin, true).await;
        let mut phone = f.phone().await;
        let response = phone
            .sender
            .send_request(request("/api/health", Some(TEST_TOKEN), full("")))
            .await
            .unwrap();
        assert_eq!(response.status(), 502);
        bytes(response).await;
        assert_eq!(f.upstream.hits.load(Ordering::Relaxed), 0);
        drop(phone);
        f.shutdown().await;
    }
}

#[tokio::test]
async fn unauthenticated_ws_and_query_token_are_rejected_before_dialing_upstream() {
    let f = Fixture::start(|_| {}).await;
    let mut phone = f.phone().await;
    for path in [
        "/ws/events",
        "/ws/browser-tunnel",
        "/api/health?token=test-only-web-value-01",
    ] {
        let response = phone
            .sender
            .send_request(request(path, None, full("")))
            .await
            .unwrap();
        assert_eq!(response.status(), 401);
        bytes(response).await;
    }
    assert_eq!(f.upstream.hits.load(Ordering::Relaxed), 0);
    assert_eq!(f.agent.gateway.metrics.opened.load(Ordering::Relaxed), 0);
    let response = phone
        .sender
        .send_request(request("/_next/static/example.js", None, full("")))
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    bytes(response).await;
    drop(phone);
    f.shutdown().await;
}

#[tokio::test]
async fn live_certificate_rotation_rejects_old_pin_and_accepts_repairing() {
    let f = Fixture::start(|_| {}).await;
    let old_pin = f.agent.status.snapshot().home_fingerprint;
    let mut existing = f.phone().await;
    bytes(
        existing
            .sender
            .send_request(request("/api/health", Some(TEST_TOKEN), full("")))
            .await
            .unwrap(),
    )
    .await;
    codeg_relay_agent::certificate::load(&f._temp.path().join("agent"), true).unwrap();
    wait_for(|| f.agent.status.snapshot().home_fingerprint != old_pin).await;
    let url = url::Url::parse(&format!("https://{}", f.relay_addr)).unwrap();
    assert!(tls::connect(&url, Some(&old_pin), &[b"http/1.1"])
        .await
        .is_err());
    let response = existing
        .sender
        .send_request(request("/api/health", Some(TEST_TOKEN), full("")))
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    bytes(response).await;
    let mut new_phone = f.phone().await;
    let response = new_phone
        .sender
        .send_request(request("/api/health", Some(TEST_TOKEN), full("")))
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    bytes(response).await;
    drop(existing);
    drop(new_phone);
    f.shutdown().await;
}

#[tokio::test]
async fn sqlite_rotation_is_reread_by_running_agent() {
    let f = Fixture::start(|c| {
        c.token_refresh_secs = 1;
        let TokenSource::File(path) = &c.token_source else {
            unreachable!()
        };
        let path = path.with_extension("sqlite");
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch("CREATE TABLE app_metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .unwrap();
        db.execute(
            "INSERT INTO app_metadata VALUES ('web_service_token', ?1)",
            [TEST_TOKEN],
        )
        .unwrap();
        c.token_source = TokenSource::Database(path);
    })
    .await;
    let TokenSource::Database(path) = &f.agent.gateway.config.token_source else {
        unreachable!()
    };
    let db = rusqlite::Connection::open(path).unwrap();
    db.execute(
        "UPDATE app_metadata SET value=?1",
        ["test-only-new-db-value"],
    )
    .unwrap();
    wait_for(|| f.agent.gateway.tokens.current() != TEST_TOKEN).await;
    let mut phone = f.phone().await;
    let response = phone
        .sender
        .send_request(request("/api/health", Some(TEST_TOKEN), full("")))
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    bytes(response).await;
    let response = phone
        .sender
        .send_request(request(
            "/api/health",
            Some("test-only-new-db-value"),
            full(""),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    bytes(response).await;
    drop(db);
    drop(phone);
    f.shutdown().await;
}

#[tokio::test]
async fn dropping_agent_future_closes_its_mux_instead_of_leaving_an_orphan() {
    let mut f = Fixture::start(|_| {}).await;
    f.agent_task.abort();
    assert!(timeout(Duration::from_secs(1), &mut f.agent_task)
        .await
        .unwrap()
        .unwrap_err()
        .is_cancelled());
    f.agent_task = tokio::spawn(async {});
    timeout(Duration::from_secs(2), async {
        loop {
            let mut socket = tokio::net::TcpStream::connect(f.relay_addr).await.unwrap();
            socket.write_all(b"GET /__codeg_relay/v1/health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").await.unwrap();
            let mut reply = String::new(); socket.read_to_string(&mut reply).await.unwrap();
            let (_, body) = reply.split_once("\r\n\r\n").unwrap();
            let body: serde_json::Value = serde_json::from_str(body).unwrap();
            if body["agent_connected"] == false { break; }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.expect("orphaned agent mux still connected after its owner was dropped");
    f.shutdown().await;
}
