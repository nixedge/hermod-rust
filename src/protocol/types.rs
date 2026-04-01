//! Core protocol types for trace-forward protocol
//!
//! These types match the Haskell definitions in cardano-node to ensure
//! wire-protocol compatibility.

use chrono::{DateTime, Utc};
use pallas_codec::minicbor::{Decode, Encode};
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
    fn encode<W: pallas_codec::minicbor::encode::Write>(
        &self,
        e: &mut pallas_codec::minicbor::Encoder<W>,
        _ctx: &mut (),
    ) -> Result<(), pallas_codec::minicbor::encode::Error<W::Error>> {
        // Haskell Generic Serialise for nullary constructors: array(1)[constructor_index]
        e.array(1)?.u8(*self as u8)?;
        Ok(())
    }
}

impl<'b> Decode<'b, ()> for Severity {
    fn decode(
        d: &mut pallas_codec::minicbor::Decoder<'b>,
        _ctx: &mut (),
    ) -> Result<Self, pallas_codec::minicbor::decode::Error> {
        d.array()?;
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
            _ => Err(pallas_codec::minicbor::decode::Error::message(
                "invalid severity value",
            )),
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
    fn encode<W: pallas_codec::minicbor::encode::Write>(
        &self,
        e: &mut pallas_codec::minicbor::Encoder<W>,
        _ctx: &mut (),
    ) -> Result<(), pallas_codec::minicbor::encode::Error<W::Error>> {
        // Haskell Generic Serialise for nullary constructors: array(1)[constructor_index]
        e.array(1)?.u8(*self as u8)?;
        Ok(())
    }
}

impl<'b> Decode<'b, ()> for DetailLevel {
    fn decode(
        d: &mut pallas_codec::minicbor::Decoder<'b>,
        _ctx: &mut (),
    ) -> Result<Self, pallas_codec::minicbor::decode::Error> {
        d.array()?;
        let val = d.u8()?;
        match val {
            0 => Ok(DetailLevel::DMinimal),
            1 => Ok(DetailLevel::DNormal),
            2 => Ok(DetailLevel::DDetailed),
            3 => Ok(DetailLevel::DMaximum),
            _ => Err(pallas_codec::minicbor::decode::Error::message(
                "invalid detail level",
            )),
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
    fn encode<W: pallas_codec::minicbor::encode::Write>(
        &self,
        e: &mut pallas_codec::minicbor::Encoder<W>,
        ctx: &mut (),
    ) -> Result<(), pallas_codec::minicbor::encode::Error<W::Error>> {
        // Haskell Generic Serialise for a product type with N fields:
        // array(N+1)[constructor_index, field1, ..., fieldN]
        // TraceObject has 8 fields, constructor index 0.
        e.array(9)?;
        e.u8(0)?;

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
        // Haskell's Serialise UTCTime uses tag 1000 (extended time) + map(2):
        //   key 1  -> i64 POSIX seconds
        //   key -12 -> u64 picoseconds within the second
        let secs = self.to_timestamp.timestamp();
        let psecs = self.to_timestamp.timestamp_subsec_nanos() as u64 * 1_000;
        e.tag(pallas_codec::minicbor::data::Tag::new(1000))?;
        e.map(2)?;
        e.u8(1)?;
        e.i64(secs)?;
        e.i64(-12)?;
        e.u64(psecs)?;

        // toHostname :: Text
        e.str(&self.to_hostname)?;

        // toThreadId :: Text
        e.str(&self.to_thread_id)?;

        Ok(())
    }
}

impl<'b> Decode<'b, ()> for TraceObject {
    fn decode(
        d: &mut pallas_codec::minicbor::Decoder<'b>,
        ctx: &mut (),
    ) -> Result<Self, pallas_codec::minicbor::decode::Error> {
        let len = d.array()?;
        if len != Some(9) {
            return Err(pallas_codec::minicbor::decode::Error::message(
                "TraceObject must have 9 elements (constructor index + 8 fields)",
            ));
        }
        // Skip constructor index
        let _constructor_idx = d.u8()?;

        // toHuman :: Maybe Text
        let to_human = {
            let opt_len = d.array()?;
            match opt_len {
                Some(0) => None,
                Some(1) => Some(d.str()?.to_string()),
                _ => {
                    return Err(pallas_codec::minicbor::decode::Error::message(
                        "invalid Maybe encoding",
                    ))
                }
            }
        };

        // toMachine :: Text
        let to_machine = d.str()?.to_string();

        // toNamespace :: [Text]
        // Haskell's Serialise [a] uses indefinite-length encoding for non-empty lists
        let mut to_namespace = Vec::new();
        for s in d.array_iter::<String>()? {
            to_namespace.push(s?);
        }

        // toSeverity :: SeverityS
        let to_severity = Severity::decode(d, ctx)?;

        // toDetails :: DetailLevel
        let to_details = DetailLevel::decode(d, ctx)?;

        // toTimestamp :: UTCTime
        // Haskell's Serialise UTCTime encodes as tag 1000 + map(2): {1: i64_secs, -12: u64_psecs}
        // Also accepts tag 1 + float for compatibility.
        let tag = d.tag()?;
        let to_timestamp = if tag == pallas_codec::minicbor::data::Tag::new(1000) {
            let map_len = d.map()?;
            if map_len != Some(2) {
                return Err(pallas_codec::minicbor::decode::Error::message(
                    "expected map of length 2 for UTCTime (tag 1000)",
                ));
            }
            let k0 = d.i64()?;
            if k0 != 1 {
                return Err(pallas_codec::minicbor::decode::Error::message(
                    "expected key 1 (secs) in tag-1000 UTCTime",
                ));
            }
            let secs = d.i64()?;
            let k1 = d.i64()?;
            if k1 != -12 {
                return Err(pallas_codec::minicbor::decode::Error::message(
                    "expected key -12 (psecs) in tag-1000 UTCTime",
                ));
            }
            let psecs = d.u64()?;
            let nanos = (psecs / 1_000) as u32;
            DateTime::from_timestamp(secs, nanos).ok_or_else(|| {
                pallas_codec::minicbor::decode::Error::message("invalid timestamp")
            })?
        } else if tag == pallas_codec::minicbor::data::Tag::new(1) {
            // Compatibility: tag 1 with float64
            let timestamp_f64 = d.f64()?;
            let secs = timestamp_f64.floor() as i64;
            let nanos = ((timestamp_f64 - secs as f64) * 1_000_000_000.0) as u32;
            DateTime::from_timestamp(secs, nanos).ok_or_else(|| {
                pallas_codec::minicbor::decode::Error::message("invalid timestamp")
            })?
        } else {
            return Err(pallas_codec::minicbor::decode::Error::message(
                "expected UTCTime tag (1000 or 1)",
            ));
        };

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
        let mut encoder = pallas_codec::minicbor::Encoder::new(&mut buf);
        Severity::Info.encode(&mut encoder, &mut ()).unwrap();

        let mut decoder = pallas_codec::minicbor::Decoder::new(&buf);
        let decoded = Severity::decode(&mut decoder, &mut ()).unwrap();
        assert_eq!(decoded, Severity::Info);
    }

    #[test]
    fn test_detail_level_encoding() {
        let mut buf = Vec::new();
        let mut encoder = pallas_codec::minicbor::Encoder::new(&mut buf);
        DetailLevel::DNormal.encode(&mut encoder, &mut ()).unwrap();

        let mut decoder = pallas_codec::minicbor::Decoder::new(&buf);
        let decoded = DetailLevel::decode(&mut decoder, &mut ()).unwrap();
        assert_eq!(decoded, DetailLevel::DNormal);
    }
}
