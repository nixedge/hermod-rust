//! Integration test: TracerServer AcceptAt + file logging
//!
//! Starts a TracerServer configured with AcceptAt (Unix socket) and file logging,
//! connects a forwarder that sends 10 traces, then asserts the log file exists
//! and contains all 10 trace entries.

use hermod::forwarder::{ForwarderConfig, TraceForwarder};
use hermod::protocol::types::{DetailLevel, Severity};
use hermod::protocol::TraceObject;
use hermod::server::{config::*, TracerServer};
use std::path::PathBuf;
use std::time::Duration;
use tokio::time::sleep;

/// Build a simple test TraceObject
fn make_trace(i: usize) -> TraceObject {
    TraceObject {
        to_human: Some(format!("test trace {}", i)),
        to_machine: format!("{{\"n\":{}}}", i),
        to_namespace: vec!["Test".to_string(), "Integration".to_string()],
        to_severity: Severity::Info,
        to_details: DetailLevel::DNormal,
        to_timestamp: chrono::Utc::now(),
        to_hostname: "localhost".to_string(),
        to_thread_id: "1".to_string(),
    }
}

#[tokio::test]
async fn test_tracer_server_file_logging() {
    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("test-tracer.sock");
    let log_root = dir.path().join("logs");

    let config = TracerConfig {
        network_magic: 42,
        network: Network::AcceptAt(Address::LocalPipe(socket_path.clone())),
        lo_request_num: Some(50),
        ekg_request_freq: None,
        has_ekg: None,
        has_prometheus: None,
        has_forwarding: None,
        logging: vec![LoggingParams {
            log_root: log_root.clone(),
            log_mode: LogMode::FileMode,
            log_format: LogFormat::ForMachine,
        }],
        rotation: None,
        verbosity: None,
        metrics_no_suffix: None,
        ekg_request_full: None,
        prometheus_labels: None,
    };

    // Start the server in the background
    let server = TracerServer::new(config);
    tokio::spawn(async move {
        let _ = server.run().await;
    });

    // Give the server time to bind the socket
    sleep(Duration::from_millis(200)).await;

    // Connect a forwarder and send 10 traces
    let fwd_config = ForwarderConfig {
        socket_path: socket_path.clone(),
        queue_size: 100,
        network_magic: 42,
        max_reconnect_delay: 5,
        node_name: Some("test-node".to_string()),
    };
    let forwarder = TraceForwarder::new(fwd_config);
    let handle = forwarder.handle();

    tokio::spawn(async move {
        let _ = forwarder.run().await;
    });

    // Give the forwarder time to connect
    sleep(Duration::from_millis(300)).await;

    // Send 10 traces
    for i in 0..10 {
        handle.send(make_trace(i)).await.expect("send trace");
    }

    // Give the server time to receive and write the traces
    sleep(Duration::from_millis(500)).await;

    // Find the log file written for the unix-1 node
    let node_dir = std::fs::read_dir(&log_root)
        .expect("log_root should exist")
        .filter_map(|e| e.ok())
        .find(|e| e.path().is_dir())
        .expect("should have a node subdirectory");

    let log_files: Vec<PathBuf> = std::fs::read_dir(node_dir.path())
        .expect("read node dir")
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name();
            let name_str = name.to_string_lossy();
            name_str.starts_with("node-") && name_str.ends_with(".json")
        })
        .map(|e| e.path())
        .collect();

    assert!(!log_files.is_empty(), "should have at least one log file");

    // Count JSON lines across all log files
    let total_lines: usize = log_files
        .iter()
        .map(|p| {
            std::fs::read_to_string(p)
                .unwrap_or_default()
                .lines()
                .filter(|l| !l.trim().is_empty())
                .count()
        })
        .sum();

    assert_eq!(
        total_lines, 10,
        "expected 10 trace lines in log files, got {}",
        total_lines
    );
}

/// Verify TracerConfig YAML parsing works for the standard Haskell example configs
#[test]
fn test_tracer_config_parses_minimal_yaml() {
    let yaml = r#"
networkMagic: 764824073
network:
  tag: AcceptAt
  contents: "/tmp/forwarder.sock"
logging:
- logRoot: "/tmp/logs"
  logMode: FileMode
  logFormat: ForMachine
"#;
    let cfg = TracerConfig::from_yaml(yaml).expect("parse minimal yaml");
    assert_eq!(cfg.network_magic, 764824073);
    assert!(matches!(cfg.network, Network::AcceptAt(_)));
}

/// Verify that the bundled example config file parses successfully
#[test]
fn test_example_config_parses() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("config/hermod-tracer.yaml");
    let cfg = TracerConfig::from_file(&path).expect("example config should parse");
    assert_eq!(cfg.network_magic, 42);
    assert!(matches!(cfg.network, Network::AcceptAt(_)));
    assert!(!cfg.logging.is_empty());
    assert!(cfg.rotation.is_some());
    assert!(cfg.has_prometheus.is_some());
    assert!(cfg.has_ekg.is_some());
}

/// Verify that the server doesn't panic when run with a ConnectTo config that fails immediately
#[tokio::test]
async fn test_tracer_server_connect_to_missing_peer() {
    let dir = tempfile::tempdir().unwrap();
    let log_root = dir.path().join("logs");
    // Use a socket path that does not exist
    let missing_sock = dir.path().join("missing.sock");

    let config = TracerConfig {
        network_magic: 42,
        network: Network::ConnectTo(vec![Address::LocalPipe(missing_sock)]),
        lo_request_num: Some(10),
        ekg_request_freq: None,
        has_ekg: None,
        has_prometheus: None,
        has_forwarding: None,
        logging: vec![LoggingParams {
            log_root,
            log_mode: LogMode::FileMode,
            log_format: LogFormat::ForMachine,
        }],
        rotation: None,
        verbosity: None,
        metrics_no_suffix: None,
        ekg_request_full: None,
        prometheus_labels: None,
    };

    let server = TracerServer::new(config);
    let handle = tokio::spawn(async move {
        let _ = server.run().await;
    });

    // Server should not panic; it will keep retrying
    sleep(Duration::from_millis(300)).await;
    assert!(!handle.is_finished(), "server should still be running");
    handle.abort();
}
