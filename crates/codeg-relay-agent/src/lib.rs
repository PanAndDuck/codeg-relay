//! A standalone, TLS-only connector for the unmodified codeg web service.
pub mod auth;
pub mod buffer;
pub mod certificate;
pub mod client;
pub mod config;
pub mod headers;
pub mod limiter;
pub mod logging;
pub mod pairing;
pub mod proxy;
pub mod secrets;
pub mod serve;
pub mod service;
pub mod status;
pub mod tls;
pub mod token;
pub mod transport;
