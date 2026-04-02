//! Trace forwarder client implementation
//!
//! This module implements the forwarder side of the trace-forward protocol,
//! which connects to a hermod-tracer acceptor and sends trace objects via the
//! Ouroboros Network multiplexer.

use crate::dispatcher::backend::datapoint::DataPointStore;
use crate::mux::{
    version_table_v1, HandshakeMessage, TraceForwardClient, PROTOCOL_DATA_POINT, PROTOCOL_EKG,
    PROTOCOL_HANDSHAKE, PROTOCOL_TRACE_OBJECT,
};
use crate::protocol::TraceObject;
use crate::server::datapoint::DataPointMessage;
use chrono::{DateTime, Utc};
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

/// Address the forwarder should connect to
#[derive(Debug, Clone)]
pub enum ForwarderAddress {
    /// Unix domain socket path
    Unix(PathBuf),
    /// TCP host and port
    Tcp(String, u16),
}

impl Default for ForwarderAddress {
    fn default() -> Self {
        ForwarderAddress::Unix(PathBuf::from("/tmp/hermod-tracer.sock"))
    }
}

impl std::fmt::Display for ForwarderAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ForwarderAddress::Unix(p) => write!(f, "{}", p.display()),
            ForwarderAddress::Tcp(host, port) => write!(f, "{}:{}", host, port),
        }
    }
}

/// Configuration for the trace forwarder
#[derive(Debug, Clone)]
pub struct ForwarderConfig {
    /// Address to connect to (Unix socket path or TCP host:port)
    pub address: ForwarderAddress,

    /// Maximum number of traces to buffer before dropping
    pub queue_size: usize,

    /// Maximum reconnection delay in seconds
    pub max_reconnect_delay: u64,

    /// Cardano network magic (must match the acceptor's)
    pub network_magic: u64,

    /// Node display name advertised via the `NodeInfo` DataPoint.
    ///
    /// When `Some`, the forwarder responds to `"NodeInfo"` DataPoint requests
    /// with `{\"niName\": name, ...}`, which `hermod-tracer` (and Haskell
    /// `cardano-tracer`) use as the node's display name, Prometheus slug, and
    /// log subdirectory name.
    ///
    /// When `None`, the acceptor falls back to the connection-address node ID
    /// (e.g. `unix-1` for the first inbound Unix socket connection).
    pub node_name: Option<String>,
}

