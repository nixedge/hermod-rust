//! DataPoint mini-protocol client (protocol 3)
//!
//! The acceptor-side can request named data points from the connected forwarder.
//!
//! ## Wire protocol (`trace-forward` DataPoint protocol)
//!
//! | Message | CBOR |
//! |---------|------|
//! | `MsgDataPointsRequest([String])` | `array(2)\[1, [name...]\]` |
//! | `MsgDataPointsReply([(String, Option<Bytes>)])` | `array(2)\[3, [[name, maybe_value]...]\]` |
//! | `MsgDone` | `array(1)\[2\]` |
//!
//! `DataPointValue` is raw JSON bytes (lazy bytestring in Haskell → `Vec<u8>` here).

use pallas_codec::minicbor::{self, Decode, Decoder, Encode, Encoder};
use pallas_network::multiplexer::{ChannelBuffer, Error};
use tracing::debug;

// ---------------------------------------------------------------------------
// Protocol message types
// ---------------------------------------------------------------------------

/// Messages in the DataPoint mini-protocol
#[derive(Debug)]
pub enum DataPointMessage {
    /// Request the given named data points
    Request(Vec<String>),
    /// Reply with values (None if a named point doesn't exist)
    Reply(Vec<(String, Option<Vec<u8>>)>),
    /// Terminate the session
    Done,
}

impl Encode<()> for DataPointMessage {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        _ctx: &mut (),
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        match self {
            DataPointMessage::Request(names) => {
                e.array(2)?.u8(1)?;
                e.array(names.len() as u64)?;
                for n in names {
                    e.str(n)?;
                }
            }
            DataPointMessage::Reply(items) => {
                e.array(2)?.u8(3)?;
                e.array(items.len() as u64)?;
                for (name, value) in items {
                    e.array(2)?;
                    e.str(name)?;
                    match value {
                        None => {
                            e.array(0)?;
                        }
                        Some(bytes) => {
                            e.array(1)?;
                            e.bytes(bytes)?;
                        }
                    }
                }
            }
            DataPointMessage::Done => {
                e.array(1)?.u8(2)?;
            }
        }
        Ok(())
    }
}

impl<'b> Decode<'b, ()> for DataPointMessage {
    fn decode(d: &mut Decoder<'b>, _ctx: &mut ()) -> Result<Self, minicbor::decode::Error> {
        d.array()?;
        let tag = d.u8()?;
        match tag {
            1 => {
                let mut names = Vec::new();
                for item in d.array_iter::<String>()? {
                    names.push(item?);
                }
                Ok(DataPointMessage::Request(names))
            }
            2 => Ok(DataPointMessage::Done),
            3 => {
                // The Haskell `serialise` library encodes non-empty lists as
                // indefinite-length arrays (0x9F ... 0xFF).  We must handle
                // both definite (Some(n)) and indefinite (None) cases.
                let count = d.array()?;
                let mut items = Vec::new();

                match count {
                    Some(n) => {
                        for _ in 0..n {
                            items.push(decode_reply_item(d)?);
                        }
                    }
                    None => {
                        // Indefinite array: read items until Break (0xFF).
                        loop {
                            if d.datatype()? == minicbor::data::Type::Break {
                                d.skip()?; // consume the break token
                                break;
                            }
                            items.push(decode_reply_item(d)?);
                        }
                    }
                }

                Ok(DataPointMessage::Reply(items))
            }
            _ => Err(minicbor::decode::Error::message("unknown DataPoint tag")),
        }
    }
}

/// Decode one `(DataPointName, Maybe LBS.ByteString)` item.
///
/// The Haskell `Serialise (a, b)` instance writes `array(2)[a, b]`.
/// `Maybe LBS.ByteString` is `array(0)` for `Nothing` or `array(1)[bytes]`
/// for `Just bs`.  The bytes may be indefinite-length (Haskell encodes
/// `LBS.ByteString` via `encodeBytesIndef`), so we use `bytes_iter()` which
/// handles both definite and indefinite byte strings.
fn decode_reply_item(
    d: &mut Decoder<'_>,
) -> Result<(String, Option<Vec<u8>>), minicbor::decode::Error> {
    d.array()?; // array(2) for the tuple
    let name = d.str()?.to_string();

    let maybe_len = d.array()?; // array(0) = Nothing, array(1) = Just
    let value = match maybe_len {
        Some(0) => None,
        Some(1) => {
            // Collect bytes from all chunks (handles both definite and
            // indefinite-length byte strings from Haskell LBS.ByteString).
            let mut buf = Vec::new();
            for chunk in d.bytes_iter()? {
                buf.extend_from_slice(chunk?);
            }
            Some(buf)
        }
        _ => {
            return Err(minicbor::decode::Error::message(
                "invalid Maybe encoding in DataPointsReply",
            ));
        }
    };

    Ok((name, value))
}

// ---------------------------------------------------------------------------
// DataPoint client
// ---------------------------------------------------------------------------

/// Holds the DataPoint channel and allows on-demand queries
pub struct DataPointClient {
    channel: ChannelBuffer,
}

impl DataPointClient {
    /// Create a new DataPoint client
    pub fn new(channel: pallas_network::multiplexer::AgentChannel) -> Self {
        DataPointClient {
            channel: ChannelBuffer::new(channel),
        }
    }

    /// Request the given named data points and return the responses.
    /// Each value is parsed as JSON if possible.
    pub async fn request(
        &mut self,
        names: Vec<String>,
    ) -> Result<Vec<(String, Option<serde_json::Value>)>, Error> {
        self.channel
            .send_msg_chunks(&DataPointMessage::Request(names))
            .await?;

        let msg: DataPointMessage = self.channel.recv_full_msg().await?;
        match msg {
            DataPointMessage::Reply(items) => {
                let parsed = items
                    .into_iter()
                    .map(|(name, bytes)| {
                        let value = bytes.and_then(|b| serde_json::from_slice(&b).ok());
                        (name, value)
                    })
                    .collect();
                Ok(parsed)
            }
            DataPointMessage::Done => Err(Error::Decoding("DataPoint connection closed".into())),
            DataPointMessage::Request(_) => {
                Err(Error::Decoding("unexpected Request from forwarder".into()))
            }
        }
    }

    /// Hold the channel open indefinitely (keeps the mux alive)
    /// without making any requests. Returns when the channel closes.
    pub async fn run_idle_loop(mut self) {
        // The forwarder drives the protocol; we just need to keep the channel
        // alive so the mux doesn't stall. We can wait for the Done message.
        loop {
            match self.channel.recv_full_msg::<DataPointMessage>().await {
                Ok(DataPointMessage::Done) => {
                    debug!("DataPoint: remote sent Done");
                    return;
                }
                Ok(_) => {
                    // Unexpected message while idle; ignore
                }
                Err(_) => {
                    return;
                }
            }
        }
    }
}
