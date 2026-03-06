//! Multiplexer-aware trace-forward protocol implementation
//!
//! This module implements the trace-forward protocol as an Ouroboros Network
//! mini-protocol using the pallas-network multiplexer infrastructure.

mod handshake;
mod client;

pub use handshake::*;
pub use client::*;

// Protocol numbers for trace-forward (from Haskell trace-forward/src/Trace/Forward/Forwarding.hs)
pub const PROTOCOL_HANDSHAKE: u16 = 0;
pub const PROTOCOL_TRACE_OBJECT: u16 = 2;
pub const PROTOCOL_EKG: u16 = 1;
pub const PROTOCOL_DATA_POINT: u16 = 3;
