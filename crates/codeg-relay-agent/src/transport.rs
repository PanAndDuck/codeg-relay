//! WSS handshake and mux adapter ported from the E2EE-a home relay transport.
use crate::{config::Config, secrets, tls};
use codeg_tunnel::mux::MuxMessage;
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use std::{
    io,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::time::Instant;
use tokio_tungstenite::{
    tungstenite::{self, client::IntoClientRequest, protocol::WebSocketConfig, Message},
    WebSocketStream,
};

pub const PROTOCOL: &str = "codeg-relay.v1";
pub struct ConnectError {
    pub code: &'static str,
    pub message: &'static str,
}
fn error(code: &'static str, message: &'static str) -> ConnectError {
    ConnectError { code, message }
}

pub async fn connect(config: &Config) -> Result<WebSocketStream<tls::Socket>, ConnectError> {
    tokio::time::timeout(Duration::from_secs(10), connect_inner(config))
        .await
        .unwrap_or_else(|_| Err(error("timeout", "relay connection timed out")))
}
async fn connect_inner(config: &Config) -> Result<WebSocketStream<tls::Socket>, ConnectError> {
    let key = secrets::read_private(&config.agent_key_file)
        .map_err(|_| error("config", "cannot read private agent key file"))?;
    let key = key.trim_end_matches(['\r', '\n']);
    if key.len() < 16 || key.len() > 4096 || key.chars().any(char::is_whitespace) {
        return Err(error(
            "config",
            "agent key must contain 16..4096 non-whitespace bytes",
        ));
    }
    let url = config
        .agent_url()
        .map_err(|_| error("config", "invalid relay URL"))?;
    // No HTTP bytes/credentials are sent until the leaf pin and handshake
    // signatures have been verified. No legacy ALPN/plaintext fallback.
    let stream = tls::connect(
        &url,
        Some(&config.relay_fingerprint),
        &[b"codeg-relay-agent/1"],
    )
    .await
    .map_err(|_| {
        error(
            "unreachable",
            "relay TCP/TLS connection or certificate pin failed",
        )
    })?;
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|_| error("protocol", "cannot create relay request"))?;
    let mut authorization = format!("Bearer {key}")
        .parse::<http::HeaderValue>()
        .map_err(|_| error("config", "invalid agent key"))?;
    authorization.set_sensitive(true);
    request.headers_mut().insert("authorization", authorization);
    request
        .headers_mut()
        .insert("sec-websocket-protocol", PROTOCOL.parse().unwrap());
    request.headers_mut().insert(
        "user-agent",
        concat!("codeg/", env!("CARGO_PKG_VERSION"))
            .parse()
            .unwrap(),
    );
    let ws_config = WebSocketConfig::default()
        .read_buffer_size(4096)
        .write_buffer_size(0)
        .max_write_buffer_size(128 * 1024)
        .max_message_size(Some(32773))
        .max_frame_size(Some(32773));
    let (socket, response) =
        tokio_tungstenite::client_async_with_config(request, stream, Some(ws_config))
            .await
            .map_err(|e| match e {
                tungstenite::Error::Http(r) if matches!(r.status().as_u16(), 401 | 403) => {
                    error("auth_rejected", "relay rejected the agent key")
                }
                _ => error("protocol", "relay WebSocket handshake failed"),
            })?;
    if response
        .headers()
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
        != Some(PROTOCOL)
    {
        return Err(error("protocol", "relay did not negotiate codeg-relay.v1"));
    }
    Ok(socket)
}

#[derive(Default)]
pub struct Observations {
    ping: Option<(Vec<u8>, Instant)>,
    pub rtt_ms: Option<u64>,
    pub replaced: bool,
}

pub fn adapter<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send>(
    socket: WebSocketStream<S>,
    observations: Arc<Mutex<Observations>>,
) -> impl Stream<Item = io::Result<MuxMessage>> + Sink<MuxMessage, Error = tungstenite::Error> + Send
{
    let sent = observations.clone();
    socket
        .with(move |message: MuxMessage| {
            futures_util::future::ready(Ok(match message {
                MuxMessage::Binary(v) => Message::Binary(v.into()),
                MuxMessage::Ping(v) => {
                    sent.lock().unwrap().ping = Some((v.clone(), Instant::now()));
                    Message::Ping(v.into())
                }
                MuxMessage::Pong(v) => Message::Pong(v.into()),
                MuxMessage::Close => Message::Close(None),
            }))
        })
        .map(move |message| {
            Ok(
                match message.map_err(|_| io::Error::other("relay transport failed"))? {
                    Message::Binary(v) => MuxMessage::Binary(v.to_vec()),
                    Message::Ping(v) => MuxMessage::Ping(v.to_vec()),
                    Message::Pong(v) => {
                        let mut observed = observations.lock().unwrap();
                        if let Some((payload, at)) = &observed.ping {
                            if payload.as_slice() == v.as_ref() {
                                observed.rtt_ms = Some(at.elapsed().as_millis() as u64);
                                observed.ping = None;
                            }
                        }
                        MuxMessage::Pong(v.to_vec())
                    }
                    Message::Close(frame) => {
                        observations.lock().unwrap().replaced =
                            frame.is_some_and(|v| u16::from(v.code) == 4001);
                        MuxMessage::Close
                    }
                    _ => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "expected binary relay frame",
                        ))
                    }
                },
            )
        })
}
