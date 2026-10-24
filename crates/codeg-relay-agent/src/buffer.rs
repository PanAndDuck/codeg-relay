use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
pub struct Metrics {
    pub stalls: AtomicU64,
    pub active: AtomicU64,
    pub opened: AtomicU64,
}

pub const CHUNK: usize = 32 * 1024;

struct Active(Arc<Metrics>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::Relaxed);
    }
}

pub fn wrap<S: crate::tls::Io + 'static>(
    upstream: S,
    capacity: usize,
    stall: Duration,
    cancel: CancellationToken,
    metrics: Arc<Metrics>,
) -> DuplexStream {
    metrics.opened.fetch_add(1, Ordering::Relaxed);
    metrics.active.fetch_add(1, Ordering::Relaxed);
    // DuplexStream bounds each direction independently. The reader drains the
    // real upstream even when hyper is waiting for the phone to consume a body.
    let (client, server) = tokio::io::duplex(capacity);
    tokio::spawn(async move {
        let _active = Active(metrics.clone());
        let (up_read, up_write) = tokio::io::split(upstream);
        let (buf_read, buf_write) = tokio::io::split(server);
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => return,
            result = async {
                tokio::try_join!(pump(up_read, buf_write, stall, "upstream_to_phone"), pump(buf_read, up_write, stall, "phone_to_upstream"))
            } => result,
        };
        if let Err(reason) = result {
            if let Some(direction) = reason {
                metrics.stalls.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(direction, "buffer stalled; closing phone and upstream");
            }
            cancel.cancel();
        }
    });
    client
}

async fn pump<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    mut reader: R,
    mut writer: W,
    stall: Duration,
    direction: &'static str,
) -> Result<(), Option<&'static str>> {
    let mut bytes = vec![0; CHUNK];
    loop {
        let n = reader.read(&mut bytes).await.map_err(|_| None)?;
        if n == 0 {
            tokio::time::timeout(stall, writer.shutdown())
                .await
                .map_err(|_| Some(direction))?
                .map_err(|_| None)?;
            return Ok(());
        }
        let mut remaining = &bytes[..n];
        while !remaining.is_empty() {
            // Reset only when bytes actually advance, never on wakeups.
            let n = tokio::time::timeout(stall, writer.write(remaining))
                .await
                .map_err(|_| Some(direction))?
                .map_err(|_| None)?;
            if n == 0 {
                return Err(None);
            }
            remaining = &remaining[n..];
        }
        tokio::time::timeout(stall, writer.flush())
            .await
            .map_err(|_| Some(direction))?
            .map_err(|_| None)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    #[tokio::test]
    async fn drains_a_burst_while_consumer_is_not_reading_and_preserves_both_directions() {
        let (mut upstream, socket) = tokio::io::duplex(64);
        let cancel = CancellationToken::new();
        let mut phone = wrap(
            socket,
            4096,
            Duration::from_secs(1),
            cancel.clone(),
            Arc::default(),
        );
        tokio::time::timeout(Duration::from_secs(1), upstream.write_all(&[7; 2048]))
            .await
            .unwrap()
            .unwrap();
        let mut result = [0; 2048];
        phone.read_exact(&mut result).await.unwrap();
        assert!(result.iter().all(|b| *b == 7));
        phone.write_all(b"request").await.unwrap();
        let mut request = [0; 7];
        upstream.read_exact(&mut request).await.unwrap();
        assert_eq!(&request, b"request");
        cancel.cancel();
    }
    #[tokio::test(start_paused = true)]
    async fn saturated_buffer_cancels_both_ends_within_stall_deadline() {
        let (mut upstream, socket) = tokio::io::duplex(64);
        let cancel = CancellationToken::new();
        let metrics = Arc::new(Metrics::default());
        let _phone = wrap(
            socket,
            1024,
            Duration::from_secs(30),
            cancel.clone(),
            metrics.clone(),
        );
        let started = tokio::time::Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(31),
            upstream.write_all(&vec![7; 128 * 1024]),
        )
        .await
        .unwrap();
        assert!(result.is_err());
        assert!(cancel.is_cancelled());
        assert_eq!(metrics.stalls.load(Ordering::Relaxed), 1);
        assert!(started.elapsed() >= Duration::from_secs(30));
        assert_eq!(metrics.active.load(Ordering::Relaxed), 0);
    }
}
