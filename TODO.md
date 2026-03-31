# TODO: Hermod Rust Implementation

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

4. **CBOR Encoding Fixes**
   - ✅ Fixed Haskell Generic Serialise newtype encoding
   - ✅ NumberOfTraceObjects decodes correctly (was [constructor_index, value])
   - ✅ Fixed TraceObject constructor index prefix (array(9)[0, ...])
   - ✅ Fixed Severity encoding (array(1)[N])
   - ✅ Fixed DetailLevel encoding (array(1)[N])
   - ✅ Traces appear in hermod-tracer log files

---

## Next Steps

### 1. Implement hermod::dispatcher

**Goal:** Full compatibility with the Haskell `hermod-tracing` dispatcher, allowing Rust applications to act as drop-in replacements.

**Components needed:**
- Dispatcher API that mirrors `Cardano.Logging.Dispatcher`
- Trace filtering and routing
- Multiple backend support (file, stdout, hermod-tracer)
- Configuration system matching hermod-tracing config format

### 2. Complete EKG and DataPoint Protocols (Optional)

The warnings about unregistered protocols 32769 (0x8001) and 32771 (0x8003) are:
- 0x8001 = 1 with initiator flag = EKG
- 0x8003 = 3 with initiator flag = DataPoint

These are optional metrics protocols. Implement if needed for full compatibility.

### 3. Add Documentation

- Document protocol loop pattern
- Add rustdoc comments to mux module
- Explain Haskell Generic Serialise pattern

### 4. Clean Up Warnings

Fix missing docs warnings in src/mux/:
- mod.rs: Protocol constants need docs
- handshake.rs: Enum variants and fields need docs
- client.rs: Error variants need docs

### 5. Production Readiness

- [ ] Error handling improvements
- [ ] Configurable timeouts
- [ ] Reconnection logic
- [ ] Metrics/monitoring
- [ ] Integration with real trace sources

---

## Technical Context

### Protocol Numbers (Confirmed)

From `hermod-tracing/trace-forward/src/Trace/Forward/Acceptors/Client.hs`:
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

### Haskell Generic Serialise Pattern

**Key Discovery:** Haskell types with `deriving anyclass (Serialise)` via Generic encode with constructor indices.

Product types (records) with N fields: `array(N+1)[constructor_index, field1, ..., fieldN]`
Nullary constructors (enum variants): `array(1)[constructor_index]`

Example:
```haskell
data SeverityS = Debug | Info | Notice | Warning | Error | Critical | Alert | Emergency
  deriving (Generic, Serialise)
-- Info encodes as: [1]  (array(1)[1])
```

---

## Test Setup Reference

### hermod-tracer Configuration

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

## Additional Resources

- Pallas documentation: https://github.com/txpipe/pallas
- Ouroboros Network spec: https://ouroboros-network.cardano.intersectmbo.org/
- CBOR spec: https://www.rfc-editor.org/rfc/rfc8949.html
- Haskell Serialise: https://hackage.haskell.org/package/serialise

---

## Contact / Notes

This implementation is based on hermod-tracing's trace-forward protocol from:
`~/work/iohk/hermod-tracing/`

The protocol is correctly implemented and wire-compatible with the Haskell implementation.
