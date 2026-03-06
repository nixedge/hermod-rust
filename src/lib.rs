//! Cardano Tracer Rust - Trace-Forward Protocol Implementation
//!
//! This library provides a Rust implementation of the hermod-tracer
//! trace-forward protocol for sending traces to hermod-tracer acceptors.
//!
//! The protocol enables Rust applications (like alternative Cardano nodes)
//! to forward their traces to the standard hermod-tracer infrastructure
//! for monitoring and analysis.

#![warn(missing_docs)]

pub mod forwarder;
pub mod protocol;
pub mod tracer;

/// Library version
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version() {
        assert!(!VERSION.is_empty());
    }
}
