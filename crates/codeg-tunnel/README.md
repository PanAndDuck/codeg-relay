# codeg-tunnel

An edition-2021, unpublished library shared by the browser tunnel, the home
relay client and the public relay. It has no WebSocket, HTTP, DNS or TLS
dependency. `frame` and `pump` are copied unchanged from codeg v0.34.0;
`mux` manages sessions on any asynchronous `Stream + Sink` transport.

## Wire format

Each binary WebSocket message is one frame:

```text
[kind: u8][stream: u32 big endian][payload]
```

| Kind | Name | Payload |
| --- | --- | --- |
| 1 | OPEN | port: u16 BE, host byte length: u8, UTF-8 host |
| 2 | OPENED | empty |
| 3 | DATA | stream bytes, at most 32 KiB per message |
| 4 | WINDOW | credit: u32 BE |
| 5 | EOF | empty; half-close of sender's write direction |
| 6 | CLOSE | code: u8, UTF-8 reason (at most 256 bytes) |

Close codes are `Normal=0`, `Refused=1`, `Unreachable=2`, `NotAllowed=3`,
`Timeout=4`, `BadRequest=5`, `Protocol=6`, `Failed=7`.
Hosts contain 1–255 UTF-8 bytes and ports are nonzero. IPv6 literals have no
brackets. A malformed frame closes the session. Invalid stream state closes
that stream. Late frames for a removed stream are ignored, as in the existing
browser tunnel. The standalone frame decoder retains its original behavior;
the mux additionally checks message size and state before queueing events.

The browser constants remain `frame::TUNNEL_PROTOCOL = "codeg-tunnel"` and
`frame::TUNNEL_PATH = "/ws/browser-tunnel"`. The relay's WebSocket handshake
uses **`codeg-relay.v1`**, at `/__codeg_relay/v1/agent`; these are application
handshake settings, not a change to the frame encoding.

## Roles and lifecycle

Configure exactly one **Opener** and one **Acceptor**. The role is independent
of which side dialled the WebSocket. For the relay, the public server is the
Opener and home codeg is the Acceptor. Both use the same `start` function.
`MuxConfig::default()` is the Opener configuration.

IDs start at 1, increase by 2, and are never reused. The Acceptor rejects zero,
even and non-increasing IDs with `BadRequest`; it keeps one high-water mark,
not a growing set of retired IDs. ID exhaustion requires a fresh session.
`max_streams` includes pending handshakes and pumps still draining CLOSE.

`open()` waits for OPENED or CLOSE; its timeout includes command queueing.
Cancelling an open future or dropping an undecided IncomingStream cancels the
request. Incoming requests also expire after `open_timeout`. `accept()` queues
OPENED before permitting DATA and returns an error if the request/session is
already closed. Neither `accept()` nor `open()` performs DNS or TCP dialing.
In the relay, OPEN host/port mean the public client's IP and source port;
**The home agent must validate the IP literal** and serve the accepted stream in-process.

`TunnelStream` implements `AsyncRead + AsyncWrite + Unpin + Send`.
`flush()` waits until previously written DATA has been flushed to the transport;
`AsyncWriteExt::shutdown()` waits for the writer to transmit EOF and preserves
the read direction. Dropping after successful shutdown sends a normal CLOSE,
so preceding DATA still drains on the peer. Dropping without shutdown cancels
both directions. Session loss wakes reads/writes with
EOF or `ConnectionAborted`; pending opens fail. Buffered data may be discarded
on transport loss. A normal stream CLOSE drains preceding DATA.

Call `MuxHandle::close()` and await the returned session task to stop and join
all pumps. `close()` drops the transport promptly; it does not wait for a
WebSocket closing handshake or send an application-specific WS close code.
Aborting the task also cancels streams; pump workers finish asynchronously.
Dropping public handles alone is not a shutdown request. A handle clone can
be retained for stats or closing while another task accepts incoming streams.

## Public API

All mux types are under `codeg_tunnel::mux`; `CloseCode` is under `frame`.

