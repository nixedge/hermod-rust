//! Trace configuration types and YAML parsing
//!
//! Mirrors Haskell `TraceConfig`, `ConfigOption`, `BackendConfig`, `FormatLogging`
//! from `Cardano.Logging.Types` and `ConfigurationParser`.

use crate::dispatcher::traits::SeverityF;
use crate::protocol::types::{DetailLevel, Severity};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// Logging format for the Stdout backend
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum FormatLogging {
    /// Human-readable with ANSI colour codes
    HumanFormatColoured,
    /// Human-readable without colour codes
    HumanFormatUncoloured,
    /// Machine-readable JSON
    MachineFormat,
}

/// Which backend should receive a trace message
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum BackendConfig {
    /// Forward to hermod-tracer via the trace-forward protocol
    Forwarder,
    /// Write to standard output
    Stdout(FormatLogging),
    /// Push to EKG / Prometheus
    EkgBackend,
    /// Send to a datapoint backend (stub)
    DatapointBackend,
}

/// Configuration option for a single namespace entry
///
/// Mirrors Haskell `ConfigOption`.
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigOption {
    /// Severity filter (None = Silence)
    Severity(SeverityF),
    /// Detail level
    Detail(DetailLevel),
    /// List of backends to route to
    Backends(Vec<BackendConfig>),
    /// Rate limiter: maximum messages per second
    Limiter(f64),
}

/// Forwarder connection options
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForwarderOptions {
    /// Path to the Unix socket
    pub socket_path: Option<String>,
    /// Outbound queue size
    pub queue_size: Option<u32>,
    /// Maximum reconnection delay in seconds
    pub max_reconnect_delay: Option<u32>,
}

/// Top-level trace configuration
///
/// Mirrors Haskell `TraceConfig`.
#[derive(Debug, Clone, Default)]
pub struct TraceConfig {
    /// Namespace-keyed configuration options (longest-prefix-match lookup)
    pub options: BTreeMap<Vec<String>, Vec<ConfigOption>>,
    /// Optional forwarder connection settings
    pub forwarder: Option<ForwarderOptions>,
    /// Optional human-readable node name
    pub node_name: Option<String>,
}

impl TraceConfig {
    /// Look up a config option using longest-prefix-match semantics.
    ///
    /// Mirrors Haskell `getOption sel config ns`.
    pub fn get_option<F, T>(&self, ns: &[String], selector: F) -> Option<T>
    where
        F: Fn(&ConfigOption) -> Option<T>,
    {
        // Try the exact key, then progressively shorter prefixes down to `[]`
        let mut key = ns.to_vec();
        loop {
            if let Some(opts) = self.options.get(&key) {
                if let Some(v) = opts.iter().find_map(&selector) {
                    return Some(v);
                }
            }
            if key.is_empty() {
                return None;
            }
            key.pop();
        }
    }

    /// Get the severity filter for a namespace
    pub fn severity_for(&self, ns: &[String]) -> SeverityF {
        self.get_option(ns, |o| {
            if let ConfigOption::Severity(s) = o {
                Some(*s)
            } else {
                None
            }
        })
        .unwrap_or(SeverityF(Some(Severity::Warning)))
    }

    /// Get the detail level for a namespace
    pub fn detail_for(&self, ns: &[String]) -> DetailLevel {
        self.get_option(ns, |o| {
            if let ConfigOption::Detail(d) = o {
                Some(*d)
            } else {
                None
            }
        })
        .unwrap_or(DetailLevel::DNormal)
    }

    /// Get the backend list for a namespace
    pub fn backends_for(&self, ns: &[String]) -> Vec<BackendConfig> {
        self.get_option(ns, |o| {
            if let ConfigOption::Backends(b) = o {
                Some(b.clone())
            } else {
                None
            }
        })
        .unwrap_or_else(|| {
            vec![
                BackendConfig::Stdout(FormatLogging::MachineFormat),
                BackendConfig::EkgBackend,
                BackendConfig::Forwarder,
            ]
        })
    }

    /// Get the rate limiter max-frequency for a namespace, if configured
    pub fn limiter_for(&self, ns: &[String]) -> Option<f64> {
        self.get_option(ns, |o| {
            if let ConfigOption::Limiter(f) = o {
                Some(*f)
            } else {
                None
            }
        })
    }

