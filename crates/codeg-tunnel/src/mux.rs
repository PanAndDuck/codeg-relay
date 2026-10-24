//! Transport-independent, single-opener multiplexing over the tunnel protocol.
//!
//! Peer DATA is charged against receive credit **before** entering a pump's
//! unbounded event channel. Credit is returned only when the writer emits a
//! WINDOW from that pump. Nonempty DATA events therefore hold at most one
//! window per stream across the dispatcher and pump writer queues. WINDOW
//! events must acknowledge bytes actually sent, empty DATA/WINDOW are ignored,
//! and EOF/CLOSE occur once. Even one-byte frames cannot create an unbounded
//! number of queued events. The duplex adds a fixed 32 KiB per direction.
//! Pending requests, commands and transport output are independently bounded.

mod session;

use crate::frame::{CloseCode, Frame};
use futures_util::{Sink, Stream};
use std::{
    fmt, io,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc,
    },
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, DuplexStream, ReadBuf},
    sync::{mpsc, oneshot, Notify},
    task::JoinHandle,
};
use tokio_util::sync::{CancellationToken, WaitForCancellationFutureOwned};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MuxMessage {
    Binary(Vec<u8>),
    Ping(Vec<u8>),
    Pong(Vec<u8>),
    Close,
}

/// Exactly one side opens streams; this is independent of who dialled the WS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MuxRole {
    Opener,
    Acceptor,
}

#[derive(Debug, Clone)]
pub struct MuxConfig {
    pub role: MuxRole,
    pub ping_interval: Duration,
    pub idle_timeout: Duration,
    pub outgoing_queue: usize,
    pub open_timeout: Duration,
    pub max_streams: usize,
}

