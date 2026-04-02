//! Log rotation background task
//!
//! [`run_rotation_loop`] wakes every `rpFrequencySecs` seconds and, for each
//! connected node × logging config pair, enforces three limits:
//!
//! - **Size** (`rpLogLimitBytes`) — if the current log file exceeds this byte
//!   count, [`LogWriter::rotate_if_needed`] opens a fresh timestamped file and
//!   updates the `node.{ext}` symlink.
//!
//! - **Age** (`rpMaxAgeHours`) — timestamped files older than this threshold
//!   are deleted.
//!
//! - **Count** (`rpKeepFilesNum`) — after age-based pruning, only the `N`
//!   newest files are retained regardless of age.
//!
//! The current symlink target (`node.{ext}`) is never deleted.

use crate::server::config::{LogFormat, LoggingParams, RotationParams};
use crate::server::logging::LogWriter;
use crate::server::node::TracerState;
use chrono::Utc;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, warn};

/// Run the rotation loop; call this as a background task
pub async fn run_rotation_loop(
    writer: Arc<LogWriter>,
    state: Arc<TracerState>,
    params: RotationParams,
    logging: Vec<LoggingParams>,
) {
    let interval = Duration::from_secs(params.rp_frequency_secs as u64);
    loop {
        tokio::time::sleep(interval).await;

        let nodes = state.node_list().await;
        for (node_id, _slug) in &nodes {
            for lp in &logging {
                // Rotate active file if over the size limit
                if let Err(e) = writer.rotate_if_needed(node_id, lp, params.rp_log_limit_bytes) {
                    warn!("Rotation error for node {}: {}", node_id, e);
                }

                // Enforce age and count limits
                let node_dir_name: String = node_id
                    .chars()
                    .map(|c| {
                        if c.is_alphanumeric() || c == '-' || c == '_' {
                            c
                        } else {
                            '_'
                        }
                    })
                    .collect();
                let node_dir = lp.log_root.join(&node_dir_name);
                if let Err(e) = prune_old_files(
                    &node_dir,
                    ext(lp.log_format),
                    params.rp_max_age_hours,
                    params.rp_keep_files_num,
                ) {
                    warn!("Prune error for node {}: {}", node_id, e);
                }
            }
        }
        debug!("Rotation pass complete ({} nodes)", nodes.len());
    }
}

fn ext(fmt: LogFormat) -> &'static str {
    match fmt {
        LogFormat::ForHuman => "log",
        LogFormat::ForMachine => "json",
    }
}

/// Delete timestamped log files that are either too old or exceed the keep count.
///
/// The symlink `node.{ext}` is always preserved.
fn prune_old_files(
    node_dir: &PathBuf,
    extension: &str,
    max_age_hours: u64,
    keep_files_num: u32,
) -> std::io::Result<()> {
    if !node_dir.exists() {
        return Ok(());
    }

    // Collect all timestamped files (node-*.{ext}, not the symlink node.{ext})
    let symlink_name = format!("node.{}", extension);
    let prefix = "node-";

    let mut files: Vec<(PathBuf, std::time::SystemTime)> = fs::read_dir(node_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name();
            let name_str = name.to_string_lossy();
            name_str.starts_with(prefix)
                && name_str.ends_with(extension)
                && name_str != symlink_name
        })
        .filter_map(|e| {
            let path = e.path();
            // Skip symlinks
            if path
                .symlink_metadata()
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false)
            {
                return None;
            }
            let mtime = e
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .unwrap_or(std::time::UNIX_EPOCH);
            Some((path, mtime))
        })
        .collect();

    // Sort newest-first
    files.sort_by_key(|f| std::cmp::Reverse(f.1));

    let now = Utc::now();
    let max_age = chrono::Duration::hours(max_age_hours as i64);

    for (idx, (path, mtime)) in files.iter().enumerate() {
        let file_age = {
            let mtime_secs = mtime
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let file_dt = chrono::DateTime::from_timestamp(mtime_secs as i64, 0).unwrap_or(now);
            now.signed_duration_since(file_dt)
        };

        let too_old = max_age_hours > 0 && file_age > max_age;
        let exceeds_count = (idx as u32) >= keep_files_num;

        if too_old || exceeds_count {
            debug!("Pruning log file: {}", path.display());
            if let Err(e) = fs::remove_file(path) {
                warn!("Failed to remove {}: {}", path.display(), e);
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_file(dir: &std::path::Path, name: &str) {
        fs::write(dir.join(name), name).unwrap();
    }

    #[test]
    fn prune_nonexistent_dir_is_ok() {
        let tmp = TempDir::new().unwrap();
        let nonexistent = tmp.path().join("does-not-exist");
        prune_old_files(&nonexistent, "log", 24, 10).unwrap();
    }

    #[test]
    fn prune_within_count_limit_keeps_all_files() {
        let tmp = TempDir::new().unwrap();
        write_file(tmp.path(), "node-2024-01-01T00-00-00.log");
        write_file(tmp.path(), "node-2024-01-02T00-00-00.log");
        write_file(tmp.path(), "node-2024-01-03T00-00-00.log");
        // max_age_hours=0 means no age pruning; keep_files_num=10 keeps all 3
        prune_old_files(&tmp.path().to_path_buf(), "log", 0, 10).unwrap();
        assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 3);
    }

    #[test]
    fn prune_removes_excess_files_by_count() {
        let tmp = TempDir::new().unwrap();
        write_file(tmp.path(), "node-2024-01-01T00-00-00.log");
        write_file(tmp.path(), "node-2024-01-02T00-00-00.log");
        write_file(tmp.path(), "node-2024-01-03T00-00-00.log");
        // keep_files_num=1 → prune 2 oldest (which have oldest mtime)
        // (all files have the same mtime here since we write them immediately;
        //  the sort is stable so at least 1 must survive)
        prune_old_files(&tmp.path().to_path_buf(), "log", 0, 1).unwrap();
        assert!(fs::read_dir(tmp.path()).unwrap().count() <= 1);
    }

    #[test]
    fn prune_ignores_symlink_style_current_file() {
        // node.log doesn't start with "node-" so it is never picked up as a candidate
        let tmp = TempDir::new().unwrap();
        write_file(tmp.path(), "node.log");
        write_file(tmp.path(), "node-2024-01-01T00-00-00.log");
        write_file(tmp.path(), "node-2024-01-02T00-00-00.log");
        // keep 1 timestamped file; node.log is always preserved
        prune_old_files(&tmp.path().to_path_buf(), "log", 0, 1).unwrap();
        assert!(tmp.path().join("node.log").exists(), "node.log must survive");
        // node.log + 1 timestamped = 2 files total
        assert!(fs::read_dir(tmp.path()).unwrap().count() <= 2);
    }

    #[test]
    fn prune_ignores_different_extension() {
        // .json files should not be pruned when extension is "log"
        let tmp = TempDir::new().unwrap();
        write_file(tmp.path(), "node-2024-01-01T00-00-00.json");
        write_file(tmp.path(), "node-2024-01-02T00-00-00.json");
        prune_old_files(&tmp.path().to_path_buf(), "log", 0, 0).unwrap();
        // keep_files_num=0 prunes all log files, but these are .json
        assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 2);
    }
}
