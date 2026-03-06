//! Mux-aware trace-forward client
//!
//! This module implements a trace-forward client that uses the Pallas multiplexer
//! to communicate with hermod-tracer over the Ouroboros Network protocol.

use crate::protocol::{Message, MsgTraceObjectsReply, TraceObject};
use pallas_network::multiplexer;
use thiserror::Error;
use tracing::{debug, info};

/// Errors that can occur in the mux client
#[derive(Error, Debug)]
pub enum ClientError {
    #[error("multiplexer error: {0}")]
    Multiplexer(#[from] multiplexer::Error),

    #[error("invalid inbound message")]
    InvalidInbound,

    #[error("connection closed")]
    ConnectionClosed,
}

/// Mux-aware trace-forward client
pub struct TraceForwardClient {
    channel: multiplexer::ChannelBuffer,
}

impl TraceForwardClient {
    /// Create a new client from a multiplexer channel
    pub fn new(channel: multiplexer::AgentChannel) -> Self {
        Self {
            channel: multiplexer::ChannelBuffer::new(channel),
        }
    }

    /// Send a message to the acceptor
    pub async fn send_message(&mut self, msg: &Message) -> Result<(), ClientError> {
        debug!("Sending message: {:?}", msg);
        self.channel
            .send_msg_chunks(msg)
            .await
            .map_err(ClientError::Multiplexer)?;
        Ok(())
    }

    /// Receive a message from the acceptor
    pub async fn recv_message(&mut self) -> Result<Message, ClientError> {
        let msg = self
            .channel
            .recv_full_msg()
            .await
            .map_err(ClientError::Multiplexer)?;
        debug!("Received message: {:?}", msg);
        Ok(msg)
    }

    /// Wait for a trace objects request from the acceptor and send traces
    pub async fn handle_request(
        &mut self,
        traces: Vec<TraceObject>,
    ) -> Result<(), ClientError> {
        // Wait for request
        let msg = self.recv_message().await?;

        match msg {
            Message::TraceObjectsRequest(req) => {
                debug!(
                    "Received request for {} traces (blocking: {})",
                    req.number_of_trace_objects, req.blocking
                );

                // Take up to the requested number of traces
                let to_send = traces
                    .into_iter()
                    .take(req.number_of_trace_objects as usize)
                    .collect();

                // Send reply
                let reply = Message::TraceObjectsReply(MsgTraceObjectsReply {
                    trace_objects: to_send,
                });

                self.send_message(&reply).await?;
                Ok(())
            }
            Message::Done => {
                info!("Received Done message");
                Err(ClientError::ConnectionClosed)
            }
            _ => Err(ClientError::InvalidInbound),
        }
    }

    /// Send a Done message to close the connection gracefully
    pub async fn send_done(&mut self) -> Result<(), ClientError> {
        self.send_message(&Message::Done).await
    }
}
