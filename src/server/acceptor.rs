//! Network accept/connect loops for `hermod-tracer`
//!
//! Two operating modes, selected by [`crate::server::config::Network`]:
//!
//! - **`AcceptAt`** — bind a Unix or TCP socket and spawn a connection handler
//!   for each inbound forwarder connection (passive mode, mirrors the Haskell
//!   `cardano-tracer` default).
//!
//! - **`ConnectTo`** — dial out to one or more forwarder addresses, one task
//!   per address, each with an exponential-backoff reconnect loop (active mode,
//!   used when the node cannot accept inbound connections).
//!
//! Each accepted or established connection performs the Ouroboros handshake and
//! then drives three concurrent sub-tasks: the trace request loop, the EKG
//! polling loop, and the DataPoint idle loop.  When any sub-task exits the node
//! is deregistered and the other tasks are cancelled.

use crate::mux::{
    version_table_v1, ForwardingVersionData, HandshakeMessage, TraceAcceptorClient,
    PROTOCOL_DATA_POINT, PROTOCOL_EKG, PROTOCOL_HANDSHAKE, PROTOCOL_TRACE_OBJECT,
};
use crate::server::config::{Address, Network};
use crate::server::datapoint::DataPointClient;
use crate::server::ekg::EkgPoller;
use crate::server::logging::LogWriter;
use crate::server::node::{NodeId, TracerState};
use crate::server::reforwarder::ReForwarder;
use crate::server::trace_handler::handle_traces;
use pallas_network::multiplexer::{Bearer, ChannelBuffer, Plexer};
use std::sync::Arc;
use tokio::net::{TcpListener, UnixListener};
use tokio::task::JoinSet;
use tokio::time::Duration;
use tracing::{debug, info, warn};

/// Run the network layer based on the config's `Network` variant
pub async fn run_network(
    network: &Network,
    state: Arc<TracerState>,
    writer: Arc<LogWriter>,
    reforwarder: Option<Arc<ReForwarder>>,
) -> anyhow::Result<()> {
    match network {
        Network::AcceptAt(addr) => run_accept_server(addr, state, writer, reforwarder).await,
        Network::ConnectTo(addrs) => {
            run_connect_clients(addrs, state, writer, reforwarder).await
        }
    }
}

// ---------------------------------------------------------------------------
// AcceptAt — listen for incoming connections
// ---------------------------------------------------------------------------

async fn run_accept_server(
    addr: &Address,
    state: Arc<TracerState>,
    writer: Arc<LogWriter>,
    reforwarder: Option<Arc<ReForwarder>>,
) -> anyhow::Result<()> {
    match addr {
        Address::LocalPipe(path) => {
            let _ = std::fs::remove_file(path);
            let listener = UnixListener::bind(path)?;
            info!("Listening on Unix socket {}", path.display());
            let mut counter = 0u64;
            loop {
                let (bearer, _) = Bearer::accept_unix(&listener).await?;
                counter += 1;
                let node_id = format!("unix-{}", counter);
                spawn_handler(bearer, node_id, state.clone(), writer.clone(), reforwarder.clone());
            }
        }
        Address::RemoteSocket(host, port) => {
            let bind_addr = format!("{}:{}", host, port);
            let listener = TcpListener::bind(&bind_addr).await?;
            info!("Listening on TCP {}", bind_addr);
            loop {
                let (bearer, peer) = Bearer::accept_tcp(&listener).await?;
                let node_id = peer.to_string();
                spawn_handler(bearer, node_id, state.clone(), writer.clone(), reforwarder.clone());
            }
        }
    }
}

// ---------------------------------------------------------------------------
// ConnectTo — connect out to each forwarder
// ---------------------------------------------------------------------------

async fn run_connect_clients(
    addrs: &[Address],
    state: Arc<TracerState>,
    writer: Arc<LogWriter>,
    reforwarder: Option<Arc<ReForwarder>>,
) -> anyhow::Result<()> {
    let mut set = JoinSet::new();
    for addr in addrs {
        let addr = addr.clone();
        let state = state.clone();
        let writer = writer.clone();
        let rf = reforwarder.clone();
        set.spawn(async move {
            connect_with_retry(&addr, state, writer, rf).await;
        });
    }
    while set.join_next().await.is_some() {}
    Ok(())
}

async fn connect_with_retry(
    addr: &Address,
    state: Arc<TracerState>,
    writer: Arc<LogWriter>,
    reforwarder: Option<Arc<ReForwarder>>,
) {
    let node_id = addr.to_node_id();
    let mut delay = 1u64;
    loop {
        info!("Connecting to {}", node_id);
        let bearer_result = match addr {
            Address::LocalPipe(path) => Bearer::connect_unix(path).await.map_err(|e| e.into()),
            Address::RemoteSocket(host, port) => {
                let addr_str = format!("{}:{}", host, port);
                Bearer::connect_tcp(&addr_str)
                    .await
                    .map_err(|e| anyhow::anyhow!(e))
            }
        };

        match bearer_result {
            Ok(bearer) => {
                delay = 1; // reset on success
                if let Err(e) =
                    handle_connection(bearer, node_id.clone(), state.clone(), writer.clone(), reforwarder.clone(), true).await
                {
                    warn!("Connection to {} ended: {}", node_id, e);
                }
            }
            Err(e) => {
                warn!("Failed to connect to {}: {}, retrying in {}s", node_id, e, delay);
            }
        }

        tokio::time::sleep(Duration::from_secs(delay)).await;
        delay = (delay * 2).min(45);
    }
}

// ---------------------------------------------------------------------------
// Per-connection handler
// ---------------------------------------------------------------------------

