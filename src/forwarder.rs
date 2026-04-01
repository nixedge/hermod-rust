//! Trace forwarder client implementation
//!
//! This module implements the forwarder side of the trace-forward protocol,
//! which connects to a hermod-tracer acceptor and sends trace objects via the
//! Ouroboros Network multiplexer.

use crate::mux::{
    version_table_v1, HandshakeMessage, TraceForwardClient, PROTOCOL_DATA_POINT, PROTOCOL_EKG,
    PROTOCOL_HANDSHAKE, PROTOCOL_TRACE_OBJECT,
};
use crate::protocol::TraceObject;
use pallas_network::multiplexer::{Bearer, ChannelBuffer, Plexer};
use std::path::PathBuf;
use thiserror::Error;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

/// Errors that can occur in the forwarder
#[derive(Debug, Error)]
pub enum ForwarderError {
    /// IO error
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Multiplexer error
    #[error("Multiplexer error: {0}")]
    Multiplexer(#[from] pallas_network::multiplexer::Error),

    /// Handshake was refused by the acceptor
    #[error("Handshake refused")]
    HandshakeRefused,

    /// Unexpected message during handshake
    #[error("Unexpected handshake message")]
    UnexpectedHandshake,

    /// Connection closed unexpectedly
    #[error("Connection closed unexpectedly")]
    ConnectionClosed,

    /// Queue full (traces being dropped)
    #[error("Trace queue full, dropping traces")]
    QueueFull,
}

/// Configuration for the trace forwarder
#[derive(Debug, Clone)]
pub struct ForwarderConfig {
    /// Path to the Unix socket to connect to
    pub socket_path: PathBuf,

    /// Maximum number of traces to buffer before dropping
    pub queue_size: usize,

    /// Maximum reconnection delay in seconds
    pub max_reconnect_delay: u64,

    /// Cardano network magic (must match the acceptor's)
    pub network_magic: u64,
}

impl Default for ForwarderConfig {
    fn default() -> Self {
        Self {
            socket_path: PathBuf::from("/tmp/hermod-tracer.sock"),
            queue_size: 1000,
            max_reconnect_delay: 45,
            network_magic: 764824073,
        }
    }
}

/// Handle for sending traces to the forwarder
#[derive(Clone)]
pub struct ForwarderHandle {
    tx: mpsc::Sender<TraceObject>,
}

impl ForwarderHandle {
    /// Send a trace object
    ///
    /// Returns `Err(ForwarderError::QueueFull)` if the queue is full
    pub async fn send(&self, trace: TraceObject) -> Result<(), ForwarderError> {
        self.tx
            .send(trace)
            .await
            .map_err(|_| ForwarderError::QueueFull)
    }

    /// Try to send a trace object without waiting
    ///
    /// Returns `Err(ForwarderError::QueueFull)` if the queue is full
    pub fn try_send(&self, trace: TraceObject) -> Result<(), ForwarderError> {
        self.tx
            .try_send(trace)
            .map_err(|_| ForwarderError::QueueFull)
    }
}

/// Trace forwarder that connects to hermod-tracer
pub struct TraceForwarder {
    config: ForwarderConfig,
    rx: mpsc::Receiver<TraceObject>,
    handle: ForwarderHandle,
}

impl TraceForwarder {
    /// Create a new trace forwarder
    pub fn new(config: ForwarderConfig) -> Self {
        let (tx, rx) = mpsc::channel(config.queue_size);
        let handle = ForwarderHandle { tx };
        Self { config, rx, handle }
    }

    /// Get a handle for sending traces
    pub fn handle(&self) -> ForwarderHandle {
        self.handle.clone()
    }

    /// Run the forwarder (connects and handles protocol, reconnecting on error)
    pub async fn run(mut self) -> Result<(), ForwarderError> {
        info!("Starting trace forwarder");

        let mut reconnect_delay = 1;

        loop {
            match self.connect_and_run().await {
                Ok(()) => {
                    info!("Forwarder connection closed gracefully");
                    break Ok(());
                }
                Err(e) => {
                    error!(
                        "Forwarder error: {}, reconnecting in {}s",
                        e, reconnect_delay
                    );
                    tokio::time::sleep(tokio::time::Duration::from_secs(reconnect_delay)).await;
                    reconnect_delay = (reconnect_delay * 2).min(self.config.max_reconnect_delay);
                }
            }
        }
    }

    async fn connect_and_run(&mut self) -> Result<(), ForwarderError> {
        debug!("Connecting to {}", self.config.socket_path.display());

        debug!("Connecting to {}", self.config.socket_path.display());
        let bearer = Bearer::connect_unix(&self.config.socket_path).await?;
        info!(
            "Connected to hermod-tracer at {}",
            self.config.socket_path.display()
        );

        let mut plexer = Plexer::new(bearer);

        let handshake_channel = plexer.subscribe_client(PROTOCOL_HANDSHAKE);
        let trace_channel = plexer.subscribe_client(PROTOCOL_TRACE_OBJECT);
        let _ekg_channel = plexer.subscribe_server(PROTOCOL_EKG);
        let _datapoint_channel = plexer.subscribe_server(PROTOCOL_DATA_POINT);

        let _plexer_handle = plexer.spawn();

        // Perform handshake
        let mut hs_buf = ChannelBuffer::new(handshake_channel);
        let versions = version_table_v1(self.config.network_magic);
        hs_buf
            .send_msg_chunks(&HandshakeMessage::Propose(versions))
            .await?;
        let response: HandshakeMessage = hs_buf.recv_full_msg().await?;
        match response {
            HandshakeMessage::Accept(version, data) => {
                info!(
                    "Handshake accepted: version={}, magic={}",
                    version, data.network_magic
                );
            }
            HandshakeMessage::Refuse(_) => {
                return Err(ForwarderError::HandshakeRefused);
            }
            _ => {
                return Err(ForwarderError::UnexpectedHandshake);
            }
        }

        let mut client = TraceForwardClient::new(trace_channel);

        loop {
            // Wait for at least one trace (blocking)
            let first = match self.rx.recv().await {
                Some(t) => t,
                None => return Ok(()), // channel closed, shut down
            };

            // Drain any additional pending traces
            let mut traces = vec![first];
            while let Ok(t) = self.rx.try_recv() {
                traces.push(t);
            }

            debug!("Sending {} traces to acceptor", traces.len());

            match client.handle_request(traces).await {
                Ok(()) => {}
                Err(crate::mux::ClientError::ConnectionClosed) => {
                    info!("Acceptor sent Done, closing connection");
                    return Ok(());
                }
                Err(e) => {
                    warn!("Client error: {}", e);
                    return Err(ForwarderError::ConnectionClosed);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_forwarder_config_default() {
        let config = ForwarderConfig::default();
        assert_eq!(config.queue_size, 1000);
        assert_eq!(config.max_reconnect_delay, 45);
    }
}
