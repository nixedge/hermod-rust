//! Multiplexer-aware trace-forward protocol implementation
//!
//! This module implements the trace-forward protocol as an Ouroboros Network
//! mini-protocol using the pallas-network multiplexer infrastructure.

mod client;
mod handshake;

pub use client::*;
pub use handshake::*;

/// Protocol number for the handshake mini-protocol
pub const PROTOCOL_HANDSHAKE: u16 = 0;
/// Protocol number for the trace object forwarding mini-protocol
pub const PROTOCOL_TRACE_OBJECT: u16 = 2;
/// Protocol number for the EKG metrics mini-protocol
pub const PROTOCOL_EKG: u16 = 1;
/// Protocol number for the data point mini-protocol
pub const PROTOCOL_DATA_POINT: u16 = 3;
