//! Haskell-compatible YAML configuration types for `hermod-tracer`
//!
//! [`TracerConfig`] mirrors the Haskell `TracerConfig` record from
//! `cardano-tracer`.  Field names use Haskell camelCase via
//! `#[serde(rename = "...")]` so that existing `cardano-tracer` YAML config
//! files work with `hermod-tracer` unchanged.
//!
//! See `config/hermod-tracer.yaml` in the repository for a fully-annotated
//! example with all options and their defaults.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Top-level tracer configuration
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TracerConfig {
    /// Cardano network magic
    #[serde(rename = "networkMagic")]
    pub network_magic: u32,

    /// How to connect to forwarder nodes
    pub network: Network,

    /// Number of trace objects to request per round-trip (default 100)
    #[serde(rename = "loRequestNum")]
    pub lo_request_num: Option<u16>,

    /// Frequency of EKG metric polls in seconds (default 1.0)
    #[serde(rename = "ekgRequestFreq")]
    pub ekg_request_freq: Option<f64>,

    /// Enable EKG HTTP endpoint at this address
    #[serde(rename = "hasEKG")]
    pub has_ekg: Option<Endpoint>,

    /// Enable Prometheus HTTP endpoint at this address
    #[serde(rename = "hasPrometheus")]
    pub has_prometheus: Option<Endpoint>,

    /// Re-forwarding configuration
    #[serde(rename = "hasForwarding")]
    pub has_forwarding: Option<ReForwardingConfig>,

    /// Log output configurations (at least one required)
    pub logging: Vec<LoggingParams>,

    /// Log rotation parameters
    pub rotation: Option<RotationParams>,

    /// Verbosity level for tracer's own logging
    pub verbosity: Option<Verbosity>,

    /// If true, strip `_total`/`_int`/`_double` suffixes from Prometheus metric names
    #[serde(rename = "metricsNoSuffix")]
    pub metrics_no_suffix: Option<bool>,

    /// Whether to request all metrics (true) or only updated metrics (false)
    #[serde(rename = "ekgRequestFull")]
    pub ekg_request_full: Option<bool>,

    /// Extra labels to attach to Prometheus metrics
    #[serde(rename = "prometheusLabels")]
    pub prometheus_labels: Option<HashMap<String, String>>,
}

impl TracerConfig {
    /// Parse from a YAML file path
    pub fn from_file(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("reading config file {}", path.display()))?;
        Self::from_str(&content)
    }

    /// Parse from a YAML string
    pub fn from_str(yaml: &str) -> Result<Self> {
        serde_yaml::from_str(yaml).context("parsing TracerConfig YAML")
    }

    /// Number of traces to request per round-trip
    pub fn lo_request_num(&self) -> u16 {
        self.lo_request_num.unwrap_or(100)
    }

    /// EKG poll frequency in seconds
    pub fn ekg_request_freq(&self) -> f64 {
        self.ekg_request_freq.unwrap_or(1.0)
    }
}

/// How to connect to forwarder nodes
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "tag", content = "contents")]
pub enum Network {
    /// Listen on this address for forwarder connections
    AcceptAt(Address),
    /// Connect out to these addresses (each is a forwarder)
    ConnectTo(Vec<Address>),
}

/// A network address — either a Unix socket path or a TCP host:port
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Address {
    /// Unix domain socket
    LocalPipe(PathBuf),
    /// TCP address
    RemoteSocket(String, u16),
}

impl Address {
    /// Display as a string (for node ID assignment)
    pub fn to_node_id(&self) -> String {
        match self {
            Address::LocalPipe(p) => p.display().to_string(),
            Address::RemoteSocket(host, port) => format!("{}:{}", host, port),
        }
    }
}

impl<'de> Deserialize<'de> for Address {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let s = String::deserialize(de)?;
        // If string looks like "host:port" and port is a valid u16, treat as TCP
        if let Some(idx) = s.rfind(':') {
            let potential_port = &s[idx + 1..];
            if let Ok(port) = potential_port.parse::<u16>() {
                let host = s[..idx].to_string();
                // Sanity check: host should not contain '/' (which would indicate a path)
                if !host.contains('/') {
                    return Ok(Address::RemoteSocket(host, port));
                }
            }
        }
        Ok(Address::LocalPipe(PathBuf::from(s)))
    }
}

impl Serialize for Address {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Address::LocalPipe(p) => s.serialize_str(&p.display().to_string()),
            Address::RemoteSocket(host, port) => s.serialize_str(&format!("{}:{}", host, port)),
        }
    }
}

/// An HTTP endpoint (host + port)
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Endpoint {
    /// Hostname or IP address
    #[serde(rename = "epHost")]
    pub ep_host: String,
    /// Port number
    #[serde(rename = "epPort")]
    pub ep_port: u16,
}

impl Endpoint {
    /// Return `"host:port"` string
    pub fn to_addr(&self) -> String {
        format!("{}:{}", self.ep_host, self.ep_port)
    }
}

/// Per-logging-destination configuration
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LoggingParams {
    /// Root directory for log files
    #[serde(rename = "logRoot")]
    pub log_root: PathBuf,

    /// Whether to use file-based or journal-based logging
    #[serde(rename = "logMode")]
    pub log_mode: LogMode,

    /// Human-readable or machine-readable format
    #[serde(rename = "logFormat")]
    pub log_format: LogFormat,
}

/// Logging mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum LogMode {
    /// Write to files under `log_root`
    FileMode,
    /// Write to the system journal (journald)
    JournalMode,
}

/// Log format
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub enum LogFormat {
    /// Human-readable text
    ForHuman,
    /// Machine-readable JSON
    ForMachine,
}

