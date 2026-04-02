//! Re-forwarding — relay received traces to a downstream acceptor
//!
//! Two forwarding modes:
//!
//! - **Outbound** (`AcceptAt` re-forwarding config): `hermod-tracer` connects
//!   out to the downstream acceptor's socket via a [`TraceForwarder`].  Traces
//!   are queued through the forwarder's MPSC channel.
//!
//! - **Inbound** (`ConnectTo` re-forwarding config): `hermod-tracer` listens on
//!   the given addresses and each downstream acceptor connects in.
//!   [`run_accepting_loop`] manages the listen/accept cycle.  Traces are fanned
//!   out to all active downstream connections via a broadcast channel.

use crate::forwarder::ForwarderHandle;
use crate::mux::{
    ForwardingVersionData, HandshakeMessage, PROTOCOL_DATA_POINT, PROTOCOL_EKG, PROTOCOL_HANDSHAKE,
    PROTOCOL_TRACE_OBJECT, TraceForwardClient, version_table_v1,
};
use crate::protocol::TraceObject;
use crate::server::config::Address;
use pallas_network::multiplexer::{Bearer, Plexer};
use std::sync::Arc;
use tokio::net::{TcpListener, UnixListener};
use tokio::sync::broadcast;
use tokio::task::JoinSet;
use tracing::{debug, info, warn};

// ---------------------------------------------------------------------------
// ReForwarder — the public handle used by TraceHandler
// ---------------------------------------------------------------------------

/// Re-forwarder that relays traces to a downstream acceptor
pub struct ReForwarder {
    inner: ReForwarderInner,
    /// If set, only forward traces whose namespace starts with one of these prefixes
    namespace_filters: Option<Vec<Vec<String>>>,
}

enum ReForwarderInner {
    /// Outbound: the re-forwarder dials out (AcceptAt re-forwarding config)
    Outbound(ForwarderHandle),
    /// Inbound: the re-forwarder listens; downstream dials in (ConnectTo config)
    Inbound(broadcast::Sender<Arc<Vec<TraceObject>>>),
}

impl ReForwarder {
    /// Create an outbound re-forwarder backed by a [`ForwarderHandle`]
    pub fn new(handle: ForwarderHandle, namespace_filters: Option<Vec<Vec<String>>>) -> Self {
        ReForwarder {
            inner: ReForwarderInner::Outbound(handle),
            namespace_filters,
        }
    }

    /// Create an inbound re-forwarder backed by a broadcast channel.
    ///
    /// Callers must also spawn [`run_accepting_loop`] so that downstream
    /// connections are accepted and receive from the channel.
    pub fn new_inbound(
        tx: broadcast::Sender<Arc<Vec<TraceObject>>>,
        namespace_filters: Option<Vec<Vec<String>>>,
    ) -> Self {
        ReForwarder {
            inner: ReForwarderInner::Inbound(tx),
            namespace_filters,
        }
    }

    /// Forward a batch of traces, applying namespace filters
    pub async fn forward(&self, traces: &[TraceObject]) {
        let filtered: Vec<TraceObject> = traces
            .iter()
            .filter(|t| self.matches_filter(t))
            .cloned()
            .collect();
        if filtered.is_empty() {
            return;
        }
        match &self.inner {
            ReForwarderInner::Outbound(handle) => {
                for trace in filtered {
                    if let Err(e) = handle.send(trace).await {
                        warn!("ReForwarder send error: {}", e);
                    }
                }
            }
            ReForwarderInner::Inbound(tx) => {
                // Ignore the error: it just means no receivers are currently connected
                let _ = tx.send(Arc::new(filtered));
            }
        }
    }

    fn matches_filter(&self, trace: &TraceObject) -> bool {
        let Some(filters) = &self.namespace_filters else {
            return true; // no filter → forward everything
        };
        filters
            .iter()
            .any(|prefix| trace.to_namespace.starts_with(prefix))
    }
}

// ---------------------------------------------------------------------------
// Inbound (ConnectTo) accepting loop
// ---------------------------------------------------------------------------

/// Listen on all `addrs` and forward received broadcasts to each connected
/// downstream via the trace-forward protocol.
///
/// This is the server side of re-forwarding: `hermod-tracer` listens and
/// downstream acceptors connect.  Each accepted connection performs the
/// Ouroboros handshake as TCP responder and then acts as a trace-forward
/// forwarder (waits for `TraceObjectsRequest`, responds with buffered traces).
pub async fn run_accepting_loop(
    addrs: &[Address],
    tx: broadcast::Sender<Arc<Vec<TraceObject>>>,
    network_magic: u64,
) {
    let mut set = JoinSet::new();
    for addr in addrs {
        let addr = addr.clone();
        let tx = tx.clone();
        set.spawn(async move {
            listen_and_accept(addr, tx, network_magic).await;
        });
    }
    while set.join_next().await.is_some() {}
}

