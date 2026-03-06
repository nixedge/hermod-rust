//! Simple example of using hermod
//!
//! This example demonstrates how to:
//! 1. Create a trace forwarder
//! 2. Integrate with Rust tracing
//! 3. Send traces to hermod-tracer

use hermod::forwarder::{ForwarderConfig, TraceForwarder};
use hermod::tracer::init_tracing_with_forwarder;
use std::path::PathBuf;
use tracing::{error, info, warn};
use tracing_subscriber::util::SubscriberInitExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Configure the forwarder
    let config = ForwarderConfig {
        socket_path: PathBuf::from("/tmp/hermod-tracer.sock"),
        queue_size: 1000,
        max_reconnect_delay: 45,
    };

    // Create the forwarder
    let forwarder = TraceForwarder::new(config);

    // Initialize tracing with forwarder integration
    let (subscriber, forwarder_handle) = init_tracing_with_forwarder(forwarder);
    subscriber.init();

    info!("Starting example application");
    warn!("This is a warning message");
    error!("This is an error message");

    // Simulate some work
    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

    info!("Example completed");

    // Wait a bit for traces to be sent
    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

    // Note: In production you'd keep the forwarder_handle and
    // await it on shutdown
    forwarder_handle.abort();

    Ok(())
}
