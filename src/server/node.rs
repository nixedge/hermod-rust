//! Per-node state and shared tracer state
//!
//! Every Cardano node that connects to `hermod-tracer` gets a [`NodeState`]
//! instance, which holds:
//!
//! - A unique [`NodeId`] (the socket path or `ip:port` of the connection)
//! - A URL-safe [`NodeSlug`] derived from the node's display name for Prometheus routes
//! - A dedicated [`prometheus::Registry`] that accumulates EKG metrics for
//!   that node
//! - The connection timestamp
//!
//! All active nodes are tracked in the shared [`TracerState`], which is
//! `Arc`-cloned across every connection-handling task.

use crate::server::config::TracerConfig;
use indexmap::IndexMap;
use prometheus::{GaugeVec, Registry};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::RwLock;

/// Unique identifier for a connected node (socket path or ip:port)
pub type NodeId = String;

/// URL-safe slug derived from the node's display name, used as Prometheus route segment
pub type NodeSlug = String;

/// All state associated with one connected node
pub struct NodeState {
    /// The node's connection address (internal key — not shown to users)
    pub id: NodeId,
    /// Human-friendly display name from the node's `NodeInfo` DataPoint
    /// (`niName`). Falls back to the raw `NodeId` if the DataPoint request
    /// fails or returns an empty name.
    pub name: String,
    /// URL-safe slug derived from `name`, used in Prometheus routes and as
    /// the log subdirectory name
    pub slug: NodeSlug,
    /// This node's dedicated Prometheus registry
    pub registry: Arc<Registry>,
    /// When this node connected
    pub connected_at: Instant,
    /// Cache of Prometheus gauges derived from incoming trace object fields
    pub trace_gauge_cache: Mutex<HashMap<String, GaugeVec>>,
}

impl NodeState {
    /// Create new node state.
    ///
    /// `id` is the connection address (internal key).
    /// `name` is the display name (from `NodeInfo.niName`, fallback to `id`).
    pub fn new(id: NodeId, name: String) -> Self {
        let slug = slugify(&name);
        let registry = Arc::new(Registry::new());
        NodeState {
            id,
            name,
            slug,
            registry,
            connected_at: Instant::now(),
            trace_gauge_cache: Mutex::new(HashMap::new()),
        }
    }
}

/// State shared across all connections
pub struct TracerState {
    /// All currently-connected nodes, keyed by NodeId
    pub nodes: RwLock<IndexMap<NodeId, Arc<NodeState>>>,
    /// The loaded configuration
    pub config: Arc<TracerConfig>,
}

impl TracerState {
    /// Create a new empty tracer state
    pub fn new(config: Arc<TracerConfig>) -> Self {
        TracerState {
            nodes: RwLock::new(IndexMap::new()),
            config,
        }
    }

    /// Register a node; returns the new NodeState.
    ///
    /// `name` is the display name (from `NodeInfo.niName`).  Pass the same
    /// value as `id` when no name has been resolved yet.
    pub async fn register(&self, id: NodeId, name: String) -> Arc<NodeState> {
        let node = Arc::new(NodeState::new(id.clone(), name));
        self.nodes.write().await.insert(id, node.clone());
        node
    }

    /// Remove a node by ID
    pub async fn deregister(&self, id: &NodeId) {
        self.nodes.write().await.shift_remove(id);
    }

    /// Get a snapshot of connected nodes as (name, slug) pairs.
    ///
    /// `name` is the human-friendly display name (from `NodeInfo.niName`);
    /// `slug` is the URL-safe Prometheus route segment derived from it.
    pub async fn node_list(&self) -> Vec<(String, NodeSlug)> {
        self.nodes
            .read()
            .await
            .values()
            .map(|n| (n.name.clone(), n.slug.clone()))
            .collect()
    }

    /// Look up a node by slug
    pub async fn find_by_slug(&self, slug: &str) -> Option<Arc<NodeState>> {
        self.nodes
            .read()
            .await
            .values()
            .find(|n| n.slug == slug)
            .cloned()
    }

    /// Return all currently-connected nodes
    pub async fn all_nodes(&self) -> Vec<Arc<NodeState>> {
        self.nodes.read().await.values().cloned().collect()
    }
}

