# Testing Results and Findings

## Integration Test with hermod-tracer

### Test Setup

1. **hermod-tracer**: Built from ~/work/iohk/cardano-node/scratch
2. **Configuration**: AcceptAt mode listening on /tmp/hermod-tracer.sock
3. **Rust client**: Connects to socket and attempts to forward traces

### Findings

#### ✓ What Works

- **Protocol implementation**: The trace-forward wire protocol is correctly implemented
  - CBOR encoding matches Haskell implementation
  - Message types (MsgTraceObjectsRequest, MsgTraceObjectsReply, MsgDone) are correct
  - TraceObject structure is wire-compatible
  - All unit tests pass

- **Connection**: Unix socket connection establishes successfully
  - Client connects to /tmp/hermod-tracer.sock
  - Socket connection is stable

#### ❌ What's Missing

**Ouroboros Network Mux Protocol**

The trace-forward protocol is implemented as a *mini-protocol* within the **Ouroboros Network framework**. This framework uses a multiplexing layer (network-mux) that:

1. **Handshake Phase**: Establishes protocol version and capabilities
2. **Mux Layer**: Multiplexes multiple mini-protocols over a single connection
3. **Protocol Negotiation**: Agrees on which mini-protocols to run

Dependencies in trace-forward.cabal:
```
network-mux
ouroboros-network:{api, framework} ^>= 0.24
```

### Current Status

Our implementation correctly implements the **trace-forward mini-protocol** but lacks the **Ouroboros Network mux wrapper**. This means:

- ✓ Wire format is correct for the trace-forward messages themselves
- ✓ Protocol state machine is correct
- ✗ Missing mux handshake prevents actual communication with hermod-tracer
- ✗ Cannot integrate with existing hermod-tracer infrastructure without mux support

### Next Steps

**UPDATE**: Mux protocol implementation is in progress using [Pallas](https://github.com/txpipe/pallas):

1. ✅ **Integrated Pallas Network** (v0.35.0)
   - Added `pallas-network` and `pallas-codec` dependencies
   - Migrated to `pallas_codec::minicbor` for compatibility

2. ✅ **Implemented Trace-Forward Miniprotocol**
   - Created `src/mux/` module with mux-aware components
   - Implemented `TraceForwardClient` using Pallas `multiplexer::ChannelBuffer`
   - Implemented trace-forward handshake (ForwardingV_1)
   - Protocol numbers: Handshake=0, TraceObject=2

3. ✅ **Created Full Integration Example** (examples/mux_test.rs)
   - ✅ Connect via Bearer (Unix socket)
   - ✅ Perform handshake with hermod-tracer
   - ✅ Send traces via mux protocol
   - ✅ Correctly decode trace requests (fixed Haskell Generic Serialise encoding)

4. **Partial: End-to-End Testing**
   - ✅ Connection established and handshake successful
   - ✅ Trace requests correctly decoded (100 traces requested)
   - ✅ Trace objects sent successfully (3 traces sent)
   - ⚠️  Traces not yet appearing in log files (investigation ongoing)
   - ⚠️  Warnings about unregistered EKG/DataPoint protocols (may be optional)

### Key Discovery: Haskell Generic Serialise Encoding

**Problem**: Trace request count was decoding as 0 instead of 100.

**Root Cause**: Haskell's `Generic` `Serialise` instance for newtypes encodes them as 2-element arrays:
```haskell
newtype NumberOfTraceObjects = NumberOfTraceObjects { nTraceObjects :: Word16 }
-- Encodes as CBOR: [0, 100]  (constructor index, value)
```

**Solution**: Updated decoder in `src/protocol/messages.rs` to:
1. Read the array wrapper (`d.array()`)
2. Skip the constructor index (`d.u16()`)
3. Read the actual value (`d.u16()`)

This pattern applies to all Haskell newtypes with `deriving Generic` and `Serialise`.

### Test Evidence

```bash
# hermod-tracer starts successfully
$ ~/work/iohk/cardano-node/scratch/result/bin/hermod-tracer --config /tmp/tracer-test-config.yaml
{"ns":"Tracer.SockListen","data":{"kind":"TracerSockListen","listenAt":"/tmp/hermod-tracer.sock"},"sev":"Info"}

# Rust client connects successfully
$ cargo run --example test_with_tracer
Connected!

# But no traces received (no mux handshake)
$ ls /tmp/hermod-tracer-test-logs/
# Empty - no log files created
```

## Conclusion

The trace-forward protocol implementation is **correct and wire-compatible**. Integration with hermod-tracer requires implementing the Ouroboros Network mux layer, which is a separate (and substantial) piece of infrastructure.

The implementation as-is can serve as:
1. A reference for the trace-forward mini-protocol
2. A foundation for Rust-native tracing infrastructure
3. A starting point for full Ouroboros Network integration
