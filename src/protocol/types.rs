//! Core protocol types for trace-forward protocol
//!
//! These types match the Haskell definitions in cardano-node to ensure
//! wire-protocol compatibility.

use chrono::{DateTime, Utc};
use minicbor::{Decode, Encode};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Severity level for trace messages
///
/// Must match the Haskell `SeverityS` enum exactly for wire compatibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(u8)]
pub enum Severity {
    /// Debug messages
    Debug = 0,
    /// Information
    Info = 1,
    /// Normal runtime conditions
    Notice = 2,
    /// General warnings
    Warning = 3,
    /// General errors
    Error = 4,
    /// Severe situations
    Critical = 5,
    /// Take immediate action
    Alert = 6,
    /// System is unusable
    Emergency = 7,
}

impl Encode<()> for Severity {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut minicbor::Encoder<W>,
        _ctx: &mut (),
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        e.u8(*self as u8)?;
        Ok(())
    }
}

impl<'b> Decode<'b, ()> for Severity {
    fn decode(
        d: &mut minicbor::Decoder<'b>,
        _ctx: &mut (),
    ) -> Result<Self, minicbor::decode::Error> {
        let val = d.u8()?;
        match val {
            0 => Ok(Severity::Debug),
            1 => Ok(Severity::Info),
            2 => Ok(Severity::Notice),
            3 => Ok(Severity::Warning),
            4 => Ok(Severity::Error),
            5 => Ok(Severity::Critical),
            6 => Ok(Severity::Alert),
            7 => Ok(Severity::Emergency),
            _ => Err(minicbor::decode::Error::message("invalid severity value")),
        }
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Severity::Debug => write!(f, "Debug"),
            Severity::Info => write!(f, "Info"),
            Severity::Notice => write!(f, "Notice"),
            Severity::Warning => write!(f, "Warning"),
            Severity::Error => write!(f, "Error"),
            Severity::Critical => write!(f, "Critical"),
            Severity::Alert => write!(f, "Alert"),
            Severity::Emergency => write!(f, "Emergency"),
        }
    }
}

/// Detail level (formerly known as verbosity)
///
/// Must match the Haskell `DetailLevel` enum exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(u8)]
pub enum DetailLevel {
    /// Minimal detail
    DMinimal = 0,
    /// Normal detail
    DNormal = 1,
    /// Detailed
    DDetailed = 2,
    /// Maximum detail
    DMaximum = 3,
}

impl Encode<()> for DetailLevel {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut minicbor::Encoder<W>,
        _ctx: &mut (),
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        e.u8(*self as u8)?;
        Ok(())
    }
}

impl<'b> Decode<'b, ()> for DetailLevel {
    fn decode(
        d: &mut minicbor::Decoder<'b>,
        _ctx: &mut (),
    ) -> Result<Self, minicbor::decode::Error> {
        let val = d.u8()?;
        match val {
            0 => Ok(DetailLevel::DMinimal),
            1 => Ok(DetailLevel::DNormal),
            2 => Ok(DetailLevel::DDetailed),
            3 => Ok(DetailLevel::DMaximum),
            _ => Err(minicbor::decode::Error::message("invalid detail level")),
        }
    }
}

/// A trace object sent over the wire
///
/// This must match the Haskell `TraceObject` structure exactly:
/// ```haskell
/// data TraceObject = TraceObject {
///     toHuman     :: !(Maybe Text)
///   , toMachine   :: !Text
///   , toNamespace :: ![Text]
///   , toSeverity  :: !SeverityS
///   , toDetails   :: !DetailLevel
///   , toTimestamp :: !UTCTime
///   , toHostname  :: !Text
///   , toThreadId  :: !Text
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceObject {
    /// Human-readable representation (if available)
    pub to_human: Option<String>,
    /// Machine-readable representation (JSON)
    pub to_machine: String,
    /// Hierarchical namespace for the trace
    pub to_namespace: Vec<String>,
    /// Severity level
    pub to_severity: Severity,
    /// Detail level
    pub to_details: DetailLevel,
    /// Timestamp when the trace was created
    pub to_timestamp: DateTime<Utc>,
    /// Hostname of the machine generating the trace
    pub to_hostname: String,
    /// Thread ID that generated the trace
    pub to_thread_id: String,
}

