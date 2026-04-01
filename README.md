# hermod

Rust implementation of the hermod trace-forward protocol. Allows Rust applications (such as alternative Cardano nodes and tooling) to forward structured traces to a `hermod-tracer` acceptor and to act as a trace acceptor themselves.

## Features

- **Wire-compatible**: Byte-identical CBOR encoding to the Haskell `hermod-tracing` implementation
- **Both roles**: Ship as a forwarder *or* receive as an acceptor
- **Async I/O**: Built on Tokio for high-performance async networking
- **Tracing integration**: Works with the Rust `tracing` ecosystem via `hermod::tracer`
- **Dispatcher**: Route, filter, and rate-limit traces before forwarding
- **Automatic reconnection**: Exponential backoff on connection failure
- **Standalone binary**: `hermod-acceptor` binary for drop-in use with any forwarder

## Architecture

```
Your Rust app
    │  tracing::info!("...")
    ▼
hermod::tracer           — tracing subscriber that captures Rust log events
    │
    ▼
hermod::dispatcher       — filter/route/rate-limit (optional)
    │
    ▼
hermod::forwarder        — CBOR-encodes and sends via Unix socket
    │  (trace-forward protocol over Ouroboros Network mux)
    ▼
hermod-tracer acceptor   — Haskell or hermod-acceptor binary
```

## Protocol Messages

The trace-forward protocol uses three CBOR-encoded messages:

| Message | Wire format | Direction |
|---------|-------------|-----------|
| `MsgTraceObjectsRequest` | `array(3)[1, blocking: bool, array(2)[0, count: u16]]` | Acceptor → Forwarder |
| `MsgTraceObjectsReply` | `array(2)[3, array(N)[TraceObject...]]` | Forwarder → Acceptor |
| `MsgDone` | `array(1)[2]` | Acceptor → Forwarder |

### TraceObject Fields

| Field | Type | Description |
|-------|------|-------------|
| `to_human` | `Option<String>` | Human-readable text (optional) |
| `to_machine` | `String` | Machine-readable JSON |
| `to_namespace` | `Vec<String>` | Hierarchical namespace, e.g. `["node", "chain"]` |
| `to_severity` | `Severity` | Debug / Info / Notice / Warning / Error / Critical / Alert / Emergency |
| `to_details` | `DetailLevel` | DMinimal / DNormal / DDetailed / DMaximum |
| `to_timestamp` | `DateTime<Utc>` | UTC timestamp (CBOR tag 1000) |
| `to_hostname` | `String` | Source hostname |
| `to_thread_id` | `String` | Source thread ID |

## Usage

### As a Forwarder (sending traces)

```rust
use hermod::forwarder::{ForwarderConfig, TraceForwarder};
use hermod::tracer::HermodTracerBuilder;
use std::path::PathBuf;
use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = ForwarderConfig {
        socket_path: PathBuf::from("/tmp/hermod-tracer.sock"),
        queue_size: 1000,
        max_reconnect_delay: 45,
        network_magic: 764824073,
    };

    let forwarder = TraceForwarder::new(config);
    let handle = forwarder.handle();

    // Run the forwarder in the background
    tokio::spawn(async move { forwarder.run().await });

    // Wire up the tracing subscriber
    HermodTracerBuilder::new(handle).init();

    info!("Application started");
    Ok(())
}
```

### Sending TraceObjects Directly

```rust
use hermod::forwarder::{ForwarderConfig, TraceForwarder};
use hermod::protocol::{DetailLevel, Severity, TraceObject};
use chrono::Utc;

let config = ForwarderConfig::default();
let forwarder = TraceForwarder::new(config);
let handle = forwarder.handle();

tokio::spawn(async move { forwarder.run().await });

handle.send(TraceObject {
    to_human: Some("User logged in".to_string()),
    to_machine: r#"{"user_id": 123}"#.to_string(),
    to_namespace: vec!["auth".to_string(), "login".to_string()],
    to_severity: Severity::Info,
    to_details: DetailLevel::DNormal,
    to_timestamp: Utc::now(),
    to_hostname: "node-1".to_string(),
    to_thread_id: format!("{:?}", std::thread::current().id()),
}).await?;
```

### As an Acceptor (receiving traces)

```rust
use hermod::acceptor::{AcceptorConfig, TraceAcceptor};
use std::path::PathBuf;

let config = AcceptorConfig {
    socket_path: PathBuf::from("/tmp/hermod-tracer.sock"),
    network_magic: 764824073,
    request_count: 100,
    channel_capacity: 1000,
};

let (acceptor, mut handle) = TraceAcceptor::new(config);
tokio::spawn(async move { acceptor.run().await });

while let Some(trace) = handle.recv().await {
    println!("{}: {}", trace.to_severity, trace.to_machine);
}
```

### Standalone Acceptor Binary

```bash
# Print all received traces as JSON to stdout
hermod-acceptor --socket /tmp/hermod-tracer.sock --magic 764824073
```

## Building

```bash
nix build          # build hermod-acceptor binary
nix develop        # enter dev shell with nightly Rust toolchain
cargo build
cargo test
```

## Testing

```bash
nix develop --command cargo test
```

See [TESTING.md](TESTING.md) for conformance test details.

## Wire Protocol Compatibility

This implementation maintains byte-level compatibility with the Haskell `hermod-tracing` library:

- Same CBOR encoding for all messages and types
- Haskell Generic Serialise conventions: product types as `array(N+1)[constructor_index, fields...]`, nullary enum variants as `array(1)[constructor_index]`
- `UTCTime` encoded as CBOR tag 1000 + `map(2){1: i64_secs, -12: u64_psecs}` (picoseconds)
- Indefinite-length CBOR arrays for Haskell `[a]` (non-empty lists)

## License

Apache-2.0
