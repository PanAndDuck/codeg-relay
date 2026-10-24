use super::*;
use crate::{
    frame::{INITIAL_WINDOW, MAX_CLOSE_MESSAGE, MAX_DATA_CHUNK},
    pump::{pump, PumpEnd, StreamEvent},
};
use futures_util::{stream::FuturesUnordered, SinkExt, StreamExt};
use std::{
    collections::HashMap,
    sync::{atomic::AtomicU32, Mutex},
};
use tokio::{
    sync::watch,
    time::{Instant, MissedTickBehavior},
};

struct Credit {
    receive: AtomicU32,
    send: AtomicU32,
    progress: Arc<WriteProgress>,
}

type Credits = Arc<Mutex<HashMap<u32, Arc<Credit>>>>;

/// Atomically replaces the credit with `f(current)`, or fails without writing
/// when `f` rejects it. Same semantics as `AtomicU32::fetch_update`, which Rust
/// 1.99 deprecates in favour of `try_update`; spelled out so the crate builds
/// warning-free on both older and newer toolchains.
fn update_credit(credit: &AtomicU32, f: impl Fn(u32) -> Option<u32>) -> Result<u32, u32> {
    let mut current = credit.load(Ordering::Acquire);
    loop {
        let next = f(current).ok_or(current)?;
        match credit.compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Acquire) {
            Ok(previous) => return Ok(previous),
            Err(actual) => current = actual,
        }
    }
}

enum Sent {
    Data(usize),
    Eof,
    Close,
}

enum Phase {
    Opening(Reply),
    Incoming,
    Open(mpsc::UnboundedSender<StreamEvent>),
    Draining,
}
struct Slot {
    phase: Phase,
    cancelled: CancellationToken,
    credit: Arc<Credit>,
    eof: bool,
}

struct Session {
    cfg: MuxConfig,
    handle: MuxHandle,
    slots: HashMap<u32, Slot>,
    credits: Credits,
    frames: mpsc::Sender<Frame>,
    requests: Option<mpsc::Sender<IncomingStream>>,
    workers: FuturesUnordered<JoinHandle<(u32, PumpEnd)>>,
    deadlines: HashMap<u32, Instant>,
    next_id: u64,
    last_peer_id: u32,
}

impl Drop for Session {
    fn drop(&mut self) {
        // JoinHandle::abort skips run's async cleanup. The cancellation guard
        // still lets detached pump workers reach their own cleanup, and the
        // public snapshot must not retain the now-unreachable stream slots.
        self.handle.shared.stop.cancel();
        self.handle.shared.active.store(0, Ordering::Release);
    }
}

pub(super) async fn run<T, E>(
    transport: T,
    cfg: MuxConfig,
    handle: MuxHandle,
    commands: mpsc::Receiver<Command>,
    requests: mpsc::Sender<IncomingStream>,
) -> MuxEnd
where
    T: Stream<Item = Result<MuxMessage, E>> + Sink<MuxMessage> + Send + 'static,
    E: fmt::Display + Send + 'static,
    T::Error: fmt::Display + Send + 'static,
{
    // Also cancels detached pump workers if the caller aborts this task.
    let stop = handle.shared.stop.clone();
    let _guard = stop.clone().drop_guard();
    if cfg.outgoing_queue == 0
        || cfg.max_streams == 0
        || cfg.ping_interval.is_zero()
        || cfg.idle_timeout.is_zero()
        || cfg.open_timeout.is_zero()
    {
        return MuxEnd::InvalidConfig("capacities and durations must be nonzero");
    }
    let (sink, source) = Box::pin(transport).split();
    let (frames, output) = mpsc::channel(cfg.outgoing_queue);
    let (controls, control_rx) = mpsc::channel(cfg.outgoing_queue);
    let (activity, activity_rx) = watch::channel(Instant::now());
    let credits = Arc::new(Mutex::new(HashMap::new()));
    let writer = write(sink, output, control_rx, credits.clone(), cfg.clone());
    let mut session = Session {
        requests: (cfg.role == MuxRole::Acceptor).then_some(requests),
        cfg,
        handle,
        slots: HashMap::new(),
        credits,
        frames,
        workers: FuturesUnordered::new(),
        deadlines: HashMap::new(),
        next_id: 1,
        last_peer_id: 0,
    };
    let end = {
        tokio::pin!(writer);
        tokio::select! {
            biased;
            _ = stop.cancelled() => MuxEnd::Closed,
            end = &mut writer => end,
            end = idle(activity_rx, session.cfg.idle_timeout) => end,
            end = session.read(source, commands, controls, activity) => end,
        }
        // The writer is dropped here, releasing its queue and the transport.
    };
    stop.cancel();
    session.slots.clear();
    session.requests.take();
    session.credits.lock().unwrap().clear();
    session.handle.shared.active.store(0, Ordering::Release);
    while session.workers.next().await.is_some() {}
    tracing::debug!(?end, "tunnel session ended");
    end
}

