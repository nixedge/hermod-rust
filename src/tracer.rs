//! High-level tracing integration for hermod
//!
//! This module provides integration with the Rust `tracing` ecosystem,
//! allowing applications to forward their traces to hermod-tracer acceptors.

use crate::forwarder::{ForwarderHandle, TraceForwarder};
use crate::protocol::{DetailLevel, Severity, TraceObject};
use chrono::Utc;
use std::sync::Arc;
use tracing::Level;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::{Layer, Registry};

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

    /// Extract namespace from span metadata
    fn extract_namespace(&self, meta: &tracing::Metadata<'_>) -> Vec<String> {
        let mut namespace = self.namespace_prefix.as_ref().clone();

        // Add module path
        if let Some(module) = meta.module_path() {
            namespace.extend(module.split("::").map(|s| s.to_string()));
        }

        // Add span name
        namespace.push(meta.name().to_string());

        namespace
    }
}

impl<S> Layer<S> for TraceForwarderLayer
where
    S: tracing::Subscriber,
{
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();

        // Build the trace object
        let trace_obj = TraceObject {
            to_human: None, // Could be extracted from fields
            to_machine: format!(
                r#"{{"message": "{}", "target": "{}"}}"#,
                metadata.name(),
                metadata.target()
            ),
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
