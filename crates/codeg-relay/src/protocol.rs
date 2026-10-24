use sha2::{Digest, Sha256};
use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use subtle::ConstantTimeEq;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};

pub const AGENT_PATH: &str = "/__codeg_relay/v1/agent";
pub const HEALTH_PATH: &str = "/__codeg_relay/v1/health";
pub const SUBPROTOCOL: &str = "codeg-relay.v1";
pub const HEAD_LIMIT: usize = 64 * 1024;

pub struct Head {
    pub bytes: Vec<u8>,
    pub method: String,
    pub path: String,
    pub authorized: bool,
    pub upgrade: bool,
}

pub async fn read_head<S: AsyncRead + Unpin>(
    stream: &mut S,
    key_hash: &[u8; 32],
) -> Result<Head, u16> {
    let mut bytes = Vec::with_capacity(4096);
    loop {
        let start = bytes.len().saturating_sub(3);
        let mut chunk = [0u8; 4096];
        let remaining = (HEAD_LIMIT - bytes.len()).min(chunk.len());
        if remaining == 0 {
            return Err(431);
        }
        let n = stream
            .read(&mut chunk[..remaining])
            .await
            .map_err(|_| 400u16)?;
        if n == 0 {
            return Err(400);
        }
        bytes.extend_from_slice(&chunk[..n]);
        if bytes[start..].windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
    let mut headers = [httparse::EMPTY_HEADER; 128];
    let mut req = httparse::Request::new(&mut headers);
    if !req.parse(&bytes).map_err(|_| 400u16)?.is_complete() {
        return Err(400);
    }
    if req.version != Some(1) {
        return Err(400);
    }
    let method = req.method.ok_or(400u16)?.to_owned();
    let path = req
        .path
        .ok_or(400u16)?
        .split('?')
        .next()
        .unwrap_or("/")
        .to_owned();
    let values = |name: &'static str| {
        req.headers
            .iter()
            .filter(move |h| h.name.eq_ignore_ascii_case(name))
            .map(|h| h.value)
    };
    let mut auth = values("authorization");
    let candidate = auth.next().and_then(|v| v.strip_prefix(b"Bearer "));
    let authorized = candidate
        .is_some_and(|v| bool::from(Sha256::digest(v).as_slice().ct_eq(key_hash)))
        && auth.next().is_none();
    let contains = |name: &'static str, token: &str| {
        values(name).any(|v| {
            std::str::from_utf8(v)
                .is_ok_and(|s| s.split(',').any(|s| s.trim().eq_ignore_ascii_case(token)))
        })
    };
    let protocol = values("sec-websocket-protocol").any(|v| {
        std::str::from_utf8(v).is_ok_and(|s| s.split(',').any(|s| s.trim() == SUBPROTOCOL))
    });
    let upgrade = contains("upgrade", "websocket") && contains("connection", "upgrade") && protocol;
    Ok(Head {
        bytes,
        method,
        path,
        authorized,
        upgrade,
    })
}

pub async fn json<S: AsyncWrite + Unpin>(
    stream: &mut S,
    status: u16,
    body: &str,
) -> io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        408 => "Request Timeout",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Error",
    };
    let response = format!("HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
    tokio::time::timeout(Duration::from_secs(2), async {
        stream.write_all(response.as_bytes()).await?;
        stream.shutdown().await
    })
    .await
    .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))?
}

pub struct PrefixedStream<S> {
    prefix: std::io::Cursor<Vec<u8>>,
    stream: S,
}
impl<S> PrefixedStream<S> {
    pub fn new(prefix: Vec<u8>, stream: S) -> Self {
        Self {
            prefix: std::io::Cursor::new(prefix),
            stream,
        }
    }
}
impl<S: AsyncRead + Unpin> AsyncRead for PrefixedStream<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let pos = self.prefix.position() as usize;
        let bytes = &self.prefix.get_ref()[pos..];
        if !bytes.is_empty() {
            let n = bytes.len().min(buf.remaining());
            buf.put_slice(&bytes[..n]);
            self.prefix.set_position((pos + n) as u64);
            Poll::Ready(Ok(()))
        } else {
            Pin::new(&mut self.stream).poll_read(cx, buf)
        }
    }
}
impl<S: AsyncWrite + Unpin> AsyncWrite for PrefixedStream<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(cx, bytes)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}
