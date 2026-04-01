//! hermod-acceptor — standalone acceptor binary
//!
//! Listens on a Unix socket for trace-forward connections and prints
//! received traces as JSON to stdout.
//!
//! Usage:
//!   hermod-acceptor [socket-path]
//!
//! The socket path defaults to /tmp/hermod-tracer.sock.

use hermod::acceptor::{AcceptorConfig, TraceAcceptor};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let socket_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/hermod-tracer.sock".into());

    let (acceptor, mut handle) = TraceAcceptor::new(AcceptorConfig {
        socket_path: socket_path.into(),
        ..Default::default()
    });

    tokio::spawn(acceptor.run());

    while let Some(trace) = handle.recv().await {
        println!("{}", serde_json::to_string(&trace)?);
    }

    Ok(())
}
