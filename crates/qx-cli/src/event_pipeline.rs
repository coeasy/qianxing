//! Outbox 中继、事件消费者与死信重放的运行时循环。
//!
//! 传输后端（文件 / SQLite / PostgreSQL / NATS）由 crate 根的选择器决定，
//! 这里的循环对后端保持一致的租约、幂等与 ACK 语义。

#[cfg(feature = "nats")]
use crate::read_runtime_config;
#[cfg(feature = "nats")]
use std::path::Path;
#[cfg(feature = "nats")]
#[cfg(feature = "nats")]
pub(crate) fn run_file_outbox_relay(
    data_root: &Path,
    nats_url: &str,
    subject_prefix: &str,
    limit: usize,
) -> Result<(), String> {
    let report = qx_service::messaging_host::pump_file_outbox_once(
        data_root,
        nats_url,
        subject_prefix,
        limit,
    )?;
    // `parked` 是"库里还有几条发不出去"的状态量：单次批处理退 0 只说明这一页搬完了，
    // 不念出停在门后的条数，运维面就会把"还剩 N 条毒事件"读成"中继干净"（V13 R5）。
    println!(
        "[Outbox relay] scanned={} published={} retried={} publish_failures={} lease_conflicts={} parked={} last_error={:?}",
        report.scanned,
        report.published,
        report.retried,
        report.publish_failures,
        report.lease_conflicts,
        report.parked,
        report.last_error
    );
    Ok(())
}

#[cfg(all(feature = "nats", feature = "postgres"))]
pub(crate) fn run_postgres_outbox_relay(
    runtime_config_path: &Path,
    nats_url: &str,
    subject_prefix: &str,
    limit: usize,
) -> Result<(), String> {
    let config = read_runtime_config(runtime_config_path)?;
    let report = qx_service::messaging_host::pump_postgres_outbox_once(
        &config,
        nats_url,
        subject_prefix,
        limit,
    )?;
    println!(
        "[PostgreSQL Outbox relay] scanned={} published={} retried={} publish_failures={} lease_conflicts={} parked={} last_error={:?}",
        report.scanned,
        report.published,
        report.retried,
        report.publish_failures,
        report.lease_conflicts,
        report.parked,
        report.last_error
    );
    Ok(())
}

#[cfg(feature = "nats")]
pub(crate) fn run_outbox_relay_worker(
    path: &Path,
    worker_id: &str,
    once: bool,
) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    qx_service::messaging_host::run_configured_outbox_relay_worker(config, worker_id, once)
}

#[cfg(feature = "nats")]
pub(crate) fn run_event_consumer_worker(
    path: &Path,
    worker_id: &str,
    once: bool,
) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    qx_service::messaging_host::run_configured_event_consumer_worker(config, worker_id, once)
}

#[cfg(feature = "nats")]
pub(crate) fn run_dead_letter_replay(
    path: &Path,
    group_id: &str,
    event_id: &str,
) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    qx_service::messaging_host::run_configured_dead_letter_replay(config, group_id, event_id)
}