    /// Build a [`crate::forwarder::ForwarderConfig`] from this `TraceConfig`.
    ///
    /// Returns `None` if `self.forwarder` is not set (no forwarder configured).
    /// The `node_name` field is propagated automatically so the forwarder
    /// advertises the correct name via the `NodeInfo` DataPoint.
    pub fn forwarder_config(&self) -> Option<crate::forwarder::ForwarderConfig> {
        let opts = self.forwarder.as_ref()?;
        let mut cfg = crate::forwarder::ForwarderConfig::default();
        if let Some(path) = &opts.socket_path {
            cfg.socket_path = std::path::PathBuf::from(path);
        }
        if let Some(qs) = opts.queue_size {
            cfg.queue_size = qs as usize;
        }
        if let Some(delay) = opts.max_reconnect_delay {
            cfg.max_reconnect_delay = delay as u64;
        }
        cfg.node_name = self.node_name.clone();
        Some(cfg)
    }

    /// Parse a `TraceConfig` from a YAML file
    pub fn from_yaml(path: &Path) -> Result<Self> {
        let content =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::from_yaml_str(&content)
    }

    /// Parse a `TraceConfig` from a YAML string
    pub fn from_yaml_str(yaml: &str) -> Result<Self> {
        let raw: RawConfig = serde_yaml::from_str(yaml).context("parsing TraceConfig YAML")?;
        Ok(raw.into_trace_config())
    }
}

// ---------------------------------------------------------------------------
// Raw YAML deserialisation helpers
// ---------------------------------------------------------------------------

