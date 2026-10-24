use crate::{
    classifier::{self, Kind},
    config::Serve,
    limits::{AuthLimiter, Connections},
    protocol::{self, PrefixedStream},
    websocket::{self, Stop},
};
use codeg_tunnel::mux::{self, MuxConfig, MuxHandle, MuxRole};
use sha2::{Digest, Sha256};
use std::{
    io,
    net::SocketAddr,
    pin::Pin,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpListener, TcpStream},
    sync::oneshot,
    task::{JoinHandle, JoinSet},
    time::timeout,
};
use tokio_tungstenite::{
    accept_hdr_async_with_config,
    tungstenite::{
        handshake::server::{Request, Response},
        protocol::WebSocketConfig,
    },
};

const DEADLINE: Duration = Duration::from_secs(10);
trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}
type Socket = Box<dyn Io>;
struct Active {
    id: u64,
    handle: MuxHandle,
    stop: oneshot::Sender<Stop>,
}
#[derive(Default)]
struct Session {
    active: Option<Active>,
    stopping: bool,
    next_id: u64,
}
struct State {
    session: Mutex<Session>,
    auth: Mutex<AuthLimiter>,
    key_hash: [u8; 32],
}

pub async fn serve(options: Serve) -> anyhow::Result<()> {
    let listener = TcpListener::bind(options.listen).await?;
    serve_with_listener(options, listener, shutdown_signal()).await
}

/// Serve an owned listener until shutdown, then join all connections and mux
/// workers. Supervisors and in-process consumers use the same server as the CLI.
pub async fn serve_with_listener(
    options: Serve,
    listener: TcpListener,
    shutdown: impl std::future::Future<Output = ()>,
) -> anyhow::Result<()> {
    let key = options.agent_key()?;
    let key_hash = Sha256::digest(key.as_bytes()).into();
    drop(key);
    let (tls, fingerprint) = crate::tls::configure(&options)?;
    // The unconditional banner also supports supervisors with RUST_LOG=error.
    println!(
        "codeg-relay version={} listen={} fingerprint={fingerprint}",
        env!("CARGO_PKG_VERSION"),
        listener.local_addr()?
    );
    let state = Arc::new(State {
        session: Mutex::default(),
        auth: Mutex::default(),
        key_hash,
    });
    let connections = Connections::new(
        options.max_conns as usize,
        options.max_conns_per_ip as usize,
    );
    let mut tasks = JoinSet::new();
    let signal = shutdown;
    tokio::pin!(signal);
    let result = loop {
        tokio::select! {
            biased;
            _ = &mut signal => break Ok(()),
            Some(_) = tasks.join_next(), if !tasks.is_empty() => {},
            accepted = listener.accept() => {
                let (socket, peer) = match accepted { Ok(pair) => pair, Err(error) => break Err(error) };
                let mut log = ConnectionLog::new(peer);
                if configure_tcp(&socket).is_err() { log.reason = "tcp_configuration_failed"; continue; }
                let Some(permit) = connections.acquire(peer.ip()) else {
                    log.reason = "connection_limit";
                    continue;
                };
                let tls = tls.clone(); let state = state.clone();
                tasks.spawn(async move {
                    let _permit = permit;
                    let mut socket = Metered { socket: Box::new(socket), counts: log.counts.clone() };
                    let (kind, prefix) = match classifier::classify(&mut socket).await {
                        Ok(value) => value,
                        Err(_) => { log.set_reason("invalid_or_timed_out_hello"); return; }
                    };
                    log.classification = match kind {
                        Kind::Agent => "agent", Kind::Client => "client", Kind::Health => "health",
                    };
                    match kind {
                        Kind::Client => passthrough(socket, prefix, peer, state, &mut log).await,
                        Kind::Health => health(PrefixedStream::new(prefix, socket), state, &mut log).await,
                        Kind::Agent => {
                            let Some(tls) = tls else { log.set_reason("agent_tls_unavailable"); return; };
                            let socket = match timeout(DEADLINE, tls.accept(PrefixedStream::new(prefix, socket))).await {
                                Ok(Ok(socket)) => Box::new(socket) as Socket,
                                _ => { log.set_reason("tls_failed_or_timeout"); return; }
                            };
                            handle_agent(socket, peer, state, &mut log).await;
                        }
                    }
                });
            }
        }
    };
    drop(listener);
    {
        let mut session = state.session.lock().unwrap();
        session.stopping = true;
        if let Some(agent) = session.active.take() {
            let _ = agent.stop.send(Stop::Shutdown);
        }
    }
    if timeout(Duration::from_secs(5), async {
        while tasks.join_next().await.is_some() {}
    })
    .await
    .is_err()
    {
        tasks.shutdown().await;
    }
    result?;
    Ok(())
}

