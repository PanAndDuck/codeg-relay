use codeg_tunnel::mux::MuxMessage;
use futures::{channel::mpsc, Sink, SinkExt, Stream, StreamExt};
use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::oneshot,
    task::JoinHandle,
};
use tokio_tungstenite::{
    tungstenite::{
        protocol::{frame::coding::CloseCode, CloseFrame},
        Message,
    },
    WebSocketStream,
};

#[derive(Clone, Copy)]
pub enum Stop {
    Replaced,
    Shutdown,
}
pub struct Transport {
    incoming: mpsc::Receiver<Result<MuxMessage, io::Error>>,
    outgoing: mpsc::Sender<MuxMessage>,
}
impl Stream for Transport {
    type Item = Result<MuxMessage, io::Error>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.incoming).poll_next(cx)
    }
}
impl Sink<MuxMessage> for Transport {
    type Error = mpsc::SendError;
    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.outgoing).poll_ready(cx)
    }
    fn start_send(mut self: Pin<&mut Self>, msg: MuxMessage) -> Result<(), Self::Error> {
        Pin::new(&mut self.outgoing).start_send(msg)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.outgoing).poll_flush(cx)
    }
    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.outgoing).poll_close(cx)
    }
}

pub fn bridge<S>(ws: WebSocketStream<S>) -> (Transport, oneshot::Sender<Stop>, JoinHandle<()>)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut incoming_tx, incoming) = mpsc::channel(16);
    let (outgoing, mut outgoing_rx) = mpsc::channel(16);
    let (stop_tx, stop_rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        let (mut sink, mut source) = ws.split();
        // The control path cancels blocked channel sends AND blocked socket writes.
        let stop = tokio::select! {
            biased;
            stop = stop_rx => stop.unwrap_or(Stop::Shutdown),
            _ = async {
                while let Some(msg) = source.next().await {
                    let mapped = match msg {
                        Ok(Message::Binary(b)) => Ok(MuxMessage::Binary(b.to_vec())),
                        Ok(Message::Ping(b)) => Ok(MuxMessage::Ping(b.to_vec())),
                        Ok(Message::Pong(b)) => Ok(MuxMessage::Pong(b.to_vec())),
                        Ok(Message::Close(_)) => Ok(MuxMessage::Close),
                        _ => Err(io::Error::other("invalid websocket transport")),
                    };
                    let terminal = mapped.is_err() || matches!(mapped, Ok(MuxMessage::Close));
                    if incoming_tx.send(mapped).await.is_err() || terminal { break; }
                }
            } => Stop::Shutdown,
            _ = async {
                while let Some(msg) = outgoing_rx.next().await {
                    let msg = match msg {
                        MuxMessage::Binary(b) => Message::Binary(b.into()), MuxMessage::Ping(b) => Message::Ping(b.into()),
                        MuxMessage::Pong(b) => Message::Pong(b.into()), MuxMessage::Close => Message::Close(None),
                    };
                    if sink.send(msg).await.is_err() { break; }
                }
            } => Stop::Shutdown,
        };
        let frame = match stop {
            Stop::Replaced => CloseFrame {
                code: CloseCode::Library(4001),
                reason: "replaced".into(),
            },
            Stop::Shutdown => CloseFrame {
                code: CloseCode::Away,
                reason: "shutdown".into(),
            },
        };
        let _ = tokio::time::timeout(
            Duration::from_secs(1),
            sink.send(Message::Close(Some(frame))),
        )
        .await;
    });
    (Transport { incoming, outgoing }, stop_tx, task)
}
