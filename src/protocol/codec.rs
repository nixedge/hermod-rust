//! CBOR codec for trace-forward protocol messages
//!
//! This implements the exact wire format used by hermod-tracer
//! to ensure compatibility with the existing infrastructure.

use super::messages::*;
use super::types::TraceObject;
use bytes::{Buf, BufMut, BytesMut};
use minicbor::{Decode, Decoder, Encode, Encoder};
use thiserror::Error;
use tokio_util::codec::{Decoder as TokioDecoder, Encoder as TokioEncoder};

/// Errors that can occur during encoding/decoding
#[derive(Debug, Error)]
pub enum CodecError {
    /// IO error
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// CBOR encoding error
    #[error("CBOR encoding error: {0}")]
    Encode(String),

    /// CBOR decoding error
    #[error("CBOR decoding error: {0}")]
    Decode(String),

    /// Protocol error (e.g., wrong message type for current state)
    #[error("Protocol error: {0}")]
    Protocol(String),
}

impl<T> From<minicbor::encode::Error<T>> for CodecError
where
    T: std::fmt::Display,
{
    fn from(e: minicbor::encode::Error<T>) -> Self {
        CodecError::Encode(e.to_string())
    }
}

impl From<minicbor::decode::Error> for CodecError {
    fn from(e: minicbor::decode::Error) -> Self {
        CodecError::Decode(e.to_string())
    }
}

/// Codec for trace-forward protocol messages
pub struct TraceObjectCodec {
    /// Current protocol state
    state: State,
}

impl TraceObjectCodec {
    /// Create a new codec starting in Idle state
    pub fn new() -> Self {
        Self { state: State::Idle }
    }

    /// Get the current state
    pub fn state(&self) -> State {
        self.state
    }

    /// Encode a message to CBOR bytes
    pub fn encode_message(msg: &Message) -> Result<Vec<u8>, CodecError> {
        let mut buf = Vec::new();
        let mut encoder = Encoder::new(&mut buf);

        match msg {
            Message::TraceObjectsRequest(req) => {
                // [3] [1] [blocking: bool] [count: u16]
                encoder.array(3)?;
                encoder.u8(1)?;
                encoder.bool(req.blocking)?;
                encoder.u16(req.number_of_trace_objects)?;
            }
            Message::TraceObjectsReply(reply) => {
                // [2] [3] [trace_objects: [TraceObject]]
                encoder.array(2)?;
                encoder.u8(3)?;
                encoder.array(reply.trace_objects.len() as u64)?;
                for trace_obj in &reply.trace_objects {
                    trace_obj.encode(&mut encoder, &mut ())?;
                }
            }
            Message::Done => {
                // [1] [2]
                encoder.array(1)?;
                encoder.u8(2)?;
            }
        }

        Ok(buf)
    }

    /// Decode a message from CBOR bytes
    pub fn decode_message(buf: &[u8], state: State) -> Result<Message, CodecError> {
        let mut decoder = Decoder::new(buf);

        let len = decoder
            .array()?
            .ok_or_else(|| CodecError::Decode("message must have definite length".to_string()))?;

        let key = decoder.u8()?;

        match (state, key, len) {
            // Idle state can receive request or done
            (State::Idle, 1, 3) => {
                let blocking = decoder.bool()?;
                let number_of_trace_objects = decoder.u16()?;
                Ok(Message::TraceObjectsRequest(MsgTraceObjectsRequest {
                    blocking,
                    number_of_trace_objects,
                }))
            }
            (State::Idle, 2, 1) => Ok(Message::Done),

            // Busy state can only receive reply
            (State::Busy(_blocking), 3, 2) => {
                let trace_objects_len = decoder.array()?.ok_or_else(|| {
                    CodecError::Decode("trace objects array must have definite length".to_string())
                })?;

                let mut trace_objects = Vec::with_capacity(trace_objects_len as usize);
                for _ in 0..trace_objects_len {
                    trace_objects.push(TraceObject::decode(&mut decoder, &mut ())?);
                }

                Ok(Message::TraceObjectsReply(MsgTraceObjectsReply {
                    trace_objects,
                }))
            }

            _ => Err(CodecError::Protocol(format!(
                "unexpected message: state={:?}, key={}, len={}",
                state, key, len
            ))),
        }
    }

    /// Update state after receiving a message
    pub fn update_state_recv(&mut self, msg: &Message) -> Result<(), CodecError> {
        self.state = match (&self.state, msg) {
            (State::Idle, Message::TraceObjectsRequest(req)) => State::Busy(req.blocking),
            (State::Idle, Message::Done) => State::Done,
            (State::Busy(_), Message::TraceObjectsReply(_)) => State::Idle,
            _ => {
                return Err(CodecError::Protocol(format!(
                    "invalid state transition: state={:?}, msg={:?}",
                    self.state, msg
                )));
            }
        };
        Ok(())
    }

