//! Trace forwarder client implementation
//!
//! This module implements the forwarder side of the trace-forward protocol,
//! which connects to a hermod-tracer acceptor and sends trace objects.

use crate::protocol::{
    TraceObject,
    codec::{CodecError, FramedTraceObjectCodec},
    messages::{Message, MsgTraceObjectsReply},
};
use futures::{SinkExt, StreamExt};
use std::path::PathBuf;
use thiserror::Error;
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio_util::codec::Framed;
use tracing::{debug, error, info, warn};

/// Errors that can occur in the forwarder
#[derive(Debug, Error)]
pub enum ForwarderError {
    /// IO error
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Codec error
    #[error("Codec error: {0}")]
    Codec(#[from] CodecError),

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
}

impl Default for ForwarderConfig {
    fn default() -> Self {
        Self {
            socket_path: PathBuf::from("/tmp/hermod-tracer.sock"),
            queue_size: 1000,
            max_reconnect_delay: 45,
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

    /// Run the forwarder (connects and handles protocol)
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

        let stream = UnixStream::connect(&self.config.socket_path).await?;
        info!(
            "Connected to hermod-tracer at {}",
            self.config.socket_path.display()
        );

        let mut framed = Framed::new(stream, FramedTraceObjectCodec::new());

        // Buffer for collecting traces to send
        let mut trace_buffer = Vec::new();

        loop {
            // Wait for a request from the acceptor
            let msg = framed
                .next()
                .await
                .ok_or(ForwarderError::ConnectionClosed)??;

            match msg {
                Message::TraceObjectsRequest(req) => {
                    debug!(
                        "Received request for {} traces (blocking: {})",
                        req.number_of_trace_objects, req.blocking
                    );

                    trace_buffer.clear();

                    // Collect requested number of traces
                    let mut count = 0;
                    while count < req.number_of_trace_objects {
                        if req.blocking && count == 0 {
                            // Blocking request must wait for at least one trace
                            match self.rx.recv().await {
                                Some(trace) => {
                                    trace_buffer.push(trace);
                                    count += 1;
                                }
                                None => return Err(ForwarderError::ConnectionClosed),
                            }
                        } else {
                            // Non-blocking or already have some traces
                            match self.rx.try_recv() {
                                Ok(trace) => {
                                    trace_buffer.push(trace);
                                    count += 1;
                                }
                                Err(mpsc::error::TryRecvError::Empty) => break,
                                Err(mpsc::error::TryRecvError::Disconnected) => {
                                    return Err(ForwarderError::ConnectionClosed);
                                }
                            }
                        }
                    }

                    debug!("Sending {} traces", trace_buffer.len());

                    // Send reply
                    let reply = Message::TraceObjectsReply(MsgTraceObjectsReply {
                        trace_objects: trace_buffer.clone(),
                    });

                    framed.send(reply).await?;
                }
                Message::Done => {
                    info!("Received Done message, closing connection");
                    return Ok(());
                }
                _ => {
                    warn!("Unexpected message from acceptor: {:?}", msg);
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
