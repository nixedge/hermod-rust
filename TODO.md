# TODO: Cardano Tracer Rust Implementation

## Current Status Summary

### ✅ What's Working

1. **Protocol Implementation**
   - ✅ Trace-forward mini-protocol correctly implemented
   - ✅ CBOR encoding/decoding for all message types
   - ✅ TraceObject structure is wire-compatible
   - ✅ All unit tests pass

2. **Mux Integration (Pallas)**
   - ✅ Ouroboros Network handshake works (ForwardingV_1)
   - ✅ Multiplexer channels established
   - ✅ Protocol numbers correct: Handshake=0, EKG=1, TraceObject=2, DataPoint=3
   - ✅ Connection to hermod-tracer successful

3. **Protocol Loop**
   - ✅ Continuous request-response loop implemented
   - ✅ Handles multiple MsgTraceObjectsRequest
   - ✅ Proper MsgTraceObjectsReply responses
   - ✅ Timeout handling for idle acceptor

4. **CBOR Decoding Fixes**
   - ✅ Fixed Haskell Generic Serialise newtype encoding
   - ✅ NumberOfTraceObjects decodes correctly (was [constructor_index, value])
   - ✅ Trace count now reads as 100 (not 0)

### ❌ What's Not Working

**CRITICAL ISSUE: Traces Not Appearing in Log Files**

Despite successful protocol communication, hermod-tracer doesn't write traces to `/tmp/hermod-tracer-test-logs/`.

**Symptoms:**
- Cardano-tracer accepts connection
- No errors in logs
- Shows "AddNewNodeIdMapping" messages
- But no trace files created
- `traceObjectsHandler` may be receiving empty list

**Most Likely Cause:**
Subtle CBOR encoding difference in `TraceObject` fields causing hermod-tracer to decode an empty array instead of our 3 traces.

---

## Immediate Next Steps

### 1. Debug CBOR Encoding (HIGHEST PRIORITY)

**Goal:** Determine exact CBOR encoding difference between Rust and Haskell TraceObject

**Steps:**

1. **Add CBOR hex dump to Rust client**
   ```rust
   // In examples/mux_test.rs, before sending reply
   use pallas_codec::minicbor::Encoder;
   let mut buf = Vec::new();
   let mut encoder = Encoder::new(&mut buf);
   reply.encode(&mut encoder, &mut ()).unwrap();
   println!("CBOR hex: {}", hex::encode(&buf));
   ```

2. **Create equivalent Haskell test**
   - Write a simple Haskell program that creates a TraceObject
   - Encode it with `Codec.Serialise`
   - Print hex dump
   - Compare byte-by-byte with Rust output

3. **Focus areas to check:**
   - `to_human: Option<String>` → Maybe Text encoding
     - Rust: `[0]` for None, `[1, str]` for Some
     - Verify this matches Haskell Maybe encoding

   - `to_timestamp: DateTime<Utc>` → UTCTime encoding
     - Rust: CBOR tag 1 + f64 (seconds + nanos)
     - Haskell: May use different timestamp format
     - Check if Haskell uses tag 1 or different representation

   - Array length markers
     - MsgTraceObjectsReply: `[2] [3] [array_len] [trace1] [trace2] [trace3]`
     - Verify array_len encoding

4. **Test with minimal TraceObject**
   ```rust
   // Try with absolute minimal fields
   TraceObject {
       to_human: None,  // Simplest case
       to_machine: "{}".to_string(),  // Empty JSON
       to_namespace: vec![],  // Empty
       to_severity: Severity::Info,
       to_details: DetailLevel::DNormal,
       to_timestamp: Utc::now(),
       to_hostname: "test".to_string(),
       to_thread_id: "1".to_string(),
   }
   ```
   If this works, add fields back one by one.

### 2. Add Wire-Level Debugging

**Enable Pallas network debugging:**
```rust
// In examples/mux_test.rs
std::env::set_var("RUST_LOG", "pallas_network=trace,hermod=debug");
```

This may reveal what's being sent/received at mux level.

### 3. Check Cardano-Tracer Logs in Detail

Run hermod-tracer with maximum verbosity:
```bash
~/work/iohk/cardano-node/scratch/result/bin/hermod-tracer \
  --config /tmp/tracer-test-config.yaml \
  --verbosity Debug
```

Look for:
- CBOR decode errors (may be swallowed)
- Empty array logs
- Handler invocations

---

## Investigation Tasks

### A. Compare with Working Haskell Implementation

