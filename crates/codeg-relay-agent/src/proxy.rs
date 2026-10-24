use crate::{buffer::Metrics, config::Config, limiter::Limiter, token::TokenStore};
use bytes::Bytes;
use http_body_util::{combinators::UnsyncBoxBody, BodyExt, Full};
use hyper::{body::Incoming, Request, Response};
use hyper_util::rt::TokioIo;
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;
pub type Body = UnsyncBoxBody<Bytes, BoxError>;

pub struct Gateway {
    pub config: Arc<Config>,
    pub tokens: Arc<TokenStore>,
    pub metrics: Arc<Metrics>,
    pub limiter: Mutex<Limiter>,
}
impl Gateway {
    pub fn new(config: Arc<Config>, tokens: Arc<TokenStore>) -> Self {
        let limiter = Mutex::new(Limiter::new(
            config.auth_failures,
            Duration::from_secs(config.auth_window_secs),
            Duration::from_secs(config.auth_block_secs),
        ));
        Self {
            config,
            tokens,
            metrics: Arc::default(),
            limiter,
        }
    }
}
pub struct ProxySession {
    pub gateway: Arc<Gateway>,
    pub peer: SocketAddr,
    pub cancel: CancellationToken,
    upstream: tokio::sync::Mutex<Option<hyper::client::conn::http1::SendRequest<Incoming>>>,
    upgrade: Mutex<Option<JoinHandle<()>>>,
}
impl ProxySession {
    pub fn new(gateway: Arc<Gateway>, peer: SocketAddr, cancel: CancellationToken) -> Self {
        Self {
            gateway,
            peer,
            cancel,
            upstream: tokio::sync::Mutex::new(None),
            upgrade: Mutex::new(None),
        }
    }
    pub async fn request(
        self: Arc<Self>,
        mut request: Request<Incoming>,
    ) -> Result<Response<Body>, std::convert::Infallible> {
        use crate::{auth, headers, limiter::Decision};
        // The upstream CORS layer answers OPTIONS without entering auth.
        if request.method() != http::Method::OPTIONS && auth::protected_path(request.uri().path()) {
            let valid = self.gateway.tokens.authorized(request.headers());
            let decision = self.gateway.limiter.lock().unwrap().check(
                self.peer.ip(),
                valid,
                std::time::Instant::now(),
            );
            match decision {
                Decision::Allow => {}
                Decision::Unauthorized => {
                    let message = if self.gateway.tokens.current().is_empty() {
                        "Server token is not configured"
                    } else {
                        "Invalid or missing token"
                    };
                    let mut rejected = response(401, message);
                    rejected.headers_mut().insert(
                        "www-authenticate",
                        http::HeaderValue::from_static("Bearer realm=\"codeg\""),
                    );
                    return Ok(rejected);
                }
                Decision::Limited(wait) => {
                    let mut rejected = response(429, "Too many authentication failures");
                    let seconds = wait.as_secs() + u64::from(wait.subsec_nanos() != 0);
                    rejected
                        .headers_mut()
                        .insert("retry-after", seconds.to_string().parse().unwrap());
                    return Ok(rejected);
                }
            }
        }
        if request.method() == http::Method::CONNECT {
            return Ok(response(405, "CONNECT is not supported"));
        }
        let upgrade =
            request.method() == http::Method::GET && headers::websocket_upgrade(request.headers());
        if request.headers().contains_key("upgrade") && !upgrade {
            return Ok(response(400, "Unsupported upgrade"));
        }
        let phone_upgrade = upgrade.then(|| hyper::upgrade::on(&mut request));
        headers::strip_hop_by_hop(request.headers_mut());
        if upgrade {
            headers::set_websocket_upgrade(request.headers_mut());
        }
        // Only the OPEN peer is authoritative. Never append a caller-supplied
        // forwarding chain, which could defeat an upstream IP policy later.
        request.headers_mut().insert(
            "x-forwarded-for",
            crate::limiter::canonical_ip(self.peer.ip())
                .to_string()
                .parse()
                .unwrap(),
        );
        request
            .headers_mut()
            .insert("x-forwarded-proto", http::HeaderValue::from_static("https"));
        request.headers_mut().remove("forwarded");
        let path = request
            .uri()
            .path_and_query()
            .map(|v| v.as_str())
            .unwrap_or("/")
            .to_owned();
        *request.uri_mut() = path.parse().unwrap();
        *request.version_mut() = http::Version::HTTP_11;
        if !request.headers().contains_key("host") {
            let upstream: http::Uri = self.gateway.config.upstream_url.parse().unwrap();
            request.headers_mut().insert(
                "host",
                upstream.authority().unwrap().as_str().parse().unwrap(),
            );
        }
        let mut response = match self.forward(request).await {
            Ok(response) => response,
            Err(_) => {
                tracing::warn!("upstream request failed");
                return Ok(crate::proxy::response(502, "Upstream unavailable"));
            }
        };
        if response.status() == http::StatusCode::SWITCHING_PROTOCOLS {
            if !upgrade || !headers::websocket_upgrade(response.headers()) {
                return Ok(crate::proxy::response(502, "Invalid upstream upgrade"));
            }
            let upstream_upgrade = hyper::upgrade::on(&mut response);
            let phone_upgrade = phone_upgrade.unwrap();
            let cancel = self.cancel.clone();
            let task = tokio::spawn(async move {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {},
                    _ = async {
                        let Ok(Ok((phone, upstream))) = tokio::time::timeout(Duration::from_secs(10), async { tokio::try_join!(phone_upgrade, upstream_upgrade) }).await else { return; };
                        // Hyper returns the buffered IO created in forward(),
                        // so the configured capacity and stall timer survive
                        // the upgrade. This splice adds only fixed scratch
                        // space; frames, ping/pong and close stay byte-exact.
                        let _ = tokio::io::copy_bidirectional_with_sizes(&mut TokioIo::new(phone), &mut TokioIo::new(upstream), crate::buffer::CHUNK, crate::buffer::CHUNK).await;
                    } => {},
                }
                cancel.cancel();
            });
            *self.upgrade.lock().unwrap() = Some(task);
            headers::strip_hop_by_hop(response.headers_mut());
            headers::set_websocket_upgrade(response.headers_mut());
        } else {
            headers::strip_hop_by_hop(response.headers_mut());
        }
        Ok(response.map(|body| body.map_err(|e| Box::new(e) as BoxError).boxed_unsync()))
    }
    async fn forward(&self, request: Request<Incoming>) -> anyhow::Result<Response<Incoming>> {
        let mut sender = self.upstream.lock().await;
        if sender.as_ref().is_none_or(|sender| sender.is_closed()) {
            let config = &self.gateway.config;
            let url = url::Url::parse(&config.upstream_url)?;
            let socket = tokio::time::timeout(
                Duration::from_secs(10),
                crate::tls::connect(&url, config.upstream_fingerprint.as_deref(), &[b"http/1.1"]),
            )
            .await??;
            let buffered = crate::buffer::wrap(
                socket,
                config.buffer_bytes,
                Duration::from_secs(config.stall_timeout_secs),
                self.cancel.clone(),
                self.gateway.metrics.clone(),
            );
            let (new_sender, connection) = hyper::client::conn::http1::Builder::new()
                .max_buf_size(64 * 1024)
                .handshake(TokioIo::new(buffered))
                .await?;
            let cancel = self.cancel.clone();
            tokio::spawn(async move {
                tokio::select! { biased; _ = cancel.cancelled() => {}, _ = connection.with_upgrades() => {} }
            });
            *sender = Some(new_sender);
        }
        let sender = sender.as_mut().unwrap();
        sender.ready().await?;
        // Do not retry a request after sending it: POST may have side effects.
        Ok(sender.send_request(request).await?)
    }
    pub async fn finish_upgrade(&self) {
        let task = self.upgrade.lock().unwrap().take();
        if let Some(mut task) = task {
            tokio::select! {
                biased;
                _ = self.cancel.cancelled() => { task.abort(); let _ = task.await; },
                _ = &mut task => {},
            }
        }
    }
}
pub fn response(status: u16, message: &'static str) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("content-type", "text/plain; charset=utf-8")
        .body(
            Full::new(Bytes::from_static(message.as_bytes()))
                .map_err(|never| match never {})
                .boxed_unsync(),
        )
        .unwrap()
}
