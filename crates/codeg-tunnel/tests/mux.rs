use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use codeg_tunnel::{
    frame::{CloseCode, Frame, INITIAL_WINDOW},
    mux::*,
};
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::mpsc,
    time::timeout,
};

struct Transport {
    tx: tokio_util::sync::PollSender<Result<MuxMessage, io::Error>>,
    rx: mpsc::Receiver<Result<MuxMessage, io::Error>>,
}

impl Stream for Transport {
    type Item = Result<MuxMessage, io::Error>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx)
    }
}

impl Sink<MuxMessage> for Transport {
    type Error = io::Error;
    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.tx
            .poll_reserve(cx)
            .map_err(|_| io::ErrorKind::BrokenPipe.into())
    }
    fn start_send(mut self: Pin<&mut Self>, item: MuxMessage) -> io::Result<()> {
        self.tx
            .send_item(Ok(item))
            .map_err(|_| io::ErrorKind::BrokenPipe.into())
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

fn transport_pair() -> (Transport, Transport) {
    transport_pair_with_capacity(32)
}

fn transport_pair_with_capacity(capacity: usize) -> (Transport, Transport) {
    let (a_tx, b_rx) = mpsc::channel(capacity);
    let (b_tx, a_rx) = mpsc::channel(capacity);
    (
        Transport {
            tx: tokio_util::sync::PollSender::new(a_tx),
            rx: a_rx,
        },
        Transport {
            tx: tokio_util::sync::PollSender::new(b_tx),
            rx: b_rx,
        },
    )
}

type Running = (MuxHandle, MuxIncoming, tokio::task::JoinHandle<MuxEnd>);

#[tokio::test]
async fn abort_before_first_poll_marks_session_closed() {
    let (transport, _peer) = transport_pair();
    let (handle, _, task) = start(transport, MuxConfig::default());
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(!handle.stats().connected);
}

fn pair() -> (Running, Running) {
    let (a, b) = transport_pair();
    (
        start(a, MuxConfig::default()),
        start(
            b,
            MuxConfig {
                role: MuxRole::Acceptor,
                ..MuxConfig::default()
            },
        ),
    )
}

async fn connected(handle: &MuxHandle, incoming: &mut MuxIncoming) -> (TunnelStream, TunnelStream) {
    let (a, b) = tokio::join!(handle.open("::1", 12345), async {
        let request = incoming.accept().await.unwrap();
        assert_eq!(request.host(), "::1");
        assert_eq!(request.port(), 12345);
        request.accept().await.unwrap()
    });
    (a.unwrap(), b)
}

#[tokio::test]
async fn open_accept_reject_and_ids_are_never_reused() {
    let ((a, _, a_task), (b, mut incoming, b_task)) = pair();
    let (first, peer) = connected(&a, &mut incoming).await;
    assert_eq!(first.id(), 1);
    drop((first, peer));
    let (rejected, ()) = tokio::join!(a.open("127.0.0.1", 80), async {
        incoming
            .accept()
            .await
            .unwrap()
            .reject(CloseCode::NotAllowed, "policy")
            .await
            .unwrap();
    });
    assert!(
        matches!(rejected, Err(OpenError::Rejected { code: CloseCode::NotAllowed, message }) if message == "policy")
    );
    let (third, _) = connected(&a, &mut incoming).await;
    assert_eq!(third.id(), 5);
    assert_eq!(a.stats().total_streams, 3);
    assert!(matches!(
        b.open("localhost", 80).await,
        Err(OpenError::WrongRole)
    ));
    a.close();
    b.close();
    assert_eq!(a_task.await.unwrap(), MuxEnd::Closed);
    assert_eq!(b_task.await.unwrap(), MuxEnd::Closed);
}

fn random_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}

