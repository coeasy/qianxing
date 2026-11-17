//! Runtime worker and event relay command adapters.

use super::*;

#[cfg(feature = "nats")]
pub(super) fn dispatch_outbox_relay(
    root: PathBuf,
    url: String,
    subject_prefix: String,
    limit: Option<usize>,
) {
    if let Err(error) = run_file_outbox_relay(&root, &url, &subject_prefix, limit.unwrap_or(100)) {
        eprintln!("[qx-cli · CLI] Outbox relay 失败: {error}");
        std::process::exit(2);
    }
}

#[cfg(not(feature = "nats"))]
pub(super) fn dispatch_outbox_relay(
    _root: PathBuf,
    _url: String,
    _subject_prefix: String,
    _limit: Option<usize>,
) {
    eprintln!("[qx-cli · CLI] outbox-relay 需要使用 --features nats 构建 qx-cli");
    std::process::exit(2);
}

#[cfg(all(feature = "nats", feature = "postgres"))]
pub(super) fn dispatch_outbox_relay_postgres(
    runtime: PathBuf,
    url: String,
    subject_prefix: String,
    limit: Option<usize>,
) {
    if let Err(error) =
        run_postgres_outbox_relay(&runtime, &url, &subject_prefix, limit.unwrap_or(100))
    {
        eprintln!("[qx-cli · CLI] PostgreSQL Outbox relay 失败: {error}");
        std::process::exit(2);
    }
}

#[cfg(not(all(feature = "nats", feature = "postgres")))]
pub(super) fn dispatch_outbox_relay_postgres(
    _runtime: PathBuf,
    _url: String,
    _subject_prefix: String,
    _limit: Option<usize>,
) {
    eprintln!(
        "[qx-cli · CLI] outbox-relay-postgres 需要使用 --features 'nats postgres' 构建 qx-cli"
    );
    std::process::exit(2);
}

#[cfg(feature = "nats")]
pub(super) fn dispatch_outbox_relay_worker(path: PathBuf, worker_id: String, once: bool) {
    if let Err(error) = run_outbox_relay_worker(&path, &worker_id, once) {
        eprintln!("[qx-cli · CLI] Outbox relay worker 失败: {error}");
        std::process::exit(2);
    }
}

#[cfg(not(feature = "nats"))]
pub(super) fn dispatch_outbox_relay_worker(_path: PathBuf, _worker_id: String, _once: bool) {
    eprintln!("[qx-cli · CLI] outbox-relay-worker 需要使用 --features nats 构建 qx-cli");
    std::process::exit(2);
}

#[cfg(feature = "nats")]
pub(super) fn dispatch_event_consumer_worker(path: PathBuf, worker_id: String, once: bool) {
    if let Err(error) = run_event_consumer_worker(&path, &worker_id, once) {
        eprintln!("[qx-cli · CLI] Event consumer worker 失败: {error}");
        std::process::exit(2);
    }
}

#[cfg(not(feature = "nats"))]
pub(super) fn dispatch_event_consumer_worker(_path: PathBuf, _worker_id: String, _once: bool) {
    eprintln!("[qx-cli · CLI] event-consumer-worker 需要使用 --features nats 构建 qx-cli");
    std::process::exit(2);
}

#[cfg(feature = "nats")]
pub(super) fn dispatch_consumer_dlq_replay(runtime: PathBuf, group_id: String, event_id: String) {
    if let Err(error) = run_dead_letter_replay(&runtime, &group_id, &event_id) {
        eprintln!("[qx-cli · CLI] DLQ 重放失败: {error}");
        std::process::exit(2);
    }
}

#[cfg(not(feature = "nats"))]
pub(super) fn dispatch_consumer_dlq_replay(
    _runtime: PathBuf,
    _group_id: String,
    _event_id: String,
) {
    eprintln!("[qx-cli · CLI] consumer-dlq-replay 需要使用 --features nats 构建 qx-cli");
    std::process::exit(2);
}

pub(super) fn dispatch_scheduler_worker(path: PathBuf, worker_id: String, once: bool) {
    if let Err(error) = run_scheduler_worker(&path, &worker_id, once, false) {
        eprintln!("[qx-cli · CLI] scheduler-worker 启动/运行失败: {error}");
        std::process::exit(2);
    }
}

pub(super) fn dispatch_strategy_worker(path: PathBuf, worker_id: String, once: bool) {
    if let Err(error) = run_strategy_worker(&path, &worker_id, once, false) {
        eprintln!("[qx-cli · CLI] strategy-worker 启动/运行失败: {error}");
        std::process::exit(2);
    }
}

pub(super) fn dispatch_paper_worker(path: PathBuf, worker_id: String, once: bool) {
    if let Err(error) = run_paper_execution_worker(&path, &worker_id, once) {
        eprintln!("[qx-cli · CLI] Paper Execution worker 启动/运行失败: {error}");
        std::process::exit(2);
    }
}

pub(super) fn dispatch_binance_worker(path: PathBuf, worker_id: String, once: bool) {
    if let Err(error) = run_binance_worker(&path, &worker_id, once) {
        eprintln!("[qx-cli · CLI] Binance worker 启动/运行失败: {error}");
        std::process::exit(2);
    }
}

pub(super) fn dispatch_ccxt_worker(path: PathBuf, worker_id: String, config: PathBuf, once: bool) {
    if let Err(error) = run_ccxt_worker(&path, &worker_id, &config, once) {
        eprintln!("[qx-cli · CLI] CCXT worker 启动/运行失败: {error}");
        std::process::exit(2);
    }
}
