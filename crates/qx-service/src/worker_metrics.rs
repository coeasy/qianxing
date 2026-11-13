//! Shared worker metrics file storage, aggregation, and stale-state projection.

use qx_runtime::RuntimeConfig;
use std::path::{Path, PathBuf};

pub fn prometheus_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

pub fn worker_metrics_dir(config: &RuntimeConfig) -> PathBuf {
    Path::new(&config.storage.data_dir).join("worker-metrics")
}

pub fn worker_metrics_path(config: &RuntimeConfig, worker_id: &str) -> PathBuf {
    worker_metrics_dir(config).join(format!("{worker_id}.prom"))
}

pub fn write_worker_metrics(path: &Path, body: &str) -> Result<(), String> {
    qx_core::atomic_file::write_atomic_file(path, body.as_bytes())
        .map_err(|error| format!("提交 worker metrics 文件失败 {}: {error}", path.display()))
}

pub fn read_worker_metrics(directory: &Path, now_ms: u64, stale_after_ms: u64) -> String {
    let mut paths = match std::fs::read_dir(directory) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("prom"))
            .collect::<Vec<_>>(),
        Err(_) => return String::new(),
    };
    paths.sort();
    let mut output = String::new();
    for path in paths {
        if let Ok(content) = std::fs::read_to_string(&path) {
            let content = if worker_metrics_stale(&content, now_ms, stale_after_ms) {
                force_worker_metrics_down(&content)
            } else {
                content
            };
            output.push_str(&content);
            if !output.ends_with('\n') {
                output.push('\n');
            }
        }
    }
    output
}

pub fn worker_metrics_stale(content: &str, now_ms: u64, stale_after_ms: u64) -> bool {
    let heartbeat_ms = content.lines().find_map(|line| {
        if !line.starts_with("qx_worker_heartbeat_timestamp_seconds") {
            return None;
        }
        line.split_whitespace()
            .last()
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| value.is_finite() && *value >= 0.0)
            .map(|value| (value * 1_000.0) as u64)
    });
    heartbeat_ms.is_none_or(|heartbeat| now_ms.saturating_sub(heartbeat) > stale_after_ms)
}

pub fn force_worker_metrics_down(content: &str) -> String {
    let mut output = String::new();
    let mut worker_label = None;
    for line in content.lines() {
        if line.starts_with("qx_worker_up{") {
            worker_label = line
                .split("worker=\"")
                .nth(1)
                .and_then(|value| value.split('\"').next())
                .map(str::to_string);
            let mut fields = line.split_whitespace().collect::<Vec<_>>();
            if let Some(value) = fields.last_mut() {
                *value = "0";
            }
            output.push_str(&fields.join(" "));
        } else {
            output.push_str(line);
        }
        output.push('\n');
    }
    if let Some(worker) = worker_label {
        output.push_str(&format!(
            "qx_worker_metrics_stale{{worker=\"{worker}\"}} 1\n"
        ));
    }
    output
}

pub fn worker_metrics_unhealthy(content: &str, now_ms: u64, stale_after_ms: u64) -> bool {
    if worker_metrics_stale(content, now_ms, stale_after_ms) {
        return true;
    }
    content.lines().any(|line| {
        line.starts_with("qx_worker_up{")
            && line
                .split_whitespace()
                .last()
                .is_none_or(|value| value != "1")
    })
}

#[cfg(test)]
mod label_tests {
    use super::{prometheus_label, write_worker_metrics};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn scratch_dir() -> std::path::PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        std::env::temp_dir().join(format!(
            "qx-worker-metrics-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn prometheus_labels_escape_control_characters() {
        assert_eq!(prometheus_label("a\\b\"c\nd\re"), "a\\\\b\\\"c\\nd\\re");
    }

    #[test]
    fn worker_metrics_updates_existing_file_atomically() {
        let root = scratch_dir();
        let path = root.join("worker.prom");
        write_worker_metrics(&path, "qx_worker_up 0\n").unwrap();
        write_worker_metrics(&path, "qx_worker_up 1\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "qx_worker_up 1\n");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn worker_metrics_failure_preserves_existing_destination() {
        let root = scratch_dir();
        let path = root.join("worker.prom");
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("sentinel"), b"keep").unwrap();

        assert!(write_worker_metrics(&path, "qx_worker_up 1\n").is_err());
        assert_eq!(std::fs::read(path.join("sentinel")).unwrap(), b"keep");
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }
}