async fn listen_and_accept(
    addr: Address,
    tx: broadcast::Sender<Arc<Vec<TraceObject>>>,
    network_magic: u64,
) {
    match &addr {
        Address::LocalPipe(path) => {
            let _ = std::fs::remove_file(path);
            let listener = match UnixListener::bind(path) {
                Ok(l) => l,
                Err(e) => {
                    warn!(
                        "AcceptingReForwarder: failed to bind {}: {}",
                        path.display(),
                        e
                    );
                    return;
                }
            };
            info!("AcceptingReForwarder: listening on {}", path.display());
            loop {
                match Bearer::accept_unix(&listener).await {
                    Ok((bearer, _)) => {
                        let rx = tx.subscribe();
                        tokio::spawn(handle_accepting_connection(bearer, rx, network_magic));
                    }
                    Err(e) => {
                        warn!("AcceptingReForwarder accept error: {}", e);
                        break;
                    }
                }
            }
        }
        Address::RemoteSocket(host, port) => {
            let bind_addr = format!("{}:{}", host, port);
            let listener = match TcpListener::bind(&bind_addr).await {
                Ok(l) => l,
                Err(e) => {
                    warn!(
                        "AcceptingReForwarder: failed to bind TCP {}: {}",
                        bind_addr, e
                    );
                    return;
                }
            };
            info!("AcceptingReForwarder: listening on TCP {}", bind_addr);
            loop {
                match Bearer::accept_tcp(&listener).await {
                    Ok((bearer, _)) => {
                        let rx = tx.subscribe();
                        tokio::spawn(handle_accepting_connection(bearer, rx, network_magic));
                    }
                    Err(e) => {
                        warn!("AcceptingReForwarder TCP accept error: {}", e);
                        break;
                    }
                }
            }
        }
    }
}

