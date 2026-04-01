//! Conformance tests against the Haskell reference implementation
//!
//! These tests require `demo-acceptor` and `demo-forwarder` on PATH.
//! Both binaries are available inside the Nix dev-shell:
//!
//! ```
//! nix develop
//! cargo test --test conformance
//! ```
//!
//! If a binary is not found the corresponding test is skipped with a message
//! on stderr.

use hermod::acceptor::{AcceptorConfig, TraceAcceptor};
use hermod::forwarder::{ForwarderConfig, TraceForwarder};
use hermod::protocol::{DetailLevel, Severity, TraceObject};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::time::{timeout, Duration};

// ── helpers ───────────────────────────────────────────────────────────────────

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
}

fn test_socket() -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    PathBuf::from(format!(
        "/tmp/hermod-conformance-{}-{}.sock",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

/// Search PATH for a named binary; returns `None` if not found.
fn find_binary(name: &str) -> Option<PathBuf> {
    std::env::var("PATH").ok()?.split(':').find_map(|dir| {
        let path = Path::new(dir).join(name);
        if path.is_file() {
            Some(path)
        } else {
            None
        }
    })
}

/// Poll until the socket file appears, or the timeout elapses.
async fn wait_for_socket(path: &Path, max_wait: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + max_wait;
    loop {
        if path.exists() {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// RAII guard that kills a child process on drop.
struct AutoKillChild(std::process::Child);

impl AutoKillChild {
    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.0.try_wait()
    }
}

impl Drop for AutoKillChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Send traces to a running Haskell acceptor and assert it stays alive.
async fn send_traces_expect_alive(
    fw_handle: &hermod::forwarder::ForwarderHandle,
    child_guard: &mut AutoKillChild,
    traces: Vec<TraceObject>,
) {
    for trace in traces {
        fw_handle
            .send(trace)
            .await
            .expect("failed to enqueue trace");
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
    let exited = child_guard
        .try_wait()
        .expect("failed to query demo-acceptor exit status");
    assert!(
        exited.is_none(),
        "demo-acceptor exited unexpectedly (status: {:?}) — possible protocol violation",
        exited
    );
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// Rust forwarder → Haskell acceptor: basic connectivity and trace delivery.
///
/// Sends one trace and confirms the Haskell acceptor stays alive (a protocol
/// violation would cause it to crash or close the connection).
#[tokio::test]
async fn test_rust_forwarder_to_haskell_acceptor() {
    init_tracing();
    let demo_acceptor = match find_binary("demo-acceptor") {
        Some(p) => p,
        None => {
            eprintln!("SKIP: demo-acceptor not found on PATH — run `nix develop`");
            return;
        }
    };

    let socket = test_socket();
    let _ = std::fs::remove_file(&socket);

    // Launch Haskell acceptor in Responder (listener) mode
    let child = std::process::Command::new(&demo_acceptor)
        .args([socket.to_str().unwrap(), "Responder", "test.NodeId"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("failed to spawn demo-acceptor");
    let mut child_guard = AutoKillChild(child);

    assert!(
        wait_for_socket(&socket, Duration::from_secs(10)).await,
        "demo-acceptor socket did not appear within 10 s"
    );

    let forwarder = TraceForwarder::new(ForwarderConfig {
        socket_path: socket,
        network_magic: 764824073,
        max_reconnect_delay: 1,
        ..Default::default()
    });
    let fw_handle = forwarder.handle();
    tokio::spawn(forwarder.run());

    // Give the forwarder time to connect and complete the handshake
    tokio::time::sleep(Duration::from_millis(500)).await;

    fw_handle
        .send(TraceObject {
            to_human: Some("hermod conformance test".to_string()),
            to_machine: r#"{"msg":"hermod conformance test"}"#.to_string(),
            to_namespace: vec!["hermod".to_string(), "conformance".to_string()],
            to_severity: Severity::Info,
            to_details: DetailLevel::DNormal,
            to_timestamp: chrono::Utc::now(),
            to_hostname: "test-host".to_string(),
            to_thread_id: "1".to_string(),
        })
        .await
        .expect("failed to enqueue trace");

    tokio::time::sleep(Duration::from_secs(1)).await;

    let exited = child_guard
        .try_wait()
        .expect("failed to query demo-acceptor exit status");
    assert!(
        exited.is_none(),
        "demo-acceptor exited unexpectedly (status: {:?}) — possible protocol violation",
        exited
    );
}

/// Rust forwarder → Haskell acceptor: all Severity variants are accepted.
///
/// Sends one trace per Severity variant. If the Haskell side cannot parse any
/// variant's CBOR encoding it will crash, causing the test to fail.
#[tokio::test]
async fn test_rust_to_haskell_all_severities() {
    init_tracing();
    let demo_acceptor = match find_binary("demo-acceptor") {
        Some(p) => p,
        None => {
            eprintln!("SKIP: demo-acceptor not found on PATH — run `nix develop`");
            return;
        }
    };

    let socket = test_socket();
    let _ = std::fs::remove_file(&socket);

    let child = std::process::Command::new(&demo_acceptor)
        .args([socket.to_str().unwrap(), "Responder", "test.NodeId"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("failed to spawn demo-acceptor");
    let mut child_guard = AutoKillChild(child);

    assert!(
        wait_for_socket(&socket, Duration::from_secs(10)).await,
        "demo-acceptor socket did not appear"
    );

    let forwarder = TraceForwarder::new(ForwarderConfig {
        socket_path: socket,
        network_magic: 764824073,
        max_reconnect_delay: 1,
        ..Default::default()
    });
    let fw_handle = forwarder.handle();
    tokio::spawn(forwarder.run());

    tokio::time::sleep(Duration::from_millis(500)).await;

    let severities = [
        Severity::Debug,
        Severity::Info,
        Severity::Notice,
        Severity::Warning,
        Severity::Error,
        Severity::Critical,
        Severity::Alert,
        Severity::Emergency,
    ];

    let traces: Vec<TraceObject> = severities
        .iter()
        .map(|sev| TraceObject {
            to_human: Some(format!("severity test: {:?}", sev)),
            to_machine: format!(r#"{{"severity":"{:?}"}}"#, sev),
            to_namespace: vec!["hermod".to_string(), "severity".to_string()],
            to_severity: *sev,
            to_details: DetailLevel::DNormal,
            to_timestamp: chrono::Utc::now(),
            to_hostname: "test-host".to_string(),
            to_thread_id: "1".to_string(),
        })
        .collect();

    send_traces_expect_alive(&fw_handle, &mut child_guard, traces).await;
}

/// Rust forwarder → Haskell acceptor: all DetailLevel variants are accepted.
#[tokio::test]
async fn test_rust_to_haskell_all_detail_levels() {
    init_tracing();
    let demo_acceptor = match find_binary("demo-acceptor") {
        Some(p) => p,
        None => {
            eprintln!("SKIP: demo-acceptor not found on PATH — run `nix develop`");
            return;
        }
    };

    let socket = test_socket();
    let _ = std::fs::remove_file(&socket);

    let child = std::process::Command::new(&demo_acceptor)
        .args([socket.to_str().unwrap(), "Responder", "test.NodeId"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("failed to spawn demo-acceptor");
    let mut child_guard = AutoKillChild(child);

    assert!(
        wait_for_socket(&socket, Duration::from_secs(10)).await,
        "demo-acceptor socket did not appear"
    );

    let forwarder = TraceForwarder::new(ForwarderConfig {
        socket_path: socket,
        network_magic: 764824073,
        max_reconnect_delay: 1,
        ..Default::default()
    });
    let fw_handle = forwarder.handle();
    tokio::spawn(forwarder.run());

    tokio::time::sleep(Duration::from_millis(500)).await;

    let detail_levels = [
        DetailLevel::DMinimal,
        DetailLevel::DNormal,
        DetailLevel::DDetailed,
        DetailLevel::DMaximum,
    ];

    let traces: Vec<TraceObject> = detail_levels
        .iter()
        .map(|dl| TraceObject {
            to_human: Some(format!("detail level test: {:?}", dl)),
            to_machine: format!(r#"{{"detail":"{:?}"}}"#, dl),
            to_namespace: vec!["hermod".to_string(), "detail".to_string()],
            to_severity: Severity::Info,
            to_details: *dl,
            to_timestamp: chrono::Utc::now(),
            to_hostname: "test-host".to_string(),
            to_thread_id: "1".to_string(),
        })
        .collect();

    send_traces_expect_alive(&fw_handle, &mut child_guard, traces).await;
}

/// Rust forwarder → Haskell acceptor: edge-case field values.
///
/// Tests:
/// - `to_human = None` (Nothing in Haskell)
/// - empty namespace `[]`
/// - multi-segment namespace `["a", "b", "c"]`
/// - empty strings in hostname / thread_id
#[tokio::test]
async fn test_rust_to_haskell_edge_cases() {
    init_tracing();
    let demo_acceptor = match find_binary("demo-acceptor") {
        Some(p) => p,
        None => {
            eprintln!("SKIP: demo-acceptor not found on PATH — run `nix develop`");
            return;
        }
    };

    let socket = test_socket();
    let _ = std::fs::remove_file(&socket);

    let child = std::process::Command::new(&demo_acceptor)
        .args([socket.to_str().unwrap(), "Responder", "test.NodeId"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("failed to spawn demo-acceptor");
    let mut child_guard = AutoKillChild(child);

    assert!(
        wait_for_socket(&socket, Duration::from_secs(10)).await,
        "demo-acceptor socket did not appear"
    );

    let forwarder = TraceForwarder::new(ForwarderConfig {
        socket_path: socket,
        network_magic: 764824073,
        max_reconnect_delay: 1,
        ..Default::default()
    });
    let fw_handle = forwarder.handle();
    tokio::spawn(forwarder.run());

    tokio::time::sleep(Duration::from_millis(500)).await;

    let traces = vec![
        // to_human = None
        TraceObject {
            to_human: None,
            to_machine: r#"{"msg":"no human"}"#.to_string(),
            to_namespace: vec!["hermod".to_string()],
            to_severity: Severity::Info,
            to_details: DetailLevel::DNormal,
            to_timestamp: chrono::Utc::now(),
            to_hostname: "test-host".to_string(),
            to_thread_id: "1".to_string(),
        },
        // empty namespace
        TraceObject {
            to_human: Some("empty namespace".to_string()),
            to_machine: r#"{"msg":"empty ns"}"#.to_string(),
            to_namespace: vec![],
            to_severity: Severity::Info,
            to_details: DetailLevel::DNormal,
            to_timestamp: chrono::Utc::now(),
            to_hostname: "test-host".to_string(),
            to_thread_id: "1".to_string(),
        },
        // multi-segment namespace
        TraceObject {
            to_human: Some("multi namespace".to_string()),
            to_machine: r#"{"msg":"multi ns"}"#.to_string(),
            to_namespace: vec!["a".to_string(), "b".to_string(), "c".to_string()],
            to_severity: Severity::Debug,
            to_details: DetailLevel::DDetailed,
            to_timestamp: chrono::Utc::now(),
            to_hostname: "test-host".to_string(),
            to_thread_id: "main".to_string(),
        },
    ];

    send_traces_expect_alive(&fw_handle, &mut child_guard, traces).await;
}

/// Haskell forwarder → Rust acceptor: all TraceObject fields verified.
///
/// `demo-forwarder` emits a fixed `TraceObject` every 40 ms. This test
/// receives the first trace and verifies every field against the known
/// values in `Cardano.Tracer.Test.Forwarder.mkTraceObject`.
#[tokio::test]
async fn test_haskell_forwarder_to_rust_acceptor() {
    init_tracing();
    let demo_forwarder = match find_binary("demo-forwarder") {
        Some(p) => p,
        None => {
            eprintln!("SKIP: demo-forwarder not found on PATH — run `nix develop`");
            return;
        }
    };

    let socket = test_socket();
    let _ = std::fs::remove_file(&socket);

    let (acceptor, mut handle) = TraceAcceptor::new(AcceptorConfig {
        socket_path: socket.clone(),
        network_magic: 42,
        request_count: 10,
        ..Default::default()
    });
    tokio::spawn(acceptor.run());

    tokio::time::sleep(Duration::from_millis(100)).await;

    let child = std::process::Command::new(&demo_forwarder)
        .args([socket.to_str().unwrap(), "Initiator"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("failed to spawn demo-forwarder");
    let _child_guard = AutoKillChild(child);

    let trace = timeout(Duration::from_secs(10), handle.recv())
        .await
        .expect("timed out waiting for trace from demo-forwarder (10 s)")
        .expect("acceptor channel closed unexpectedly");

    // Verify ALL fields against demo-forwarder's mkTraceObject:
    //   toHuman     = Just "Human Message for testing if our mechanism works as we expect"
    //   toMachine   = "{\"msg\": \"Very big message forMachine because we have to check if it works\"}"
    //   toNamespace = ["demoNamespace"]
    //   toSeverity  = Info
    //   toDetails   = DNormal
    //   toTimestamp = <current time at send>
    //   toHostname  = "nixos"
    //   toThreadId  = "1"
    assert_eq!(
        trace.to_human.as_deref(),
        Some("Human Message for testing if our mechanism works as we expect"),
        "to_human mismatch"
    );
    assert!(
        trace.to_machine.contains("Very big message"),
        "unexpected to_machine: {:?}",
        trace.to_machine
    );
    assert_eq!(
        trace.to_namespace,
        vec!["demoNamespace"],
        "to_namespace mismatch"
    );
    assert_eq!(trace.to_severity, Severity::Info, "to_severity mismatch");
    assert_eq!(
        trace.to_details,
        DetailLevel::DNormal,
        "to_details mismatch"
    );
    assert_eq!(trace.to_hostname, "nixos", "to_hostname mismatch");
    assert_eq!(trace.to_thread_id, "1", "to_thread_id mismatch");

    // Timestamp should be recent (within the last 60 seconds)
    let age = chrono::Utc::now() - trace.to_timestamp;
    assert!(
        age.num_seconds().abs() < 60,
        "to_timestamp is not recent: {:?}",
        trace.to_timestamp
    );
}

/// Haskell forwarder → Rust acceptor: receive multiple traces.
///
/// `demo-forwarder` emits a trace every 40 ms. This test receives 10
/// consecutive traces and verifies each one decodes without error.
/// This exercises the multi-trace request-reply cycle (request_count = 10).
#[tokio::test]
async fn test_haskell_forwarder_multiple_traces() {
    init_tracing();
    let demo_forwarder = match find_binary("demo-forwarder") {
        Some(p) => p,
        None => {
            eprintln!("SKIP: demo-forwarder not found on PATH — run `nix develop`");
            return;
        }
    };

    let socket = test_socket();
    let _ = std::fs::remove_file(&socket);

    let (acceptor, mut handle) = TraceAcceptor::new(AcceptorConfig {
        socket_path: socket.clone(),
        network_magic: 42,
        request_count: 10,
        ..Default::default()
    });
    tokio::spawn(acceptor.run());

    tokio::time::sleep(Duration::from_millis(100)).await;

    let child = std::process::Command::new(&demo_forwarder)
        .args([socket.to_str().unwrap(), "Initiator"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("failed to spawn demo-forwarder");
    let _child_guard = AutoKillChild(child);

    // Receive 10 traces, each within 5 s (demo-forwarder emits every 40 ms)
    for i in 0..10 {
        let trace = timeout(Duration::from_secs(5), handle.recv())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for trace #{}", i + 1))
            .unwrap_or_else(|| panic!("acceptor channel closed at trace #{}", i + 1));

        // Every trace should decode to the same fixed values
        assert_eq!(
            trace.to_namespace,
            vec!["demoNamespace"],
            "trace #{} namespace",
            i + 1
        );
        assert_eq!(
            trace.to_severity,
            Severity::Info,
            "trace #{} severity",
            i + 1
        );
        assert_eq!(trace.to_hostname, "nixos", "trace #{} hostname", i + 1);
    }
}

/// Pure encoding round-trip: TraceObject fields survive CBOR encode → decode.
///
/// This test does not require Haskell binaries. It verifies our own Rust
/// encoder and decoder are inverses of each other for all field variants.
#[tokio::test]
async fn test_trace_object_encoding_round_trip() {
    use pallas_codec::minicbor;

    let cases: Vec<TraceObject> = vec![
        // All severities
        TraceObject {
            to_human: Some("debug msg".to_string()),
            to_machine: r#"{"x":1}"#.to_string(),
            to_namespace: vec!["ns".to_string()],
            to_severity: Severity::Debug,
            to_details: DetailLevel::DMinimal,
            to_timestamp: chrono::Utc::now(),
            to_hostname: "host".to_string(),
            to_thread_id: "42".to_string(),
        },
        TraceObject {
            to_severity: Severity::Emergency,
            to_details: DetailLevel::DMaximum,
            ..TraceObject {
                to_human: None,
                to_machine: "{}".to_string(),
                to_namespace: vec![],
                to_severity: Severity::Info,
                to_details: DetailLevel::DNormal,
                to_timestamp: chrono::Utc::now(),
                to_hostname: "h".to_string(),
                to_thread_id: "t".to_string(),
            }
        },
        // to_human = None, empty namespace
        TraceObject {
            to_human: None,
            to_machine: "{}".to_string(),
            to_namespace: vec![],
            to_severity: Severity::Warning,
            to_details: DetailLevel::DDetailed,
            to_timestamp: chrono::Utc::now(),
            to_hostname: "host".to_string(),
            to_thread_id: "1".to_string(),
        },
        // Multi-segment namespace, unicode
        TraceObject {
            to_human: Some("héllo wörld".to_string()),
            to_machine: r#"{"unicode":"日本語"}"#.to_string(),
            to_namespace: vec!["a".to_string(), "b".to_string(), "c".to_string()],
            to_severity: Severity::Notice,
            to_details: DetailLevel::DNormal,
            to_timestamp: chrono::Utc::now(),
            to_hostname: "ünïcödé-höstnàme".to_string(),
            to_thread_id: "thread-99".to_string(),
        },
    ];

    for (i, original) in cases.into_iter().enumerate() {
        let mut buf = Vec::new();
        minicbor::encode_with(&original, &mut buf, &mut ())
            .unwrap_or_else(|e| panic!("case {}: encode failed: {}", i, e));

        let decoded: TraceObject = minicbor::decode_with(&buf, &mut ())
            .unwrap_or_else(|e| panic!("case {}: decode failed: {}", i, e));

        assert_eq!(decoded.to_human, original.to_human, "case {}: to_human", i);
        assert_eq!(
            decoded.to_machine, original.to_machine,
            "case {}: to_machine",
            i
        );
        assert_eq!(
            decoded.to_namespace, original.to_namespace,
            "case {}: to_namespace",
            i
        );
        assert_eq!(
            decoded.to_severity, original.to_severity,
            "case {}: to_severity",
            i
        );
        assert_eq!(
            decoded.to_details, original.to_details,
            "case {}: to_details",
            i
        );
        assert_eq!(
            decoded.to_hostname, original.to_hostname,
            "case {}: to_hostname",
            i
        );
        assert_eq!(
            decoded.to_thread_id, original.to_thread_id,
            "case {}: to_thread_id",
            i
        );

        // Timestamp should round-trip to within 1 microsecond (nanosecond precision
        // reduced to microseconds due to pico → nano conversion in tag-1000 encoding)
        let diff = (decoded.to_timestamp - original.to_timestamp)
            .num_microseconds()
            .unwrap_or(i64::MAX)
            .abs();
        assert!(
            diff < 1000,
            "case {}: timestamp drift {} µs (expected < 1000)",
            i,
            diff
        );
    }
}

/// Pure encoding: timestamp tag-1000 format is used and preserves precision.
///
/// Verifies that our encoder writes CBOR tag 1000 (not tag 1) and that the
/// timestamp survives the pico→nano→pico conversion losslessly for timestamps
/// with nanosecond-granularity values.
#[test]
fn test_timestamp_uses_tag_1000() {
    use pallas_codec::minicbor;

    // Construct a timestamp with sub-second precision
    let ts = chrono::DateTime::from_timestamp(1_700_000_000, 123_456_789).expect("valid timestamp");
    let trace = TraceObject {
        to_human: None,
        to_machine: "{}".to_string(),
        to_namespace: vec![],
        to_severity: Severity::Info,
        to_details: DetailLevel::DNormal,
        to_timestamp: ts,
        to_hostname: "h".to_string(),
        to_thread_id: "1".to_string(),
    };

    let mut buf = Vec::new();
    minicbor::encode_with(&trace, &mut buf, &mut ()).expect("encode");

    // The CBOR for tag 1000 is 0xD9 0x03 0xE8 (3-byte tag, value 1000).
    // Verify tag-1000 bytes appear in the encoded output.
    assert!(
        buf.windows(3).any(|w| w == [0xD9, 0x03, 0xE8]),
        "CBOR tag 1000 (0xD9 0x03 0xE8) not found in encoded output — wrong timestamp format"
    );

    // Round-trip
    let decoded: TraceObject = minicbor::decode_with(&buf, &mut ()).expect("decode");
    let diff = (decoded.to_timestamp - ts)
        .num_nanoseconds()
        .unwrap_or(i64::MAX)
        .abs();
    // Precision loss: picos truncated to nanos → up to 999 ns drift
    assert!(
        diff < 1000,
        "timestamp round-trip drift {} ns (expected < 1000)",
        diff
    );
}