**Reference Implementation:**
`~/work/iohk/cardano-node/scratch/trace-forward/test/Test/Trace/Forward/Protocol/TraceObject/`

Files:
- `Examples.hs` - Example forwarder/acceptor implementations
- `Tests.hs` - Property tests with encode/decode
- `Codec.hs` - CBOR codec tests

**Tasks:**
1. Extract a working Haskell TraceObject from tests
2. Get its CBOR hex dump
3. Manually create same object in Rust
4. Compare CBOR bytes

### B. Verify timestamp Encoding

**Question:** Does Haskell use CBOR tag 1 for UTCTime?

**Check in:**
```bash
cd ~/work/iohk/cardano-node/scratch
grep -r "UTCTime" --include="*.hs" | grep -i "serialise\|encode"
```

**Possible issues:**
- Haskell may use different CBOR tag
- May use string representation
- May use different precision (microseconds vs nanoseconds)

**Our current encoding** (src/protocol/types.rs:196-199):
```rust
e.tag(pallas_codec::minicbor::data::Tag::new(1))?;
let timestamp_secs = self.to_timestamp.timestamp() as f64
    + (self.to_timestamp.timestamp_subsec_nanos() as f64 / 1_000_000_000.0);
e.f64(timestamp_secs)?;
```

### C. Test Message Encoding in Isolation

Create a unit test that encodes just a MsgTraceObjectsReply:

```rust
#[test]
fn test_trace_objects_reply_encoding() {
    let traces = vec![/* minimal trace */];
    let reply = Message::TraceObjectsReply(MsgTraceObjectsReply {
        trace_objects: traces,
    });

    let mut buf = Vec::new();
    let mut encoder = Encoder::new(&mut buf);
    reply.encode(&mut encoder, &mut ()).unwrap();

    println!("CBOR hex: {}", hex::encode(&buf));

    // Decode it back to verify round-trip
    let mut decoder = Decoder::new(&buf);
    let decoded = Message::decode(&mut decoder, &mut ()).unwrap();
    // Assert equality
}
```

---

## Technical Context

### Protocol Numbers (Confirmed)

From `hermod-tracer/src/Cardano/Tracer/Acceptors/Client.hs`:
```haskell
appInitiator
  [ (runEKGAcceptorInit          tracerEnv ekgConfig errorHandler, 1)
  , (runTraceObjectsAcceptorInit tracerEnv tracerEnvRTView tfConfig errorHandler, 2)
  , (runDataPointsAcceptorInit   tracerEnv dpfConfig errorHandler, 3)
  ]
```

- Protocol 0: Handshake
- Protocol 1: EKG
- Protocol 2: TraceObject (what we implement)
- Protocol 3: DataPoint

### Protocol Loop Pattern

From `Trace/Forward/Run/TraceObject/Acceptor.hs:82-87`:
```haskell
acceptorActions config@AcceptorConfiguration{whatToRequest, shouldWeStop} loHandler =
  Acceptor.SendMsgTraceObjectsRequest TokBlocking whatToRequest $ \replyWithTraceObjects -> do
    loHandler $ getTraceObjectsFromReply replyWithTraceObjects
    ifM (readTVarIO shouldWeStop)
      (return $ Acceptor.SendMsgDone $ return ())
      (return $ acceptorActions config loHandler)  -- RECURSIVE LOOP
```

The acceptor:
1. Sends MsgTraceObjectsRequest with count=100, blocking=true
2. Waits for MsgTraceObjectsReply
3. Calls `loHandler` (traceObjectsHandler) with received traces
4. If not stopped, **loops back to step 1**

### Handler Early Return

From `hermod-tracer/src/Cardano/Tracer/Handlers/Logs/TraceObjects.hs:37`:
```haskell
traceObjectsHandler _ _ _ [] = return ()  -- Early return if empty!
```

If the handler receives an empty list, it returns immediately without writing logs.

**This is why we see no log files!** Cardano-tracer is likely decoding our reply as an empty array.

### Haskell Generic Serialise Pattern

**Key Discovery:** Haskell newtypes with `deriving Generic, Serialise` encode as `[constructor_index, value]`.

Example:
```haskell
newtype NumberOfTraceObjects = NumberOfTraceObjects { nTraceObjects :: Word16 }
  deriving (Generic, Serialise)
```
Encodes as: `[0, 100]` not just `100`

We fixed this in `src/protocol/messages.rs:98-106`.

**Check if TraceObject has similar wrapping!**

