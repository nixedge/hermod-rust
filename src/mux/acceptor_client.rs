//! Acceptor-side mux client for the trace-forward protocol
//!
//! The acceptor is the listener side: it sends requests and receives replies,
//! mirroring the forwarder's `TraceForwardClient`.

use crate::protocol::{Message, MsgTraceObjectsRequest, TraceObject};
use pallas_network::multiplexer;
use tracing::debug;

/// Acceptor-side mux client — sends requests, receives replies
pub struct TraceAcceptorClient {
    channel: multiplexer::ChannelBuffer,
}

impl TraceAcceptorClient {
    /// Create a new acceptor client from a multiplexer channel
    pub fn new(channel: multiplexer::AgentChannel) -> Self {
        Self {
            channel: multiplexer::ChannelBuffer::new(channel),
        }
    }

    /// Send a blocking request for `count` traces and return the received traces
    pub async fn request_traces(
        &mut self,
        count: u16,
    ) -> Result<Vec<TraceObject>, multiplexer::Error> {
        let req = Message::TraceObjectsRequest(MsgTraceObjectsRequest {
            blocking: true,
            number_of_trace_objects: count,
        });

        debug!("Sending trace request (count: {})", count);
        self.channel.send_msg_chunks(&req).await?;

        let msg: Message = self.channel.recv_full_msg().await?;
        debug!("Received reply: {:?}", msg);

        match msg {
            Message::TraceObjectsReply(reply) => Ok(reply.trace_objects),
            _ => Err(multiplexer::Error::Decoding(
                "expected TraceObjectsReply".into(),
            )),
        }
    }

    /// Send a Done message to close the session gracefully
    pub async fn send_done(&mut self) -> Result<(), multiplexer::Error> {
        self.channel.send_msg_chunks(&Message::Done).await
    }
}
