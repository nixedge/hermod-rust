# Claude Development Notes

## Git Commit Rules

### Conventional Commits Format

Use conventional commits format for all commits:

```
<type>(<scope>): <subject>

<body>

<footer>
```

**Types:**
- `feat`: New feature
- `fix`: Bug fix
- `docs`: Documentation changes
- `style`: Code style changes (formatting, no logic change)
- `refactor`: Code refactoring
- `perf`: Performance improvements
- `test`: Adding or updating tests
- `chore`: Maintenance tasks, dependency updates
- `ci`: CI/CD changes
- `build`: Build system changes

**Scope:**
- `protocol`: Protocol types and messages
- `codec`: CBOR encoding/decoding
- `forwarder`: Forwarder client
- `tracer`: Tracing integration
- `nix`: Nix flake/build configuration
- `docs`: Documentation

**Rules:**
- NO attribution in commits (no Co-Authored-By, no Claude signatures)
- Keep subject line under 72 characters
- Use imperative mood ("add" not "added")
- Body optional for complex changes
- Footer for breaking changes: `BREAKING CHANGE: description`

### Examples

```
feat(protocol): implement trace-forward protocol types

Add Severity, DetailLevel, and TraceObject types with CBOR encoding
matching the Haskell implementation for wire compatibility.
```

```
feat(forwarder): add trace forwarder client

Implement async forwarder that connects to hermod-tracer via Unix
socket with automatic reconnection and buffering.
```

```
docs: add README with usage examples
```

## Wire Protocol Compatibility

**CRITICAL**: The wire protocol MUST maintain byte-level compatibility with the
Haskell implementation in hermod-tracing. Any changes to protocol types, encoding,
or message format require verification against the Haskell reference.

### Testing Protocol Changes

1. Run tests: `nix develop --command cargo test`
2. Verify CBOR encoding matches Haskell
3. Test with actual hermod-tracer if possible

## Build and Test Checklist

Before committing:
- [ ] `nix develop --command cargo check` - code compiles
- [ ] `nix develop --command cargo test` - tests pass
- [ ] `nix develop --command cargo fmt` - code formatted
- [ ] `nix fmt` - nix files formatted
- [ ] `nix flake check` - flake checks pass
- [ ] `nix build` - package builds successfully

## Project Structure

```
hermod/
├── flake.nix               # Main flake configuration
├── flake/
│   └── lib.nix            # Flake utilities
├── perSystem/
│   ├── devShells.nix      # Development environment
│   ├── formatter.nix      # Code formatting
│   └── packages.nix       # Package definitions
├── src/
│   ├── lib.rs             # Library entry point
│   ├── protocol/          # Wire protocol implementation
│   │   ├── types.rs       # TraceObject, Severity, DetailLevel
│   │   ├── messages.rs    # Protocol messages
│   │   └── codec.rs       # CBOR encoding/decoding
│   ├── forwarder.rs       # Forwarder client
│   └── tracer.rs          # Tracing integration
└── examples/
    └── simple_tracer.rs   # Usage example
```
