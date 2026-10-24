//! TLS-only stream admission, derived from E2EE-a's relay/serve.rs.
use crate::{
    certificate::HomeTls,
    limiter::canonical_ip,
    proxy::{Gateway, ProxySession},
};
use codeg_tunnel::{frame::CloseCode, mux::IncomingStream};
use hyper_util::rt::{TokioIo, TokioTimer};
use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
pub struct StreamLimits(Mutex<HashMap<IpAddr, usize>>);
struct Permit {
    limits: Arc<StreamLimits>,
    ip: IpAddr,
}
impl StreamLimits {
    fn acquire(self: &Arc<Self>, ip: IpAddr) -> Option<Permit> {
        let ip = canonical_ip(ip);
        let mut peers = self.0.lock().unwrap();
        if peers.values().sum::<usize>() >= 256 || peers.get(&ip).copied().unwrap_or(0) >= 64 {
            return None;
        }
        *peers.entry(ip).or_default() += 1;
        Some(Permit {
            limits: self.clone(),
            ip,
        })
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        let mut peers = self.limits.0.lock().unwrap();
        if let Some(n) = peers.get_mut(&self.ip) {
            *n -= 1;
            if *n == 0 {
                peers.remove(&self.ip);
            }
        }
    }
}

pub async fn serve(
    incoming: IncomingStream,
    gateway: Arc<Gateway>,
    limits: Arc<StreamLimits>,
    tls: HomeTls,
    cancel: CancellationToken,
) {
    let Ok(ip) = incoming.host().parse::<IpAddr>() else {
        let _ = incoming
            .reject(CloseCode::BadRequest, "OPEN host must be an IP literal")
            .await;
        return;
    };
    let peer = std::net::SocketAddr::new(ip, incoming.port());
    // Keep the permit until BOTH HTTP and any upgraded connection finish.
    let Some(_permit) = limits.acquire(ip) else {
        let _ = incoming
            .reject(CloseCode::NotAllowed, "stream limit reached")
            .await;
        return;
    };
    let _cancel_on_drop = cancel.clone().drop_guard();
    let Ok(stream) = incoming.accept().await else {
        return;
    };
    let Ok(Ok(stream)) =
        tokio::time::timeout(Duration::from_secs(10), tls.acceptor.accept(stream)).await
    else {
        return;
    };
    let session = Arc::new(ProxySession::new(gateway, peer, cancel.clone()));
    let handler = session.clone();
    let service = hyper::service::service_fn(move |request| handler.clone().request(request));
    let mut builder = hyper::server::conn::http1::Builder::new();
    builder
        .timer(TokioTimer::new())
        .keep_alive(true)
        .max_buf_size(64 * 1024)
        .header_read_timeout(Duration::from_secs(30));
    tokio::select! {
        biased;
        _ = cancel.cancelled() => {},
        _ = builder.serve_connection(TokioIo::new(stream), service).with_upgrades() => {},
    }
    session.finish_upgrade().await;
}
