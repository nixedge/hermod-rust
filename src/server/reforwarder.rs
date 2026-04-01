//! Re-forwarding — relay received traces to a downstream acceptor
//!
//! The `ReForwarder` holds a `ForwarderHandle` and optionally filters traces
//! by namespace prefix before forwarding.

use crate::forwarder::ForwarderHandle;
use crate::protocol::TraceObject;
use tracing::warn;

/// Re-forwarder that relays traces to a downstream acceptor
pub struct ReForwarder {
    handle: ForwarderHandle,
    /// If set, only forward traces whose namespace starts with one of these prefixes
    namespace_filters: Option<Vec<Vec<String>>>,
}

impl ReForwarder {
    /// Create a new re-forwarder
    pub fn new(handle: ForwarderHandle, namespace_filters: Option<Vec<Vec<String>>>) -> Self {
        ReForwarder {
            handle,
            namespace_filters,
        }
    }

    /// Forward a batch of traces, applying namespace filters
    pub async fn forward(&self, traces: &[TraceObject]) {
        for trace in traces {
            if self.matches_filter(trace) {
                if let Err(e) = self.handle.send(trace.clone()).await {
                    warn!("ReForwarder send error: {}", e);
                }
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