async fn idle(mut activity: watch::Receiver<Instant>, timeout: Duration) -> MuxEnd {
    loop {
        let deadline = *activity.borrow_and_update() + timeout;
        tokio::select! {
            biased;
            changed = activity.changed() => if changed.is_err() { return MuxEnd::PeerClosed; },
            _ = tokio::time::sleep_until(deadline) => return MuxEnd::IdleTimeout,
        }
    }
}

async fn write<S>(
    mut sink: S,
    mut frames: mpsc::Receiver<Frame>,
    mut controls: mpsc::Receiver<MuxMessage>,
    credits: Credits,
    cfg: MuxConfig,
) -> MuxEnd
where
    S: Sink<MuxMessage> + Unpin,
    S::Error: fmt::Display,
{
    let mut ping = tokio::time::interval_at(Instant::now() + cfg.ping_interval, cfg.ping_interval);
    ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        let mut completion = None;
        let message = tokio::select! {
            _ = ping.tick() => MuxMessage::Ping(Vec::new()),
            Some(message) = controls.recv() => message,
            Some(frame) = frames.recv() => {
                if let Some(credit) = credits.lock().unwrap().get(&frame.stream()).cloned() {
                    match &frame {
                        Frame::Window { credit: grant, .. } => { credit.receive.fetch_add(*grant, Ordering::AcqRel); }
                        Frame::Data { payload, .. } => { credit.send.fetch_sub(payload.len() as u32, Ordering::AcqRel); }
                        _ => {}
                    }
                    let sent = match &frame {
                        Frame::Data { payload, .. } => Some(Sent::Data(payload.len())),
                        Frame::Eof { .. } => Some(Sent::Eof),
                        Frame::Close { .. } => Some(Sent::Close),
                        _ => None,
                    };
                    completion = sent.map(|sent| (frame.stream(), credit, sent));
                }
                MuxMessage::Binary(frame.encode())
            }
        };
        match tokio::time::timeout(cfg.idle_timeout, sink.send(message)).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => return MuxEnd::Transport(error.to_string()),
            Err(_) => return MuxEnd::WriteTimeout,
        }
        if let Some((id, credit, sent)) = completion {
            match sent {
                Sent::Data(n) => {
                    credit.progress.bytes.fetch_add(n as u64, Ordering::AcqRel);
                }
                Sent::Eof => credit.progress.eof.store(true, Ordering::Release),
                Sent::Close => {
                    credit.progress.closed.store(true, Ordering::Release);
                    credits.lock().unwrap().remove(&id);
                }
            }
            credit.progress.waker.wake();
        }
    }
}

impl Session {
    fn remove(&mut self, id: u32) {
        self.retire(id, false);
    }

    fn retire(&mut self, id: u32, close_queued: bool) {
        if let Some(slot) = self.slots.remove(&id) {
            self.deadlines.remove(&id);
            // Pending IncomingStream objects become unusable immediately.
            if matches!(slot.phase, Phase::Incoming | Phase::Opening(_)) {
                slot.cancelled.cancel();
            }
            // Done/Failed pumps queued their CLOSE behind the last DATA/EOF.
            // Keep writer acknowledgements until that bounded queue drains.
            if !close_queued {
                self.credits.lock().unwrap().remove(&id);
                slot.credit.progress.closed.store(true, Ordering::Release);
                slot.credit.progress.waker.wake();
            }
            self.handle.shared.active.fetch_sub(1, Ordering::AcqRel);
        }
    }

    fn insert(&mut self, id: u32, phase: Phase, cancelled: CancellationToken) {
        let deadline = Instant::now() + self.cfg.open_timeout;
        self.deadlines.insert(id, deadline);
        let credit = Arc::new(Credit {
            receive: AtomicU32::new(INITIAL_WINDOW),
            send: AtomicU32::new(INITIAL_WINDOW),
            progress: Arc::new(WriteProgress::default()),
        });
        self.credits.lock().unwrap().insert(id, credit.clone());
        self.slots.insert(
            id,
            Slot {
                phase,
                cancelled,
                credit,
                eof: false,
            },
        );
        self.handle.shared.active.fetch_add(1, Ordering::AcqRel);
        self.handle.shared.total.fetch_add(1, Ordering::AcqRel);
    }

