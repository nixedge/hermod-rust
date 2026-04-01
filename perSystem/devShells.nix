{
  perSystem = {
    config,
    pkgs,
    inputs',
    ...
  }: let
    # Use nightly toolchain - same as packages.nix
    toolchain = with inputs'.fenix.packages;
      combine [
        minimal.rustc
        minimal.cargo
        complete.clippy
        complete.rustfmt
        complete.rust-analyzer
      ];
  in {
    devShells.default = with pkgs;
      mkShell {
        packages = [
          # Rust toolchain (nightly from fenix)
          toolchain
          cmake
          pkg-config
          openssl
          zlib

          # Utilities
          jq
          fd
          ripgrep

          # Tree formatter
          config.treefmt.build.wrapper

          # Haskell reference binaries (for conformance tests)
          inputs'.hermod-tracing.packages.demo-acceptor
          inputs'.hermod-tracing.packages.demo-forwarder
        ];

        shellHook = ''
          echo "Cardano Tracer Rust - Trace-Forward Protocol Implementation"
          echo ""
          echo "Rust: $(rustc --version)"
          echo "Cargo: $(cargo --version)"
          echo ""
          echo "Commands:"
          echo "  cargo build              # Build the project"
          echo "  cargo test               # Run tests"
          echo "  cargo run                # Run the tracer"
        '';
      };
  };
}