---

## Long-Term Tasks

### 1. Complete EKG and DataPoint Protocols (Optional)

The warnings about unregistered protocols 32769 (0x8001) and 32771 (0x8003) are:
- 0x8001 = 1 with initiator flag = EKG
- 0x8003 = 3 with initiator flag = DataPoint

These are optional metrics protocols. Implement if needed for full compatibility.

### 2. Add Documentation

- Document protocol loop pattern
- Add rustdoc comments to mux module
- Explain Haskell Generic Serialise pattern

### 3. Clean Up Warnings

Fix missing docs warnings in src/mux/:
- mod.rs: Protocol constants need docs
- handshake.rs: Enum variants and fields need docs
- client.rs: Error variants need docs

### 4. Production Readiness

- [ ] Error handling improvements
- [ ] Configurable timeouts
- [ ] Reconnection logic
- [ ] Metrics/monitoring
- [ ] Integration with real trace sources

---

## Test Setup Reference

### Cardano-Tracer Configuration

File: `/tmp/tracer-test-config.yaml`
```yaml
network:
  tag: AcceptAt
  contents: /tmp/hermod-tracer.sock
networkMagic: 764824073
logging:
  - logRoot: /tmp/hermod-tracer-test-logs
    logMode: FileMode
    logFormat: ForHuman
loRequestNum: 100
```

### Running Tests

```bash
# Terminal 1: Start hermod-tracer
rm -rf /tmp/hermod-tracer-test-logs
mkdir /tmp/hermod-tracer-test-logs
pkill hermod-tracer
~/work/iohk/cardano-node/scratch/result/bin/hermod-tracer \
  --config /tmp/tracer-test-config.yaml > /tmp/tracer-output.log 2>&1 &

# Terminal 2: Run Rust client
cd ~/work/iohk/hermod
RUST_LOG=info nix develop --command cargo run --example mux_test

# Check logs
cat /tmp/tracer-output.log
ls -la /tmp/hermod-tracer-test-logs/
```

---

## Quick Reference Commands

```bash
# Find Haskell TraceObject encoding
cd ~/work/iohk/cardano-node/scratch
grep -r "instance.*Serialise.*TraceObject" --include="*.hs"

# Find UTCTime CBOR encoding
grep -r "UTCTime" --include="*.hs" | grep -i encode

# Check hermod-tracer handler
cat hermod-tracer/src/Cardano/Tracer/Handlers/Logs/TraceObjects.hs

# View our TraceObject encoding
cat ~/work/iohk/hermod/src/protocol/types.rs

# Compare message encoding
cat ~/work/iohk/hermod/src/protocol/messages.rs
```

---

## Files to Focus On

### Rust Files
- `src/protocol/types.rs` - TraceObject encoding (lines 159-209)
- `src/protocol/messages.rs` - Message encoding (lines 64-132)
- `examples/mux_test.rs` - Integration test

### Haskell Reference Files
- `cardano-node/scratch/trace-forward/src/Trace/Forward/Protocol/TraceObject/Type.hs`
- `cardano-node/scratch/cardano-logging/src/Cardano/Logging/Types.hs`
- `cardano-node/scratch/trace-forward/src/Trace/Forward/Protocol/TraceObject/Codec.hs`

---

## Success Criteria

When debugging is complete, we should see:

1. ✅ Handshake successful (already working)
2. ✅ Protocol loop working (already working)
3. ✅ Cardano-tracer receives traces (already working)
4. ✅ **Log files created in `/tmp/hermod-tracer-test-logs/`** (NOT WORKING - FIX THIS!)
5. ✅ **Trace content appears in log files** (NOT WORKING - FIX THIS!)

Expected log file location:
```
/tmp/hermod-tracer-test-logs/rust-mux-test/YYYY-MM-DD-HH-MM-SS.log
```

---

## Additional Resources

- Pallas documentation: https://github.com/txpipe/pallas
- Ouroboros Network spec: https://ouroboros-network.cardano.intersectmbo.org/
- CBOR spec: https://www.rfc-editor.org/rfc/rfc8949.html
- Haskell Serialise: https://hackage.haskell.org/package/serialise

---

## Contact / Notes

This implementation is based on cardano-node's trace-forward protocol from:
`~/work/iohk/cardano-node/scratch/trace-forward/`

The protocol is correctly implemented at the message level. The issue is purely CBOR field-level encoding compatibility with the Haskell implementation.

**Next session: Start with CBOR hex dump comparison!**