impl Default for MuxConfig {
    fn default() -> Self {
        Self {
            role: MuxRole::Opener,
            ping_interval: Duration::from_secs(15),
            idle_timeout: Duration::from_secs(45),
            outgoing_queue: 256,
            open_timeout: Duration::from_secs(10),
            max_streams: 256,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MuxStats {
    /// Includes handshakes and pumps still draining a graceful CLOSE.
    pub active_streams: usize,
    pub total_streams: u64,
    pub connected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MuxEnd {
    Closed,
    PeerClosed,
    Transport(String),
    Protocol(String),
    IdleTimeout,
    WriteTimeout,
    InvalidConfig(&'static str),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    WrongRole,
    InvalidDestination,
    TooManyStreams,
    IdExhausted,
    Timeout,
    Closed,
    Rejected { code: CloseCode, message: String },
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongRole => f.write_str("only the Opener can open streams"),
            Self::InvalidDestination => f.write_str("empty/oversized host or port zero"),
            Self::TooManyStreams => f.write_str("stream limit reached"),
            Self::IdExhausted => f.write_str("stream ids exhausted; start a new session"),
            Self::Timeout => f.write_str("stream open timed out"),
            Self::Closed => f.write_str("session or request closed"),
            Self::Rejected { code, message } => write!(f, "stream rejected ({code:?}): {message}"),
        }
    }
}
impl std::error::Error for OpenError {}

struct Shared {
    stop: CancellationToken,
    pending_changed: Notify,
    active: AtomicUsize,
    total: AtomicU64,
}

struct PendingGuard {
    cancelled: CancellationToken,
    shared: Arc<Shared>,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.cancelled.cancel();
        self.shared.pending_changed.notify_one();
    }
}

type Reply = oneshot::Sender<Result<TunnelStream, OpenError>>;
enum Command {
    Open {
        host: String,
        port: u16,
        reply: Reply,
        cancelled: CancellationToken,
    },
    Accept {
        id: u32,
        reply: Reply,
    },
    Reject {
        id: u32,
        code: CloseCode,
        message: String,
        reply: oneshot::Sender<Result<(), OpenError>>,
    },
}

#[derive(Clone)]
pub struct MuxHandle {
    commands: mpsc::Sender<Command>,
    shared: Arc<Shared>,
    role: MuxRole,
    open_timeout: Duration,
}

impl MuxHandle {
    /// Wait for OPENED or CLOSE. Cancelling this future cancels the request.
    pub async fn open(&self, host: &str, port: u16) -> Result<TunnelStream, OpenError> {
        if self.role != MuxRole::Opener {
            return Err(OpenError::WrongRole);
        }
        if Frame::open(1, host, port).is_none() {
            return Err(OpenError::InvalidDestination);
        }
        let cancelled = CancellationToken::new();
        let _guard = PendingGuard {
            cancelled: cancelled.clone(),
            shared: self.shared.clone(),
        };
        let (reply, response) = oneshot::channel();
        let command = Command::Open {
            host: host.into(),
            port,
            reply,
            cancelled,
        };
        tokio::time::timeout(self.open_timeout, async {
            self.submit(command).await?;
            response.await.unwrap_or(Err(OpenError::Closed))
        })
        .await
        .unwrap_or(Err(OpenError::Timeout))
    }

    pub fn stats(&self) -> MuxStats {
        MuxStats {
            active_streams: self.shared.active.load(Ordering::Acquire),
            total_streams: self.shared.total.load(Ordering::Acquire),
            connected: !self.shared.stop.is_cancelled(),
        }
    }

    /// Request shutdown. Await the JoinHandle returned by start for completion.
    pub fn close(&self) {
        self.shared.stop.cancel();
    }

    async fn submit(&self, command: Command) -> Result<(), OpenError> {
        tokio::select! {
            biased;
            _ = self.shared.stop.cancelled() => Err(OpenError::Closed),
            result = self.commands.send(command) => result.map_err(|_| OpenError::Closed),
        }
    }
}

pub struct MuxIncoming {
    requests: mpsc::Receiver<IncomingStream>,
    stop: CancellationToken,
}

impl MuxIncoming {
    /// None after session shutdown (or immediately on the Opener side).
    pub async fn accept(&mut self) -> Option<IncomingStream> {
        loop {
            let request = tokio::select! {
                biased;
                _ = self.stop.cancelled() => return None,
                request = self.requests.recv() => request?,
            };
            if !request.cancelled.is_cancelled() {
                return Some(request);
            }
        }
    }
}

pub struct IncomingStream {
    id: u32,
    host: String,
    port: u16,
    handle: MuxHandle,
    cancelled: CancellationToken,
}

impl IncomingStream {
    pub fn id(&self) -> u32 {
        self.id
    }
    pub fn host(&self) -> &str {
        &self.host
    }
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Sends OPENED before returning usable I/O. Does not dial host:port.
    pub async fn accept(self) -> Result<TunnelStream, OpenError> {
        if self.cancelled.is_cancelled() {
            return Err(OpenError::Closed);
        }
        let (reply, response) = oneshot::channel();
        self.handle
            .submit(Command::Accept { id: self.id, reply })
            .await?;
        response.await.unwrap_or(Err(OpenError::Closed))
    }

    pub async fn reject(self, code: CloseCode, msg: &str) -> Result<(), OpenError> {
        if self.cancelled.is_cancelled() {
            return Err(OpenError::Closed);
        }
        let (reply, response) = oneshot::channel();
        self.handle
            .submit(Command::Reject {
                id: self.id,
                code,
                message: msg.into(),
                reply,
            })
            .await?;
        response.await.unwrap_or(Err(OpenError::Closed))
    }
}

impl Drop for IncomingStream {
    fn drop(&mut self) {
        self.cancelled.cancel();
        self.handle.shared.pending_changed.notify_one();
    }
}

/// In-memory stream backed by the unchanged credit-controlled pump.
/// shutdown() sends EOF; dropping the stream cancels both directions.
pub struct TunnelStream {
    id: u32,
    io: CancelIo,
    written: u64,
    progress: Arc<WriteProgress>,
}

#[derive(Default)]
struct WriteProgress {
    bytes: AtomicU64,
    eof: AtomicBool,
    closed: AtomicBool,
    waker: futures_util::task::AtomicWaker,
}

impl WriteProgress {
    fn poll(&self, cx: &mut Context<'_>, written: u64, shutdown: bool) -> Poll<io::Result<()>> {
        self.waker.register(cx.waker());
        if if shutdown {
            self.eof.load(Ordering::Acquire)
        } else {
            self.bytes.load(Ordering::Acquire) >= written
        } {
            Poll::Ready(Ok(()))
        } else if self.closed.load(Ordering::Acquire) {
            Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()))
        } else {
            Poll::Pending
        }
    }
}

