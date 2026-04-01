//! Protocol messages for trace-forward protocol
//!
//! The protocol has three messages:
//! - MsgTraceObjectsRequest: Acceptor requests N trace objects
//! - MsgTraceObjectsReply: Forwarder replies with trace objects
//! - MsgDone: Acceptor terminates the session

use super::types::TraceObject;
use pallas_codec::minicbor::{decode, encode, Decode, Decoder, Encode, Encoder};

/// Request for trace objects from the acceptor
///
/// Wire format: `[3] [1] [blocking: bool] [count: u16]`
#[derive(Debug, Clone)]
pub struct MsgTraceObjectsRequest {
    /// Whether this is a blocking request
    pub blocking: bool,
    /// Number of trace objects requested
    pub number_of_trace_objects: u16,
}

/// Reply with trace objects from the forwarder
///
/// Wire format: `[2] [3] [trace_objects: [TraceObject]]`
///
/// Note: For blocking requests, the list must be non-empty
#[derive(Debug, Clone)]
pub struct MsgTraceObjectsReply {
    /// The trace objects being sent
    /// For blocking requests, this must be non-empty
    pub trace_objects: Vec<TraceObject>,
}

/// Termination message from acceptor
///
/// Wire format: `[1] [2]`
#[derive(Debug, Clone, Copy)]
pub struct MsgDone;

/// All possible messages in the protocol
#[derive(Debug, Clone)]
pub enum Message {
    /// Request for trace objects
    TraceObjectsRequest(MsgTraceObjectsRequest),
    /// Reply with trace objects
    TraceObjectsReply(MsgTraceObjectsReply),
    /// Termination
    Done,
}

/// Protocol state machine states
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Idle state - acceptor can send request or done
    Idle,
    /// Busy state - forwarder must send reply
    /// The bool indicates if the request was blocking
    Busy(bool),
    /// Terminal state
    Done,
}

// CBOR encoding/decoding implementations
impl Encode<()> for Message {
    fn encode<W: encode::Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut (),
    ) -> Result<(), encode::Error<W::Error>> {
        match self {
            Message::TraceObjectsRequest(req) => {
                // NumberOfTraceObjects is a Haskell newtype, encoded as [constructor_index, value]
                e.array(3)?.u16(1)?.bool(req.blocking)?;
                e.array(2)?.u16(0)?.u16(req.number_of_trace_objects)?;
            }
            Message::TraceObjectsReply(reply) => {
                e.array(2)?.u16(3)?;
                e.array(reply.trace_objects.len() as u64)?;
                for trace_obj in &reply.trace_objects {
                    e.encode_with(trace_obj, ctx)?;
                }
            }
            Message::Done => {
                e.array(1)?.u16(2)?;
            }
        }
        Ok(())
    }
}

impl<'b> Decode<'b, ()> for Message {
    fn decode(d: &mut Decoder<'b>, ctx: &mut ()) -> Result<Self, decode::Error> {
        d.array()?;
        let tag = d.u16()?;

        match tag {
            1 => {
                // MsgTraceObjectsRequest
                let blocking = d.bool()?;
                // NumberOfTraceObjects newtype with Generic Serialise is encoded as [constructor_index, Word16]
                let arr_len = d.array()?;
                tracing::debug!("Array length for NumberOfTraceObjects: {:?}", arr_len);
                // Skip the constructor index (always 0 for newtypes)
                let _constructor_idx = d.u16()?;
                tracing::debug!("Constructor index: {}", _constructor_idx);
                // Now read the actual value
                let number_of_trace_objects = d.u16()?;
                tracing::debug!("Decoded count: {}", number_of_trace_objects);
                Ok(Message::TraceObjectsRequest(MsgTraceObjectsRequest {
                    blocking,
                    number_of_trace_objects,
                }))
            }
            2 => {
                // MsgDone
                Ok(Message::Done)
            }
            3 => {
                // MsgTraceObjectsReply
                let len = d
                    .array()?
                    .ok_or_else(|| decode::Error::message("expected definite array"))?;
                let mut trace_objects = Vec::with_capacity(len as usize);
                for _ in 0..len {
                    trace_objects.push(d.decode_with(ctx)?);
                }
                Ok(Message::TraceObjectsReply(MsgTraceObjectsReply {
                    trace_objects,
                }))
            }
            _ => Err(decode::Error::message("unknown message tag")),
        }
    }
}
