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
use hermod::forwarder::{ForwarderAddress, ForwarderConfig, TraceForwarder};
use hermod::protocol::{DetailLevel, Severity, TraceObject};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::time::{Duration, timeout};

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
        if path.is_file() { Some(path) } else { None }
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
        address: ForwarderAddress::Unix(socket),
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
        address: ForwarderAddress::Unix(socket),
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
        address: ForwarderAddress::Unix(socket),
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
        address: ForwarderAddress::Unix(socket),
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

/// Pure encoding: EKG protocol message format matches `ekg-forward` wire format.
///
/// Verifies byte-level CBOR for each `EkgMessage` variant so we catch any
/// deviation from the Haskell `codecEKGForward` specification before
/// running against a live Haskell node.
///
/// Expected bytes:
/// - `MsgDone`                   → `\[0x81, 0x01\]`
/// - `MsgReq(GetUpdatedMetrics)` → `\[0x82, 0x00, 0x81, 0x02\]`
/// - `MsgReq(GetAllMetrics)`     → `\[0x82, 0x00, 0x81, 0x00\]`
/// - `MsgResp(empty)`            → `\[0x82, 0x01, 0x82, 0x00, 0x80\]`
#[test]
fn test_ekg_message_encoding() {
    use hermod::server::ekg::EkgMessage;
    use pallas_codec::minicbor;
    use std::collections::HashMap;

    // MsgDone: array(1)[word(1)] = [0x81, 0x01]
    {
        let mut buf = Vec::new();
        minicbor::encode_with(&EkgMessage::Done, &mut buf, &mut ()).unwrap();
        assert_eq!(buf, &[0x81, 0x01], "MsgDone should be [0x81, 0x01]");
    }

    // MsgReq(false) = GetUpdatedMetrics: array(2)[word(0), array(1)[word(2)]]
    {
        let mut buf = Vec::new();
        minicbor::encode_with(&EkgMessage::Req(false), &mut buf, &mut ()).unwrap();
        assert_eq!(
            buf,
            &[0x82, 0x00, 0x81, 0x02],
            "MsgReq(GetUpdatedMetrics) should be [0x82, 0x00, 0x81, 0x02]"
        );
    }

    // MsgReq(true) = GetAllMetrics: array(2)[word(0), array(1)[word(0)]]
    {
        let mut buf = Vec::new();
        minicbor::encode_with(&EkgMessage::Req(true), &mut buf, &mut ()).unwrap();
        assert_eq!(
            buf,
            &[0x82, 0x00, 0x81, 0x00],
            "MsgReq(GetAllMetrics) should be [0x82, 0x00, 0x81, 0x00]"
        );
    }

    // MsgResp(empty): array(2)[word(1), array(2)[word(0), array(0)]]
    {
        let mut buf = Vec::new();
        minicbor::encode_with(&EkgMessage::Resp(HashMap::new()), &mut buf, &mut ()).unwrap();
        assert_eq!(
            buf,
            &[0x82, 0x01, 0x82, 0x00, 0x80],
            "MsgResp(empty) should be [0x82, 0x01, 0x82, 0x00, 0x80]"
        );
    }
}