    /// Update state after sending a message
    pub fn update_state_send(&mut self, msg: &Message) -> Result<(), CodecError> {
        self.state = match (&self.state, msg) {
            (State::Idle, Message::TraceObjectsRequest(req)) => State::Busy(req.blocking),
            (State::Idle, Message::Done) => State::Done,
            (State::Busy(_), Message::TraceObjectsReply(_)) => State::Idle,
            _ => {
                return Err(CodecError::Protocol(format!(
                    "invalid state transition: state={:?}, msg={:?}",
                    self.state, msg
                )));
            }
        };
        Ok(())
    }
}

impl Default for TraceObjectCodec {
    fn default() -> Self {
        Self::new()
    }
}

/// Tokio codec for framing messages over a stream
///
/// This wraps messages with a 4-byte big-endian length prefix
pub struct FramedTraceObjectCodec {
    inner: TraceObjectCodec,
}

impl FramedTraceObjectCodec {
    /// Create a new framed codec
    pub fn new() -> Self {
        Self {
            inner: TraceObjectCodec::new(),
        }
    }

    /// Get the inner codec
    pub fn inner(&self) -> &TraceObjectCodec {
        &self.inner
    }

    /// Get the inner codec mutably
    pub fn inner_mut(&mut self) -> &mut TraceObjectCodec {
        &mut self.inner
    }
}

impl Default for FramedTraceObjectCodec {
    fn default() -> Self {
        Self::new()
    }
}

impl TokioEncoder<Message> for FramedTraceObjectCodec {
    type Error = CodecError;

    fn encode(&mut self, item: Message, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let encoded = TraceObjectCodec::encode_message(&item)?;

        // Write 4-byte length prefix
        dst.reserve(4 + encoded.len());
        dst.put_u32(encoded.len() as u32);
        dst.put_slice(&encoded);

        self.inner.update_state_send(&item)?;

        Ok(())
    }
}

impl TokioDecoder for FramedTraceObjectCodec {
    type Item = Message;
    type Error = CodecError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if src.len() < 4 {
            // Need more data for length prefix
            return Ok(None);
        }

        // Peek at the length
        let mut length_bytes = [0u8; 4];
        length_bytes.copy_from_slice(&src[..4]);
        let length = u32::from_be_bytes(length_bytes) as usize;

        if src.len() < 4 + length {
            // Need more data for full message
            src.reserve(4 + length - src.len());
            return Ok(None);
        }

        // We have a complete message
        src.advance(4);
        let msg_bytes = src.split_to(length);

        let msg = TraceObjectCodec::decode_message(&msg_bytes, self.inner.state)?;
        self.inner.update_state_recv(&msg)?;

        Ok(Some(msg))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::types::{DetailLevel, Severity};
    use chrono::Utc;

    #[test]
    fn test_encode_decode_request() {
        let msg = Message::TraceObjectsRequest(MsgTraceObjectsRequest {
            blocking: true,
            number_of_trace_objects: 100,
        });

        let encoded = TraceObjectCodec::encode_message(&msg).unwrap();
        let decoded = TraceObjectCodec::decode_message(&encoded, State::Idle).unwrap();

        match decoded {
            Message::TraceObjectsRequest(req) => {
                assert_eq!(req.blocking, true);
                assert_eq!(req.number_of_trace_objects, 100);
            }
            _ => panic!("wrong message type"),
        }
    }

    #[test]
    fn test_encode_decode_done() {
        let msg = Message::Done;

        let encoded = TraceObjectCodec::encode_message(&msg).unwrap();
        let decoded = TraceObjectCodec::decode_message(&encoded, State::Idle).unwrap();

        assert!(matches!(decoded, Message::Done));
    }

    #[test]
    fn test_encode_decode_reply() {
        let trace_obj = TraceObject {
            to_human: Some("Test trace".to_string()),
            to_machine: r#"{"test": true}"#.to_string(),
            to_namespace: vec!["test".to_string(), "module".to_string()],
            to_severity: Severity::Info,
            to_details: DetailLevel::DNormal,
            to_timestamp: Utc::now(),
            to_hostname: "test-host".to_string(),
            to_thread_id: "1".to_string(),
        };

        let msg = Message::TraceObjectsReply(MsgTraceObjectsReply {
            trace_objects: vec![trace_obj.clone()],
        });

        let encoded = TraceObjectCodec::encode_message(&msg).unwrap();
        let decoded = TraceObjectCodec::decode_message(&encoded, State::Busy(false)).unwrap();

        match decoded {
            Message::TraceObjectsReply(reply) => {
                assert_eq!(reply.trace_objects.len(), 1);
                assert_eq!(reply.trace_objects[0].to_hostname, "test-host");
            }
            _ => panic!("wrong message type"),
        }
    }
}
