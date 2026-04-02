//! Full integration test with hermod-tracer using Pallas multiplexer
//!
//! This example connects to hermod-tracer via the Ouroboros Network mux protocol
//! and sends trace objects.

use chrono::Utc;
use hermod::mux::{
    HandshakeMessage, PROTOCOL_DATA_POINT, PROTOCOL_EKG, PROTOCOL_HANDSHAKE, PROTOCOL_TRACE_OBJECT,
    TraceForwardClient, version_table_v1,
};
use hermod::protocol::{DetailLevel, Message, MsgTraceObjectsReply, Severity, TraceObject};
use pallas_network::multiplexer::{Bearer, Plexer};
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::fmt::init();

    println!("=== Hermod - Mux Integration Test ===\n");

    let socket_path = PathBuf::from("/tmp/hermod-tracer.sock");
    let network_magic = 764824073; // mainnet magic

    println!("Connecting to hermod-tracer at {:?}...", socket_path);

    // Connect via Unix socket
    let bearer = Bearer::connect_unix(&socket_path).await?;
    println!("✓ Unix socket connected");

    // Create multiplexer
    let mut plexer = Plexer::new(bearer);

    // Subscribe to handshake and trace-forward channels
    let handshake_channel = plexer.subscribe_client(PROTOCOL_HANDSHAKE);
    let trace_channel = plexer.subscribe_client(PROTOCOL_TRACE_OBJECT);

    // Subscribe to EKG and DataPoint protocols as client (TCP initiator role)
    let _ekg_channel = plexer.subscribe_client(PROTOCOL_EKG);
    let _datapoint_channel = plexer.subscribe_client(PROTOCOL_DATA_POINT);

    // Spawn the multiplexer
    let _plexer_handle = plexer.spawn();
    println!("✓ Multiplexer started");

    // Perform handshake
    println!("\nPerforming trace-forward handshake...");
    let mut handshake_buf = pallas_network::multiplexer::ChannelBuffer::new(handshake_channel);

    let versions = version_table_v1(network_magic);
    let propose = HandshakeMessage::Propose(versions);

    handshake_buf.send_msg_chunks(&propose).await?;
    println!("  Sent version proposal (ForwardingV_1)");

    let response: HandshakeMessage = handshake_buf.recv_full_msg().await?;
    match response {
        HandshakeMessage::Accept(version, data) => {
            println!(
                "  ✓ Handshake accepted! Version: {}, Magic: {}",
                version, data.network_magic
            );
        }
        HandshakeMessage::Refuse(versions) => {
            eprintln!("  ✗ Handshake refused: {:?}", versions);
            return Err("Handshake refused".into());
        }
        _ => {
            eprintln!("  ✗ Unexpected handshake response");
            return Err("Unexpected handshake response".into());
        }
    }

    // Create trace-forward client
    let mut client = TraceForwardClient::new(trace_channel);
    println!("\n✓ Trace-forward client ready");

    // Create test traces
    println!("\nCreating test traces...");
    let traces = vec![
        TraceObject {
            to_human: Some("Test trace #1 - Info".to_string()),
            to_machine: r#"{"test_number": 1, "message": "Test from Rust with mux"}"#.to_string(),
            to_namespace: vec!["hermod".to_string(), "mux-test".to_string()],
            to_severity: Severity::Info,
            to_details: DetailLevel::DNormal,
            to_timestamp: Utc::now(),
            to_hostname: "rust-mux-test".to_string(),
            to_thread_id: format!("{:?}", std::thread::current().id()),
        },
        TraceObject {
            to_human: Some("Test trace #2 - Warning".to_string()),
            to_machine: r#"{"test_number": 2, "message": "Warning from mux test"}"#.to_string(),
            to_namespace: vec!["hermod".to_string(), "mux-test".to_string()],
            to_severity: Severity::Warning,
            to_details: DetailLevel::DNormal,
            to_timestamp: Utc::now(),
            to_hostname: "rust-mux-test".to_string(),
            to_thread_id: format!("{:?}", std::thread::current().id()),
        },
        TraceObject {
            to_human: Some("Test trace #3 - Error".to_string()),
            to_machine: r#"{"test_number": 3, "message": "Error from mux test"}"#.to_string(),
            to_namespace: vec!["hermod".to_string(), "mux-test".to_string()],
            to_severity: Severity::Error,
            to_details: DetailLevel::DDetailed,
            to_timestamp: Utc::now(),
            to_hostname: "rust-mux-test".to_string(),
            to_thread_id: format!("{:?}", std::thread::current().id()),
        },
    ];

    println!("  Created {} traces", traces.len());

    // Protocol loop: keep handling requests until Done is received
    // For testing, we'll handle a few requests then exit
    println!("\nEntering protocol loop...");
    let mut request_count = 0;
    const MAX_REQUESTS: usize = 3; // Handle 3 requests then exit for testing

    loop {
        println!("\nWaiting for message from hermod-tracer...");

        // Use a timeout so we can exit if no more requests come
        let msg =
            tokio::time::timeout(tokio::time::Duration::from_secs(5), client.recv_message()).await;

        match msg {
            Ok(Ok(Message::TraceObjectsRequest(req))) => {
                request_count += 1;
                println!(
                    "  [Request #{}] Received request for {} traces (blocking: {})",
                    request_count, req.number_of_trace_objects, req.blocking
                );

                // Send traces - we'll send our 3 test traces
                // In a real implementation, you'd pull from a queue or generate new traces
                println!("  Sending {} traces...", traces.len());
                let reply = Message::TraceObjectsReply(MsgTraceObjectsReply {
                    trace_objects: traces.clone(),
                });

                client.send_message(&reply).await?;
                println!("  ✓ Sent reply with {} traces!", traces.len());

                // For testing, exit after handling MAX_REQUESTS
                if request_count >= MAX_REQUESTS {
                    println!("  Handled {} requests, exiting test...", MAX_REQUESTS);
                    break;
                }
            }
            Ok(Ok(Message::Done)) => {
                println!("  Received Done message - acceptor terminated session");
                break;
            }
            Ok(Ok(_)) => {
                eprintln!("  ✗ Unexpected message");
                return Err("Unexpected message".into());
            }
            Ok(Err(e)) => {
                eprintln!("  ✗ Error receiving message: {}", e);
                return Err(e.into());
            }
            Err(_) => {
                println!("  Timeout waiting for next message - acceptor may be idle");
                break;
            }
        }
    }

    println!("\n✓ Protocol loop completed successfully!");
    println!("  Total requests handled: {}", request_count);
    println!("\nCheck /tmp/hermod-tracer-test-logs/ for received traces");

    // Give time for traces to be processed and written to disk
    println!("\nWaiting 2 seconds for traces to be written...");
    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

    Ok(())
}