fn spawn_handler(
    bearer: Bearer,
    node_id: NodeId,
    state: Arc<TracerState>,
    writer: Arc<LogWriter>,
    reforwarder: Option<Arc<ReForwarder>>,
) {
    tokio::spawn(async move {
        if let Err(e) =
            handle_connection(bearer, node_id.clone(), state, writer, reforwarder, false).await
        {
            warn!("Connection handler for {} ended: {}", node_id, e);
        }
    });
}

/// Main per-connection logic
///
/// `is_initiator`: true when we dialled out (ConnectTo), false when we accepted (AcceptAt).
/// Channel subscription roles flip depending on who initiated the TCP/Unix connection.
async fn handle_connection(
    bearer: Bearer,
    node_id: NodeId,
    state: Arc<TracerState>,
    writer: Arc<LogWriter>,
    reforwarder: Option<Arc<ReForwarder>>,
    is_initiator: bool,
) -> anyhow::Result<()> {
    let config = state.config.clone();
    let mut plexer = Plexer::new(bearer);

    let (handshake_ch, trace_ch, ekg_ch, dp_ch) = if is_initiator {
        // We dialled out: we are the client (subscribe_client for initiator protocols)
        // Trace-forward: acceptor is initiator → subscribe_client(TRACE_OBJECT)
        // EKG: acceptor is responder → subscribe_server(EKG)
        // DataPoint: acceptor is responder → subscribe_server(DATA_POINT)
        (
            plexer.subscribe_client(PROTOCOL_HANDSHAKE),
            plexer.subscribe_client(PROTOCOL_TRACE_OBJECT),
            plexer.subscribe_server(PROTOCOL_EKG),
            plexer.subscribe_server(PROTOCOL_DATA_POINT),
        )
    } else {
        // They dialled in: we are the server
        (
            plexer.subscribe_server(PROTOCOL_HANDSHAKE),
            plexer.subscribe_server(PROTOCOL_TRACE_OBJECT),
            plexer.subscribe_client(PROTOCOL_EKG),
            plexer.subscribe_client(PROTOCOL_DATA_POINT),
        )
    };

    let _plexer_handle = plexer.spawn();

    // Handshake
    let mut hs = ChannelBuffer::new(handshake_ch);
    let network_magic = config.network_magic as u64;
    let versions = version_table_v1(network_magic);

    if is_initiator {
        // We send Propose
        hs.send_msg_chunks(&HandshakeMessage::Propose(versions)).await?;
        let resp: HandshakeMessage = hs.recv_full_msg().await?;
        match resp {
            HandshakeMessage::Accept(ver, data) => {
                info!("Handshake accepted v={} magic={} node={}", ver, data.network_magic, node_id);
            }
            HandshakeMessage::Refuse(_) => {
                anyhow::bail!("Handshake refused by {}", node_id);
            }
            _ => anyhow::bail!("Unexpected handshake message from {}", node_id),
        }
    } else {
        // We receive Propose
        let msg: HandshakeMessage = hs.recv_full_msg().await?;
        match msg {
            HandshakeMessage::Propose(proposed) => {
                let chosen = proposed
                    .keys()
                    .filter(|v| versions.contains_key(v))
                    .max()
                    .copied();
                match chosen {
                    Some(ver) => {
                        let accept = HandshakeMessage::Accept(
                            ver,
                            ForwardingVersionData { network_magic },
                        );
                        hs.send_msg_chunks(&accept).await?;
                        debug!("Handshake accepted v={} for {}", ver, node_id);
                    }
                    None => {
                        let offered: Vec<u64> = proposed.into_keys().collect();
                        hs.send_msg_chunks(&HandshakeMessage::Refuse(offered)).await?;
                        anyhow::bail!("No compatible version with {}", node_id);
                    }
                }
            }
            other => anyhow::bail!("Expected Propose, got {:?} from {}", other, node_id),
        }
    }

    // Register node
    let node = state.register(node_id.clone()).await;
    info!("Node connected: {} (slug={})", node_id, node.slug);

    // Launch sub-tasks in a JoinSet so we can cancel on first exit
    let mut tasks: JoinSet<()> = JoinSet::new();

    // --- Trace request loop ---
    {
        let node = node.clone();
        let writer = writer.clone();
        let config = config.clone();
        let rf = reforwarder.clone();
        let logging = config.logging.clone();
        let request_count = config.lo_request_num();
        tasks.spawn(async move {
            let mut client = TraceAcceptorClient::new(trace_ch);
            loop {
                match client.request_traces(request_count).await {
                    Ok(traces) => {
                        debug!("Received {} traces from {}", traces.len(), node.id);
                        handle_traces(
                            traces,
                            &node,
                            &writer,
                            &logging,
                            rf.as_deref(),
                        )
                        .await;
                    }
                    Err(e) => {
                        info!("Trace loop ended for {}: {}", node.id, e);
                        return;
                    }
                }
            }
        });
    }

    // --- EKG polling loop ---
    if config.has_ekg.is_some() {
        let node = node.clone();
        let config = config.clone();
        tasks.spawn(async move {
            let mut poller =
                EkgPoller::new(ekg_ch, node.clone(), config.ekg_request_full.unwrap_or(false));
            poller.run_poll_loop(config.ekg_request_freq()).await;
        });
    } else {
        // Keep channel alive by dropping — nothing to poll
        drop(ekg_ch);
    }

    // --- DataPoint idle loop ---
    {
        tasks.spawn(async move {
            let client = DataPointClient::new(dp_ch);
            client.run_idle_loop().await;
        });
    }

    // Wait for the first sub-task to finish, then cancel the rest
    tasks.join_next().await;
    tasks.abort_all();

    // Deregister the node
    state.deregister(&node_id).await;
    info!("Node disconnected: {}", node_id);

    Ok(())
}