/// Handle one downstream connection.
///
/// Performs the Ouroboros handshake as TCP **responder** (the downstream
/// is the TCP initiator) and then acts as the trace-forward **forwarder**:
/// waits for `TraceObjectsRequest` messages and replies with buffered traces.
async fn handle_accepting_connection(
    bearer: Bearer,
    mut rx: broadcast::Receiver<Arc<Vec<TraceObject>>>,
    network_magic: u64,
) {
    let mut plexer = Plexer::new(bearer);

    // TCP responder: use subscribe_server.
    // The downstream (TCP initiator) uses subscribe_client.
    let hs_ch = plexer.subscribe_server(PROTOCOL_HANDSHAKE);
    let trace_ch = plexer.subscribe_server(PROTOCOL_TRACE_OBJECT);
    // Subscribe to EKG and DataPoint to prevent mux stalls, then drop them.
    drop(plexer.subscribe_server(PROTOCOL_EKG));
    drop(plexer.subscribe_server(PROTOCOL_DATA_POINT));
    let _plexer_handle = plexer.spawn();

    // Handshake: receive Propose, send Accept
    use pallas_network::multiplexer::ChannelBuffer;
    let mut hs = ChannelBuffer::new(hs_ch);
    let versions = version_table_v1(network_magic);
    let msg: HandshakeMessage = match hs.recv_full_msg().await {
        Ok(m) => m,
        Err(e) => {
            warn!("AcceptingReForwarder: handshake recv failed: {}", e);
            return;
        }
    };
    match msg {
        HandshakeMessage::Propose(proposed) => {
            let chosen = proposed
                .keys()
                .filter(|v| versions.contains_key(v))
                .max()
                .copied();
            match chosen {
                Some(ver) => {
                    let accept =
                        HandshakeMessage::Accept(ver, ForwardingVersionData { network_magic });
                    if let Err(e) = hs.send_msg_chunks(&accept).await {
                        warn!("AcceptingReForwarder: handshake accept send failed: {}", e);
                        return;
                    }
                    debug!("AcceptingReForwarder: handshake accepted v={}", ver);
                }
                None => {
                    let offered: Vec<u64> = proposed.into_keys().collect();
                    let _ = hs.send_msg_chunks(&HandshakeMessage::Refuse(offered)).await;
                    warn!("AcceptingReForwarder: no compatible version");
                    return;
                }
            }
        }
        other => {
            warn!("AcceptingReForwarder: expected Propose, got {:?}", other);
            return;
        }
    }

    // Trace forwarding loop: wait for traces, send on request
    let mut client = TraceForwardClient::new(trace_ch);
    loop {
        // Wait for a batch of traces from the broadcast channel
        let batch: Arc<Vec<TraceObject>> = loop {
            match rx.recv().await {
                Ok(b) => break b,
                Err(broadcast::error::RecvError::Closed) => {
                    info!("AcceptingReForwarder: broadcast channel closed");
                    return;
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!("AcceptingReForwarder: lagged by {} batches, skipping", n);
                    continue;
                }
            }
        };

        // Drain any additional immediately-available batches
        let mut traces: Vec<TraceObject> = (*batch).clone();
        while let Ok(extra) = rx.try_recv() {
            traces.extend_from_slice(&extra);
        }

        // Wait for the downstream's request, then reply
        match client.handle_request(traces).await {
            Ok(()) => {}
            Err(crate::mux::ClientError::ConnectionClosed) => {
                info!("AcceptingReForwarder: downstream sent Done");
                return;
            }
            Err(e) => {
                warn!("AcceptingReForwarder: trace error: {}", e);
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::types::{DetailLevel, Severity, TraceObject};
    use chrono::Utc;

    fn make_trace(namespace: Vec<&str>) -> TraceObject {
        TraceObject {
            to_human: None,
            to_machine: "{}".to_string(),
            to_namespace: namespace.into_iter().map(str::to_string).collect(),
            to_severity: Severity::Info,
            to_details: DetailLevel::DNormal,
            to_timestamp: Utc::now(),
            to_hostname: "host".to_string(),
            to_thread_id: "1".to_string(),
        }
    }

    #[tokio::test]
    async fn no_filter_forwards_all_traces() {
        let (tx, mut rx) = broadcast::channel(16);
        let rf = ReForwarder::new_inbound(tx, None);
        let traces = vec![make_trace(vec!["A", "B"]), make_trace(vec!["C"])];
        rf.forward(&traces).await;
        let received = rx.recv().await.unwrap();
        assert_eq!(received.len(), 2);
    }

    #[tokio::test]
    async fn prefix_filter_blocks_non_matching_namespace() {
        let (tx, mut rx) = broadcast::channel(16);
        let filters = Some(vec![vec!["Cardano".to_string(), "Node".to_string()]]);
        let rf = ReForwarder::new_inbound(tx, filters);
        let traces = vec![
            make_trace(vec!["Cardano", "Node", "Peers"]),
            make_trace(vec!["Other", "Trace"]),
        ];
        rf.forward(&traces).await;
        let received = rx.recv().await.unwrap();
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].to_namespace, vec!["Cardano", "Node", "Peers"]);
    }

    #[tokio::test]
    async fn prefix_filter_exact_match_passes() {
        let (tx, mut rx) = broadcast::channel(16);
        let filters = Some(vec![vec!["Cardano".to_string(), "Node".to_string()]]);
        let rf = ReForwarder::new_inbound(tx, filters);
        let traces = vec![make_trace(vec!["Cardano", "Node"])];
        rf.forward(&traces).await;
        let received = rx.recv().await.unwrap();
        assert_eq!(received.len(), 1);
    }

    #[tokio::test]
    async fn filter_all_out_sends_nothing() {
        let (tx, mut rx) = broadcast::channel(16);
        let filters = Some(vec![vec!["Cardano".to_string()]]);
        let rf = ReForwarder::new_inbound(tx, filters);
        let traces = vec![make_trace(vec!["Other"])];
        rf.forward(&traces).await;
        assert!(rx.try_recv().is_err(), "nothing should be broadcast");
    }

    #[tokio::test]
    async fn multiple_prefixes_any_match_passes() {
        let (tx, mut rx) = broadcast::channel(16);
        let filters = Some(vec![vec!["Cardano".to_string()], vec!["Node".to_string()]]);
        let rf = ReForwarder::new_inbound(tx, filters);
        let traces = vec![
            make_trace(vec!["Cardano", "X"]),
            make_trace(vec!["Node", "Y"]),
            make_trace(vec!["Other"]),
        ];
        rf.forward(&traces).await;
        let received = rx.recv().await.unwrap();
        assert_eq!(received.len(), 2);
    }

    #[tokio::test]
    async fn empty_input_sends_nothing() {
        let (tx, mut rx) = broadcast::channel(16);
        let rf = ReForwarder::new_inbound(tx, None);
        rf.forward(&[]).await;
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn inbound_with_no_receivers_does_not_panic() {
        let (tx, rx) = broadcast::channel::<Arc<Vec<TraceObject>>>(16);
        drop(rx); // no receivers
        let rf = ReForwarder::new_inbound(tx, None);
        // Should not panic even with no receivers
        rf.forward(&[make_trace(vec!["A"])]).await;
    }

    #[tokio::test]
    async fn inbound_broadcasts_to_multiple_receivers() {
        let (tx, mut rx1) = broadcast::channel(16);
        let mut rx2 = tx.subscribe();
        let rf = ReForwarder::new_inbound(tx, None);
        rf.forward(&[make_trace(vec!["A"])]).await;
        assert_eq!(rx1.recv().await.unwrap().len(), 1);
        assert_eq!(rx2.recv().await.unwrap().len(), 1);
    }
}
