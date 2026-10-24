use clap::Parser;
use codeg_relay::{
    config::{Cli, Command},
    server, tls,
};

#[tokio::main(worker_threads = 2)]
async fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::GenKey => {
            use base64::Engine;
            use ring::rand::SecureRandom;
            let mut bytes = [0; 32];
            ring::rand::SystemRandom::new()
                .fill(&mut bytes)
                .map_err(|_| anyhow::anyhow!("system random generator failed"))?;
            println!(
                "{}",
                base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
            );
        }
        Command::Fingerprint { data_dir, tls_cert } => {
            let cert = tls_cert.unwrap_or_else(|| data_dir.join("tls/cert.pem"));
            let fp = tls::fingerprint(&cert)?;
            println!("{fp}");
            println!(
                "SHA256: {}",
                fp.as_bytes()
                    .chunks(2)
                    .map(|p| std::str::from_utf8(p).unwrap().to_uppercase())
                    .collect::<Vec<_>>()
                    .join(":")
            );
        }
        Command::Serve(options) => {
            use tracing_subscriber::prelude::*;
            tracing_subscriber::registry()
                .with(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| "info".into()),
                )
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_ansi(false)
                        .with_target(false)
                        // Dependency TRACE logs can contain raw frames and credentials.
                        .with_filter(tracing_subscriber::filter::filter_fn(|metadata| {
                            metadata.target().starts_with("codeg_relay")
                        })),
                )
                .init();
            server::serve(options).await?;
        }
    }
    Ok(())
}
