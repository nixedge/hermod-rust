//! Trace processing pipeline
//!
//! [`handle_traces`] is called once per batch of [`TraceObject`]s received
//! from a connected node.  It fans the batch out to every configured output:
//!
//! 1. **File logging** — writes each trace to the appropriate log file via
//!    [`LogWriter`], one file per `(node, logRoot, logFormat)` triple.
//!
//! 2. **Journal logging** — on Unix/Linux, writes each trace to the systemd
//!    journal via `/run/systemd/journal/socket` using the native journal
//!    protocol.  On non-Unix platforms `JournalMode` is silently skipped.
//!
//! 3. **Re-forwarding** — if a [`ReForwarder`] is configured, passes the
//!    (optionally namespace-filtered) batch on to the downstream forwarder.

use crate::protocol::TraceObject;
use crate::protocol::types::Severity;
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
    // --- File + Journal logging ---
    for params in logging_params {
        match params.log_mode {
            LogMode::FileMode => {
                if let Err(e) = writer.write_traces(&node.name, params, &traces) {
                    warn!("Log write error for node {}: {}", node.name, e);
                }
            }
            LogMode::JournalMode => {
                #[cfg(unix)]
                for trace in &traces {
                    write_to_journal(trace, &node.name);
                }
                #[cfg(not(unix))]
                {
                    static WARNED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
                    WARNED.get_or_init(|| {
                        warn!("JournalMode is not supported on this platform; log entries are discarded");
                    });
                }
            }
        }
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
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Journald integration (Unix only)
// ---------------------------------------------------------------------------

/// Write a single trace to the systemd journal via the native journal socket.
///
/// Uses the native journald protocol over a Unix datagram socket at
/// `/run/systemd/journal/socket`.  All errors are silently ignored so that
/// a missing or full journal socket never disrupts trace processing.
///
/// Each journal entry includes:
/// - `PRIORITY` — syslog priority (0–7)
/// - `SYSLOG_IDENTIFIER` — `"hermod-tracer"`
/// - `HERMOD_NODE` — the node's display name
/// - `HERMOD_NAMESPACE` — dot-joined trace namespace
/// - `MESSAGE` — human text if available, otherwise the machine JSON
#[cfg(unix)]
fn write_to_journal(trace: &TraceObject, node_name: &str) {
    use std::os::unix::net::UnixDatagram;

    let priority = severity_to_journal_priority(trace.to_severity);
    let namespace = trace.to_namespace.join(".");
    let raw_message = trace.to_human.as_deref().unwrap_or(&trace.to_machine);
    // Replace newlines so the message stays on one line (simple key=value format)
    let message = raw_message.replace('\n', " ");

    let payload = format!(
        "PRIORITY={priority}\nSYSLOG_IDENTIFIER=hermod-tracer\n\
         HERMOD_NODE={node_name}\nHERMOD_NAMESPACE={namespace}\n\
         MESSAGE={message}\n"
    );

    if let Ok(socket) = UnixDatagram::unbound() {
        let _ = socket.send_to(payload.as_bytes(), "/run/systemd/journal/socket");
    }
}

#[cfg(unix)]
fn severity_to_journal_priority(sev: Severity) -> u8 {
    match sev {
        Severity::Debug => 7,
        Severity::Info => 6,
        Severity::Notice => 5,
        Severity::Warning => 4,
        Severity::Error => 3,
        Severity::Critical => 2,
        Severity::Alert => 1,
        Severity::Emergency => 0,
    }
}