```rust,ignore
pub enum MuxMessage { Binary(Vec<u8>), Ping(Vec<u8>), Pong(Vec<u8>), Close }
pub enum MuxRole { Opener, Acceptor }
pub struct MuxConfig {
    pub role: MuxRole,                 // Opener by default
    pub ping_interval: Duration,      // 15 s
    pub idle_timeout: Duration,       // 45 s
    pub outgoing_queue: usize,        // 256
    pub open_timeout: Duration,       // 10 s
    pub max_streams: usize,            // 256
}
pub fn start<T, E>(transport: T, cfg: MuxConfig)
    -> (MuxHandle, MuxIncoming, tokio::task::JoinHandle<MuxEnd>)
where
    T: futures_util::Stream<Item = Result<MuxMessage, E>>
        + futures_util::Sink<MuxMessage> + Send + 'static,
    E: std::fmt::Display + Send + 'static,
    T::Error: std::fmt::Display + Send + 'static;

impl MuxHandle { // Clone
    pub async fn open(&self, host: &str, port: u16) -> Result<TunnelStream, OpenError>;
    pub fn stats(&self) -> MuxStats;
    pub fn close(&self);
}
impl MuxIncoming {
    pub async fn accept(&mut self) -> Option<IncomingStream>;
}
impl IncomingStream {
    pub fn id(&self) -> u32;
    pub fn host(&self) -> &str;
    pub fn port(&self) -> u16;
    pub async fn accept(self) -> Result<TunnelStream, OpenError>;
    pub async fn reject(self, code: CloseCode, msg: &str) -> Result<(), OpenError>;
}
impl TunnelStream { pub fn id(&self) -> u32; }
pub struct MuxStats {
    pub active_streams: usize, // pending + active + draining
    pub total_streams: u64,   // admitted OPENs, including later rejections
    pub connected: bool,
}
pub enum OpenError {
    WrongRole, InvalidDestination, TooManyStreams, IdExhausted, Timeout, Closed,
    Rejected { code: CloseCode, message: String },
}
pub enum MuxEnd {
    Closed, PeerClosed, Transport(String), Protocol(String), IdleTimeout,
    WriteTimeout, InvalidConfig(&'static str),
}
```

Zero durations/capacities are invalid and return `MuxEnd::InvalidConfig`
through the session task. The task requires a running Tokio runtime. `start`
does not require `T: Unpin`, so async mapping combinators are supported.
`MuxIncoming::accept()` returns `None` on the Opener or after session shutdown.
`OpenError` implements `std::error::Error` and `Display`.

## Flow control and memory bounds

Every stream/direction starts with 256 KiB (`INITIAL_WINDOW`) credit. The
unchanged pump sends at most 32 KiB per DATA and waits for WINDOW when credit
runs out. WINDOW is generated only after the bytes have been written into the
downstream duplex. Its fixed capacity adds 32 KiB per direction; it does not
grow with traffic. Independent pump tasks keep a stalled reader from blocking
other streams, or its own opposite direction.

The mux debits receive credit **before** forwarding DATA to the pump's
`UnboundedSender<StreamEvent>`. Its writer restores credit when it takes a
pump WINDOW for transport. Across the event queue and the pump's writer queue,
unconsumed DATA therefore fits within one credit window. Nonempty DATA has at
least one byte, so even one-byte fragmentation has a finite event count
(up to 262,144, with allocation overhead). Empty DATA/WINDOW are ignored;
WINDOW must acknowledge actual sent bytes and cannot raise available credit
above the initial window. Thus queued WINDOW events are also bounded by
unacknowledged byte credit. Repeated EOF, DATA after EOF and excess WINDOW
close the stream; CLOSE is delivered once. This validation is necessary:
checking only inside an asynchronously scheduled pump would be too late.

