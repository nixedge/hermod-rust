//! hermod — Rust implementation of the Cardano trace-forward protocol
//!
//! This crate provides two things:
//!
//! 1. **A forwarder library** — embed in your Cardano node or any Rust application
//!    to ship traces to a running `hermod-tracer` instance via the Ouroboros Network
//!    mux protocol.
//!
//! 2. **A full tracer server** ([`server`]) — run as the `hermod-tracer` binary to
//!    accept trace connections, write log files, serve Prometheus metrics, and
//!    optionally re-forward to a downstream collector.  The server reads a
//!    Haskell-compatible YAML config, so existing `cardano-tracer` config files work
//!    unchanged.
//!
//! # Quick start — forwarding traces from your application
//!
//! ```no_run
//! use hermod::forwarder::{ForwarderConfig, TraceForwarder};
//! use hermod::tracer::TracerBuilder;
//! use std::path::PathBuf;
//! use tracing_subscriber::prelude::*;
//!
//! #[tokio::main]
//! async fn main() {
//!     let config = ForwarderConfig {
//!         socket_path: PathBuf::from("/tmp/hermod-tracer.sock"),
//!         network_magic: 764824073, // mainnet
//!         ..Default::default()
//!     };
//!     let forwarder = TraceForwarder::new(config);
//!
//!     // Build and plug into tracing-subscriber (also spawns forwarder)
//!     let (layer, _fwd) = TracerBuilder::new(forwarder).build();
//!     tracing_subscriber::registry().with(layer).init();
//!
//!     tracing::info!("Hello from hermod!");
//! }
//! ```
//!
//! # Module overview
//!
//! | Module | Description |
//! |--------|-------------|
//! | [`protocol`] | Wire types: `TraceObject`, `Severity`, `DetailLevel` and CBOR codecs |
//! | [`mux`] | Ouroboros Network mux layer — handshake and mini-protocol clients |
//! | [`forwarder`] | Async forwarder that ships traces to `hermod-tracer` |
//! | [`acceptor`] | Lightweight acceptor — receive traces over a socket |
//! | [`dispatcher`] | Route and filter traces to multiple backends |
//! | [`tracer`] | `tracing-subscriber` integration layer |
//! | [`server`] | Full `hermod-tracer` server (file logging, Prometheus, EKG, re-forwarding) |

#![warn(missing_docs)]

pub mod acceptor;
pub mod dispatcher;
pub mod forwarder;
pub mod mux;
pub mod protocol;
pub mod server;
pub mod tracer;

/// Library version
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version() {
        assert!(!VERSION.is_empty());
    }
}