impl Default for ForwarderConfig {
    fn default() -> Self {
        Self {
            address: ForwarderAddress::default(),
            queue_size: 1000,
            max_reconnect_delay: 45,
            network_magic: 764824073,
            node_name: None,
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
    /// When this forwarder process started (used in `NodeInfo` DataPoint replies)
    start_time: DateTime<Utc>,
    /// Optional shared data-point store (serves named data points on request)
    datapoint_store: Option<DataPointStore>,
}

impl TraceForwarder {
    /// Create a new trace forwarder
    pub fn new(config: ForwarderConfig) -> Self {
        let (tx, rx) = mpsc::channel(config.queue_size);
        let handle = ForwarderHandle { tx };
        Self {
            config,
            rx,
            handle,
            start_time: Utc::now(),
            datapoint_store: None,
        }
    }

    /// Attach a [`DataPointStore`] so the forwarder can serve named data points
    /// to the acceptor on request.
    ///
    /// The same store should be passed to [`DatapointBackend::with_store`] so
    /// that dispatched trace objects are automatically stored and served.
    pub fn with_datapoint_store(mut self, store: DataPointStore) -> Self {
        self.datapoint_store = Some(store);
        self
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
        debug!("Connecting to {}", self.config.address);
        let bearer = match &self.config.address {
            ForwarderAddress::Unix(path) => Bearer::connect_unix(path).await?,
            ForwarderAddress::Tcp(host, port) => {
                let addr = format!("{}:{}", host, port);
                Bearer::connect_tcp(&addr)
                    .await
                    .map_err(|e| std::io::Error::other(e.to_string()))?
            }
        };
        info!("Connected to hermod-tracer at {}", self.config.address);

        let mut plexer = Plexer::new(bearer);

        let handshake_channel = plexer.subscribe_client(PROTOCOL_HANDSHAKE);
        let trace_channel = plexer.subscribe_client(PROTOCOL_TRACE_OBJECT);
        let _ekg_channel = plexer.subscribe_client(PROTOCOL_EKG);
        let datapoint_channel = plexer.subscribe_client(PROTOCOL_DATA_POINT);

        let _plexer_handle = plexer.spawn();

        // Respond to DataPoint requests.
        // The acceptor requests "NodeInfo" immediately after the handshake to
        // resolve our display name.  We serialise a NodeInfo-compatible JSON
        // object so the acceptor can extract `niName` and use it as the node's
        // display name, Prometheus slug, and log subdirectory name.
        //
        // Any other named data point is looked up in the optional DataPointStore
        // (set via `with_datapoint_store`).
        let node_info_bytes: Option<Vec<u8>> = self.config.node_name.as_deref().map(|name| {
            serde_json::json!({
                "niName":            name,
                "niProtocol":        "",
                "niVersion":         env!("CARGO_PKG_VERSION"),
                "niCommit":          "",
                "niStartTime":       self.start_time,
                "niSystemStartTime": self.start_time,
            })
            .to_string()
            .into_bytes()
        });

        let dp_store = self.datapoint_store.clone();
        tokio::spawn(async move {
            let mut buf = ChannelBuffer::new(datapoint_channel);
            while let Ok(DataPointMessage::Request(names)) =
                buf.recv_full_msg::<DataPointMessage>().await
            {
                let reply = names
                    .into_iter()
                    .map(|n| {
                        let val = if n == "NodeInfo" {
                            node_info_bytes.clone()
                        } else {
                            dp_store.as_ref().and_then(|s| s.get(&n))
                        };
                        (n, val)
                    })
                    .collect();
                if buf
                    .send_msg_chunks(&DataPointMessage::Reply(reply))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });

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

    use crate::protocol::types::{DetailLevel, Severity, TraceObject};
    use chrono::Utc;

    fn make_trace() -> TraceObject {
        TraceObject {
            to_human: None,
            to_machine: "{}".to_string(),
            to_namespace: vec!["Test".to_string()],
            to_severity: Severity::Info,
            to_details: DetailLevel::DNormal,
            to_timestamp: Utc::now(),
            to_hostname: "host".to_string(),
            to_thread_id: "1".to_string(),
        }
    }

    #[test]
    fn test_forwarder_config_default() {
        let config = ForwarderConfig::default();
        assert_eq!(config.queue_size, 1000);
        assert_eq!(config.max_reconnect_delay, 45);
        assert!(matches!(config.address, ForwarderAddress::Unix(_)));
        assert!(config.node_name.is_none());
    }

    #[test]
    fn test_forwarder_address_display() {
        let unix = ForwarderAddress::Unix(PathBuf::from("/tmp/test.sock"));
        assert_eq!(unix.to_string(), "/tmp/test.sock");

        let tcp = ForwarderAddress::Tcp("127.0.0.1".to_string(), 9090);
        assert_eq!(tcp.to_string(), "127.0.0.1:9090");
    }

    #[test]
    fn try_send_succeeds_when_queue_has_space() {
        let forwarder = TraceForwarder::new(ForwarderConfig {
            queue_size: 10,
            ..Default::default()
        });
        let handle = forwarder.handle();
        assert!(handle.try_send(make_trace()).is_ok());
        // Keep forwarder alive (owns the receiver)
        drop(forwarder);
    }

    #[test]
    fn try_send_returns_queue_full_when_channel_full() {
        let forwarder = TraceForwarder::new(ForwarderConfig {
            queue_size: 1,
            ..Default::default()
        });
        let handle = forwarder.handle();
        // Fill the single-slot queue
        let _ = handle.try_send(make_trace());
        // Next send must fail
        let result = handle.try_send(make_trace());
        assert!(
            matches!(result, Err(ForwarderError::QueueFull)),
            "expected QueueFull, got {:?}",
            result
        );
        drop(forwarder);
    }

    #[test]
    fn forwarder_address_tcp_variant() {
        let addr = ForwarderAddress::Tcp("localhost".to_string(), 3001);
        assert_eq!(addr.to_string(), "localhost:3001");
    }
}
