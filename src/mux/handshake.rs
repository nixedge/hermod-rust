//! Trace-forward protocol handshake implementation
//!
//! Implements the handshake for the trace-forward protocol, which negotiates
//! the protocol version (ForwardingV_1) and network magic.

use pallas_codec::minicbor::{decode, encode, Decode, Decoder, Encode, Encoder};
use std::collections::HashMap;
use thiserror::Error;

/// Protocol version for trace-forward
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForwardingVersion {
    /// Version 1 of the forwarding protocol
    V1 = 1,
    /// Version 2 of the forwarding protocol
    V2 = 2,
}

/// Version data containing network magic
#[derive(Debug, Clone)]
pub struct ForwardingVersionData {
    /// The Cardano network magic number
    pub network_magic: u64,
}

impl Encode<()> for ForwardingVersionData {
    fn encode<W: encode::Write>(
        &self,
        e: &mut Encoder<W>,
        _ctx: &mut (),
    ) -> Result<(), encode::Error<W::Error>> {
        e.u64(self.network_magic)?;
        Ok(())
    }
}

impl<'b> Decode<'b, ()> for ForwardingVersionData {
    fn decode(d: &mut Decoder<'b>, _ctx: &mut ()) -> Result<Self, decode::Error> {
        let network_magic = d.u64()?;
        Ok(ForwardingVersionData { network_magic })
    }
}

/// Version table for handshake negotiation
pub type VersionTable = HashMap<u64, ForwardingVersionData>;

/// Creates a version table with ForwardingV_1
pub fn version_table_v1(network_magic: u64) -> VersionTable {
    let mut table = HashMap::new();
    table.insert(1, ForwardingVersionData { network_magic });
    table
}

/// Handshake message types
#[derive(Debug, Clone)]
pub enum HandshakeMessage {
    /// Propose versions
    Propose(VersionTable),
    /// Accept a version
    Accept(u64, ForwardingVersionData),
    /// Refuse all versions
    Refuse(Vec<u64>),
}

/// Errors that can occur during handshake negotiation
#[derive(Error, Debug)]
pub enum HandshakeError {
    /// CBOR codec error
    #[error("codec error: {0}")]
    Codec(String),

    /// The remote peer refused all proposed versions
    #[error("version refused: {0:?}")]
    Refused(Vec<u64>),

    /// No mutually acceptable protocol version could be found
    #[error("no compatible version")]
    NoCompatibleVersion,
}

impl Encode<()> for HandshakeMessage {
    fn encode<W: encode::Write>(
        &self,
        e: &mut Encoder<W>,
        _ctx: &mut (),
    ) -> Result<(), encode::Error<W::Error>> {
        match self {
            HandshakeMessage::Propose(versions) => {
                e.array(2)?.u16(0)?;
                e.map(versions.len() as u64)?;
                for (version, data) in versions {
                    e.encode(version)?;
                    e.encode_with(data, _ctx)?;
                }
            }
            HandshakeMessage::Accept(version, data) => {
                e.array(3)?.u16(1)?;
                e.encode(version)?;
                e.encode_with(data, _ctx)?;
            }
            HandshakeMessage::Refuse(versions) => {
                e.array(2)?.u16(2)?;
                e.array(versions.len() as u64)?;
                for v in versions {
                    e.encode(v)?;
                }
            }
        }
        Ok(())
    }
}

impl<'b> Decode<'b, ()> for HandshakeMessage {
    fn decode(d: &mut Decoder<'b>, _ctx: &mut ()) -> Result<Self, decode::Error> {
        d.array()?;
        let label = d.u16()?;

        match label {
            0 => {
                let map_len = d
                    .map()?
                    .ok_or_else(|| decode::Error::message("expected definite map"))?;
                let mut versions = HashMap::new();
                for _ in 0..map_len {
                    let version = d.decode()?;
                    let data = d.decode_with(_ctx)?;
                    versions.insert(version, data);
                }
                Ok(HandshakeMessage::Propose(versions))
            }
            1 => {
                let version = d.decode()?;
                let data = d.decode_with(_ctx)?;
                Ok(HandshakeMessage::Accept(version, data))
            }
            2 => {
                let arr_len = d
                    .array()?
                    .ok_or_else(|| decode::Error::message("expected definite array"))?;
                let mut versions = Vec::new();
                for _ in 0..arr_len {
                    versions.push(d.decode()?);
                }
                Ok(HandshakeMessage::Refuse(versions))
            }
            _ => Err(decode::Error::message("unknown handshake message")),
        }
    }
}
