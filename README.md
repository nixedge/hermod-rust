# hermod

Rust implementation of the hermod-tracer trace-forward protocol. This library allows Rust applications (like alternative Cardano nodes and tooling) to forward their traces to the standard `hermod-tracer` infrastructure for monitoring and analysis.

## Overview

The trace-forward protocol enables distributed systems to send structured log messages (traces) to a centralized tracer service. This implementation maintains **full wire-protocol compatibility** with the existing Haskell implementation used in `hermod-tracing`, ensuring that Rust applications can seamlessly integrate with the existing benchmarking and monitoring infrastructure.

## Features

- **Wire-compatible**: Identical CBOR encoding to the Haskell implementation
- **Async I/O**: Built on Tokio for high-performance async networking
- **Tracing integration**: Works with the Rust `tracing` ecosystem
- **Automatic reconnection**: Handles connection failures with exponential backoff
- **Buffering**: Queues traces during disconnections to prevent loss

## Architecture

The protocol has two main roles:

- **Acceptor**: The `hermod-tracer` service that receives traces (acts as a "client" in protocol terms)
- **Forwarder**: Your Rust application that sends traces (acts as a "server" in protocol terms)

The forwarder connects to the acceptor via Unix socket and responds to requests for trace objects.

## Protocol Messages

The trace-forward protocol uses CBOR encoding with three message types:

1. **MsgTraceObjectsRequest** `[3] [1] [bool] [u16]` - Acceptor requests N traces
2. **MsgTraceObjectsReply** `[2] [3] [TraceObject list]` - Forwarder sends traces
3. **MsgDone** `[1] [2]` - Acceptor terminates session

### TraceObject Structure

Each trace object contains:

- `to_human`: Optional human-readable text
- `to_machine`: JSON machine-readable data
- `to_namespace`: Hierarchical namespace (e.g., `["node", "chain", "block"]`)
- `to_severity`: Debug | Info | Notice | Warning | Error | Critical | Alert | Emergency
- `to_details`: Minimal | Normal | Detailed | Maximum
- `to_timestamp`: UTC timestamp
- `to_hostname`: Host generating the trace
- `to_thread_id`: Thread ID generating the trace

## Usage

### Basic Setup

```rust
use hermod::forwarder::{ForwarderConfig, TraceForwarder};
use hermod::tracer::init_tracing_with_forwarder;
use std::path::PathBuf;
use tracing::{info, error};
use tracing_subscriber::util::SubscriberInitExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Configure the forwarder
    let config = ForwarderConfig {
        socket_path: PathBuf::from("/tmp/hermod-tracer.sock"),
        queue_size: 1000,
        max_reconnect_delay: 45,
    };

    // Create forwarder and initialize tracing
    let forwarder = TraceForwarder::new(config);
    let (subscriber, forwarder_handle) = init_tracing_with_forwarder(forwarder);
    subscriber.init();

    // Use standard Rust tracing
    info!("Application started");
    error!("Something went wrong");

    // Keep forwarder_handle alive and await on shutdown
    forwarder_handle.await?;
    Ok(())
}
```

### Manual Trace Sending

For more control, you can send traces directly:

```rust
use hermod::forwarder::{ForwarderConfig, TraceForwarder};
use hermod::protocol::{TraceObject, Severity, DetailLevel};
use chrono::Utc;

let config = ForwarderConfig::default();
let forwarder = TraceForwarder::new(config);
let handle = forwarder.handle();

// Spawn the forwarder
tokio::spawn(async move {
    forwarder.run().await
});

// Send a trace
let trace = TraceObject {
    to_human: Some("User logged in".to_string()),
    to_machine: r#"{"user_id": 123, "action": "login"}"#.to_string(),
    to_namespace: vec!["auth".to_string(), "login".to_string()],
    to_severity: Severity::Info,
    to_details: DetailLevel::DNormal,
    to_timestamp: Utc::now(),
    to_hostname: "node-1".to_string(),
    to_thread_id: format!("{:?}", std::thread::current().id()),
};

handle.send(trace).await?;
```

## Integration with hermod-tracer

1. Start `hermod-tracer` with a Unix socket acceptor:

```yaml
# tracer-config.yaml
network:
  - socket: /tmp/hermod-tracer.sock
```

```bash
hermod-tracer --config tracer-config.yaml
```

2. Run your Rust application configured to connect to the same socket

3. Traces will appear in `hermod-tracer`'s output (logs, metrics, RTView, etc.)

## Testing

Run the test suite:

```bash
nix develop --command cargo test
```

## Wire Protocol Compatibility

This implementation maintains byte-level compatibility with the Haskell version by:

- Using the same CBOR encoding for all messages
- Matching the exact field order in `TraceObject`
- Using the same tag numbers and encoding schemes
- Following the same state machine transitions

This ensures Rust nodes can participate in the same monitoring infrastructure as Haskell nodes.

## Use Cases

- **Alternative Cardano implementations**: Enable Rust-based nodes to use hermod-tracer
- **Tooling and utilities**: Index builders, chain analyzers, etc.
- **Benchmarking**: Integrate with existing hermod-tracing benchmark infrastructure
- **Monitoring**: Unified observability across Haskell and Rust components

## License

Apache-2.0

## Contributing

Contributions are welcome! Please ensure:

- Wire protocol compatibility is maintained
- All tests pass
- Code follows Rust best practices
