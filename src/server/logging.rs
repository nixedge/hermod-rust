//! File-based log writing for `hermod-tracer`
//!
//! Each node gets its own subdirectory under the configured `logRoot`.
//! Within that directory, log output is written to a timestamped file:
//!
//! ```text
//! {logRoot}/
//! └── {node-id}/
//!     ├── node-2024-01-15T10-30-00.json   ← previous file
//!     ├── node-2024-01-15T12-00-00.json   ← current file
//!     └── node.json                        ← symlink → current file
//! ```
//!
//! The symlink is updated atomically via a `.tmp` rename so readers always
//! see a consistent target.
//!
//! Two formats are supported:
//!
//! - [`LogFormat::ForHuman`] — `"{timestamp} [{severity}] {namespace} {message}\n"`
//! - [`LogFormat::ForMachine`] — one JSON object per line (newline-delimited JSON)

use crate::protocol::TraceObject;
use crate::server::config::{LogFormat, LoggingParams};
use crate::server::node::NodeId;
use chrono::Utc;
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Mutex;

/// Extension for a given log format
fn ext(fmt: LogFormat) -> &'static str {
    match fmt {
        LogFormat::ForHuman => "log",
        LogFormat::ForMachine => "json",
    }
}

/// A key into the handle cache
#[derive(Hash, PartialEq, Eq, Clone, Debug)]
pub struct LogKey {
    /// Node identifier
    pub node_id: NodeId,
    /// Log root directory
    pub log_root: PathBuf,
    /// Log format (Human or Machine)
    pub log_format: LogFormat,
}

/// One open log file
pub struct LogHandle {
    /// The open file
    pub file: File,
    /// Absolute path to this file
    pub path: PathBuf,
    /// Bytes written so far
    pub bytes_written: u64,
}

/// Shared writer — holds open file handles for all active (node, logging-config) pairs
pub struct LogWriter {
    handles: Mutex<HashMap<LogKey, LogHandle>>,
}

impl LogWriter {
    /// Create a new log writer
    pub fn new() -> Self {
        LogWriter {
            handles: Mutex::new(HashMap::new()),
        }
    }

    /// Write a batch of traces for one (node, logging-params) pair
    pub fn write_traces(
        &self,
        node_id: &NodeId,
        params: &LoggingParams,
        traces: &[TraceObject],
    ) -> io::Result<()> {
        let key = LogKey {
            node_id: node_id.clone(),
            log_root: params.log_root.clone(),
            log_format: params.log_format,
        };

        let mut handles = self.handles.lock().unwrap();
        if !handles.contains_key(&key) {
            let handle = Self::open_new_file(&params.log_root, node_id, params.log_format)?;
            handles.insert(key.clone(), handle);
        }
        let handle = handles.get_mut(&key).unwrap();

        for trace in traces {
            let line = format_trace(trace, params.log_format);
            let bytes = line.as_bytes();
            handle.file.write_all(bytes)?;
            handle.bytes_written += bytes.len() as u64;
        }
        handle.file.flush()?;
        Ok(())
    }

    /// Rotate the log file for a key if it exceeds `limit_bytes`
    pub fn rotate_if_needed(
        &self,
        node_id: &NodeId,
        params: &LoggingParams,
        limit_bytes: u64,
    ) -> io::Result<()> {
        let key = LogKey {
            node_id: node_id.clone(),
            log_root: params.log_root.clone(),
            log_format: params.log_format,
        };

        let mut handles = self.handles.lock().unwrap();
        if let Some(handle) = handles.get(&key) {
            if handle.bytes_written >= limit_bytes {
                let new_handle =
                    Self::open_new_file(&params.log_root, node_id, params.log_format)?;
                handles.insert(key, new_handle);
            }
        }
        Ok(())
    }