/// Log rotation parameters
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RotationParams {
    /// How often to check for rotation (seconds)
    #[serde(rename = "rpFrequencySecs")]
    pub rp_frequency_secs: u32,

    /// Rotate when the current file exceeds this size (bytes)
    #[serde(rename = "rpLogLimitBytes")]
    pub rp_log_limit_bytes: u64,

    /// Delete files older than this many hours
    #[serde(rename = "rpMaxAgeHours")]
    pub rp_max_age_hours: u64,

    /// Always keep at least this many of the newest files
    #[serde(rename = "rpKeepFilesNum")]
    pub rp_keep_files_num: u32,
}

/// Tracer's own log verbosity
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum Verbosity {
    /// Log everything
    Maximum,
    /// Log errors only
    ErrorsOnly,
    /// Log nothing
    Minimum,
}

/// Re-forwarding configuration
///
/// Receives traces and relays them to a downstream acceptor socket,
/// optionally filtering by namespace prefix.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReForwardingConfig {
    /// Where to accept downstream connections (or connect to downstream)
    pub network: Network,

    /// Optional namespace prefix filters; only matching traces are forwarded
    #[serde(rename = "namespaceFilters")]
    pub namespace_filters: Option<Vec<Vec<String>>>,

    /// Forwarding options (queue size, reconnect delay, etc.)
    #[serde(rename = "forwarderOpts")]
    pub forwarder_opts: TraceOptionForwarder,
}

/// Forwarder options within a re-forwarding config
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TraceOptionForwarder {
    /// Outbound queue capacity
    #[serde(rename = "queueSize", default = "default_queue_size")]
    pub queue_size: usize,
}

fn default_queue_size() -> usize {
    1000
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL_YAML: &str = r#"
networkMagic: 42
network:
  tag: AcceptAt
  contents: "/tmp/hermod.sock"
logging:
- logRoot: "/tmp/hermod-logs"
  logMode: FileMode
  logFormat: ForMachine
"#;

    const COMPLETE_YAML: &str = r#"
networkMagic: 42
network:
  tag: ConnectTo
  contents:
  - "/tmp/hermod.sock"
loRequestNum: 100
ekgRequestFreq: 2
hasEKG:
  epHost: 127.0.0.1
  epPort: 9754
hasPrometheus:
  epHost: 127.0.0.1
  epPort: 9753
logging:
- logRoot: "/tmp/hermod-logs-human"
  logMode: FileMode
  logFormat: ForHuman
- logRoot: "/tmp/hermod-logs"
  logMode: FileMode
  logFormat: ForMachine
rotation:
  rpFrequencySecs: 15
  rpKeepFilesNum: 1
  rpLogLimitBytes: 50000
  rpMaxAgeHours: 1
verbosity: ErrorsOnly
"#;

    #[test]
    fn test_parse_minimal_yaml() {
        let cfg = TracerConfig::from_str(MINIMAL_YAML).unwrap();
        assert_eq!(cfg.network_magic, 42);
        assert!(matches!(cfg.network, Network::AcceptAt(_)));
        if let Network::AcceptAt(addr) = &cfg.network {
            assert_eq!(*addr, Address::LocalPipe("/tmp/hermod.sock".into()));
        }
        assert_eq!(cfg.logging.len(), 1);
        assert_eq!(cfg.logging[0].log_format, LogFormat::ForMachine);
        assert_eq!(cfg.lo_request_num(), 100); // default
        assert!((cfg.ekg_request_freq() - 1.0).abs() < f64::EPSILON); // default
    }

    #[test]
    fn test_parse_complete_yaml() {
        let cfg = TracerConfig::from_str(COMPLETE_YAML).unwrap();
        assert_eq!(cfg.network_magic, 42);
        assert!(matches!(cfg.network, Network::ConnectTo(_)));
        if let Network::ConnectTo(addrs) = &cfg.network {
            assert_eq!(addrs.len(), 1);
            assert_eq!(addrs[0], Address::LocalPipe("/tmp/hermod.sock".into()));
        }
        assert_eq!(cfg.lo_request_num(), 100);
        assert!((cfg.ekg_request_freq() - 2.0).abs() < f64::EPSILON);
        assert!(cfg.has_ekg.is_some());
        assert!(cfg.has_prometheus.is_some());
        let prom = cfg.has_prometheus.as_ref().unwrap();
        assert_eq!(prom.ep_host, "127.0.0.1");
        assert_eq!(prom.ep_port, 9753);
        assert_eq!(cfg.logging.len(), 2);
        assert_eq!(cfg.logging[0].log_format, LogFormat::ForHuman);
        assert_eq!(cfg.logging[1].log_format, LogFormat::ForMachine);
        let rot = cfg.rotation.as_ref().unwrap();
        assert_eq!(rot.rp_frequency_secs, 15);
        assert_eq!(rot.rp_log_limit_bytes, 50000);
        assert_eq!(rot.rp_max_age_hours, 1);
        assert_eq!(rot.rp_keep_files_num, 1);
        assert_eq!(cfg.verbosity, Some(Verbosity::ErrorsOnly));
    }

    #[test]
    fn test_address_parsing_unix() {
        let addr: Address = serde_yaml::from_str("\"/tmp/my.sock\"").unwrap();
        assert_eq!(addr, Address::LocalPipe("/tmp/my.sock".into()));
    }

    #[test]
    fn test_address_parsing_tcp() {
        let addr: Address = serde_yaml::from_str("\"127.0.0.1:9999\"").unwrap();
        assert_eq!(addr, Address::RemoteSocket("127.0.0.1".to_string(), 9999));
    }

    #[test]
    fn test_lo_request_num_default() {
        let cfg = TracerConfig::from_str(MINIMAL_YAML).unwrap();
        assert_eq!(cfg.lo_request_num(), 100);
    }
}
