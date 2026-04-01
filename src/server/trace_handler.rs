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
use prometheus::GaugeVec;
use std::collections::HashMap;
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

    // --- Prometheus metrics from trace fields ---
    push_trace_metrics(&traces, node);
}

/// Extract numeric fields from each `TraceObject.to_machine` JSON blob and
/// push them as Prometheus gauges on the node's registry.
///
/// Metric names are built as `<namespace>_<field>` where namespace segments
/// are joined with `_`.  Non-alphanumeric characters are replaced with `_`.
fn push_trace_metrics(traces: &[TraceObject], node: &NodeState) {
    let mut cache = node.trace_gauge_cache.lock().unwrap();
    for trace in traces {
        let prefix = trace.to_namespace.join("_");
        if prefix.is_empty() {
            continue;
        }
        let map = match serde_json::from_str::<serde_json::Value>(&trace.to_machine) {
            Ok(serde_json::Value::Object(m)) => m,
            _ => continue,
        };
        for (field, value) in &map {
            let f_val = match value {
                serde_json::Value::Number(n) => match n.as_f64() {
                    Some(f) => f,
                    None => continue,
                },
                _ => continue,
            };
            let metric_name = sanitise_metric_name(&format!("{}_{}", prefix, field));
            let gauge = get_or_create_gauge(&node.registry, &mut cache, &metric_name);
            if let Some(g) = gauge {
                g.with_label_values(&[]).set(f_val);
            }
        }
    }
}

fn get_or_create_gauge(
    registry: &prometheus::Registry,
    cache: &mut HashMap<String, GaugeVec>,
    name: &str,
) -> Option<GaugeVec> {
    if let Some(g) = cache.get(name) {
        return Some(g.clone());
    }
    let opts = prometheus::Opts::new(name.to_string(), name.to_string());
    let g = GaugeVec::new(opts, &[]).ok()?;
    registry.register(Box::new(g.clone())).ok()?;
    cache.insert(name.to_string(), g.clone());
    Some(g)
}

fn sanitise_metric_name(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' })
        .collect()
}
