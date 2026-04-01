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
/// Wire format: `array(3)[1, blocking: bool, array(2)[0, count: u16]]`
#[derive(Debug, Clone)]
pub struct MsgTraceObjectsRequest {
    /// Whether this is a blocking request
    pub blocking: bool,
    /// Number of trace objects requested
    pub number_of_trace_objects: u16,
}

/// Reply with trace objects from the forwarder
///
/// Wire format: `array(2)[3, trace_objects: [TraceObject]]`
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
/// Wire format: `array(1)[2]`
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
                // NumberOfTraceObjects is a Haskell newtype; Generic Serialise encodes it as
                // array(2)[constructor_index=0, value]
                d.array()?;
                let _constructor_idx = d.u16()?;
                let number_of_trace_objects = d.u16()?;
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
                // Haskell's Serialise [a] uses indefinite-length encoding for non-empty lists
                let mut trace_objects = Vec::new();
                for item in d.array_iter_with::<(), TraceObject>(ctx)? {
                    trace_objects.push(item?);
                }
                Ok(Message::TraceObjectsReply(MsgTraceObjectsReply {
                    trace_objects,
                }))
            }
            _ => Err(decode::Error::message("unknown message tag")),
        }
    }
}
