use clap::{Args, Parser, Subcommand};
use std::{net::SocketAddr, path::PathBuf};

#[derive(Parser)]
#[command(version, about = "Single-port TLS relay for home codeg")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    Serve(Serve),
    Fingerprint {
        #[arg(long, default_value = "/var/lib/codeg-relay")]
        data_dir: PathBuf,
        #[arg(long)]
        tls_cert: Option<PathBuf>,
    },
    GenKey,
}

#[derive(Args)]
pub struct Serve {
    #[arg(long, default_value = "0.0.0.0:18443")]
    pub listen: SocketAddr,
    #[arg(long)]
    pub agent_key_file: Option<PathBuf>,
    #[arg(long, requires = "tls_key", conflicts_with_all = ["self_signed", "insecure_plaintext"])]
    pub tls_cert: Option<PathBuf>,
    #[arg(long, requires = "tls_cert")]
    pub tls_key: Option<PathBuf>,
    #[arg(long, requires = "public_host", conflicts_with = "insecure_plaintext")]
    pub self_signed: bool,
    #[arg(long, value_delimiter = ',')]
    pub public_host: Vec<String>,
    #[arg(long, default_value = "/var/lib/codeg-relay")]
    pub data_dir: PathBuf,
    #[arg(long)]
    pub insecure_plaintext: bool,
    #[arg(long, default_value_t = 512, value_parser = clap::value_parser!(u32).range(1..=512))]
    pub max_conns: u32,
    #[arg(long, default_value_t = 64, value_parser = clap::value_parser!(u32).range(1..=64))]
    pub max_conns_per_ip: u32,
}

impl Serve {
    pub fn agent_key(&self) -> anyhow::Result<String> {
        let key = match &self.agent_key_file {
            Some(path) => std::fs::read_to_string(path)?,
            None => std::env::var("CODEG_RELAY_AGENT_KEY").unwrap_or_default(),
        };
        let key = key.trim_end_matches(['\r', '\n']).to_owned();
        anyhow::ensure!(
            key.chars().count() >= 16 && key.len() <= 4096 && !key.chars().any(char::is_whitespace),
            "agent key must contain 16–4096 non-whitespace characters"
        );
        Ok(key)
    }
}
