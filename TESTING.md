# Testing hermod

## Unit Tests

```bash
nix develop --command cargo test --lib
```

15 unit tests cover CBOR encoding, dispatcher configuration, rate limiting, and the tracing subscriber.

## Integration Tests

```bash
nix develop --command cargo test --test forwarder_integration
```

End-to-end Rust forwarder ↔ Rust acceptor over a Unix socket.

## Conformance Tests

Cross-language tests against the Haskell `hermod-tracing` reference binaries (`demo-acceptor` and `demo-forwarder`). These are included in the dev shell via the `hermod-tracing` flake input.

```bash
nix develop --command cargo test --test conformance
```

8 conformance tests:

| Test | What it checks |
|------|----------------|
| `test_rust_forwarder_to_haskell_acceptor` | Rust forwarder → Haskell demo-acceptor |
| `test_rust_to_haskell_all_severities` | All 8 `Severity` variants accepted by Haskell |
| `test_rust_to_haskell_all_detail_levels` | All 4 `DetailLevel` variants accepted by Haskell |
| `test_rust_to_haskell_edge_cases` | `None` toHuman, empty namespace, multi-segment namespace |
| `test_haskell_forwarder_to_rust_acceptor` | Haskell demo-forwarder → Rust acceptor (all 8 fields checked) |
| `test_haskell_forwarder_multiple_traces` | 10 consecutive traces from Haskell forwarder |
| `test_trace_object_encoding_round_trip` | Pure Rust CBOR encode → decode round-trip |
| `test_timestamp_uses_tag_1000` | Asserts UTCTime uses CBOR tag 1000 bytes |

---

## Verified Wire Format

The following have been verified to match the Haskell implementation byte-for-byte:

- **TraceObject**: `array(9)[0, toHuman, toMachine, toNamespace, toSeverity, toDetails, toTimestamp, toHostname, toThreadId]`
  - Constructor index `0` prefix required (Haskell Generic Serialise)
- **Severity**: `array(1)[constructor_index]` — e.g., `Info` → `[82 01]`
- **DetailLevel**: `array(1)[constructor_index]` — e.g., `DNormal` → `[82 01]`
- **Maybe/Option**: `Nothing` → `[]` (empty array), `Just x` → `[x]` (single-element array)
- **UTCTime**: CBOR tag 1000 + `map(2){key 1 → i64 secs, key -12 → u64 psecs}`
  - Tag bytes: `0xD9 0x03 0xE8`
  - The `serialise` Haskell package uses tag 1000, not tag 1
- **MsgTraceObjectsRequest**: `array(3)[1, bool, array(2)[0, count]]`
- **MsgTraceObjectsReply**: `array(2)[3, array(N)[...]]` (indefinite-length for non-empty lists)
- **MsgDone**: `array(1)[2]`

## Haskell Generic Serialise Encoding Rules

Types using `deriving anyclass (Serialise)` via GHC Generics encode as:

| Type | Encoding |
|------|----------|
| Product with N fields | `array(N+1)[constructor_index, field1, ..., fieldN]` |
| Nullary constructor (enum) | `array(1)[constructor_index]` |
| Newtype | `array(2)[0, value]` |

## Protocol Numbers

| Protocol | Number |
|----------|--------|
| Handshake | 0 |
| EKG | 1 |
| TraceObject | 2 |
| DataPoint | 3 |

Pallas logs warnings for unregistered protocols 32769 (0x8001) and 32771 (0x8003) — these are the initiator-flagged versions of EKG and DataPoint and are expected.