/// Raw YAML structure before conversion
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawConfig {
    #[serde(default)]
    trace_options: BTreeMap<String, RawNamespaceOptions>,
    #[serde(default)]
    node_name: Option<String>,
    // We intentionally ignore unknown fields (UseTraceDispatcher, etc.)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawNamespaceOptions {
    severity: Option<RawSeverity>,
    detail: Option<RawDetailLevel>,
    #[serde(default)]
    backends: Vec<String>,
    max_frequency: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
enum RawSeverity {
    Debug,
    Info,
    Notice,
    Warning,
    Error,
    Critical,
    Alert,
    Emergency,
    Silence,
}

#[derive(Debug, Deserialize)]
#[allow(clippy::enum_variant_names)] // Haskell-compatible names: DMinimal, DNormal, etc.
enum RawDetailLevel {
    DMinimal,
    DNormal,
    DDetailed,
    DMaximum,
}

impl RawConfig {
    fn into_trace_config(self) -> TraceConfig {
        let mut options: BTreeMap<Vec<String>, Vec<ConfigOption>> = BTreeMap::new();

        for (key, raw_opts) in self.trace_options {
            // "" → [], "ChainDB.AddBlock" → ["ChainDB", "AddBlock"]
            let ns_key: Vec<String> = if key.is_empty() {
                vec![]
            } else {
                key.split('.').map(|s| s.to_string()).collect()
            };

            let mut opts = Vec::new();

            if let Some(sev) = raw_opts.severity {
                opts.push(ConfigOption::Severity(sev.into()));
            }
            if let Some(det) = raw_opts.detail {
                opts.push(ConfigOption::Detail(det.into()));
            }
            if !raw_opts.backends.is_empty() {
                let backends: Vec<BackendConfig> = raw_opts
                    .backends
                    .iter()
                    .filter_map(|s| parse_backend(s))
                    .collect();
                if !backends.is_empty() {
                    opts.push(ConfigOption::Backends(backends));
                }
            }
            if let Some(freq) = raw_opts.max_frequency {
                opts.push(ConfigOption::Limiter(freq));
            }

            if !opts.is_empty() {
                options.insert(ns_key, opts);
            }
        }

        TraceConfig {
            options,
            forwarder: None,
            node_name: self.node_name,
        }
    }
}

impl From<RawSeverity> for SeverityF {
    fn from(r: RawSeverity) -> Self {
        match r {
            RawSeverity::Debug => SeverityF(Some(Severity::Debug)),
            RawSeverity::Info => SeverityF(Some(Severity::Info)),
            RawSeverity::Notice => SeverityF(Some(Severity::Notice)),
            RawSeverity::Warning => SeverityF(Some(Severity::Warning)),
            RawSeverity::Error => SeverityF(Some(Severity::Error)),
            RawSeverity::Critical => SeverityF(Some(Severity::Critical)),
            RawSeverity::Alert => SeverityF(Some(Severity::Alert)),
            RawSeverity::Emergency => SeverityF(Some(Severity::Emergency)),
            RawSeverity::Silence => SeverityF(None),
        }
    }
}

impl From<RawDetailLevel> for DetailLevel {
    fn from(r: RawDetailLevel) -> Self {
        match r {
            RawDetailLevel::DMinimal => DetailLevel::DMinimal,
            RawDetailLevel::DNormal => DetailLevel::DNormal,
            RawDetailLevel::DDetailed => DetailLevel::DDetailed,
            RawDetailLevel::DMaximum => DetailLevel::DMaximum,
        }
    }
}

fn parse_backend(s: &str) -> Option<BackendConfig> {
    match s.trim() {
        "Forwarder" => Some(BackendConfig::Forwarder),
        "EKGBackend" => Some(BackendConfig::EkgBackend),
        "DatapointBackend" => Some(BackendConfig::DatapointBackend),
        "Stdout HumanFormatColoured" => {
            Some(BackendConfig::Stdout(FormatLogging::HumanFormatColoured))
        }
        "Stdout HumanFormatUncoloured" => {
            Some(BackendConfig::Stdout(FormatLogging::HumanFormatUncoloured))
        }
        "Stdout MachineFormat" => Some(BackendConfig::Stdout(FormatLogging::MachineFormat)),
        other => {
            tracing::warn!("Unknown backend config string: {:?}", other);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_YAML: &str = r#"
UseTraceDispatcher: True

TraceOptions:
  "":
    severity: Notice
    detail: DNormal
    backends:
      - Stdout MachineFormat
      - EKGBackend
      - Forwarder

  ChainDB:
    severity: Info

  ChainDB.AddBlockEvent.AddedBlockToQueue:
    maxFrequency: 2.0
"#;

    #[test]
    fn test_parse_yaml() {
        let cfg = TraceConfig::from_yaml_str(SAMPLE_YAML).unwrap();

        // Global default
        let global = cfg.options.get(&vec![] as &Vec<String>).unwrap();
        assert!(global
            .iter()
            .any(|o| matches!(o, ConfigOption::Severity(_))));
        assert!(global
            .iter()
            .any(|o| matches!(o, ConfigOption::Backends(_))));

        // ChainDB severity
        let chaindb = cfg.options.get(&vec!["ChainDB".to_string()]).unwrap();
        assert!(chaindb
            .iter()
            .any(|o| matches!(o, ConfigOption::Severity(SeverityF(Some(Severity::Info))))));

        // Rate limiter
        let limiter_key = vec![
            "ChainDB".to_string(),
            "AddBlockEvent".to_string(),
            "AddedBlockToQueue".to_string(),
        ];
        let limiter_opts = cfg.options.get(&limiter_key).unwrap();
        assert!(limiter_opts
            .iter()
            .any(|o| matches!(o, ConfigOption::Limiter(_))));
    }

    #[test]
    fn test_longest_prefix_match() {
        let cfg = TraceConfig::from_yaml_str(SAMPLE_YAML).unwrap();

        // Exact key "ChainDB" → Info
        let sev = cfg.severity_for(&["ChainDB".to_string()]);
        assert_eq!(sev, SeverityF(Some(Severity::Info)));

        // Subnamespace falls back to "ChainDB"
        let sev2 = cfg.severity_for(&["ChainDB".to_string(), "SomeChild".to_string()]);
        assert_eq!(sev2, SeverityF(Some(Severity::Info)));

        // Unknown namespace falls back to global default (Notice)
        let sev3 = cfg.severity_for(&["Unknown".to_string()]);
        assert_eq!(sev3, SeverityF(Some(Severity::Notice)));
    }

    #[test]
    fn test_backends_parsing() {
        let cfg = TraceConfig::from_yaml_str(SAMPLE_YAML).unwrap();
        let backends = cfg.backends_for(&[]);
        assert!(backends.contains(&BackendConfig::Forwarder));
        assert!(backends.contains(&BackendConfig::Stdout(FormatLogging::MachineFormat)));
        assert!(backends.contains(&BackendConfig::EkgBackend));
    }
}