    fn activate(&mut self, id: u32) -> TunnelStream {
        let slot = self.slots.get_mut(&id).unwrap();
        self.deadlines.remove(&id);
        let (app, connection) = tokio::io::duplex(MAX_DATA_CHUNK);
        let stop = self.handle.shared.stop.child_token();
        let progress = slot.credit.progress.clone();
        let app = TunnelStream {
            id,
            io: CancelIo::new(app, stop.clone()),
            written: 0,
            progress: progress.clone(),
        };
        let connection = CancelIo::new(connection, stop.clone());
        let (events, receiver) = mpsc::unbounded_channel();
        slot.phase = Phase::Open(events.clone());
        let frames = self.frames.clone();
        self.workers.push(tokio::spawn(async move {
            let pumping = pump(connection, id, receiver, frames.clone());
            tokio::pin!(pumping);
            let end = tokio::select! {
                end = &mut pumping => end,
                _ = stop.cancelled() => {
                    // Do not abort pump: it owns spawned read/write tasks and
                    // must reach its own cleanup. CancelIo also breaks a
                    // graceful CLOSE that is waiting on a stalled reader.
                    let _ = events.send(StreamEvent::Close(CloseCode::Failed, "stream dropped".into()));
                    let code = if progress.eof.load(Ordering::Acquire) { CloseCode::Normal } else { CloseCode::Failed };
                    let _ = frames.send(Frame::Close { stream: id, code, message: "stream dropped".into() }).await;
                    pumping.await
                }
            };
            (id, end)
        }));
        app
    }

    async fn close_frame(&mut self, id: u32, code: CloseCode, message: &str) {
        let _ = self
            .frames
            .send(Frame::Close {
                stream: id,
                code,
                message: message.into(),
            })
            .await;
    }

    async fn fail_stream(&mut self, id: u32, code: CloseCode, message: &str) {
        if let Some(slot) = self.slots.get_mut(&id) {
            if let Phase::Open(events) = &slot.phase {
                let _ = events.send(StreamEvent::Close(code, message.into()));
                slot.phase = Phase::Draining;
            } else if !matches!(slot.phase, Phase::Draining) {
                self.remove(id);
            }
        }
        self.close_frame(id, code, message).await;
    }

    async fn command(&mut self, command: Command) {
        match command {
            Command::Open {
                host,
                port,
                reply,
                cancelled,
            } => {
                if cancelled.is_cancelled() {
                    return;
                }
                if self.slots.len() >= self.cfg.max_streams {
                    let _ = reply.send(Err(OpenError::TooManyStreams));
                    return;
                }
                let Ok(id) = u32::try_from(self.next_id) else {
                    let _ = reply.send(Err(OpenError::IdExhausted));
                    return;
                };
                self.next_id += 2;
                self.insert(id, Phase::Opening(reply), cancelled);
                let _ = self
                    .frames
                    .send(Frame::Open {
                        stream: id,
                        host,
                        port,
                    })
                    .await;
            }
            Command::Accept { id, reply } => {
                if !self.slots.get(&id).is_some_and(|slot| {
                    matches!(slot.phase, Phase::Incoming) && !slot.cancelled.is_cancelled()
                }) {
                    let _ = reply.send(Err(OpenError::Closed));
                    return;
                }
                let _ = self.frames.send(Frame::Opened { stream: id }).await;
                let app = self.activate(id);
                let _ = reply.send(Ok(app));
            }
            Command::Reject {
                id,
                code,
                message,
                reply,
            } => {
                if !self.slots.get(&id).is_some_and(|slot| {
                    matches!(slot.phase, Phase::Incoming) && !slot.cancelled.is_cancelled()
                }) {
                    let _ = reply.send(Err(OpenError::Closed));
                    return;
                }
                self.remove(id);
                self.close_frame(id, code, &message).await;
                let _ = reply.send(Ok(()));
            }
        }
    }