Commands, frame output and Pong output each use a bounded `outgoing_queue`.
Incoming requests are bounded by `max_streams`; full/unavailable queues reject
new requests. Handshake deadlines are indexed by live stream ID and removed
synchronously on acceptance/closure, so rejected OPEN floods cannot accumulate
cancelled timer futures. Stream IDs use counters rather than tombstones.
Writer acknowledgements for retired pumps survive only until their queued
CLOSE is transmitted, bounded by active streams plus the outgoing queue.
No peer-fed queue grows without a bound. The transport adapter must also bound
WebSocket message assembly **before allocation**: configure a 32 KiB + 5 byte
message/frame limit (or another small finite limit), with compression disabled.
Mux checks binary/control sizes before decoding but cannot undo an allocation
already made by the transport. Per-event allocation overhead means 256 streams
with adversarial one-byte fragments can use substantially more than 64 MiB;
choose `max_streams` for the hosting process's memory budget.

## Heartbeat

Both sides emit Ping every 15 s and respond to Ping with an identical Pong.
Any received message resets a 45 s idle deadline. Config fields adjust these
values for tests/deployments. A stalled sink send is also limited to
`idle_timeout` and ends with `WriteTimeout`, even if incoming traffic remains
active. The watchdog and transport writer run independently of stream pumps.

## WebSocket adapters

Set subprotocol/auth/TLS and finite message limits before `start`. Preserve
Ping/Pong/Close; reject text messages. `MuxMessage::Close` discards WebSocket
close code/reason. If the home agent needs to distinguish replacement code 4001, inspect it
in the input mapper before mapping it to Close. Some WebSocket implementations
auto-answer Ping; an explicit same-payload Pong is harmless and preserves the
generic transport contract.

For **tokio-tungstenite 0.26**, using the dependencies already in codeg:

```rust,ignore
use codeg_tunnel::mux::{self, MuxConfig, MuxMessage, MuxRole};
use futures_util::{future, SinkExt, StreamExt};
use std::io;
use tokio_tungstenite::tungstenite::{Error, Message};

let transport = ws.with(|message: MuxMessage| {
    future::ready(Ok::<_, Error>(match message {
        MuxMessage::Binary(b) => Message::Binary(b.into()),
        MuxMessage::Ping(b) => Message::Ping(b.into()),
        MuxMessage::Pong(b) => Message::Pong(b.into()),
        MuxMessage::Close => Message::Close(None),
    }))
}).map(|message| -> io::Result<MuxMessage> {
    Ok(match message.map_err(io::Error::other)? {
        Message::Binary(b) => MuxMessage::Binary(b.to_vec()),
        Message::Ping(b) => MuxMessage::Ping(b.to_vec()),
        Message::Pong(b) => MuxMessage::Pong(b.to_vec()),
        Message::Close(_) => MuxMessage::Close,
        _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "expected binary WS")),
    })
});
let (handle, mut incoming, task) = mux::start(transport, MuxConfig {
    role: MuxRole::Acceptor, // home client dialled OUT but accepts streams
    ..MuxConfig::default()
});
```

For **axum 0.8** `extract::ws::WebSocket`, use the same combinators and mapping
with these changes: import `axum::extract::ws::Message`, use
`Ok::<_, axum::Error>` in `with`, and the unsupported input variant is
`Message::Text(_)` (there is no raw `Frame` variant). Binary/Ping/Pong use
`Bytes` in both libraries, hence `into()` outbound and `to_vec()` inbound.
Use `MuxRole::Opener` at the public relay and call
`handle.open(&client_addr.ip().to_string(), client_addr.port()).await`.

## Verification

```sh
cargo test --locked --manifest-path crates/codeg-tunnel/Cargo.toml
cargo clippy --locked --manifest-path crates/codeg-tunnel/Cargo.toml --all-targets -- -D warnings
```

The original 12 frame/pump tests are retained. Mux tests use bounded in-memory
transport pairs and include 5 MiB of deterministic pseudorandom data each way,
100 concurrent streams, half-close, stalled readers, transport loss, handshake
cancellation/limits, hostile control frames and paused-time heartbeat tests.