fn configure_tcp(stream: &TcpStream) -> io::Result<()> {
    stream.set_nodelay(true)?;
    socket2::SockRef::from(stream).set_tcp_keepalive(
        &socket2::TcpKeepalive::new()
            .with_time(Duration::from_secs(60))
            .with_interval(Duration::from_secs(15)),
    )
}
async fn health<S: AsyncRead + AsyncWrite + Unpin>(
    mut socket: S,
    state: Arc<State>,
    log: &mut ConnectionLog,
) {
    let Ok(Ok(head)) = timeout(DEADLINE, protocol::read_head(&mut socket, &state.key_hash)).await
    else {
        log.reason = "invalid_or_timed_out_head";
        return;
    };
    if head.path == protocol::HEALTH_PATH && head.method == "GET" {
        let connected = state
            .session
            .lock()
            .unwrap()
            .active
            .as_ref()
            .is_some_and(|a| a.handle.stats().connected);
        let body = serde_json::json!({"status":"ok", "version":env!("CARGO_PKG_VERSION"), "mode":"e2ee", "agent_connected":connected});
        log.reason = "health";
        let _ = protocol::json(&mut socket, 200, &body.to_string()).await;
    } else {
        log.reason = "relay_route_not_found";
        let _ = protocol::json(&mut socket, 404, r#"{"code":"relay_not_found"}"#).await;
    }
}

async fn handle_agent(
    mut socket: Socket,
    peer: SocketAddr,
    state: Arc<State>,
    log: &mut ConnectionLog,
) {
    let head = match timeout(DEADLINE, protocol::read_head(&mut socket, &state.key_hash)).await {
        Ok(Ok(head)) => head,
        error => {
            let status = match error {
                Ok(Err(status)) => status,
                _ => 408,
            };
            log.reason = "invalid_or_timed_out_head";
            let _ =
                protocol::json(&mut socket, status, r#"{"code":"relay_invalid_request"}"#).await;
            return;
        }
    };
    if head.path == protocol::AGENT_PATH {
        let status = state.auth.lock().unwrap().check(peer.ip(), head.authorized);
        if status != 200 {
            log.reason = "agent_auth_rejected";
            let _ = protocol::json(
                &mut socket,
                status,
                r#"{"code":"relay_agent_auth_rejected"}"#,
            )
            .await;
            return;
        }
        if head.method != "GET" || !head.upgrade {
            log.reason = "agent_upgrade_invalid";
            let _ = protocol::json(&mut socket, 400, r#"{"code":"relay_invalid_upgrade"}"#).await;
            return;
        }
        let config = WebSocketConfig::default()
            .read_buffer_size(4096)
            .write_buffer_size(0)
            .max_write_buffer_size(128 * 1024)
            .max_message_size(Some(32 * 1024 + 5))
            .max_frame_size(Some(32 * 1024 + 5));
        let handshake = accept_hdr_async_with_config(
            PrefixedStream::new(head.bytes, socket),
            echo_protocol,
            Some(config),
        );
        match timeout(DEADLINE, handshake).await {
            Ok(Ok(ws)) => agent(ws, state, peer, log).await,
            _ => log.reason = "agent_handshake_failed",
        }
        return;
    }
    log.reason = "relay_route_not_found";
    let _ = protocol::json(&mut socket, 404, r#"{"code":"relay_not_found"}"#).await;
}

async fn passthrough(
    mut socket: Metered,
    prefix: Vec<u8>,
    peer: SocketAddr,
    state: Arc<State>,
    log: &mut ConnectionLog,
) {
    let handle = state
        .session
        .lock()
        .unwrap()
        .active
        .as_ref()
        .filter(|a| a.handle.stats().connected)
        .map(|a| a.handle.clone());
    let Some(handle) = handle else {
        log.reason = "agent_offline";
        return;
    };
    let mut tunnel = match handle.open(&peer.ip().to_string(), peer.port()).await {
        Ok(stream) => stream,
        Err(_) => {
            log.reason = "stream_open_failed";
            return;
        }
    };
    log.reason = if crate::splice::copy(&mut socket, &mut tunnel, &prefix)
        .await
        .is_ok()
    {
        "eof"
    } else {
        "stream_io_failed"
    };
}

// Tungstenite's callback fixes the unboxed ErrorResponse signature.
#[allow(clippy::result_large_err)]
fn echo_protocol(
    _: &Request,
    mut response: Response,
) -> Result<Response, tokio_tungstenite::tungstenite::handshake::server::ErrorResponse> {
    response.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        protocol::SUBPROTOCOL.parse().unwrap(),
    );
    Ok(response)
}

struct AgentTasks {
    handle: MuxHandle,
    mux: JoinHandle<mux::MuxEnd>,
    bridge: JoinHandle<()>,
}
impl Drop for AgentTasks {
    fn drop(&mut self) {
        self.handle.close();
        self.mux.abort();
        self.bridge.abort();
    }
}
async fn agent<S>(
    ws: tokio_tungstenite::WebSocketStream<S>,
    state: Arc<State>,
    peer: SocketAddr,
    log: &mut ConnectionLog,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (transport, bridge_stop, bridge) = websocket::bridge(ws);
    let (handle, _, mux) = mux::start(
        transport,
        MuxConfig {
            role: MuxRole::Opener,
            ..Default::default()
        },
    );
    let mut tasks = AgentTasks {
        handle: handle.clone(),
        mux,
        bridge,
    };
    let (stop, stopped) = oneshot::channel();
    let id = {
        let mut session = state.session.lock().unwrap();
        if session.stopping {
            let _ = bridge_stop.send(Stop::Shutdown);
            return;
        }
        session.next_id += 1;
        let id = session.next_id;
        if let Some(old) = session.active.replace(Active {
            id,
            handle: handle.clone(),
            stop,
        }) {
            tracing::info!(ip = %peer.ip(), old_session = old.id, session = id, "agent replaced");
            let _ = old.stop.send(Stop::Replaced);
        }
        id
    };
    tracing::info!(ip = %peer.ip(), session = id, "agent connected");
    let (stop, mux_done) = tokio::select! {
        biased;
        stop = stopped => (stop.unwrap_or(Stop::Shutdown), false),
        _ = &mut tasks.mux => (Stop::Shutdown, true),
    };
    // Send the out-of-band close BEFORE dropping the mux transport. The bridge's
    // biased control branch ensures 4001 is not replaced by its generic EOF close.
    let _ = bridge_stop.send(stop);
    handle.close();
    if !mux_done {
        let _ = (&mut tasks.mux).await;
    }
    let _ = (&mut tasks.bridge).await;
    {
        let mut session = state.session.lock().unwrap();
        if session.active.as_ref().is_some_and(|a| a.id == id) {
            session.active = None;
        }
    }
    log.reason = match stop {
        Stop::Replaced => "agent_replaced",
        Stop::Shutdown => "agent_disconnected",
    };
    tracing::info!(ip = %peer.ip(), session = id, "agent disconnected");
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler");
        tokio::select! { _ = term.recv() => {}, _ = tokio::signal::ctrl_c() => {} }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[derive(Default)]
struct Counts {
    up: AtomicU64,
    down: AtomicU64,
}
struct Metered {
    socket: Socket,
    counts: Arc<Counts>,
}
impl AsyncRead for Metered {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let result = Pin::new(&mut self.socket).poll_read(cx, buf);
        self.counts
            .up
            .fetch_add((buf.filled().len() - before) as u64, Ordering::Relaxed);
        result
    }
}
impl AsyncWrite for Metered {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        let result = Pin::new(&mut self.socket).poll_write(cx, bytes);
        if let Poll::Ready(Ok(n)) = result {
            self.counts.down.fetch_add(n as u64, Ordering::Relaxed);
        }
        result
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.socket).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.socket).poll_shutdown(cx)
    }
}
struct ConnectionLog {
    peer: SocketAddr,
    started: Instant,
    counts: Arc<Counts>,
    classification: &'static str,
    reason: &'static str,
}
impl ConnectionLog {
    fn set_reason(&mut self, reason: &'static str) {
        self.reason = reason;
    }
    fn new(peer: SocketAddr) -> Self {
        Self {
            peer,
            started: Instant::now(),
            counts: Arc::default(),
            classification: "unknown",
            reason: "shutdown",
        }
    }
}
impl Drop for ConnectionLog {
    fn drop(&mut self) {
        tracing::info!(ip = %self.peer.ip(), classification = self.classification,
            bytes_up = self.counts.up.load(Ordering::Relaxed), bytes_down = self.counts.down.load(Ordering::Relaxed),
            duration_ms = self.started.elapsed().as_millis(), reason = self.reason, "connection closed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    #[tokio::test(start_paused = true)]
    async fn offline_passthrough_closes_without_http_or_waiting() {
        let (socket, mut client) = tokio::io::duplex(64);
        let peer = "192.0.2.1:12345".parse().unwrap();
        let mut log = ConnectionLog::new(peer);
        let socket = Metered {
            socket: Box::new(socket),
            counts: log.counts.clone(),
        };
        let state = Arc::new(State {
            session: Mutex::default(),
            auth: Mutex::default(),
            key_hash: [0; 32],
        });
        let start = tokio::time::Instant::now();
        passthrough(socket, vec![0x16, 3, 1], peer, state, &mut log).await;
        let mut bytes = Vec::new();
        client.read_to_end(&mut bytes).await.unwrap();
        assert!(bytes.is_empty());
        assert_eq!(start.elapsed(), Duration::ZERO);
        assert_eq!(log.reason, "agent_offline");
    }
}
