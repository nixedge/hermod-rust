//! Core traits and types for the dispatcher
//!
//! Mirrors the Haskell `MetaTrace` and `LogFormatting` typeclasses and
//! related types from `Cardano.Logging.Types`.

use crate::protocol::types::{DetailLevel, Severity};
use serde_json::{Map, Value};

/// Hierarchical namespace for a trace message
///
/// Mirrors Haskell `Namespace a = Namespace { nsPrefix :: [Text], nsInner :: [Text] }`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Namespace {
    /// Outer prefix (set by the tracer infrastructure)
    pub prefix: Vec<String>,
    /// Inner segments (set by the message type itself)
    pub inner: Vec<String>,
}

impl Namespace {
    /// Create a namespace from inner segments only (no prefix)
    pub fn new(inner: Vec<String>) -> Self {
        Self {
            prefix: vec![],
            inner,
        }
    }

    /// Combine prefix and inner into a single flat list
    pub fn complete(&self) -> Vec<String> {
        let mut v = self.prefix.clone();
        v.extend_from_slice(&self.inner);
        v
    }

    /// Convert to dot-separated string representation
    pub fn to_text(&self) -> String {
        self.complete().join(".")
    }
}

impl std::fmt::Display for Namespace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_text())
    }
}

/// Privacy classification for a trace message
///
/// Confidential messages are not forwarded to the `Forwarder` backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Privacy {
    /// Message can be forwarded to external backends
    #[default]
    Public,
    /// Message must not leave the local node
    Confidential,
}

/// A single metric value emitted by a trace message
///
/// Mirrors Haskell `Metric`.
#[derive(Debug, Clone)]
pub enum Metric {
    /// Integer gauge metric
    IntM(String, i64),
    /// Floating-point gauge metric
    DoubleM(String, f64),
    /// Counter metric (with optional initial value)
    CounterM(String, Option<i64>),
    /// Prometheus metric with label key–value pairs
    PrometheusM(String, Vec<(String, String)>),
}

impl Metric {
    /// Return the metric name
    pub fn name(&self) -> &str {
        match self {
            Metric::IntM(n, _) => n,
            Metric::DoubleM(n, _) => n,
            Metric::CounterM(n, _) => n,
            Metric::PrometheusM(n, _) => n,
        }
    }
}

/// Severity filter: `None` means Silence (block everything)
///
/// Mirrors Haskell `SeverityF = SeverityF (Maybe SeverityS)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeverityF(pub Option<Severity>);

impl SeverityF {
    /// `Silence` — blocks all messages regardless of severity
    pub const SILENCE: Self = Self(None);

    /// Check whether a message with `sev` should pass through this filter
    pub fn passes(&self, sev: Severity) -> bool {
        match self.0 {
            None => false,
            Some(threshold) => sev >= threshold,
        }
    }
}

impl Default for SeverityF {
    /// Default is `Warning` (matching the Haskell default)
    fn default() -> Self {
        Self(Some(Severity::Warning))
    }
}

/// Metadata that every traceable message type must provide
///
/// Mirrors the Haskell `MetaTrace` typeclass.
pub trait MetaTrace {
    /// Return the namespace for this specific message instance
    fn namespace(&self) -> Namespace;

    /// Return the severity for a given namespace/instance combination.
    /// `None` defers to the config-driven default.
    fn severity(&self) -> Option<Severity>;

    /// Return the privacy classification for this message
    fn privacy(&self) -> Privacy {
        Privacy::Public
    }

    /// Return the detail level preference for this message
    fn detail(&self) -> Option<DetailLevel> {
        Some(DetailLevel::DNormal)
    }

    /// Return all namespaces this message type can produce (for config validation)
    fn all_namespaces() -> Vec<Namespace>
    where
        Self: Sized,
    {
        vec![]
    }
}

/// Formatting methods for trace messages
///
/// Mirrors the Haskell `LogFormatting` typeclass.
pub trait LogFormatting {
    /// Machine-readable JSON object representation at the given detail level
    fn for_machine(&self, detail: DetailLevel) -> Map<String, Value>;

    /// Human-readable string representation.
    /// Returning an empty string falls back to the machine format.
    fn for_human(&self) -> String {
        String::new()
    }

    /// Metrics emitted alongside this trace message
    fn as_metrics(&self) -> Vec<Metric> {
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_blocks_every_severity() {
        for sev in [
            Severity::Debug,
            Severity::Info,
            Severity::Notice,
            Severity::Warning,
            Severity::Error,
            Severity::Critical,
            Severity::Alert,
            Severity::Emergency,
        ] {
            assert!(
                !SeverityF::SILENCE.passes(sev),
                "{:?} should be blocked by Silence",
                sev
            );
        }
    }

    #[test]
    fn warning_threshold_blocks_below_passes_at_and_above() {
        let f = SeverityF(Some(Severity::Warning));
        assert!(!f.passes(Severity::Debug));
        assert!(!f.passes(Severity::Info));
        assert!(!f.passes(Severity::Notice));
        assert!(f.passes(Severity::Warning));
        assert!(f.passes(Severity::Error));
        assert!(f.passes(Severity::Critical));
        assert!(f.passes(Severity::Alert));
        assert!(f.passes(Severity::Emergency));
    }

    #[test]
    fn default_is_warning() {
        assert_eq!(SeverityF::default(), SeverityF(Some(Severity::Warning)));
    }

    #[test]
    fn debug_threshold_passes_all() {
        let f = SeverityF(Some(Severity::Debug));
        for sev in [
            Severity::Debug,
            Severity::Info,
            Severity::Notice,
            Severity::Warning,
            Severity::Error,
            Severity::Critical,
            Severity::Alert,
            Severity::Emergency,
        ] {
            assert!(f.passes(sev), "{:?} should pass Debug threshold", sev);
        }
    }

    #[test]
    fn emergency_threshold_only_passes_emergency() {
        let f = SeverityF(Some(Severity::Emergency));
        for sev in [
            Severity::Debug,
            Severity::Info,
            Severity::Notice,
            Severity::Warning,
            Severity::Error,
            Severity::Critical,
            Severity::Alert,
        ] {
            assert!(
                !f.passes(sev),
                "{:?} should be blocked by Emergency threshold",
                sev
            );
        }
        assert!(f.passes(Severity::Emergency));
    }
}
