//! Integration tests wiring the acceptor and forwarder together end-to-end

use chrono::Utc;
use hermod::acceptor::{AcceptorConfig, TraceAcceptor};
use hermod::forwarder::{ForwarderConfig, TraceForwarder};
use hermod::protocol::{DetailLevel, Severity, TraceObject};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::time::{timeout, Duration};

fn test_socket() -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    PathBuf::from(format!(
        "/tmp/hermod-test-{}-{}.sock",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

fn make_trace(msg: &str) -> TraceObject {
    TraceObject {
        to_human: Some(msg.to_string()),
        to_machine: format!(r#"{{"message": "{}"}}"#, msg),
        to_namespace: vec!["hermod".to_string(), "integration-test".to_string()],
        to_severity: Severity::Info,
        to_details: DetailLevel::DNormal,
        to_timestamp: Utc::now(),
        to_hostname: "test-host".to_string(),
        to_thread_id: format!("{:?}", std::thread::current().id()),
    }
}

#[tokio::test]
async fn test_forwarder_sends_trace() {
    let socket = test_socket();

    let (acceptor, mut acceptor_handle) = TraceAcceptor::new(AcceptorConfig {
        socket_path: socket.clone(),
        network_magic: 0,
        ..Default::default()
    });
    tokio::spawn(acceptor.run());

    // Give the listener a moment to bind
    tokio::time::sleep(Duration::from_millis(50)).await;

    let forwarder = TraceForwarder::new(ForwarderConfig {
        socket_path: socket,
        network_magic: 0,
        max_reconnect_delay: 1,
        ..Default::default()
    });
    let fw_handle = forwarder.handle();
    tokio::spawn(forwarder.run());

    fw_handle.send(make_trace("hello")).await.unwrap();

    let trace = timeout(Duration::from_secs(5), acceptor_handle.recv())
        .await
        .expect("timed out waiting for trace")
        .expect("acceptor handle closed");

    assert!(
        trace.to_machine.contains("hello"),
        "expected 'hello' in to_machine, got: {}",
        trace.to_machine
    );
}

#[tokio::test]
async fn test_forwarder_sends_multiple_traces() {
    let socket = test_socket();

    let (acceptor, mut acceptor_handle) = TraceAcceptor::new(AcceptorConfig {
        socket_path: socket.clone(),
        network_magic: 0,
        ..Default::default()
    });
    tokio::spawn(acceptor.run());

    tokio::time::sleep(Duration::from_millis(50)).await;

    let forwarder = TraceForwarder::new(ForwarderConfig {
        socket_path: socket,
        network_magic: 0,
        max_reconnect_delay: 1,
        ..Default::default()
    });
    let fw_handle = forwarder.handle();
    tokio::spawn(forwarder.run());

    for i in 0..5 {
        fw_handle
            .send(make_trace(&format!("trace-{}", i)))
            .await
            .unwrap();
    }

    let mut received = Vec::new();
    for _ in 0..5 {
        let trace = timeout(Duration::from_secs(5), acceptor_handle.recv())
            .await
            .expect("timed out waiting for trace")
            .expect("acceptor handle closed");
        received.push(trace);
    }

    assert_eq!(received.len(), 5);
    for i in 0..5 {
        assert!(
            received[i].to_machine.contains(&format!("trace-{}", i)),
            "unexpected trace at index {}: {}",
            i,
            received[i].to_machine
        );
    }
}

#[tokio::test]
async fn test_tracer_layer_integration() {
    use hermod::tracer::TracerBuilder;
    use tracing_subscriber::layer::SubscriberExt;

    let socket = test_socket();

    let (acceptor, mut acceptor_handle) = TraceAcceptor::new(AcceptorConfig {
        socket_path: socket.clone(),
        network_magic: 0,
        ..Default::default()
    });
    tokio::spawn(acceptor.run());

    tokio::time::sleep(Duration::from_millis(50)).await;

    let forwarder = TraceForwarder::new(ForwarderConfig {
        socket_path: socket,
        network_magic: 0,
        max_reconnect_delay: 1,
        ..Default::default()
    });

    let (layer, _fw_task) = TracerBuilder::new(forwarder).build();

    // Use a local dispatcher scope so we don't fight with other test subscribers
    let subscriber = tracing_subscriber::registry().with(layer);
    let _guard = tracing::subscriber::set_default(subscriber);

    tracing::info!(target: "test", "from tracing");

    let trace = timeout(Duration::from_secs(5), acceptor_handle.recv())
        .await
        .expect("timed out waiting for trace")
        .expect("acceptor handle closed");

    assert!(
        trace.to_machine.contains("from tracing") || trace.to_machine.contains("test"),
        "expected trace to reference 'from tracing' or 'test', got: {}",
        trace.to_machine
    );
}