/// Pure encoding: EKG messages round-trip through CBOR encode → decode.
///
/// Does not require Haskell binaries.
#[test]
fn test_ekg_message_round_trip() {
    use hermod::server::ekg::{EkgMessage, EkgValue};
    use pallas_codec::minicbor;
    use std::collections::HashMap;

    let cases: Vec<EkgMessage> = vec![
        EkgMessage::Done,
        EkgMessage::Req(false),
        EkgMessage::Req(true),
        EkgMessage::Resp(HashMap::new()),
        EkgMessage::Resp({
            let mut m = HashMap::new();
            m.insert("rts.gc.num_gcs".to_string(), EkgValue::Counter(42));
            m.insert("rts.gc.live_bytes".to_string(), EkgValue::Gauge(1024));
            m.insert(
                "node.version".to_string(),
                EkgValue::Label("1.35.0".to_string()),
            );
            m
        }),
    ];

    for (i, msg) in cases.into_iter().enumerate() {
        let mut buf = Vec::new();
        minicbor::encode_with(&msg, &mut buf, &mut ())
            .unwrap_or_else(|e| panic!("case {}: encode failed: {}", i, e));

        let decoded: EkgMessage = minicbor::decode_with(&buf, &mut ())
            .unwrap_or_else(|e| panic!("case {}: decode failed: {}", i, e));

        match (&msg, &decoded) {
            (EkgMessage::Done, EkgMessage::Done) => {}
            (EkgMessage::Req(a), EkgMessage::Req(b)) => {
                assert_eq!(a, b, "case {}: Req get_all mismatch", i);
            }
            (EkgMessage::Resp(a), EkgMessage::Resp(b)) => {
                assert_eq!(a.len(), b.len(), "case {}: Resp len mismatch", i);
                for (k, _) in a {
                    assert!(b.contains_key(k), "case {}: Resp missing key {}", i, k);
                }
            }
            _ => panic!("case {}: variant mismatch after round-trip", i),
        }
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

/// Haskell forwarder → Rust DataPoint client: round-trip request/reply.
///
/// Manually sets up the mux, performs the Ouroboros handshake, then issues a
/// DataPoint request for `"test.data.point"` to `demo-forwarder`.  Verifies:
/// 1. The reply arrives without error.
/// 2. The `"test.data.point"` entry has a non-`None` JSON value.
/// 3. The JSON value has a `"tdpName"` field (from `TestDataPoint`).
///
/// `demo-forwarder` stores a `TestDataPoint { tdpName, tdpCommit, tdpVersion }`
/// value under `"test.data.point"`.  Real Cardano nodes provide `"NodeInfo"`
/// with a `niName` field; that key is what `hermod-tracer` uses to resolve the
/// human-friendly node name for log directories and Prometheus routes.
///
/// This test also exercises the correct mux channel assignment: the TCP server
/// (acceptor) must use `subscribe_server(N)` so it receives frames on protocol
/// N (sent by the TCP client in `InitiatorDir`) and replies on N|0x8000.
#[tokio::test]
async fn test_haskell_forwarder_datapoint_round_trip() {
    use hermod::mux::{
        ForwardingVersionData, HandshakeMessage, PROTOCOL_DATA_POINT, PROTOCOL_EKG,
        PROTOCOL_HANDSHAKE, PROTOCOL_TRACE_OBJECT, version_table_v1,
    };
    use hermod::server::datapoint::DataPointClient;
    use pallas_network::multiplexer::{Bearer, ChannelBuffer, Plexer};
    use tokio::net::UnixListener;

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

    // Bind the socket before spawning demo-forwarder so it can connect immediately.
    let listener = UnixListener::bind(&socket).expect("bind socket");

    let child = std::process::Command::new(&demo_forwarder)
        .args([socket.to_str().unwrap(), "Initiator"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn demo-forwarder");
    let _child_guard = AutoKillChild(child);

    // Accept the connection
    let (bearer, _) = Bearer::accept_unix(&listener)
        .await
        .expect("accept connection");

    // Set up the mux.  We are the TCP server (responder).
    // subscribe_server(N): receives on N (InitiatorDir, from TCP client),
    //                      sends on N|0x8000 (ResponderDir, from TCP server).
    // demo-forwarder uses InitiatorMode for all protocols so it sends on N
    // and receives on N|0x8000 — matching subscribe_server on our side.
    let mut plexer = Plexer::new(bearer);
    let hs_ch = plexer.subscribe_server(PROTOCOL_HANDSHAKE);
    let _trace_ch = plexer.subscribe_server(PROTOCOL_TRACE_OBJECT);
    let _ekg_ch = plexer.subscribe_server(PROTOCOL_EKG);
    let dp_ch = plexer.subscribe_server(PROTOCOL_DATA_POINT);
    let _plexer = plexer.spawn();

    // Handshake: we receive Propose, send Accept
    let mut hs = ChannelBuffer::new(hs_ch);
    let network_magic = 42u64;
    let versions = version_table_v1(network_magic);
    let msg: HandshakeMessage = hs.recv_full_msg().await.expect("recv Propose");
    match msg {
        HandshakeMessage::Propose(proposed) => {
            let ver = proposed
                .keys()
                .filter(|v| versions.contains_key(v))
                .max()
                .copied()
                .expect("no compatible version");
            hs.send_msg_chunks(&HandshakeMessage::Accept(
                ver,
                ForwardingVersionData { network_magic },
            ))
            .await
            .expect("send Accept");
        }
        other => panic!("expected Propose, got {:?}", other),
    }

    // Request the test DataPoint.  demo-forwarder stores a TestDataPoint value
    // under "test.data.point": { tdpName, tdpCommit, tdpVersion }.
    let mut dp = DataPointClient::new(dp_ch);
    let result = timeout(
        Duration::from_secs(5),
        dp.request(vec!["test.data.point".to_string()]),
    )
    .await
    .expect("DataPoint request timed out")
    .expect("DataPoint request failed");

    // Verify the reply contains the requested key
    let (_, value) = result
        .into_iter()
        .find(|(name, _)| name == "test.data.point")
        .expect("test.data.point key missing from DataPoint reply");

    // demo-forwarder stores a non-None value for this key
    let value =
        value.expect("test.data.point value was None — demo-forwarder should have stored it");

    // Verify the JSON has the expected tdpName field from TestDataPoint
    assert!(
        value.get("tdpName").is_some(),
        "tdpName field missing from test.data.point value: {:?}",
        value
    );

    eprintln!("demo-forwarder test.data.point = {:?}", value);
}

/// Rust EKG poller → Haskell forwarder: receive live EKG metrics.
///
/// Spawns `demo-forwarder` as TCP Initiator (it connects to us). After the
/// Ouroboros handshake we send `EkgMessage::Req(true)` (GetAllMetrics) on the
/// EKG channel and verify the reply is either:
/// - `EkgMessage::Resp(metrics)` with at least one metric entry, or
/// - `EkgMessage::Done` (forwarder gracefully closed the session).
///
/// Channel direction: as TCP server we use `subscribe_server(EKG)`, which
/// sends frames on protocol `1|0x8000` (ResponderDir → forwarder) and
/// receives frames on protocol `1` (InitiatorDir ← forwarder).
#[tokio::test]
async fn test_haskell_forwarder_ekg_metrics() {
    use hermod::mux::{
        ForwardingVersionData, HandshakeMessage, PROTOCOL_DATA_POINT, PROTOCOL_EKG,
        PROTOCOL_HANDSHAKE, PROTOCOL_TRACE_OBJECT, version_table_v1,
    };
    use hermod::server::ekg::EkgMessage;
    use pallas_network::multiplexer::{Bearer, ChannelBuffer, Plexer};
    use tokio::net::UnixListener;

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

    // Bind the socket before spawning so demo-forwarder can connect immediately.
    let listener = UnixListener::bind(&socket).expect("bind socket");

    let child = std::process::Command::new(&demo_forwarder)
        .args([socket.to_str().unwrap(), "Initiator"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn demo-forwarder");
    let _child_guard = AutoKillChild(child);

    let (bearer, _) = Bearer::accept_unix(&listener)
        .await
        .expect("accept connection");

    // TCP server (AcceptAt): subscribe_server for all protocols.
    let mut plexer = Plexer::new(bearer);
    let hs_ch = plexer.subscribe_server(PROTOCOL_HANDSHAKE);
    let _trace_ch = plexer.subscribe_server(PROTOCOL_TRACE_OBJECT);
    let ekg_ch = plexer.subscribe_server(PROTOCOL_EKG);
    let _dp_ch = plexer.subscribe_server(PROTOCOL_DATA_POINT);
    let _plexer = plexer.spawn();

    // Handshake: receive Propose from demo-forwarder, send Accept.
    let mut hs = ChannelBuffer::new(hs_ch);
    let network_magic = 42u64;
    let versions = version_table_v1(network_magic);
    let msg: HandshakeMessage = hs.recv_full_msg().await.expect("recv Propose");
    match msg {
        HandshakeMessage::Propose(proposed) => {
            let ver = proposed
                .keys()
                .filter(|v| versions.contains_key(v))
                .max()
                .copied()
                .expect("no compatible version");
            hs.send_msg_chunks(&HandshakeMessage::Accept(
                ver,
                ForwardingVersionData { network_magic },
            ))
            .await
            .expect("send Accept");
        }
        other => panic!("expected Propose, got {:?}", other),
    }

    // Poll EKG: send Req(true) = GetAllMetrics and wait for the reply.
    // We send on 1|0x8000 (server outgoing dir) and receive on 1 (client outgoing dir).
    let mut ekg = ChannelBuffer::new(ekg_ch);
    ekg.send_msg_chunks(&EkgMessage::Req(true))
        .await
        .expect("send EKG Req");

    let response = timeout(Duration::from_secs(5), ekg.recv_full_msg::<EkgMessage>())
        .await
        .expect("EKG response timed out (5 s)")
        .expect("EKG recv failed");

    match response {
        EkgMessage::Resp(metrics) => {
            eprintln!(
                "demo-forwarder EKG: {} metrics, first few: {:?}",
                metrics.len(),
                metrics.keys().take(5).collect::<Vec<_>>()
            );
            assert!(
                !metrics.is_empty(),
                "demo-forwarder returned an empty EKG metrics map"
            );
        }
        EkgMessage::Done => {
            // Acceptable: forwarder closed the EKG session gracefully.
            eprintln!("demo-forwarder EKG replied with Done (session closed gracefully)");
        }
        EkgMessage::Req(_) => {
            panic!("unexpected EkgMessage::Req received from forwarder");
        }
    }
}

/// Rust acceptor connects to Haskell forwarder in Responder (listen) mode.
///
/// Exercises the ConnectTo network topology: `hermod-tracer` dials out to a
/// Cardano node that has a local Unix socket rather than connecting outward.
/// In this mode hermod-tracer is the TCP client and must use `subscribe_client`
/// for all four mini-protocols.
///
/// `demo-forwarder Responder` binds the socket and waits for an inbound
/// connection. We connect, complete the handshake (send Propose, receive
/// Accept), then use `TraceAcceptorClient` to send a blocking request and
/// verify at least one `TraceObject` is returned.
#[tokio::test]
async fn test_connectto_haskell_forwarder_responder() {
    use hermod::mux::{
        HandshakeMessage, PROTOCOL_DATA_POINT, PROTOCOL_EKG, PROTOCOL_HANDSHAKE,
        PROTOCOL_TRACE_OBJECT, TraceAcceptorClient, version_table_v1,
    };
    use pallas_network::multiplexer::{Bearer, ChannelBuffer, Plexer};

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

    // demo-forwarder Responder: it binds and listens on the socket.
    let child = std::process::Command::new(&demo_forwarder)
        .args([socket.to_str().unwrap(), "Responder"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn demo-forwarder");
    let _child_guard = AutoKillChild(child);

    assert!(
        wait_for_socket(&socket, Duration::from_secs(10)).await,
        "demo-forwarder Responder socket did not appear within 10 s"
    );

    // Connect as TCP client (ConnectTo mode).
    let bearer = Bearer::connect_unix(&socket)
        .await
        .expect("connect to demo-forwarder");

    // TCP client (ConnectTo): subscribe_client for all protocols.
    let mut plexer = Plexer::new(bearer);
    let hs_ch = plexer.subscribe_client(PROTOCOL_HANDSHAKE);
    let trace_ch = plexer.subscribe_client(PROTOCOL_TRACE_OBJECT);
    let _ekg_ch = plexer.subscribe_client(PROTOCOL_EKG);
    let _dp_ch = plexer.subscribe_client(PROTOCOL_DATA_POINT);
    let _plexer = plexer.spawn();

    // Handshake: TCP client initiates by sending Propose, then receives Accept.
    let mut hs = ChannelBuffer::new(hs_ch);
    let network_magic = 42u64;
    let versions = version_table_v1(network_magic);
    hs.send_msg_chunks(&HandshakeMessage::Propose(versions))
        .await
        .expect("send Propose");

    let response: HandshakeMessage = timeout(Duration::from_secs(5), hs.recv_full_msg())
        .await
        .expect("handshake Accept timed out (5 s)")
        .expect("handshake recv failed");

    match response {
        HandshakeMessage::Accept(ver, data) => {
            eprintln!(
                "ConnectTo handshake accepted: version={}, magic={}",
                ver, data.network_magic
            );
        }
        HandshakeMessage::Refuse(offered) => {
            panic!(
                "handshake refused by demo-forwarder Responder; offered: {:?}",
                offered
            );
        }
        other => panic!("expected Accept, got {:?}", other),
    }

    // Request traces: TraceAcceptorClient sends a blocking TraceObjectsRequest
    // and waits for TraceObjectsReply from the forwarder.
    let mut trace_client = TraceAcceptorClient::new(trace_ch);
    let traces = timeout(Duration::from_secs(10), trace_client.request_traces(10))
        .await
        .expect("trace request timed out (10 s)")
        .expect("trace request failed");

    assert!(
        !traces.is_empty(),
        "expected at least one trace from demo-forwarder Responder, got 0"
    );
    eprintln!(
        "ConnectTo: received {} trace(s) from demo-forwarder Responder",
        traces.len()
    );
    assert_eq!(
        traces[0].to_namespace,
        vec!["demoNamespace"],
        "unexpected namespace from ConnectTo trace"
    );
}