async fn exchange(stream: TunnelStream, send: Vec<u8>, expected: Vec<u8>) {
    let (mut reader, mut writer) = tokio::io::split(stream);
    let ((), received) = tokio::join!(
        async {
            writer.write_all(&send).await.unwrap();
            writer.shutdown().await.unwrap();
        },
        async {
            let mut bytes = Vec::new();
            reader.read_to_end(&mut bytes).await.unwrap();
            bytes
        }
    );
    assert_eq!(received, expected);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn five_mebibytes_random_data_each_way() {
    let ((a, _, a_task), (b, mut incoming, b_task)) = pair();
    let (left, right) = connected(&a, &mut incoming).await;
    let x = random_bytes(17, 5 * 1024 * 1024);
    let y = random_bytes(53, 5 * 1024 * 1024);
    timeout(Duration::from_secs(20), async {
        tokio::join!(exchange(left, x.clone(), y.clone()), exchange(right, y, x));
    })
    .await
    .unwrap();
    a.close();
    b.close();
    a_task.await.unwrap();
    b_task.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_hundred_concurrent_streams() {
    let ((a, _, a_task), (b, mut incoming, b_task)) = pair();
    timeout(Duration::from_secs(20), async {
        let peers = tokio::spawn(async move {
            let mut tasks = Vec::new();
            for _ in 0..100 {
                let stream = incoming.accept().await.unwrap().accept().await.unwrap();
                tasks.push(tokio::spawn(exchange(
                    stream,
                    vec![42; 4096],
                    vec![21; 4096],
                )));
            }
            for task in tasks {
                task.await.unwrap();
            }
        });
        let mut tasks = Vec::new();
        for _ in 0..100 {
            let a = a.clone();
            tasks.push(tokio::spawn(async move {
                exchange(
                    a.open("client", 80).await.unwrap(),
                    vec![21; 4096],
                    vec![42; 4096],
                )
                .await;
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }
        peers.await.unwrap();
    })
    .await
    .unwrap();
    a.close();
    b.close();
    a_task.await.unwrap();
    b_task.await.unwrap();
}

#[tokio::test]
async fn half_close_keeps_the_other_direction_alive() {
    let ((a, _, a_task), (b, mut incoming, b_task)) = pair();
    let (mut left, mut right) = connected(&a, &mut incoming).await;
    left.write_all(b"request").await.unwrap();
    left.shutdown().await.unwrap();
    let mut request = Vec::new();
    right.read_to_end(&mut request).await.unwrap();
    assert_eq!(request, b"request");
    right.write_all(b"response after EOF").await.unwrap();
    right.shutdown().await.unwrap();
    let mut response = Vec::new();
    left.read_to_end(&mut response).await.unwrap();
    assert_eq!(response, b"response after EOF");
    a.close();
    b.close();
    a_task.await.unwrap();
    b_task.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_then_drop_preserves_all_buffered_response_bytes() {
    let ((a, _, a_task), (b, mut incoming, b_task)) = pair();
    let (mut left, mut right) = connected(&a, &mut incoming).await;
    left.shutdown().await.unwrap();
    let mut request = Vec::new();
    right.read_to_end(&mut request).await.unwrap();
    let response = random_bytes(42, 128 * 1024);
    right.write_all(&response).await.unwrap();
    right.shutdown().await.unwrap();
    drop(right);
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    let mut received = Vec::new();
    timeout(Duration::from_secs(2), left.read_to_end(&mut received))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(received.len(), response.len());
    assert_eq!(received, response);
    a.close();
    b.close();
    a_task.await.unwrap();
    b_task.await.unwrap();
}

#[tokio::test]
async fn a_stalled_reader_does_not_block_other_streams() {
    let ((a, _, a_task), (b, mut incoming, b_task)) = pair();
    let (mut blocked, _unread) = connected(&a, &mut incoming).await;
    let stalled = tokio::spawn(async move {
        blocked
            .write_all(&vec![1; 4 * INITIAL_WINDOW as usize])
            .await
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!stalled.is_finished());
    timeout(Duration::from_secs(2), async {
        let (left, right) = connected(&a, &mut incoming).await;
        tokio::join!(
            exchange(left, vec![5; 1024], vec![6; 1024]),
            exchange(right, vec![6; 1024], vec![5; 1024])
        );
    })
    .await
    .unwrap();
    a.close();
    b.close();
    a_task.await.unwrap();
    b_task.await.unwrap();
    assert!(stalled.await.unwrap().is_err());
}

#[tokio::test]
async fn transport_drop_ends_all_streams_and_pending_opens() {
    let ((a, _, a_task), (b, mut incoming, b_task)) = pair();
    let mut streams = Vec::new();
    let mut peers = Vec::new();
    for _ in 0..8 {
        let (stream, peer) = connected(&a, &mut incoming).await;
        streams.push(stream);
        peers.push(peer);
    }
    let pending = tokio::spawn({
        let a = a.clone();
        async move { a.open("pending", 80).await }
    });
    let _request = incoming.accept().await.unwrap();
    b_task.abort(); // Abrupt transport loss, not an orderly MuxHandle::close.
    timeout(Duration::from_secs(2), async {
        a_task.await.unwrap();
        assert!(pending.await.unwrap().is_err());
        for mut stream in streams {
            let mut byte = [0];
            assert!(!matches!(stream.read(&mut byte).await, Ok(n) if n > 0));
        }
    })
    .await
    .unwrap();
    assert!(!b.stats().connected);
    assert_eq!(b.stats().active_streams, 0);
}

#[tokio::test]
async fn transport_loss_wakes_concurrent_flush_and_read() {
    let (transport, mut peer) = transport_pair_with_capacity(1);
    let (_handle, _, task) = start(transport, MuxConfig::default());
    let opening = tokio::spawn(async move { _handle.open("client", 80).await.unwrap() });
    assert!(matches!(
        read_frame(&mut peer).await,
        Frame::Open { stream: 1, .. }
    ));
    peer.send(MuxMessage::Binary(Frame::Opened { stream: 1 }.encode()))
        .await
        .unwrap();
    let (mut reader, mut writer) = tokio::io::split(opening.await.unwrap());
    writer.write_all(&vec![1; 128 * 1024]).await.unwrap();
    let flushing = tokio::spawn(async move { writer.flush().await });
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    let reading = tokio::spawn(async move { reader.read(&mut [0]).await });
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert!(!flushing.is_finished());
    drop(peer);
    task.await.unwrap();
    let flushed = timeout(Duration::from_secs(1), flushing)
        .await
        .unwrap()
        .unwrap();
    let read = timeout(Duration::from_secs(1), reading)
        .await
        .unwrap()
        .unwrap();
    assert!(flushed.is_err());
    assert!(!matches!(read, Ok(n) if n > 0));
}

#[tokio::test(start_paused = true)]
async fn heartbeat_pings_answers_and_times_out_at_idle_deadline() {
    let (transport, mut peer) = transport_pair();
    let (_handle, _, task) = start(transport, MuxConfig::default());
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(15)).await;
    assert!(matches!(
        peer.next().await.unwrap().unwrap(),
        MuxMessage::Ping(_)
    ));
    peer.send(MuxMessage::Ping(vec![1, 2, 3])).await.unwrap();
    assert_eq!(
        peer.next().await.unwrap().unwrap(),
        MuxMessage::Pong(vec![1, 2, 3])
    );
    tokio::time::advance(Duration::from_secs(44)).await;
    tokio::task::yield_now().await;
    assert!(!task.is_finished());
    tokio::time::advance(Duration::from_secs(1)).await;
    assert_eq!(task.await.unwrap(), MuxEnd::IdleTimeout);
}

#[tokio::test(start_paused = true)]
async fn open_timeout_releases_the_stream_slot() {
    let ((a, _, a_task), (b, mut incoming, b_task)) = pair();
    let waiting = tokio::spawn({
        let a = a.clone();
        async move { a.open("pending", 80).await }
    });
    let request = incoming.accept().await.unwrap();
    tokio::time::advance(Duration::from_secs(10)).await;
    assert!(matches!(waiting.await.unwrap(), Err(OpenError::Timeout)));
    assert!(request.accept().await.is_err());
    tokio::task::yield_now().await;
    assert_eq!(a.stats().active_streams, 0);
    a.close();
    b.close();
    a_task.await.unwrap();
    b_task.await.unwrap();
}

async fn read_frame(peer: &mut Transport) -> Frame {
    loop {
        if let MuxMessage::Binary(bytes) = peer.next().await.unwrap().unwrap() {
            return Frame::decode(&bytes).unwrap();
        }
    }
}

#[tokio::test]
async fn invalid_reused_and_even_ids_are_refused() {
    let (transport, mut peer) = transport_pair();
    let (handle, mut incoming, task) = start(
        transport,
        MuxConfig {
            role: MuxRole::Acceptor,
            ..MuxConfig::default()
        },
    );
    for id in [0, 2] {
        peer.send(MuxMessage::Binary(
            Frame::open(id, "client", 80).unwrap().encode(),
        ))
        .await
        .unwrap();
        assert!(matches!(
            read_frame(&mut peer).await,
            Frame::Close {
                code: CloseCode::BadRequest,
                ..
            }
        ));
    }
    peer.send(MuxMessage::Binary(
        Frame::open(1, "client", 80).unwrap().encode(),
    ))
    .await
    .unwrap();
    incoming
        .accept()
        .await
        .unwrap()
        .reject(CloseCode::Refused, "no")
        .await
        .unwrap();
    read_frame(&mut peer).await;
    peer.send(MuxMessage::Binary(
        Frame::open(1, "client", 80).unwrap().encode(),
    ))
    .await
    .unwrap();
    assert!(matches!(
        read_frame(&mut peer).await,
        Frame::Close {
            code: CloseCode::BadRequest,
            ..
        }
    ));
    handle.close();
    task.await.unwrap();
}

#[tokio::test]
async fn peer_cannot_queue_more_than_its_credit_or_send_data_before_opened() {
    let (transport, mut peer) = transport_pair();
    let (handle, mut incoming, task) = start(
        transport,
        MuxConfig {
            role: MuxRole::Acceptor,
            ..MuxConfig::default()
        },
    );
    peer.send(MuxMessage::Binary(
        Frame::open(1, "client", 80).unwrap().encode(),
    ))
    .await
    .unwrap();
    let request = incoming.accept().await.unwrap();
    peer.send(MuxMessage::Binary(
        Frame::Data {
            stream: 1,
            payload: vec![9],
        }
        .encode(),
    ))
    .await
    .unwrap();
    assert!(matches!(
        read_frame(&mut peer).await,
        Frame::Close {
            code: CloseCode::Protocol,
            ..
        }
    ));
    assert!(request.accept().await.is_err());
    peer.send(MuxMessage::Binary(
        Frame::open(3, "client", 80).unwrap().encode(),
    ))
    .await
    .unwrap();
    let _unread = incoming.accept().await.unwrap().accept().await.unwrap();
    assert!(matches!(
        read_frame(&mut peer).await,
        Frame::Opened { stream: 3 }
    ));
    // A frame larger than the entire window cannot enter a stream event queue.
    peer.send(MuxMessage::Binary(
        Frame::Data {
            stream: 3,
            payload: vec![0; INITIAL_WINDOW as usize + 1],
        }
        .encode(),
    ))
    .await
    .unwrap();
    timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap();
    assert!(!handle.stats().connected);
}

#[tokio::test]
async fn pending_requests_count_against_limit_and_dropped_open_is_cancelled() {
    let (left, right) = transport_pair();
    let (a, _, a_task) = start(
        left,
        MuxConfig {
            max_streams: 1,
            ..MuxConfig::default()
        },
    );
    let (b, mut incoming, b_task) = start(
        right,
        MuxConfig {
            role: MuxRole::Acceptor,
            max_streams: 1,
            ..MuxConfig::default()
        },
    );
    let pending = tokio::spawn({
        let a = a.clone();
        async move { a.open("pending", 80).await }
    });
    let request = incoming.accept().await.unwrap();
    assert!(matches!(
        a.open("too-many", 80).await,
        Err(OpenError::TooManyStreams)
    ));
    pending.abort();
    timeout(Duration::from_secs(1), async {
        while a.stats().active_streams != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    drop(request);
    let (stream, peer) = connected(&a, &mut incoming).await;
    assert_eq!(stream.id(), 3);
    drop((stream, peer));
    a.close();
    b.close();
    a_task.await.unwrap();
    b_task.await.unwrap();
}

#[tokio::test]
async fn lost_transport_interrupts_graceful_close_draining_a_stalled_reader() {
    let (transport, mut peer) = transport_pair();
    let (a, mut incoming, task) = start(
        transport,
        MuxConfig {
            role: MuxRole::Acceptor,
            ..MuxConfig::default()
        },
    );
    peer.send(MuxMessage::Binary(
        Frame::open(1, "client", 80).unwrap().encode(),
    ))
    .await
    .unwrap();
    let mut stream = incoming.accept().await.unwrap().accept().await.unwrap();
    read_frame(&mut peer).await;
    for _ in 0..4 {
        peer.send(MuxMessage::Binary(
            Frame::Data {
                stream: 1,
                payload: vec![4; 32768],
            }
            .encode(),
        ))
        .await
        .unwrap();
    }
    peer.send(MuxMessage::Binary(Frame::Eof { stream: 1 }.encode()))
        .await
        .unwrap();
    peer.send(MuxMessage::Binary(
        Frame::Close {
            stream: 1,
            code: CloseCode::Normal,
            message: String::new(),
        }
        .encode(),
    ))
    .await
    .unwrap();
    drop(peer);
    timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap();
    let mut byte = [0];
    assert!(stream.read(&mut byte).await.is_err());
    assert_eq!(a.stats().active_streams, 0);
}

#[tokio::test]
async fn spurious_window_and_repeated_eof_close_only_the_bad_stream() {
    let (transport, mut peer) = transport_pair();
    let (a, mut incoming, task) = start(
        transport,
        MuxConfig {
            role: MuxRole::Acceptor,
            ..MuxConfig::default()
        },
    );
    for id in [1, 3] {
        peer.send(MuxMessage::Binary(
            Frame::open(id, "client", 80).unwrap().encode(),
        ))
        .await
        .unwrap();
        let _stream = incoming.accept().await.unwrap().accept().await.unwrap();
        read_frame(&mut peer).await;
        if id == 1 {
            peer.send(MuxMessage::Binary(
                Frame::Window {
                    stream: id,
                    credit: 1,
                }
                .encode(),
            ))
            .await
            .unwrap();
        } else {
            peer.send(MuxMessage::Binary(Frame::Eof { stream: id }.encode()))
                .await
                .unwrap();
            peer.send(MuxMessage::Binary(Frame::Eof { stream: id }.encode()))
                .await
                .unwrap();
        }
        assert!(matches!(
            read_frame(&mut peer).await,
            Frame::Close {
                code: CloseCode::Protocol,
                ..
            }
        ));
    }
    assert!(a.stats().connected);
    a.close();
    task.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn any_received_message_resets_configurable_idle_timeout() {
    let (transport, mut peer) = transport_pair();
    let (handle, _, task) = start(
        transport,
        MuxConfig {
            ping_interval: Duration::from_secs(2),
            idle_timeout: Duration::from_secs(5),
            ..MuxConfig::default()
        },
    );
    tokio::task::yield_now().await;
    for _ in 0..4 {
        tokio::time::advance(Duration::from_secs(4)).await;
        peer.send(MuxMessage::Pong(vec![])).await.unwrap();
        tokio::task::yield_now().await;
        assert!(!task.is_finished());
    }
    handle.close();
    assert_eq!(task.await.unwrap(), MuxEnd::Closed);
}

#[tokio::test(start_paused = true)]
async fn stalled_transport_writer_cannot_hide_shutdown_or_idle_timeout() {
    let (transport, _unread_peer) = transport_pair();
    let (handle, _, task) = start(
        transport,
        MuxConfig {
            ping_interval: Duration::from_secs(1),
            idle_timeout: Duration::from_secs(45),
            ..MuxConfig::default()
        },
    );
    tokio::task::yield_now().await;
    for _ in 0..46 {
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
    }
    assert_eq!(task.await.unwrap(), MuxEnd::IdleTimeout);
    assert!(!handle.stats().connected);
}

#[tokio::test]
async fn invalid_destination_is_local_and_invalid_config_is_reported() {
    let (transport, _peer) = transport_pair();
    let (handle, _, task) = start(transport, MuxConfig::default());
    assert!(matches!(
        handle.open("", 80).await,
        Err(OpenError::InvalidDestination)
    ));
    assert!(matches!(
        handle.open("host", 0).await,
        Err(OpenError::InvalidDestination)
    ));
    handle.close();
    task.await.unwrap();
    let (transport, _) = transport_pair();
    let (_, _, task) = start(
        transport,
        MuxConfig {
            max_streams: 0,
            ..MuxConfig::default()
        },
    );
    assert!(matches!(task.await.unwrap(), MuxEnd::InvalidConfig(_)));
}