/// Convert an arbitrary string into a URL-safe slug:
/// lowercase, replace non-alphanumeric chars with `-`, collapse runs of `-`.
pub fn slugify(s: &str) -> String {
    let raw: String = s
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();

    // Collapse consecutive dashes and trim leading/trailing dashes
    let mut result = String::with_capacity(raw.len());
    let mut last_was_dash = true; // skip leading dashes
    for c in raw.chars() {
        if c == '-' {
            if !last_was_dash {
                result.push('-');
                last_was_dash = true;
            }
        } else {
            result.push(c);
            last_was_dash = false;
        }
    }
    // Trim trailing dash
    if result.ends_with('-') {
        result.pop();
    }
    if result.is_empty() {
        result.push('x');
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::config::TracerConfig;

    fn make_config() -> Arc<TracerConfig> {
        Arc::new(
            TracerConfig::from_yaml(
                r#"
networkMagic: 42
network:
  tag: AcceptAt
  contents: "/tmp/hermod.sock"
logging:
- logRoot: "/tmp"
  logMode: FileMode
  logFormat: ForMachine
"#,
            )
            .unwrap(),
        )
    }

    // --- slugify ---

    #[test]
    fn test_slugify_unix_path() {
        assert_eq!(slugify("/tmp/forwarder.sock"), "tmp-forwarder-sock");
    }

    #[test]
    fn test_slugify_tcp() {
        assert_eq!(slugify("192.168.1.1:3000"), "192-168-1-1-3000");
    }

    #[test]
    fn test_slugify_already_clean() {
        assert_eq!(slugify("mynode"), "mynode");
    }

    #[test]
    fn test_slugify_empty_becomes_x() {
        assert_eq!(slugify("!!!"), "x");
    }

    #[test]
    fn slugify_collapses_consecutive_separators() {
        assert_eq!(slugify("a---b"), "a-b");
        assert_eq!(slugify("--leading"), "leading");
        assert_eq!(slugify("trailing--"), "trailing");
    }

    #[test]
    fn slugify_uppercased_is_lowercased() {
        assert_eq!(slugify("MyNode"), "mynode");
    }

    // --- NodeState ---

    #[test]
    fn node_state_slug_derived_from_name() {
        let node = NodeState::new("conn-id".to_string(), "My Node".to_string());
        assert_eq!(node.slug, "my-node");
        assert_eq!(node.name, "My Node");
        assert_eq!(node.id, "conn-id");
    }

    // --- TracerState ---

    #[tokio::test]
    async fn register_and_deregister_node() {
        let state = TracerState::new(make_config());
        state
            .register("node1".to_string(), "Node One".to_string())
            .await;
        assert_eq!(state.node_list().await.len(), 1);
        state.deregister(&"node1".to_string()).await;
        assert_eq!(state.node_list().await.len(), 0);
    }

    #[tokio::test]
    async fn find_by_slug_returns_correct_node() {
        let state = TracerState::new(make_config());
        state
            .register("node1".to_string(), "My Node".to_string())
            .await;
        let found = state.find_by_slug("my-node").await;
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "My Node");
    }

    #[tokio::test]
    async fn find_by_slug_missing_returns_none() {
        let state = TracerState::new(make_config());
        assert!(state.find_by_slug("nonexistent").await.is_none());
    }

    #[tokio::test]
    async fn node_list_returns_name_and_slug_pairs() {
        let state = TracerState::new(make_config());
        state
            .register("n1".to_string(), "Alpha".to_string())
            .await;
        state
            .register("n2".to_string(), "Beta".to_string())
            .await;
        let list = state.node_list().await;
        assert_eq!(list.len(), 2);
        assert!(list.iter().any(|(name, slug)| name == "Alpha" && slug == "alpha"));
        assert!(list.iter().any(|(name, slug)| name == "Beta" && slug == "beta"));
    }

    #[tokio::test]
    async fn all_nodes_returns_arc_node_states() {
        let state = TracerState::new(make_config());
        state
            .register("n1".to_string(), "One".to_string())
            .await;
        let all = state.all_nodes().await;
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "One");
    }

    #[tokio::test]
    async fn register_overwrites_existing_node_with_same_id() {
        let state = TracerState::new(make_config());
        state
            .register("n1".to_string(), "First".to_string())
            .await;
        state
            .register("n1".to_string(), "Second".to_string())
            .await;
        let list = state.node_list().await;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].0, "Second");
    }
}