    async fn frame(&mut self, frame: Frame) -> Result<(), MuxEnd> {
        let id = frame.stream();
        match frame {
            Frame::Open { host, port, .. } => {
                if self.cfg.role != MuxRole::Acceptor {
                    return Err(MuxEnd::Protocol("Opener received OPEN".into()));
                }
                if id == 0 || id & 1 == 0 || id <= self.last_peer_id {
                    self.close_frame(id, CloseCode::BadRequest, "invalid or reused stream id")
                        .await;
                    return Ok(());
                }
                self.last_peer_id = id;
                if Frame::open(id, &host, port).is_none() {
                    self.close_frame(id, CloseCode::BadRequest, "invalid destination")
                        .await;
                    return Ok(());
                }
                if self.slots.len() >= self.cfg.max_streams {
                    self.close_frame(id, CloseCode::Refused, "stream limit reached")
                        .await;
                    return Ok(());
                }
                let cancelled = CancellationToken::new();
                self.insert(id, Phase::Incoming, cancelled.clone());
                let request = IncomingStream {
                    id,
                    host,
                    port,
                    handle: self.handle.clone(),
                    cancelled,
                };
                if self.requests.as_ref().unwrap().try_send(request).is_err() {
                    self.remove(id);
                    self.close_frame(id, CloseCode::Refused, "incoming queue unavailable")
                        .await;
                }
            }
            Frame::Opened { .. } => {
                if self.cfg.role != MuxRole::Opener {
                    return Err(MuxEnd::Protocol("Acceptor received OPENED".into()));
                }
                let Some(slot) = self.slots.get_mut(&id) else {
                    return Ok(());
                };
                if !matches!(slot.phase, Phase::Opening(_)) {
                    self.fail_stream(id, CloseCode::Protocol, "duplicate OPENED")
                        .await;
                    return Ok(());
                }
                let Phase::Opening(reply) = std::mem::replace(&mut slot.phase, Phase::Draining)
                else {
                    unreachable!()
                };
                let app = self.activate(id);
                let _ = reply.send(Ok(app));
            }
            Frame::Close { code, message, .. } => {
                let Some(slot) = self.slots.get_mut(&id) else {
                    return Ok(());
                };
                let phase = std::mem::replace(&mut slot.phase, Phase::Draining);
                match phase {
                    Phase::Opening(reply) => {
                        let _ = reply.send(Err(OpenError::Rejected { code, message }));
                        self.remove(id);
                    }
                    Phase::Incoming => {
                        slot.cancelled.cancel();
                        self.remove(id);
                    }
                    Phase::Open(events) => {
                        let _ = events.send(StreamEvent::Close(code, message));
                    }
                    Phase::Draining => {}
                }
            }
            frame => {
                let Some(slot) = self.slots.get_mut(&id) else {
                    return Ok(());
                };
                let Phase::Open(events) = &slot.phase else {
                    if !matches!(slot.phase, Phase::Draining) {
                        self.fail_stream(id, CloseCode::Protocol, "sent before OPENED")
                            .await;
                    }
                    return Ok(());
                };
                let event = match frame {
                    Frame::Data { payload, .. } => {
                        if slot.eof {
                            self.fail_stream(id, CloseCode::Protocol, "DATA after EOF")
                                .await;
                            return Ok(());
                        }
                        if payload.is_empty() {
                            return Ok(());
                        }
                        let n = payload.len() as u32;
                        if update_credit(&slot.credit.receive, |credit| credit.checked_sub(n))
                            .is_err()
                        {
                            self.fail_stream(id, CloseCode::Protocol, "sent past the window")
                                .await;
                            return Ok(());
                        }
                        StreamEvent::Data(payload)
                    }
                    Frame::Window { credit: grant, .. } => {
                        if grant == 0 {
                            return Ok(());
                        }
                        if update_credit(&slot.credit.send, |credit| {
                            credit.checked_add(grant).filter(|n| *n <= INITIAL_WINDOW)
                        })
                        .is_err()
                        {
                            self.fail_stream(id, CloseCode::Protocol, "WINDOW exceeds bytes sent")
                                .await;
                            return Ok(());
                        }
                        StreamEvent::Window(grant)
                    }
                    Frame::Eof { .. } => {
                        if slot.eof {
                            self.fail_stream(id, CloseCode::Protocol, "duplicate EOF")
                                .await;
                            return Ok(());
                        }
                        slot.eof = true;
                        StreamEvent::Eof
                    }
                    _ => unreachable!(),
                };
                let _ = events.send(event);
            }
        }
        Ok(())
    }

    async fn expire_pending(&mut self) {
        let now = Instant::now();
        let expired: Vec<u32> = self
            .deadlines
            .iter()
            .filter_map(|(&id, &deadline)| {
                let cancelled = self
                    .slots
                    .get(&id)
                    .is_some_and(|slot| slot.cancelled.is_cancelled());
                (deadline <= now || cancelled).then_some(id)
            })
            .collect();
        for id in expired {
            if let Some(slot) = self.slots.get_mut(&id) {
                if let Phase::Opening(reply) = std::mem::replace(&mut slot.phase, Phase::Incoming) {
                    let _ = reply.send(Err(OpenError::Timeout));
                }
            }
            self.remove(id);
            self.close_frame(id, CloseCode::Timeout, "open timed out or cancelled")
                .await;
        }
    }