    /// Open a new timestamped log file and update the `node.{ext}` symlink
    pub fn open_new_file(
        log_root: &PathBuf,
        node_id: &NodeId,
        format: LogFormat,
    ) -> io::Result<LogHandle> {
        // Sanitise node_id for use as a directory name
        let node_dir_name: String = node_id
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
            .collect();

        let node_dir = log_root.join(&node_dir_name);
        fs::create_dir_all(&node_dir)?;

        let ts = Utc::now().format("%Y-%m-%dT%H-%M-%S");
        let filename = format!("node-{}.{}", ts, ext(format));
        let path = node_dir.join(&filename);

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;

        // Atomically update symlink: write to .tmp then rename
        let link = node_dir.join(format!("node.{}", ext(format)));
        let tmp_link = node_dir.join(format!("node.{}.tmp", ext(format)));

        // Remove stale .tmp if it exists
        let _ = fs::remove_file(&tmp_link);

        // Create new symlink pointing at the timestamped file (relative name)
        #[cfg(unix)]
        std::os::unix::fs::symlink(&filename, &tmp_link)?;

        #[cfg(unix)]
        fs::rename(&tmp_link, &link)?;

        Ok(LogHandle {
            file,
            path,
            bytes_written: 0,
        })
    }
}

impl Default for LogWriter {
    fn default() -> Self {
        Self::new()
    }
}

/// Format a single trace as a line (with trailing newline)
pub fn format_trace(trace: &TraceObject, format: LogFormat) -> String {
    match format {
        LogFormat::ForHuman => format_human(trace),
        LogFormat::ForMachine => format_machine(trace),
    }
}

/// Human-readable format:
/// `{timestamp} [{severity}] {namespace} {message}\n`
pub fn format_human(trace: &TraceObject) -> String {
    let ts = trace.to_timestamp.format("%Y-%m-%dT%H:%M:%S%.3fZ");
    let ns = trace.to_namespace.join(".");
    let msg = trace
        .to_human
        .as_deref()
        .unwrap_or(&trace.to_machine);
    format!("{} [{}] {} {}\n", ts, trace.to_severity, ns, msg)
}

/// Machine-readable format: JSON line
pub fn format_machine(trace: &TraceObject) -> String {
    let mut line = serde_json::to_string(trace).unwrap_or_else(|_| "{}".to_string());
    line.push('\n');
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::types::{DetailLevel, Severity};
    use chrono::TimeZone;

    fn make_trace() -> TraceObject {
        TraceObject {
            to_human: Some("hello world".to_string()),
            to_machine: r#"{"msg":"hello world"}"#.to_string(),
            to_namespace: vec!["TestNS".to_string(), "Sub".to_string()],
            to_severity: Severity::Info,
            to_details: DetailLevel::DNormal,
            to_timestamp: Utc.with_ymd_and_hms(2024, 1, 15, 10, 30, 0).unwrap(),
            to_hostname: "testhost".to_string(),
            to_thread_id: "1".to_string(),
        }
    }

    #[test]
    fn test_format_human() {
        let trace = make_trace();
        let line = format_human(&trace);
        assert!(line.contains("[Info]"));
        assert!(line.contains("TestNS.Sub"));
        assert!(line.contains("hello world"));
        assert!(line.ends_with('\n'));
    }

    #[test]
    fn test_format_machine() {
        let trace = make_trace();
        let line = format_machine(&trace);
        assert!(line.starts_with('{'));
        assert!(line.ends_with('\n'));
        // Should be valid JSON
        let _: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    }

    #[test]
    fn test_write_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let log_root = dir.path().to_path_buf();
        let params = LoggingParams {
            log_root: log_root.clone(),
            log_mode: crate::server::config::LogMode::FileMode,
            log_format: LogFormat::ForMachine,
        };

        let writer = LogWriter::new();
        let traces = vec![make_trace()];
        writer.write_traces(&"test-node".to_string(), &params, &traces).unwrap();

        // Find the written file
        let node_dir = log_root.join("test-node");
        let entries: Vec<_> = fs::read_dir(&node_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".json"))
            .filter(|e| !e.file_name().to_string_lossy().starts_with("node.json"))
            .collect();
        assert_eq!(entries.len(), 1);
        let content = fs::read_to_string(entries[0].path()).unwrap();
        assert!(!content.is_empty());
    }
}
