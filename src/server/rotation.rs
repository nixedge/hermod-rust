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
                    .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
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
            if path.symlink_metadata().map(|m| m.file_type().is_symlink()).unwrap_or(false) {
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
    files.sort_by(|a, b| b.1.cmp(&a.1));

    let now = Utc::now();
    let max_age = chrono::Duration::hours(max_age_hours as i64);

    for (idx, (path, mtime)) in files.iter().enumerate() {
        let file_age = {
            let mtime_secs = mtime
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let file_dt =
                chrono::DateTime::from_timestamp(mtime_secs as i64, 0).unwrap_or(now);
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