    async fn read<S, E>(
        &mut self,
        mut source: S,
        mut commands: mpsc::Receiver<Command>,
        controls: mpsc::Sender<MuxMessage>,
        activity: watch::Sender<Instant>,
    ) -> MuxEnd
    where
        S: Stream<Item = Result<MuxMessage, E>> + Unpin,
        E: fmt::Display,
    {
        let shared = self.handle.shared.clone();
        loop {
            let deadline = self.deadlines.values().copied().min();
            tokio::select! {
                Some(result) = self.workers.next(), if !self.workers.is_empty() => {
                    match result {
                        Ok((id, end)) => self.retire(id, matches!(end, PumpEnd::Done | PumpEnd::Failed(_))),
                        Err(error) => return MuxEnd::Protocol(format!("pump task failed: {error}")),
                    }
                }
                _ = shared.pending_changed.notified() => self.expire_pending().await,
                _ = tokio::time::sleep_until(deadline.unwrap_or_else(Instant::now)), if deadline.is_some() => self.expire_pending().await,
                Some(command) = commands.recv() => self.command(command).await,
                message = source.next() => {
                    activity.send_replace(Instant::now());
                    match message {
                        None | Some(Ok(MuxMessage::Close)) => return MuxEnd::PeerClosed,
                        Some(Err(error)) => return MuxEnd::Transport(error.to_string()),
                        Some(Ok(MuxMessage::Ping(payload))) => {
                            if payload.len() > 125 { return MuxEnd::Protocol("oversized Ping".into()); }
                            let _ = controls.send(MuxMessage::Pong(payload)).await;
                        }
                        Some(Ok(MuxMessage::Pong(payload))) => {
                            if payload.len() > 125 { return MuxEnd::Protocol("oversized Pong".into()); }
                        }
                        Some(Ok(MuxMessage::Binary(bytes))) => {
                            if bytes.len() > MAX_DATA_CHUNK + 5 { return MuxEnd::Protocol("oversized binary frame".into()); }
                            let frame = match Frame::decode(&bytes) { Ok(frame) => frame, Err(error) => return MuxEnd::Protocol(error.to_string()) };
                            if matches!(&frame, Frame::Close { message, .. } if message.len() > MAX_CLOSE_MESSAGE) { return MuxEnd::Protocol("oversized CLOSE reason".into()); }
                            if let Err(end) = self.frame(frame).await { return end; }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn denied_opens_do_not_accumulate_handshake_timers() {
        let shared = Arc::new(Shared {
            stop: CancellationToken::new(),
            pending_changed: Notify::new(),
            active: AtomicUsize::new(0),
            total: AtomicU64::new(0),
        });
        let (commands, _) = mpsc::channel(1);
        let handle = MuxHandle {
            commands,
            shared,
            role: MuxRole::Acceptor,
            open_timeout: Duration::from_secs(10),
        };
        let (frames, _output) = mpsc::channel(1024);
        let (requests, _incoming) = mpsc::channel(1);
        let mut session = Session {
            cfg: MuxConfig {
                role: MuxRole::Acceptor,
                max_streams: 1,
                ..MuxConfig::default()
            },
            handle,
            slots: HashMap::new(),
            credits: Arc::new(Mutex::new(HashMap::new())),
            frames,
            requests: Some(requests),
            workers: FuturesUnordered::new(),
            deadlines: Default::default(),
            next_id: 1,
            last_peer_id: 0,
        };
        // Leave a stale cancelled request in the bounded incoming queue.
        session
            .frame(Frame::open(1, "client", 80).unwrap())
            .await
            .unwrap();
        session
            .frame(Frame::Close {
                stream: 1,
                code: CloseCode::Normal,
                message: String::new(),
            })
            .await
            .unwrap();
        for id in (3..1000).step_by(2) {
            session
                .frame(Frame::open(id, "client", 80).unwrap())
                .await
                .unwrap();
        }
        assert!(
            session.deadlines.len() <= session.cfg.max_streams,
            "rejected OPENs retained {} timers",
            session.deadlines.len()
        );
        assert_eq!(session.handle.stats().active_streams, 0);
    }
}
