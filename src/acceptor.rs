//! Trace acceptor — listener side of the trace-forward protocol
//!
//! Listens on a Unix socket, performs the Ouroboros Network mux handshake,
//! and drives the request-reply loop, yielding received `TraceObject`s via
//! an async channel.

use crate::mux::{
    ForwardingVersionData, HandshakeMessage, PROTOCOL_DATA_POINT, PROTOCOL_EKG, PROTOCOL_HANDSHAKE,
    PROTOCOL_TRACE_OBJECT, TraceAcceptorClient, version_table_v1,
};
use crate::protocol::TraceObject;
use pallas_network::multiplexer::{Bearer, ChannelBuffer, Plexer};
use std::path::PathBuf;
use tokio::net::UnixListener;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

/// Configuration for the trace acceptor
#[derive(Debug, Clone)]
pub struct AcceptorConfig {
    /// Path to the Unix socket to listen on
    pub socket_path: PathBuf,

    /// Network magic (must match the forwarder's)
    pub network_magic: u64,

    /// Number of traces to request per round-trip
    pub request_count: u16,

    /// Capacity of the internal trace channel
    pub channel_capacity: usize,
}

impl Default for AcceptorConfig {
    fn default() -> Self {
        Self {
            socket_path: PathBuf::from("/tmp/hermod-tracer.sock"),
            network_magic: 764824073,
            request_count: 100,
            channel_capacity: 1000,
        }
    }
}

/// Handle for receiving traces from the acceptor
pub struct AcceptorHandle {
    rx: mpsc::Receiver<TraceObject>,
}

impl AcceptorHandle {
    /// Receive the next trace object, or `None` if the acceptor has shut down
    pub async fn recv(&mut self) -> Option<TraceObject> {
        self.rx.recv().await
    }
}

/// Trace acceptor that listens for forwarder connections
pub struct TraceAcceptor {
    config: AcceptorConfig,
    tx: mpsc::Sender<TraceObject>,
}

impl TraceAcceptor {
    /// Create a new acceptor; returns the acceptor and a handle for consuming traces
    pub fn new(config: AcceptorConfig) -> (Self, AcceptorHandle) {
        let (tx, rx) = mpsc::channel(config.channel_capacity);
        let acceptor = Self { config, tx };
        let handle = AcceptorHandle { rx };
        (acceptor, handle)
    }

    /// Run the acceptor (binds the socket and loops accepting connections)
    pub async fn run(self) -> anyhow::Result<()> {
        let path = &self.config.socket_path;

        // Clean up any stale socket from a previous run
        let _ = std::fs::remove_file(path);

        let listener = UnixListener::bind(path)?;
        info!("Acceptor listening on {}", path.display());

        loop {
            let (bearer, _addr) = Bearer::accept_unix(&listener).await?;
            let tx = self.tx.clone();
            let config = self.config.clone();
            tokio::spawn(async move {
                if let Err(e) = Self::handle_connection(bearer, tx, config).await {
                    warn!("Connection handler error: {}", e);
                }
            });
        }
    }

    async fn handle_connection(
        bearer: Bearer,
        tx: mpsc::Sender<TraceObject>,
        config: AcceptorConfig,
    ) -> anyhow::Result<()> {
        let mut plexer = Plexer::new(bearer);

        // Acceptor mirrors the forwarder's subscriptions
        let handshake_channel = plexer.subscribe_server(PROTOCOL_HANDSHAKE);
        let trace_channel = plexer.subscribe_server(PROTOCOL_TRACE_OBJECT);
        let _ekg_channel = plexer.subscribe_server(PROTOCOL_EKG);
        let _datapoint_channel = plexer.subscribe_server(PROTOCOL_DATA_POINT);

        let _plexer_handle = plexer.spawn();

        // Handshake
        let mut hs_buf = ChannelBuffer::new(handshake_channel);
        let msg: HandshakeMessage = hs_buf.recv_full_msg().await?;

        match msg {
            HandshakeMessage::Propose(versions) => {
                // Pick the highest version we both support
                let our_versions = version_table_v1(config.network_magic);
                let chosen = versions
                    .keys()
                    .filter(|v| our_versions.contains_key(v))
                    .max()
                    .copied();

                match chosen {
                    Some(version) => {
                        let accept = HandshakeMessage::Accept(
                            version,
                            ForwardingVersionData {
                                network_magic: config.network_magic,
                            },
                        );
                        hs_buf.send_msg_chunks(&accept).await?;
                        debug!("Handshake accepted version {}", version);
                    }
                    None => {
                        let offered: Vec<u64> = versions.into_keys().collect();
                        let refuse = HandshakeMessage::Refuse(offered);
                        hs_buf.send_msg_chunks(&refuse).await?;
                        error!("Handshake refused: no compatible version");
                        return Ok(());
                    }
                }
            }
            other => {
                error!("Expected Propose, got {:?}", other);
                return Ok(());
            }
        }

        // Trace request loop
        let mut client = TraceAcceptorClient::new(trace_channel);
        loop {
            match client.request_traces(config.request_count).await {
                Ok(traces) => {
                    debug!("Received {} traces", traces.len());
                    for trace in traces {
                        if tx.send(trace).await.is_err() {
                            // Receiver dropped — shut down gracefully
                            return Ok(());
                        }
                    }
                }
                Err(e) => {
                    info!("Trace request loop ended: {}", e);
                    return Ok(());
                }
            }
        }
    }
}