impl Encode<()> for TraceObject {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut minicbor::Encoder<W>,
        ctx: &mut (),
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        // Encode as an 8-element array matching the Haskell record order
        e.array(8)?;

        // toHuman :: Maybe Text
        match &self.to_human {
            Some(h) => {
                e.array(1)?;
                e.str(h)?;
            }
            None => {
                e.array(0)?;
            }
        }

        // toMachine :: Text
        e.str(&self.to_machine)?;

        // toNamespace :: [Text]
        e.array(self.to_namespace.len() as u64)?;
        for ns in &self.to_namespace {
            e.str(ns)?;
        }

        // toSeverity :: SeverityS
        self.to_severity.encode(e, ctx)?;

        // toDetails :: DetailLevel
        self.to_details.encode(e, ctx)?;

        // toTimestamp :: UTCTime
        // Encode as CBOR tag 1 (epoch time) with a float
        e.tag(minicbor::data::Tag::new(1))?;
        let timestamp_secs = self.to_timestamp.timestamp() as f64
            + (self.to_timestamp.timestamp_subsec_nanos() as f64 / 1_000_000_000.0);
        e.f64(timestamp_secs)?;

        // toHostname :: Text
        e.str(&self.to_hostname)?;

        // toThreadId :: Text
        e.str(&self.to_thread_id)?;

        Ok(())
    }
}

impl<'b> Decode<'b, ()> for TraceObject {
    fn decode(
        d: &mut minicbor::Decoder<'b>,
        ctx: &mut (),
    ) -> Result<Self, minicbor::decode::Error> {
        let len = d.array()?;
        if len != Some(8) {
            return Err(minicbor::decode::Error::message(
                "TraceObject must have 8 fields",
            ));
        }

        // toHuman :: Maybe Text
        let to_human = {
            let opt_len = d.array()?;
            match opt_len {
                Some(0) => None,
                Some(1) => Some(d.str()?.to_string()),
                _ => return Err(minicbor::decode::Error::message("invalid Maybe encoding")),
            }
        };

        // toMachine :: Text
        let to_machine = d.str()?.to_string();

        // toNamespace :: [Text]
        let ns_len = d.array()?.ok_or_else(|| {
            minicbor::decode::Error::message("namespace must have definite length")
        })?;
        let mut to_namespace = Vec::with_capacity(ns_len as usize);
        for _ in 0..ns_len {
            to_namespace.push(d.str()?.to_string());
        }

        // toSeverity :: SeverityS
        let to_severity = Severity::decode(d, ctx)?;

        // toDetails :: DetailLevel
        let to_details = DetailLevel::decode(d, ctx)?;

        // toTimestamp :: UTCTime
        let tag = d.tag()?;
        if tag != minicbor::data::Tag::new(1) {
            return Err(minicbor::decode::Error::message(
                "expected DateTime tag (tag 1)",
            ));
        }
        let timestamp_f64 = d.f64()?;
        let secs = timestamp_f64.floor() as i64;
        let nanos = ((timestamp_f64 - secs as f64) * 1_000_000_000.0) as u32;
        let to_timestamp = DateTime::from_timestamp(secs, nanos)
            .ok_or_else(|| minicbor::decode::Error::message("invalid timestamp"))?;

        // toHostname :: Text
        let to_hostname = d.str()?.to_string();

        // toThreadId :: Text
        let to_thread_id = d.str()?.to_string();

        Ok(TraceObject {
            to_human,
            to_machine,
            to_namespace,
            to_severity,
            to_details,
            to_timestamp,
            to_hostname,
            to_thread_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_severity_encoding() {
        let mut buf = Vec::new();
        let mut encoder = minicbor::Encoder::new(&mut buf);
        Severity::Info.encode(&mut encoder, &mut ()).unwrap();

        let mut decoder = minicbor::Decoder::new(&buf);
        let decoded = Severity::decode(&mut decoder, &mut ()).unwrap();
        assert_eq!(decoded, Severity::Info);
    }

    #[test]
    fn test_detail_level_encoding() {
        let mut buf = Vec::new();
        let mut encoder = minicbor::Encoder::new(&mut buf);
        DetailLevel::DNormal.encode(&mut encoder, &mut ()).unwrap();

        let mut decoder = minicbor::Decoder::new(&buf);
        let decoded = DetailLevel::decode(&mut decoder, &mut ()).unwrap();
        assert_eq!(decoded, DetailLevel::DNormal);
    }
}
