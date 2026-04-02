//! hermod-tracer — full-featured trace acceptor for Cardano nodes
//!
//! Usage:
//!   hermod-tracer --config FILE [--state-dir DIR] [--min-severity LEVEL]
//!
//! The config file must be a Haskell-compatible cardano-tracer YAML config.

use anyhow::{Context, Result};
use hermod::server::{TracerServer, config::TracerConfig};
use std::path::PathBuf;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialise logging
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();
    let config_path = parse_config_arg(&args).context("--config FILE is required")?;

    info!("Loading config from {}", config_path.display());
    let config = TracerConfig::from_file(&config_path)?;

    let server = TracerServer::new(config);
    server.run().await
}

fn parse_config_arg(args: &[String]) -> Option<PathBuf> {
    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        if arg == "--config" {
            if let Some(path) = iter.next() {
                return Some(PathBuf::from(path));
            }
        } else if let Some(path) = arg.strip_prefix("--config=") {
            return Some(PathBuf::from(path));
        }
    }
    None
}
