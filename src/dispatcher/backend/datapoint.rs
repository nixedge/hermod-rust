//! Datapoint backend — stores named data points for on-demand retrieval
//!
//! The `DatapointBackend` stores the most recent machine-JSON value for each
//! namespace key in a shared [`DataPointStore`].  The forwarder's DataPoint
//! mini-protocol handler reads from the same store when the acceptor queries a
//! named data point.
//!
//! # Usage
//!
//! ```no_run
//! use hermod::dispatcher::backend::datapoint::{DatapointBackend, DataPointStore};
//! use hermod::forwarder::TraceForwarder;
//! use std::sync::Arc;
//!
//! let store = DataPointStore::new();
//! let backend = DatapointBackend::with_store(store.clone());
//! let forwarder = TraceForwarder::new(Default::default())
//!     .with_datapoint_store(store);
//! // Wire `backend` into your Dispatcher and `forwarder` into your app.
//! ```

use super::{Backend, DispatchMessage};
use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Shared in-memory store for named data points.
///
/// Written by [`DatapointBackend::dispatch`] (keyed by the dot-joined
/// namespace of the trace object) and read by the forwarder's DataPoint
/// mini-protocol handler when the acceptor queries a named data point.
///
/// `DataPointStore` is cheap to clone — all clones share the same underlying
/// `HashMap`.
#[derive(Clone, Default)]
pub struct DataPointStore {
    inner: Arc<RwLock<HashMap<String, Vec<u8>>>>,
}

impl DataPointStore {
    /// Create a new empty store
    pub fn new() -> Self {
        Self::default()
    }

    /// Store a raw JSON value under `name`
    pub fn put(&self, name: &str, value: Vec<u8>) {
        self.inner.write().unwrap().insert(name.to_string(), value);
    }

    /// Retrieve the value stored under `name`
    pub fn get(&self, name: &str) -> Option<Vec<u8>> {
        self.inner.read().unwrap().get(name).cloned()
    }
}

/// Backend for the DataPoint protocol.
///
/// Each dispatched message is stored in the [`DataPointStore`] under its
/// dot-joined namespace key, overwriting any previous value for that key.
///
/// When no store is configured (the default created by
/// [`DispatcherBuilder::with_default_backends`]), messages are silently
/// discarded.  Use [`DatapointBackend::with_store`] with a shared
/// [`DataPointStore`] to enable full DataPoint support.
pub struct DatapointBackend {
    store: Option<DataPointStore>,
}

impl DatapointBackend {
    /// Create a no-op backend (all messages are silently discarded)
    pub fn new() -> Self {
        Self { store: None }
    }

    /// Create a backend that stores messages in `store`
    pub fn with_store(store: DataPointStore) -> Self {
        Self { store: Some(store) }
    }
}

impl Default for DatapointBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Backend for DatapointBackend {
    async fn dispatch(&self, msg: &DispatchMessage) -> Result<()> {
        if let Some(store) = &self.store {
            let key = msg.trace_object.to_namespace.join(".");
            let value = msg.trace_object.to_machine.as_bytes().to_vec();
            store.put(&key, value);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::types::{DetailLevel, Severity, TraceObject};
    use chrono::Utc;
    use serde_json::json;

    fn make_msg(namespace: Vec<&str>, machine: &str) -> DispatchMessage {
        DispatchMessage {
            trace_object: TraceObject {
                to_human: None,
                to_machine: machine.to_string(),
                to_namespace: namespace.into_iter().map(str::to_string).collect(),
                to_severity: Severity::Info,
                to_details: DetailLevel::DNormal,
                to_timestamp: Utc::now(),
                to_hostname: "host".to_string(),
                to_thread_id: "1".to_string(),
            },
            human: String::new(),
            machine: json!({}),
            metrics: vec![],
            detail: DetailLevel::DNormal,
        }
    }

    #[test]
    fn store_put_get_round_trip() {
        let store = DataPointStore::new();
        store.put("Foo.Bar", b"hello".to_vec());
        assert_eq!(store.get("Foo.Bar"), Some(b"hello".to_vec()));
    }

    #[test]
    fn store_missing_key_returns_none() {
        let store = DataPointStore::new();
        assert_eq!(store.get("Missing"), None);
    }

    #[test]
    fn store_overwrite_replaces_value() {
        let store = DataPointStore::new();
        store.put("k", b"first".to_vec());
        store.put("k", b"second".to_vec());
        assert_eq!(store.get("k"), Some(b"second".to_vec()));
    }

    #[test]
    fn store_clone_shares_underlying_data() {
        let store = DataPointStore::new();
        let clone = store.clone();
        store.put("k", b"v".to_vec());
        assert_eq!(clone.get("k"), Some(b"v".to_vec()));
    }

    #[tokio::test]
    async fn no_op_backend_discards_messages() {
        let backend = DatapointBackend::new();
        let msg = make_msg(vec!["Foo", "Bar"], r#"{"x":1}"#);
        backend.dispatch(&msg).await.unwrap();
        // No assertion — confirms no panic and Ok result
    }

    #[tokio::test]
    async fn with_store_saves_under_dotjoined_namespace() {
        let store = DataPointStore::new();
        let backend = DatapointBackend::with_store(store.clone());
        let msg = make_msg(vec!["Foo", "Bar"], r#"{"x":1}"#);
        backend.dispatch(&msg).await.unwrap();
        assert_eq!(store.get("Foo.Bar"), Some(r#"{"x":1}"#.as_bytes().to_vec()));
    }

    #[tokio::test]
    async fn with_store_single_segment_namespace() {
        let store = DataPointStore::new();
        let backend = DatapointBackend::with_store(store.clone());
        backend
            .dispatch(&make_msg(vec!["NodeInfo"], r#"{"niName":"n1"}"#))
            .await
            .unwrap();
        assert_eq!(
            store.get("NodeInfo"),
            Some(r#"{"niName":"n1"}"#.as_bytes().to_vec())
        );
    }

    #[tokio::test]
    async fn with_store_overwrites_previous_value() {
        let store = DataPointStore::new();
        let backend = DatapointBackend::with_store(store.clone());
        backend
            .dispatch(&make_msg(vec!["A"], r#"{"v":1}"#))
            .await
            .unwrap();
        backend
            .dispatch(&make_msg(vec!["A"], r#"{"v":2}"#))
            .await
            .unwrap();
        assert_eq!(store.get("A"), Some(r#"{"v":2}"#.as_bytes().to_vec()));
    }
}