impl fmt::Debug for TunnelStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TunnelStream")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl TunnelStream {
    pub fn id(&self) -> u32 {
        self.id
    }
}
impl Drop for TunnelStream {
    fn drop(&mut self) {
        self.io.stop.cancel();
    }
}

struct CancelIo {
    inner: DuplexStream,
    stop: CancellationToken,
    read_cancelled: Pin<Box<WaitForCancellationFutureOwned>>,
    write_cancelled: Pin<Box<WaitForCancellationFutureOwned>>,
}

impl CancelIo {
    fn new(inner: DuplexStream, stop: CancellationToken) -> Self {
        Self {
            inner,
            read_cancelled: Box::pin(stop.clone().cancelled_owned()),
            write_cancelled: Box::pin(stop.clone().cancelled_owned()),
            stop,
        }
    }
    fn check(&mut self, cx: &mut Context<'_>, read: bool) -> io::Result<()> {
        use std::future::Future;
        // Split read/write halves can be polled by different tasks. A single
        // cancellation future would retain only the most recent task's waker.
        let cancelled = if read {
            &mut self.read_cancelled
        } else {
            &mut self.write_cancelled
        };
        if cancelled.as_mut().poll(cx).is_ready() {
            Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "tunnel closed",
            ))
        } else {
            Ok(())
        }
    }
}

impl AsyncRead for CancelIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.check(cx, true)?;
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}
impl AsyncWrite for CancelIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.check(cx, false)?;
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.check(cx, false)?;
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.check(cx, false)?;
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}
impl AsyncRead for TunnelStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_read(cx, buf)
    }
}
impl AsyncWrite for TunnelStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match Pin::new(&mut self.io).poll_write(cx, buf) {
            Poll::Ready(Ok(n)) => {
                self.written += n as u64;
                Poll::Ready(Ok(n))
            }
            other => other,
        }
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.io.check(cx, false)?;
        self.progress.poll(cx, self.written, false)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match Pin::new(&mut self.io).poll_shutdown(cx) {
            Poll::Ready(Ok(())) => self.progress.poll(cx, self.written, true),
            other => other,
        }
    }
}

/// Starts on the current Tokio runtime. The transport must enforce a bounded
/// WebSocket message size; binary frames over 32 KiB + 5 bytes are rejected.
/// No Unpin bound on T: adapters built with async Stream/Sink combinators work.
/// Invalid zero capacities/durations return MuxEnd::InvalidConfig via the task.
pub fn start<T, E>(transport: T, cfg: MuxConfig) -> (MuxHandle, MuxIncoming, JoinHandle<MuxEnd>)
where
    T: Stream<Item = Result<MuxMessage, E>> + Sink<MuxMessage> + Send + 'static,
    E: fmt::Display + Send + 'static,
    T::Error: fmt::Display + Send + 'static,
{
    let (commands, receiver) = mpsc::channel(cfg.outgoing_queue.max(1));
    let (requests, incoming) = mpsc::channel(cfg.max_streams.max(1));
    let shared = Arc::new(Shared {
        stop: CancellationToken::new(),
        pending_changed: Notify::new(),
        active: AtomicUsize::new(0),
        total: AtomicU64::new(0),
    });
    let handle = MuxHandle {
        commands,
        shared: shared.clone(),
        role: cfg.role,
        open_timeout: cfg.open_timeout,
    };
    let incoming = MuxIncoming {
        requests: incoming,
        stop: shared.stop.clone(),
    };
    // Construct outside the spawned future: even an abort before its first
    // poll must publish the disconnected state and release incoming waiters.
    let guard = shared.stop.clone().drop_guard();
    let session_handle = handle.clone();
    let task = tokio::spawn(async move {
        let _guard = guard;
        session::run(transport, cfg, session_handle, receiver, requests).await
    });
    (handle, incoming, task)
}
