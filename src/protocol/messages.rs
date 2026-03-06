//! Protocol messages for trace-forward protocol
//!
//! The protocol has three messages:
//! - MsgTraceObjectsRequest: Acceptor requests N trace objects
//! - MsgTraceObjectsReply: Forwarder replies with trace objects
//! - MsgDone: Acceptor terminates the session

use super::types::TraceObject;

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
