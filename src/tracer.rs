//! High-level tracing integration for hermod
//!
//! This module provides integration with the Rust `tracing` ecosystem,
//! allowing applications to forward their traces to hermod-tracer acceptors.

use crate::forwarder::{ForwarderHandle, TraceForwarder};
use crate::protocol::{DetailLevel, Severity, TraceObject};
use chrono::Utc;
use std::sync::Arc;
use tracing::field::{Field, Visit};
use tracing::Level;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::{Layer, Registry};

/// Visitor that collects all tracing event fields into a JSON map.
struct JsonVisitor(serde_json::Map<String, serde_json::Value>);

impl JsonVisitor {
    fn new() -> Self {
        Self(serde_json::Map::new())
    }
}

impl Visit for JsonVisitor {
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.0.insert(field.name().to_string(), serde_json::json!(value));
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.0.insert(field.name().to_string(), serde_json::json!(value));
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0.insert(field.name().to_string(), serde_json::json!(value));
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.0.insert(field.name().to_string(), serde_json::json!(value));
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().to_string(), serde_json::json!(value));
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0.insert(field.name().to_string(), serde_json::json!(format!("{value:?}")));
    }
    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.0.insert(field.name().to_string(), serde_json::json!(value.to_string()));
    }
}

/// Builder for creating a trace forwarder with tracing integration
pub struct TracerBuilder {
    forwarder: TraceForwarder,
    hostname: String,
    namespace_prefix: Vec<String>,
}

impl TracerBuilder {
    /// Create a new tracer builder from a forwarder
    pub fn new(forwarder: TraceForwarder) -> Self {
        let hostname = hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_else(|| "unknown".to_string());

        Self {
            forwarder,
            hostname,
            namespace_prefix: Vec::new(),
        }
    }

    /// Set the hostname for traces
    pub fn with_hostname(mut self, hostname: String) -> Self {
        self.hostname = hostname;
        self
    }

    /// Set a namespace prefix for all traces
    pub fn with_namespace_prefix(mut self, prefix: Vec<String>) -> Self {
        self.namespace_prefix = prefix;
        self
    }

    /// Build the tracer layer and spawn the forwarder task
    ///
    /// Returns a tuple of (layer, forwarder_handle) where:
    /// - layer: A tracing Layer that can be added to a subscriber
    /// - handle: A handle for the spawned forwarder task
    pub fn build(self) -> (TraceForwarderLayer, tokio::task::JoinHandle<()>) {
        let handle = self.forwarder.handle();
        let layer = TraceForwarderLayer {
            handle: handle.clone(),
            hostname: Arc::new(self.hostname),
            namespace_prefix: Arc::new(self.namespace_prefix),
        };

        let forwarder_handle = tokio::spawn(async move {
            if let Err(e) = self.forwarder.run().await {
                tracing::error!("Forwarder error: {}", e);
            }
        });

        (layer, forwarder_handle)
    }
}

/// Tracing layer that forwards traces to a hermod-tracer acceptor
pub struct TraceForwarderLayer {
    handle: ForwarderHandle,
    hostname: Arc<String>,
    namespace_prefix: Arc<Vec<String>>,
}

impl TraceForwarderLayer {
    /// Convert tracing Level to Severity
    fn level_to_severity(level: &Level) -> Severity {
        match *level {
            Level::TRACE => Severity::Debug,
            Level::DEBUG => Severity::Debug,
            Level::INFO => Severity::Info,
            Level::WARN => Severity::Warning,
            Level::ERROR => Severity::Error,
        }
    }

    /// Build namespace from the event target.
    ///
    /// The target is split on `"::"` (Rust module paths) or `"."` (dot-separated
    /// namespaces set via `target: "Foo.Bar"`). The configured prefix is prepended.
    fn extract_namespace(&self, meta: &tracing::Metadata<'_>) -> Vec<String> {
        let mut namespace = self.namespace_prefix.as_ref().clone();
        let target = meta.target();
        let segments: Vec<String> = if target.contains("::") {
            target.split("::").map(|s| s.to_string()).collect()
        } else {
            target.split('.').filter(|s| !s.is_empty()).map(|s| s.to_string()).collect()
        };
        namespace.extend(segments);
        namespace
    }
}

impl<S> Layer<S> for TraceForwarderLayer
where
    S: tracing::Subscriber,
{
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();

        // Collect all structured fields into a JSON map.
        let mut visitor = JsonVisitor::new();
        event.record(&mut visitor);

        // The "message" field is the human-readable log line; extract it separately.
        let human = visitor
            .0
            .get("message")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let to_machine = serde_json::to_string(&visitor.0)
            .unwrap_or_else(|_| "{}".to_string());

        let trace_obj = TraceObject {
            to_human: human,
            to_machine,
            to_namespace: self.extract_namespace(metadata),
            to_severity: Self::level_to_severity(metadata.level()),
            to_details: DetailLevel::DNormal,
            to_timestamp: Utc::now(),
            to_hostname: self.hostname.to_string(),
            to_thread_id: format!("{:?}", std::thread::current().id()),
        };

        // Send asynchronously (non-blocking)
        let _ = self.handle.try_send(trace_obj);
    }
}

/// Helper to create a tracing subscriber with hermod forwarding
pub fn init_tracing_with_forwarder(
    forwarder: TraceForwarder,
) -> (impl tracing::Subscriber, tokio::task::JoinHandle<()>) {
    let builder = TracerBuilder::new(forwarder);
    let (layer, handle) = builder.build();

    let subscriber = Registry::default().with(layer);

    (subscriber, handle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forwarder::ForwarderConfig;

    #[test]
    fn test_level_to_severity() {
        assert_eq!(
            TraceForwarderLayer::level_to_severity(&Level::INFO),
            Severity::Info
        );
        assert_eq!(
            TraceForwarderLayer::level_to_severity(&Level::ERROR),
            Severity::Error
        );
    }

    #[test]
    fn test_tracer_builder() {
        let config = ForwarderConfig::default();
        let forwarder = TraceForwarder::new(config);
        let builder = TracerBuilder::new(forwarder);

        assert!(!builder.hostname.is_empty());
        assert_eq!(builder.namespace_prefix.len(), 0);
    }
}
