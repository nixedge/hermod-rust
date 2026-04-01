//! Trace processing pipeline
//!
//! [`handle_traces`] is called once per batch of [`TraceObject`]s received
//! from a connected node.  It fans the batch out to every configured output:
//!
//! 1. **File logging** — writes each trace to the appropriate log file via
//!    [`LogWriter`], one file per `(node, logRoot, logFormat)` triple.
//!    `JournalMode` entries are silently skipped (not implemented on this
//!    platform).
//!
//! 2. **Re-forwarding** — if a [`ReForwarder`] is configured, passes the
//!    (optionally namespace-filtered) batch on to the downstream forwarder.

use crate::protocol::TraceObject;
use crate::server::config::LogMode;
use crate::server::logging::LogWriter;
use crate::server::node::NodeState;
use crate::server::reforwarder::ReForwarder;
use std::sync::Arc;
use tracing::warn;

/// Process a batch of received TraceObjects for one node
pub async fn handle_traces(
    traces: Vec<TraceObject>,
    node: &NodeState,
    writer: &Arc<LogWriter>,
    logging_params: &[crate::server::config::LoggingParams],
    reforwarder: Option<&ReForwarder>,
) {
    // --- File logging ---
    for params in logging_params {
        if params.log_mode == LogMode::FileMode {
            if let Err(e) = writer.write_traces(&node.id, params, &traces) {
                warn!("Log write error for node {}: {}", node.id, e);
            }
        }
        // JournalMode: not implemented on this platform; skip silently
    }

    // --- Re-forwarding ---
    if let Some(rf) = reforwarder {
        rf.forward(&traces).await;
    }
}
