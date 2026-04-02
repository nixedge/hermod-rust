//! Trace-forward protocol handshake implementation
//!
//! Implements the handshake for the trace-forward protocol, which negotiates
//! the protocol version (ForwardingV_1) and network magic.

use pallas_codec::minicbor::{Decode, Decoder, Encode, Encoder, decode, encode};
use std::collections::HashMap;

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

#[cfg(test)]
mod tests {
    use super::*;
    use pallas_codec::minicbor;

    fn encode<T: minicbor::Encode<()>>(value: &T) -> Vec<u8> {
        let mut buf = Vec::new();
        minicbor::encode_with(value, &mut buf, &mut ()).unwrap();
        buf
    }

    fn decode<T: for<'b> minicbor::Decode<'b, ()>>(buf: &[u8]) -> T {
        minicbor::decode_with(buf, &mut ()).unwrap()
    }

    #[test]
    fn version_data_round_trip() {
        let data = ForwardingVersionData {
            network_magic: 764824073,
        };
        let buf = encode(&data);
        let decoded: ForwardingVersionData = decode(&buf);
        assert_eq!(decoded.network_magic, 764824073);
    }

    #[test]
    fn version_table_v1_has_single_version_1() {
        let table = version_table_v1(12345);
        assert_eq!(table.len(), 1);
        assert!(table.contains_key(&1));
        assert_eq!(table[&1].network_magic, 12345);
    }

    #[test]
    fn propose_round_trip() {
        let versions = version_table_v1(764824073);
        let msg = HandshakeMessage::Propose(versions);
        let buf = encode(&msg);
        let decoded: HandshakeMessage = decode(&buf);
        match decoded {
            HandshakeMessage::Propose(v) => {
                assert!(v.contains_key(&1));
                assert_eq!(v[&1].network_magic, 764824073);
            }
            _ => panic!("expected Propose, got something else"),
        }
    }

    #[test]
    fn accept_round_trip() {
        let msg = HandshakeMessage::Accept(1, ForwardingVersionData { network_magic: 42 });
        let buf = encode(&msg);
        let decoded: HandshakeMessage = decode(&buf);
        match decoded {
            HandshakeMessage::Accept(ver, data) => {
                assert_eq!(ver, 1);
                assert_eq!(data.network_magic, 42);
            }
            _ => panic!("expected Accept"),
        }
    }

    #[test]
    fn refuse_round_trip() {
        let msg = HandshakeMessage::Refuse(vec![1, 2, 3]);
        let buf = encode(&msg);
        let decoded: HandshakeMessage = decode(&buf);
        match decoded {
            HandshakeMessage::Refuse(mut versions) => {
                versions.sort_unstable();
                assert_eq!(versions, vec![1, 2, 3]);
            }
            _ => panic!("expected Refuse"),
        }
    }

    #[test]
    fn refuse_empty_versions_round_trip() {
        let msg = HandshakeMessage::Refuse(vec![]);
        let buf = encode(&msg);
        match decode::<HandshakeMessage>(&buf) {
            HandshakeMessage::Refuse(v) => assert!(v.is_empty()),
            _ => panic!("expected Refuse"),
        }
    }
}
