//! Worker 指标文件与就绪度聚合。
//!
//! 指标是跨进程观察边界：CLI 只读写本地 JSON，绝不把指标当作事实来源。

use super::*;

pub(crate) fn worker_metrics_dir(config: &RuntimeConfig) -> PathBuf {
    Path::new(&config.storage.data_dir).join("worker-metrics")
}

#[cfg(feature = "nats")]
pub(crate) fn worker_metrics_path(config: &RuntimeConfig, worker_id: &str) -> PathBuf {
    worker_metrics_dir(config).join(format!("{worker_id}.prom"))
}

#[cfg(feature = "nats")]
pub(crate) fn write_worker_metrics(path: &Path, body: &str) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("worker metrics 路径没有父目录: {}", path.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("创建 worker metrics 目录失败: {error}"))?;
    let temporary = path.with_extension(format!("prom.tmp.{}", std::process::id()));
    std::fs::write(&temporary, body)
        .map_err(|error| format!("写入 worker metrics 临时文件失败: {error}"))?;
    std::fs::rename(&temporary, path).map_err(|error| {
        let _ = std::fs::remove_file(&temporary);
        format!("提交 worker metrics 文件失败: {error}")
    })
}

pub(crate) fn read_worker_metrics(directory: &Path, now_ms: u64, stale_after_ms: u64) -> String {
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

fn worker_metrics_stale(content: &str, now_ms: u64, stale_after_ms: u64) -> bool {
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

fn force_worker_metrics_down(content: &str) -> String {
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

pub(crate) fn worker_metrics_unhealthy(content: &str, now_ms: u64, stale_after_ms: u64) -> bool {
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

#[cfg(feature = "nats")]
pub(crate) fn prometheus_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

#[cfg(feature = "nats")]
#[derive(Clone)]
pub(crate) struct WorkerMetricsSink {
    pub(crate) path: PathBuf,
    pub(crate) worker_id: String,
}

#[cfg(feature = "nats")]
impl WorkerMetricsSink {
    pub(crate) fn write(&self, body: &str) {
        if let Err(error) = write_worker_metrics(&self.path, body) {
            eprintln!(
                "worker={} metrics 写入失败，业务处理继续: {}",
                self.worker_id, error
            );
        }
    }
}

#[cfg(feature = "nats")]
#[derive(Default)]
pub(crate) struct RelayMetricTotals {
    scanned: u64,
    published: u64,
    retried: u64,
    lease_conflicts: u64,
    publish_failures: u64,
}

#[cfg(feature = "nats")]
impl RelayMetricTotals {
    pub(crate) fn apply(&mut self, report: &qx_storage::OutboxRelayReport) {
        self.scanned += report.scanned;
        self.published += report.published;
        self.retried += report.retried;
        self.lease_conflicts += report.lease_conflicts;
        self.publish_failures += report.publish_failures;
    }

    pub(crate) fn render(&self, sink: &WorkerMetricsSink, up: bool, now_ms: u64) -> String {
        let worker = prometheus_label(&sink.worker_id);
        format!(
            "qx_worker_up{{worker=\"{worker}\"}} {}\n\
qx_worker_heartbeat_timestamp_seconds{{worker=\"{worker}\"}} {}\n\
qx_outbox_relay_scanned_total{{worker=\"{worker}\"}} {}\n\
qx_outbox_relay_published_total{{worker=\"{worker}\"}} {}\n\
qx_outbox_relay_retried_total{{worker=\"{worker}\"}} {}\n\
qx_outbox_relay_lease_conflicts_total{{worker=\"{worker}\"}} {}\n\
qx_outbox_relay_publish_failures_total{{worker=\"{worker}\"}} {}\n",
            u8::from(up),
            now_ms / 1_000,
            self.scanned,
            self.published,
            self.retried,
            self.lease_conflicts,
            self.publish_failures,
        )
    }
}

#[cfg(feature = "nats")]
#[derive(Default)]
pub(crate) struct ConsumerMetricTotals {
    received: u64,
    applied: u64,
    duplicates: u64,
    retried: u64,
    dead_lettered: u64,
    malformed: u64,
    ack_failures: u64,
}

#[cfg(feature = "nats")]
impl ConsumerMetricTotals {
    pub(crate) fn apply(&mut self, report: &qx_storage::NatsConsumerBatchReport) {
        self.received += report.received;
        self.applied += report.applied;
        self.duplicates += report.duplicates;
        self.retried += report.retried;
        self.dead_lettered += report.dead_lettered;
        self.malformed += report.malformed;
        self.ack_failures += report.ack_failures;
    }

    pub(crate) fn render(&self, sink: &WorkerMetricsSink, up: bool, now_ms: u64) -> String {
        let worker = prometheus_label(&sink.worker_id);
        format!(
            "qx_worker_up{{worker=\"{worker}\"}} {}\n\
qx_worker_heartbeat_timestamp_seconds{{worker=\"{worker}\"}} {}\n\
qx_event_consumer_received_total{{worker=\"{worker}\"}} {}\n\
qx_event_consumer_applied_total{{worker=\"{worker}\"}} {}\n\
qx_event_consumer_duplicates_total{{worker=\"{worker}\"}} {}\n\
qx_event_consumer_retried_total{{worker=\"{worker}\"}} {}\n\
qx_event_consumer_dead_lettered_total{{worker=\"{worker}\"}} {}\n\
qx_event_consumer_malformed_total{{worker=\"{worker}\"}} {}\n\
qx_event_consumer_ack_failures_total{{worker=\"{worker}\"}} {}\n",
            u8::from(up),
            now_ms / 1_000,
            self.received,
            self.applied,
            self.duplicates,
            self.retried,
            self.dead_lettered,
            self.malformed,
            self.ack_failures,
        )
    }
}
