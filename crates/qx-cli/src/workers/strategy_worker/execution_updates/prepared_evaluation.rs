//! Bounded durable PreparedEvaluation journal persistence and recovery.

use super::*;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
#[cfg(feature = "bench-prepared-evaluation")]
use std::time::Instant;

pub(in crate::workers::strategy_worker) const PREPARED_EVALUATION_SCHEMA_VERSION: u32 = 3;
const LEGACY_PREPARED_EVALUATION_SCHEMA_V1: u32 = 1;
pub(super) const LEGACY_PREPARED_EVALUATION_SCHEMA_V2: u32 = 2;
pub(super) const MAX_PREPARED_COMMAND_RESULT_BYTES: usize = 4 * 1024;
const MAX_PREPARED_EVALUATION_BYTES: usize = 8 * 1024 * 1024;
const MAX_PREPARED_EVALUATION_DIGEST_BYTES: usize = MAX_PREPARED_EVALUATION_BYTES + 1024;
const UNKNOWN_COMMAND_RESULT_PREFIX: &str = "OUTCOME_UNKNOWN: ";

#[cfg(any(feature = "bench-prepared-evaluation", test))]
pub(in crate::workers::strategy_worker) fn benchmark_crash_after(point: &str) {
    if std::env::var("QX_BENCH_PREPARED_CRASH_AFTER").as_deref() == Ok(point) {
        eprintln!("simulated process crash after PreparedEvaluation boundary: {point}");
        std::process::exit(86);
    }
}

#[cfg(not(any(feature = "bench-prepared-evaluation", test)))]
pub(in crate::workers::strategy_worker) fn benchmark_crash_after(_point: &str) {}

/// Durable decision batch written before any order command is submitted.
/// Recovery must dispatch these exact commands and checkpoint without re-running the strategy.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::workers::strategy_worker) struct PreparedEvaluationV3 {
    pub(in crate::workers::strategy_worker) schema_version: u32,
    pub(in crate::workers::strategy_worker) worker_id: String,
    pub(in crate::workers::strategy_worker) job_id: String,
    pub(in crate::workers::strategy_worker) run_id: u64,
    pub(in crate::workers::strategy_worker) input_sha256: String,
    pub(in crate::workers::strategy_worker) facts_sha256: String,
    pub(in crate::workers::strategy_worker) config_sha256: String,
    pub(in crate::workers::strategy_worker) lease_fencing_token: u64,
    pub(in crate::workers::strategy_worker) ranges: Vec<CursorRange>,
    pub(in crate::workers::strategy_worker) orders: Vec<Order>,
    pub(in crate::workers::strategy_worker) commands: Vec<ControlCommand>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::workers::strategy_worker) spread_group_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::workers::strategy_worker) checkpoint: Option<StrategyCheckpointV1>,
    pub(in crate::workers::strategy_worker) completed_command_ids: BTreeSet<u64>,
    /// Durable local result for each command attempt. A stored error is treated as an
    /// unknown write outcome because the control transaction may have committed before I/O failed.
    #[serde(default)]
    pub(in crate::workers::strategy_worker) command_results: BTreeMap<u64, String>,
    /// Checksum of every prepared input/output and recovery marker except this field.
    #[serde(default)]
    pub(in crate::workers::strategy_worker) payload_sha256: String,
}

#[derive(Serialize)]
struct PreparedEvaluationDigestPayload<'a> {
    schema_version: u32,
    worker_id: &'a str,
    job_id: &'a str,
    run_id: u64,
    input_sha256: &'a str,
    facts_sha256: &'a str,
    config_sha256: &'a str,
    lease_fencing_token: u64,
    ranges: &'a [CursorRange],
    orders: &'a [Order],
    commands: &'a [ControlCommand],
    spread_group_id: Option<&'a str>,
    checkpoint: Option<&'a StrategyCheckpointV1>,
    completed_command_ids: &'a BTreeSet<u64>,
    command_results: &'a BTreeMap<u64, String>,
}

struct PreparedEvaluationDigestWriter {
    hasher: Sha256,
    written: usize,
}

impl PreparedEvaluationDigestWriter {
    fn new() -> Self {
        Self {
            hasher: Sha256::new(),
            written: 0,
        }
    }

    fn finish(self) -> String {
        format!("{:x}", self.hasher.finalize())
    }
}

impl Write for PreparedEvaluationDigestWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let new_len = self
            .written
            .checked_add(bytes.len())
            .filter(|len| *len <= MAX_PREPARED_EVALUATION_DIGEST_BYTES)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "PreparedEvaluation 摘要输入超过容量上限",
                )
            })?;
        self.hasher.update(bytes);
        self.written = new_len;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct PreparedEvaluationBufferWriter {
    bytes: Vec<u8>,
}

impl PreparedEvaluationBufferWriter {
    fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

impl Write for PreparedEvaluationBufferWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let new_len = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .filter(|len| *len <= MAX_PREPARED_EVALUATION_BYTES)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "PreparedEvaluation 超过容量上限",
                )
            })?;
        self.bytes.extend_from_slice(bytes);
        debug_assert_eq!(self.bytes.len(), new_len);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(in crate::workers::strategy_worker) fn prepared_evaluation_payload_sha256(
    value: &PreparedEvaluationV3,
) -> Result<String, String> {
    let payload = PreparedEvaluationDigestPayload {
        schema_version: value.schema_version,
        worker_id: &value.worker_id,
        job_id: &value.job_id,
        run_id: value.run_id,
        input_sha256: &value.input_sha256,
        facts_sha256: &value.facts_sha256,
        config_sha256: &value.config_sha256,
        lease_fencing_token: value.lease_fencing_token,
        ranges: &value.ranges,
        orders: &value.orders,
        commands: &value.commands,
        spread_group_id: value.spread_group_id.as_deref(),
        checkpoint: value.checkpoint.as_ref(),
        completed_command_ids: &value.completed_command_ids,
        command_results: &value.command_results,
    };
    let mut writer = PreparedEvaluationDigestWriter::new();
    serde_json::to_writer(&mut writer, &payload)
        .map_err(|error| format!("编码 PreparedEvaluation 摘要输入失败: {error}"))?;
    Ok(writer.finish())
}

fn read_bounded_prepared_evaluation(path: &Path) -> std::io::Result<String> {
    let file = std::fs::File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "PreparedEvaluation is not a regular file",
        ));
    }
    if metadata.len() > MAX_PREPARED_EVALUATION_BYTES as u64 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "PreparedEvaluation exceeds {} bytes",
                MAX_PREPARED_EVALUATION_BYTES
            ),
        ));
    }
    let mut limited = file.take(MAX_PREPARED_EVALUATION_BYTES as u64 + 1);
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    limited.read_to_end(&mut bytes)?;
    if bytes.len() > MAX_PREPARED_EVALUATION_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "PreparedEvaluation exceeds {} bytes",
                MAX_PREPARED_EVALUATION_BYTES
            ),
        ));
    }
    String::from_utf8(bytes).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("PreparedEvaluation is not UTF-8: {error}"),
        )
    })
}

pub(in crate::workers::strategy_worker) fn prepared_evaluation_path(
    root: &Path,
    worker_id: &str,
) -> PathBuf {
    root.join("strategy-live-state")
        .join(format!("{worker_id}.prepared-evaluation.json"))
}

pub(in crate::workers::strategy_worker) fn load_prepared_evaluation(
    path: &Path,
) -> Result<Option<PreparedEvaluationV3>, String> {
    let backup = prepared_evaluation_backup_path(path);
    let payload = match read_bounded_prepared_evaluation(path) {
        Ok(payload) => payload,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match read_bounded_prepared_evaluation(&backup) {
                Ok(payload) => payload,
                Err(backup_error) if backup_error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(None);
                }
                Err(backup_error) => {
                    return Err(format!(
                        "读取 PreparedEvaluation 主文件和恢复副本失败 {} / {}: {backup_error}",
                        path.display(),
                        backup.display()
                    ));
                }
            }
        }
        Err(error) => {
            return Err(format!(
                "读取 PreparedEvaluation 失败 {}: {error}",
                path.display()
            ));
        }
    };
    let mut value: PreparedEvaluationV3 = serde_json::from_str(&payload)
        .map_err(|error| format!("PreparedEvaluation JSON 无效 {}: {error}", path.display()))?;
    let loaded_schema_version = value.schema_version;
    if matches!(
        loaded_schema_version,
        LEGACY_PREPARED_EVALUATION_SCHEMA_V1 | LEGACY_PREPARED_EVALUATION_SCHEMA_V2
    ) {
        value.schema_version = PREPARED_EVALUATION_SCHEMA_VERSION;
        value.payload_sha256.clear();
    }
    if value.schema_version != PREPARED_EVALUATION_SCHEMA_VERSION
        || value.worker_id.trim().is_empty()
        || value.job_id.trim().is_empty()
        || value.input_sha256.len() != 64
        || !value
            .input_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || value.facts_sha256.len() != 64
        || !value
            .facts_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || value.config_sha256.len() != 64
        || !value
            .config_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || value.commands.iter().any(|command| command.dry_run)
        || value.completed_command_ids.iter().any(|command_id| {
            !value
                .commands
                .iter()
                .any(|command| command.command_id == *command_id)
        })
        || value.command_results.iter().any(|(command_id, result)| {
            result.trim().is_empty()
                || result.len() > MAX_PREPARED_COMMAND_RESULT_BYTES
                || !value
                    .commands
                    .iter()
                    .any(|command| command.command_id == *command_id)
        })
    {
        return Err(format!("PreparedEvaluation 字段非法 {}", path.display()));
    }
    if loaded_schema_version == PREPARED_EVALUATION_SCHEMA_VERSION {
        let expected_sha256 = prepared_evaluation_payload_sha256(&value)?;
        if value.payload_sha256.len() != 64
            || !value
                .payload_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || value.payload_sha256 != expected_sha256
        {
            return Err(format!(
                "PreparedEvaluation 摘要校验失败 {}",
                path.display()
            ));
        }
    }
    Ok(Some(value))
}

pub(in crate::workers::strategy_worker) fn save_prepared_evaluation(
    path: &Path,
    value: &PreparedEvaluationV3,
) -> Result<(), String> {
    if value.schema_version != PREPARED_EVALUATION_SCHEMA_VERSION
        || value.worker_id.trim().is_empty()
        || value.job_id.trim().is_empty()
        || value.input_sha256.len() != 64
        || !value
            .input_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || value.facts_sha256.len() != 64
        || !value
            .facts_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || value.config_sha256.len() != 64
        || !value
            .config_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || value.commands.iter().any(|command| command.dry_run)
        || value.completed_command_ids.iter().any(|command_id| {
            !value
                .commands
                .iter()
                .any(|command| command.command_id == *command_id)
        })
        || value.command_results.iter().any(|(command_id, result)| {
            result.trim().is_empty()
                || result.len() > MAX_PREPARED_COMMAND_RESULT_BYTES
                || !value
                    .commands
                    .iter()
                    .any(|command| command.command_id == *command_id)
        })
    {
        return Err("拒绝保存字段非法的 PreparedEvaluation".into());
    }
    let parent = path
        .parent()
        .ok_or_else(|| "PreparedEvaluation 路径缺少父目录".to_string())?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("创建 PreparedEvaluation 目录失败: {error}"))?;
    let temporary = path.with_extension(format!("prepared.tmp.{}", std::process::id()));
    let backup = prepared_evaluation_backup_path(path);
    let mut persisted = value.clone();
    persisted.payload_sha256 = prepared_evaluation_payload_sha256(&persisted)?;
    let mut writer = PreparedEvaluationBufferWriter::new();
    serde_json::to_writer(&mut writer, &persisted).map_err(|error| {
        format!(
            "PreparedEvaluation 超过 {} 字节上限或编码失败，拒绝持久化和提交 intent: {error}",
            MAX_PREPARED_EVALUATION_BYTES
        )
    })?;
    let payload = writer.finish();
    let mut file = std::fs::File::create(&temporary)
        .map_err(|error| format!("写入 PreparedEvaluation 失败: {error}"))?;
    benchmark_crash_after("temp_created");
    file.write_all(&payload)
        .map_err(|error| format!("写入 PreparedEvaluation 失败: {error}"))?;
    benchmark_crash_after("payload_written");
    file.sync_all()
        .map_err(|error| format!("同步 PreparedEvaluation 失败: {error}"))?;
    benchmark_crash_after("temp_synced");
    drop(file);
    if backup.exists() {
        std::fs::remove_file(&backup)
            .map_err(|error| format!("清理旧 PreparedEvaluation 恢复副本失败: {error}"))?;
        benchmark_crash_after("stale_backup_removed");
    }
    if path.exists() {
        std::fs::rename(path, &backup)
            .map_err(|error| format!("备份旧 PreparedEvaluation 失败: {error}"))?;
        benchmark_crash_after("backup_renamed");
    }
    if let Err(error) = std::fs::rename(&temporary, path) {
        if backup.exists() {
            std::fs::rename(&backup, path).map_err(|restore| {
                format!("提交 PreparedEvaluation 失败: {error}; 恢复失败: {restore}")
            })?;
        }
        return Err(format!("提交 PreparedEvaluation 失败: {error}"));
    }
    benchmark_crash_after("new_published");
    if backup.exists() {
        std::fs::remove_file(&backup)
            .map_err(|error| format!("清理 PreparedEvaluation 恢复副本失败: {error}"))?;
        benchmark_crash_after("backup_cleaned");
    }
    #[cfg(unix)]
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("同步 PreparedEvaluation 目录失败: {error}"))?;
    #[cfg(unix)]
    benchmark_crash_after("directory_synced");
    Ok(())
}

#[cfg(feature = "bench-prepared-evaluation")]
pub(in crate::workers::strategy_worker) fn run_prepared_evaluation_bench(
    args: &[String],
) -> Result<(), String> {
    let operation = args.get(2).map(String::as_str).ok_or_else(|| {
        "usage: qx-cli bench-prepared-evaluation <measure|prepare|recover|queue|control-queue> ..."
            .to_owned()
    })?;
    match operation {
        "prepare" => {
            let path = bench_path(args, 3)?;
            let batch = bench_usize(args, 4, "batch-size", 1, 16_384)?;
            let run_id = bench_u64(args, 5, "run-id")?;
            let value = benchmark_value(batch, run_id)?;
            save_prepared_evaluation(&path, &value)?;
            println!(
                "{}",
                serde_json::json!({"run_id": run_id, "command_count": batch})
            );
        }
        "recover" => {
            let path = bench_path(args, 3)?;
            let started = Instant::now();
            let value = load_prepared_evaluation(&path)?
                .ok_or_else(|| "PreparedEvaluation journal is missing".to_owned())?;
            let recovered_from_backup = !path.exists();
            let recovery_source = if recovered_from_backup {
                prepared_evaluation_backup_path(&path)
            } else {
                path.clone()
            };
            println!(
                "{}",
                serde_json::json!({
                    "run_id": value.run_id,
                    "command_count": value.commands.len(),
                    "recovery_read_ms": started.elapsed().as_secs_f64() * 1_000.0,
                    "journal_bytes": std::fs::metadata(recovery_source).map(|meta| meta.len()).unwrap_or(0),
                    "recovered_from_backup": recovered_from_backup,
                })
            );
        }
        "measure" => {
            let path = bench_path(args, 3)?;
            let batch = bench_usize(args, 4, "batch-size", 1, 16_384)?;
            let iterations = bench_usize(args, 5, "iterations", 2, 10_000)?;
            let mut write_samples = Vec::with_capacity(iterations);
            let mut read_samples = Vec::with_capacity(iterations);
            let mut payload_bytes = 0_u64;
            for index in 0..iterations {
                let run_id = index as u64 + 1;
                let value = benchmark_value(batch, run_id)?;
                let started = Instant::now();
                save_prepared_evaluation(&path, &value)?;
                write_samples.push(started.elapsed().as_secs_f64() * 1_000.0);
                payload_bytes = std::fs::metadata(&path)
                    .map_err(|error| format!("stat PreparedEvaluation journal failed: {error}"))?
                    .len();
                let started = Instant::now();
                let restored = load_prepared_evaluation(&path)?
                    .ok_or_else(|| "PreparedEvaluation disappeared after save".to_owned())?;
                read_samples.push(started.elapsed().as_secs_f64() * 1_000.0);
                if restored != value {
                    return Err(
                        "PreparedEvaluation save/load changed the frozen decision batch".into(),
                    );
                }
            }
            println!(
                "{}",
                serde_json::json!({
                    "batch_size": batch,
                    "iterations": iterations,
                    "journal_bytes": payload_bytes,
                    "write_p50_ms": bench_percentile(&write_samples, 0.50),
                    "write_p95_ms": bench_percentile(&write_samples, 0.95),
                    "read_p50_ms": bench_percentile(&read_samples, 0.50),
                    "read_p95_ms": bench_percentile(&read_samples, 0.95),
                    "result_run_id": iterations as u64,
                })
            );
        }
        "queue" => {
            let backend = args.get(3).map(String::as_str).ok_or_else(|| {
                "queue benchmark requires a backend (file, sqlite, postgres)".to_owned()
            })?;
            let root = bench_path(args, 4)?;
            let capacity = bench_usize(
                args,
                5,
                "max-pending-jobs",
                1,
                qx_storage::DEFAULT_MAX_PENDING_JOBS,
            )?;
            let fill_count = bench_usize(args, 6, "fill-count", 1, capacity)?;
            let run_namespace = args
                .get(7)
                .ok_or_else(|| "queue benchmark requires a unique run namespace".to_owned())?;
            let queue = bench_create_job_queue(backend, &root, capacity)?;
            let mut enqueue_samples = Vec::with_capacity(fill_count);
            let mut run_ids = Vec::with_capacity(fill_count);
            for index in 0..fill_count {
                let job = qx_scheduler::JobSpec {
                    job_id: format!("capacity-bench-{run_namespace}-{index}"),
                    job_version: "v1".into(),
                    owner: format!("capacity-bench-worker-{run_namespace}"),
                    enabled: true,
                    trigger: qx_scheduler::Trigger::Manual,
                    window: qx_scheduler::JobWindow::Any,
                    depends_on: Vec::new(),
                    input_refs: Vec::new(),
                    output_refs: Vec::new(),
                    timeout_seconds: 60,
                    retry_policy: qx_scheduler::RetryPolicy::default(),
                    concurrency_key: "capacity-bench".into(),
                    idempotency_key: format!("capacity-bench-{run_namespace}-{index}"),
                    permission_scope: "research".into(),
                    audit_reason: "bounded-queue-capacity-benchmark".into(),
                    dry_run: true,
                };
                let trading_day = "2026-10-29".to_owned();
                let run = qx_scheduler::JobRun {
                    run_id: job.stable_key(&trading_day),
                    job_id: job.job_id.clone(),
                    trading_day,
                    attempt: 0,
                    status: qx_scheduler::JobStatus::Running,
                    manifest_digest: None,
                    error_code: None,
                    next_retry_ts: None,
                    started_ts: 1,
                    deadline_ts: 61,
                };
                let started = Instant::now();
                run_ids.push(run.run_id);
                queue
                    .enqueue_job(job, run, 1)
                    .map_err(|error| format!("enqueue job {index} failed: {error:?}"))?;
                enqueue_samples.push(started.elapsed().as_secs_f64() * 1_000.0);
            }
            let overflow = qx_scheduler::JobSpec {
                job_id: format!("capacity-bench-{run_namespace}-overflow"),
                job_version: "v1".into(),
                owner: format!("capacity-bench-worker-{run_namespace}"),
                enabled: true,
                trigger: qx_scheduler::Trigger::Manual,
                window: qx_scheduler::JobWindow::Any,
                depends_on: Vec::new(),
                input_refs: Vec::new(),
                output_refs: Vec::new(),
                timeout_seconds: 60,
                retry_policy: qx_scheduler::RetryPolicy::default(),
                concurrency_key: "capacity-bench".into(),
                idempotency_key: format!("capacity-bench-{run_namespace}-overflow"),
                permission_scope: "research".into(),
                audit_reason: "bounded-queue-capacity-benchmark".into(),
                dry_run: true,
            };
            let overflow_day = "2026-10-29".to_owned();
            let overflow_run = qx_scheduler::JobRun {
                run_id: overflow.stable_key(&overflow_day),
                job_id: overflow.job_id.clone(),
                trading_day: overflow_day,
                attempt: 0,
                status: qx_scheduler::JobStatus::Running,
                manifest_digest: None,
                error_code: None,
                next_retry_ts: None,
                started_ts: 1,
                deadline_ts: 61,
            };
            let overflow_rejected = matches!(
                queue.enqueue_job(overflow, overflow_run, 1),
                Err(qx_storage::StorageError::Conflict(_))
            );
            if !overflow_rejected {
                return Err("bounded JobQueue did not reject the first job over capacity".into());
            }
            let worker_id = format!("capacity-bench-worker-{run_namespace}");
            let pending_count = queue
                .available_jobs(1)
                .map_err(|error| format!("read bounded JobQueue failed: {error:?}"))?
                .len();
            let batch_count = queue
                .available_jobs_for_owner(1, &worker_id, super::super::MAX_JOBS_PER_POLL)
                .map_err(|error| format!("read bounded worker batch failed: {error:?}"))?
                .len();
            if pending_count != fill_count || batch_count > super::super::MAX_JOBS_PER_POLL {
                return Err(format!(
                    "bounded JobQueue invariant failed: pending={pending_count}, fill={fill_count}, batch={batch_count}"
                ));
            }
            for run_id in run_ids {
                let lease = queue
                    .claim_job(run_id, &worker_id, 2, 60)
                    .map_err(|error| format!("claim benchmark job {run_id} failed: {error:?}"))?;
                queue
                    .ack_job_at(run_id, &worker_id, lease.fencing_token, 3)
                    .map_err(|error| format!("ack benchmark job {run_id} failed: {error:?}"))?;
            }
            println!(
                "{}",
                serde_json::json!({
                    "max_pending_jobs": capacity,
                    "fill_count": fill_count,
                    "pending_count": pending_count,
                    "overflow_rejected": overflow_rejected,
                    "worker_batch_limit": super::super::MAX_JOBS_PER_POLL,
                    "worker_batch_count": batch_count,
                    "backend": backend,
                    "benchmark_jobs_acknowledged": fill_count,
                    "enqueue_p50_ms": bench_percentile(&enqueue_samples, 0.50),
                    "enqueue_p95_ms": bench_percentile(&enqueue_samples, 0.95),
                })
            );
        }
        "control-queue" => {
            let backend = args
                .get(3)
                .map(String::as_str)
                .ok_or_else(|| "control-queue benchmark requires a backend".to_owned())?;
            let root = bench_path(args, 4)?;
            let fill_count = bench_usize(
                args,
                5,
                "fill-count",
                1,
                qx_storage::DEFAULT_MAX_PENDING_CONTROL_COMMANDS,
            )?;
            if fill_count != qx_storage::DEFAULT_MAX_PENDING_CONTROL_COMMANDS {
                return Err(format!(
                    "control-queue benchmark must fill the exact configured capacity of {}",
                    qx_storage::DEFAULT_MAX_PENDING_CONTROL_COMMANDS
                ));
            }
            let namespace = args.get(6).ok_or_else(|| {
                "control-queue benchmark requires a unique run namespace".to_owned()
            })?;
            let queue = bench_create_control_queue(backend, &root)?;
            let initial_pending = queue
                .pending_commands()
                .map_err(|error| {
                    format!("read ControlCommandQueue before benchmark failed: {error:?}")
                })?
                .len();
            if initial_pending != 0 {
                return Err(format!(
                    "control-queue benchmark requires an empty isolated queue; found {initial_pending} pending commands"
                ));
            }
            let base_id = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| format!("system clock is before unix epoch: {error}"))?
                .as_nanos();
            let base_id = u64::try_from(base_id)
                .map_err(|error| format!("benchmark command id exceeds u64: {error}"))?;
            let worker_id = format!("control-capacity-bench-{namespace}");
            let oversized = qx_control::ControlCommand {
                command_id: base_id,
                request_id: format!("control-capacity-{namespace}-oversized"),
                operator_id: "capacity-benchmark".into(),
                reason: "bounded-control-queue-capacity-benchmark".into(),
                kind: qx_control::CommandKind::PauseStrategy,
                target: "x".repeat(qx_storage::MAX_CONTROL_COMMAND_BYTES + 1),
                payload: Default::default(),
                permission: qx_control::Permission::Trading,
                dry_run: true,
            };
            let oversize_rejected = matches!(
                queue.enqueue_command(oversized, 1),
                Err(qx_storage::StorageError::Conflict(_))
            );
            if !oversize_rejected {
                return Err("ControlCommandQueue accepted an oversized serialized command".into());
            }
            let mut enqueue_samples = Vec::with_capacity(fill_count);
            for index in 0..fill_count {
                let command_id = base_id.saturating_add(index as u64 + 1);
                let command = qx_control::ControlCommand {
                    command_id,
                    request_id: format!("control-capacity-{namespace}-{index}"),
                    operator_id: "capacity-benchmark".into(),
                    reason: "bounded-control-queue-capacity-benchmark".into(),
                    kind: qx_control::CommandKind::PauseStrategy,
                    target: format!("capacity-benchmark-worker-{index}"),
                    payload: Default::default(),
                    permission: qx_control::Permission::Trading,
                    dry_run: true,
                };
                let started = Instant::now();
                queue.enqueue_command(command, 1).map_err(|error| {
                    format!("enqueue control command {index} failed: {error:?}")
                })?;
                enqueue_samples.push(started.elapsed().as_secs_f64() * 1_000.0);
            }
            let overflow = qx_control::ControlCommand {
                command_id: base_id.saturating_add(fill_count as u64 + 1),
                request_id: format!("control-capacity-{namespace}-overflow"),
                operator_id: "capacity-benchmark".into(),
                reason: "bounded-control-queue-capacity-benchmark".into(),
                kind: qx_control::CommandKind::PauseStrategy,
                target: "capacity-benchmark-overflow".into(),
                payload: Default::default(),
                permission: qx_control::Permission::Trading,
                dry_run: true,
            };
            let overflow_rejected = matches!(
                queue.enqueue_command(overflow, 1),
                Err(qx_storage::StorageError::Conflict(_))
            );
            if !overflow_rejected {
                return Err(
                    "ControlCommandQueue did not reject the first command over capacity".into(),
                );
            }
            let pending_count = queue
                .pending_commands()
                .map_err(|error| format!("read bounded ControlCommandQueue failed: {error:?}"))?
                .len();
            if pending_count != fill_count {
                return Err(format!(
                    "bounded ControlCommandQueue invariant failed: pending={pending_count}, fill={fill_count}"
                ));
            }
            for index in 0..fill_count {
                let command_id = base_id.saturating_add(index as u64 + 1);
                let lease =
                    queue
                        .claim_command(command_id, &worker_id, 2, 60)
                        .map_err(|error| {
                            format!("claim control command {command_id} failed: {error:?}")
                        })?;
                queue
                    .ack_command_at(command_id, &worker_id, lease.fencing_token, 3)
                    .map_err(|error| {
                        format!("ack control command {command_id} failed: {error:?}")
                    })?;
            }
            println!(
                "{}",
                serde_json::json!({
                    "backend": backend,
                    "max_pending_commands": qx_storage::DEFAULT_MAX_PENDING_CONTROL_COMMANDS,
                    "max_command_bytes": qx_storage::MAX_CONTROL_COMMAND_BYTES,
                    "fill_count": fill_count,
                    "pending_count": pending_count,
                    "overflow_rejected": overflow_rejected,
                    "oversize_rejected": oversize_rejected,
                    "commands_acknowledged": fill_count,
                    "enqueue_p50_ms": bench_percentile(&enqueue_samples, 0.50),
                    "enqueue_p95_ms": bench_percentile(&enqueue_samples, 0.95),
                })
            );
        }
        _ => {
            return Err(format!(
                "unknown PreparedEvaluation benchmark operation: {operation}"
            ));
        }
    }
    Ok(())
}

#[cfg(feature = "bench-prepared-evaluation")]
fn bench_create_job_queue(
    backend: &str,
    root: &Path,
    capacity: usize,
) -> Result<Box<dyn qx_storage::JobQueueBackend>, String> {
    match backend {
        "file" => qx_storage::FileJobQueue::with_pending_limit(root, capacity)
            .map(|queue| Box::new(queue) as Box<dyn qx_storage::JobQueueBackend>)
            .map_err(|error| format!("create bounded file JobQueue failed: {error:?}")),
        #[cfg(feature = "sqlite")]
        "sqlite" => {
            let path = if root.extension().is_some_and(|extension| extension == "db") {
                root.to_path_buf()
            } else {
                root.join("jobs.sqlite")
            };
            qx_storage::SqliteJobQueue::with_pending_limit(path, capacity)
                .map(|queue| Box::new(queue) as Box<dyn qx_storage::JobQueueBackend>)
                .map_err(|error| format!("create bounded SQLite JobQueue failed: {error:?}"))
        }
        #[cfg(feature = "postgres")]
        "postgres" => {
            let dsn = std::env::var("QX_BENCH_POSTGRES_DSN").map_err(|_| {
                "postgres queue benchmark requires QX_BENCH_POSTGRES_DSN for an isolated database".to_owned()
            })?;
            qx_storage::PostgresJobQueue::connect_with_pending_limit(&dsn, capacity)
                .map(|queue| Box::new(queue) as Box<dyn qx_storage::JobQueueBackend>)
                .map_err(|error| format!("create bounded PostgreSQL JobQueue failed: {error:?}"))
        }
        #[cfg(not(feature = "sqlite"))]
        "sqlite" => Err("SQLite queue benchmark requires the sqlite feature".into()),
        #[cfg(not(feature = "postgres"))]
        "postgres" => Err("PostgreSQL queue benchmark requires the postgres feature".into()),
        _ => Err(format!("unsupported JobQueue backend: {backend}")),
    }
}

#[cfg(feature = "bench-prepared-evaluation")]
fn bench_create_control_queue(
    backend: &str,
    root: &Path,
) -> Result<Box<dyn qx_storage::ControlCommandQueueBackend>, String> {
    match backend {
        "file" => Ok(Box::new(qx_storage::ControlCommandQueue::new(root))),
        #[cfg(feature = "sqlite")]
        "sqlite" => {
            let path = root.join("control-commands.sqlite");
            qx_storage::SqliteControlCommandQueue::new(path)
                .map(|queue| Box::new(queue) as Box<dyn qx_storage::ControlCommandQueueBackend>)
                .map_err(|error| format!("create SQLite ControlCommandQueue failed: {error:?}"))
        }
        #[cfg(feature = "postgres")]
        "postgres" => {
            let dsn = std::env::var("QX_BENCH_POSTGRES_DSN").map_err(|_| {
                "postgres queue benchmark requires QX_BENCH_POSTGRES_DSN for an isolated database".to_owned()
            })?;
            qx_storage::PostgresControlCommandQueue::connect(&dsn)
                .map(|queue| Box::new(queue) as Box<dyn qx_storage::ControlCommandQueueBackend>)
                .map_err(|error| format!("create PostgreSQL ControlCommandQueue failed: {error:?}"))
        }
        #[cfg(not(feature = "sqlite"))]
        "sqlite" => Err("SQLite control-queue benchmark requires the sqlite feature".into()),
        #[cfg(not(feature = "postgres"))]
        "postgres" => {
            Err("PostgreSQL control-queue benchmark requires the postgres feature".into())
        }
        _ => Err(format!(
            "unsupported ControlCommandQueue backend: {backend}"
        )),
    }
}

#[cfg(feature = "bench-prepared-evaluation")]
fn bench_path(args: &[String], index: usize) -> Result<PathBuf, String> {
    args.get(index)
        .map(PathBuf::from)
        .ok_or_else(|| "PreparedEvaluation benchmark requires a journal path".to_owned())
}

#[cfg(feature = "bench-prepared-evaluation")]
fn bench_usize(
    args: &[String],
    index: usize,
    name: &str,
    minimum: usize,
    maximum: usize,
) -> Result<usize, String> {
    let value = args
        .get(index)
        .ok_or_else(|| format!("PreparedEvaluation benchmark requires {name}"))?
        .parse::<usize>()
        .map_err(|error| format!("invalid {name}: {error}"))?;
    if !(minimum..=maximum).contains(&value) {
        return Err(format!("{name} must be in {minimum}..={maximum}"));
    }
    Ok(value)
}

#[cfg(feature = "bench-prepared-evaluation")]
fn bench_u64(args: &[String], index: usize, name: &str) -> Result<u64, String> {
    args.get(index)
        .ok_or_else(|| format!("PreparedEvaluation benchmark requires {name}"))?
        .parse::<u64>()
        .map_err(|error| format!("invalid {name}: {error}"))
}

#[cfg(feature = "bench-prepared-evaluation")]
fn benchmark_value(batch: usize, run_id: u64) -> Result<PreparedEvaluationV3, String> {
    let instrument = qx_core::InstrumentId::parse("BTCUSDT.OKX")
        .ok_or_else(|| "benchmark instrument is invalid".to_owned())?;
    let mut orders = Vec::with_capacity(batch);
    let mut commands = Vec::with_capacity(batch);
    for index in 0..batch {
        let command_id = index as u64 + 1;
        orders.push(Order {
            client_id: command_id,
            instrument: instrument.clone(),
            side: qx_core::Side::Buy,
            qty: qx_core::Quantity::from_raw(1_000_000_000),
            limit: None,
            status: qx_core::OrderStatus::Submitted,
            filled: qx_core::Quantity::ZERO,
            account_id: "acct-benchmark".into(),
            trace: None,
            policy: None,
            exact_amounts: None,
        });
        commands.push(ControlCommand {
            command_id,
            request_id: format!("bench:{run_id}:{command_id}"),
            operator_id: "prepared-evaluation-benchmark".into(),
            reason: "bounded synthetic benchmark decision".into(),
            kind: qx_control::CommandKind::SubmitOrder,
            target: command_id.to_string(),
            payload: BTreeMap::new(),
            permission: qx_control::Permission::Trading,
            dry_run: false,
        });
    }
    let mut value = PreparedEvaluationV3 {
        schema_version: PREPARED_EVALUATION_SCHEMA_VERSION,
        worker_id: "worker-benchmark".into(),
        job_id: "job-benchmark".into(),
        run_id,
        input_sha256: "1".repeat(64),
        facts_sha256: "2".repeat(64),
        config_sha256: "3".repeat(64),
        lease_fencing_token: 1,
        ranges: vec![CursorRange {
            log_name: "acct-benchmark.okx".into(),
            from_seq: 0,
            through_seq: batch as u64,
        }],
        orders,
        commands,
        spread_group_id: None,
        checkpoint: None,
        completed_command_ids: BTreeSet::new(),
        command_results: BTreeMap::new(),
        payload_sha256: String::new(),
    };
    value.payload_sha256 = prepared_evaluation_payload_sha256(&value)?;
    Ok(value)
}

#[cfg(feature = "bench-prepared-evaluation")]
fn bench_percentile(samples: &[f64], percentile: f64) -> f64 {
    let mut ordered = samples.to_vec();
    ordered.sort_by(f64::total_cmp);
    let index = ((ordered.len() as f64 * percentile).ceil() as usize)
        .saturating_sub(1)
        .min(ordered.len() - 1);
    ordered[index]
}

pub(in crate::workers::strategy_worker) fn unknown_command_result(error: &str) -> String {
    let max_reason_bytes = MAX_PREPARED_COMMAND_RESULT_BYTES - UNKNOWN_COMMAND_RESULT_PREFIX.len();
    let mut reason = String::new();
    for character in error.chars() {
        if reason.len() + character.len_utf8() > max_reason_bytes {
            break;
        }
        reason.push(character);
    }
    format!("{UNKNOWN_COMMAND_RESULT_PREFIX}{reason}")
}

pub(super) fn prepared_evaluation_backup_path(path: &Path) -> PathBuf {
    path.with_extension("prepared.bak")
}

pub(in crate::workers::strategy_worker) fn clear_prepared_evaluation(
    path: &Path,
) -> Result<(), String> {
    #[cfg(unix)]
    let mut removed = false;
    for candidate in [path.to_path_buf(), prepared_evaluation_backup_path(path)] {
        match std::fs::remove_file(&candidate) {
            Ok(()) => {
                #[cfg(unix)]
                {
                    removed = true;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("清理已提交 PreparedEvaluation 失败: {error}")),
        }
    }
    #[cfg(unix)]
    if removed {
        if let Some(parent) = path.parent() {
            std::fs::File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|error| format!("同步 PreparedEvaluation 清理目录失败: {error}"))?;
        }
    }
    Ok(())
}

pub(in crate::workers::strategy_worker) fn clear_prepared_evaluation_for_run(
    path: &Path,
    run_id: u64,
) -> Result<(), String> {
    if load_prepared_evaluation(path)?.is_some_and(|value| value.run_id == run_id) {
        clear_prepared_evaluation(path)?;
    }
    Ok(())
}

pub(in crate::workers::strategy_worker) fn clear_committed_prepared_evaluation(
    state: &CursorState,
    path: &Path,
) -> Result<bool, String> {
    let Some(prepared) = load_prepared_evaluation(path)? else {
        return Ok(false);
    };
    if !is_committed_run(state, prepared.run_id) {
        return Ok(false);
    }
    clear_prepared_evaluation_for_run(path, prepared.run_id)?;
    Ok(true)
}

#[cfg(test)]
mod crash_recovery_tests {
    use super::*;
    use std::process::Command;

    const TEST_NAME: &str = "workers::strategy_worker::execution_updates::prepared_evaluation::crash_recovery_tests::save_crash_boundaries_recover_a_valid_record";

    fn value(run_id: u64) -> PreparedEvaluationV3 {
        PreparedEvaluationV3 {
            schema_version: PREPARED_EVALUATION_SCHEMA_VERSION,
            worker_id: "crash-matrix".into(),
            job_id: "job-crash-matrix".into(),
            run_id,
            input_sha256: "1".repeat(64),
            facts_sha256: "2".repeat(64),
            config_sha256: "3".repeat(64),
            lease_fencing_token: 1,
            ranges: Vec::new(),
            orders: Vec::new(),
            commands: Vec::new(),
            spread_group_id: None,
            checkpoint: None,
            completed_command_ids: Default::default(),
            command_results: Default::default(),
            payload_sha256: String::new(),
        }
    }

    #[test]
    fn clearing_a_missing_journal_does_not_require_its_parent_directory() {
        let root = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "qx-prepared-clear-missing-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        let path = root.join("state.json");
        assert!(!root.exists());
        clear_prepared_evaluation(&path)
            .expect("no journal and no directory means cleanup has no durable change to sync");
        assert!(!root.exists());
    }

    #[test]
    fn save_crash_boundaries_recover_a_valid_record() {
        if let (Ok(root), Ok(point)) = (
            std::env::var("QX_TEST_PREPARED_CRASH_ROOT"),
            std::env::var("QX_TEST_PREPARED_CRASH_POINT"),
        ) {
            let root = PathBuf::from(root);
            let path = root.join("state.json");
            if point == "stale_backup_removed" {
                std::fs::write(prepared_evaluation_backup_path(&path), b"stale").unwrap();
            }
            save_prepared_evaluation(&path, &value(2)).unwrap();
            panic!("crash hook did not stop the child at {point}");
        }

        let points = vec![
            ("temp_created", 1),
            ("payload_written", 1),
            ("temp_synced", 1),
            ("stale_backup_removed", 1),
            ("backup_renamed", 1),
            ("new_published", 2),
            ("backup_cleaned", 2),
        ];
        #[cfg(unix)]
        let points = {
            let mut points = points;
            points.push(("directory_synced", 2));
            points
        };

        for (point, expected_run_id) in points {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::current_dir()
                .unwrap()
                .join("target")
                .join(format!(
                    "qx-prepared-crash-{}-{point}-{nonce}",
                    std::process::id()
                ));
            std::fs::create_dir_all(&root).unwrap();
            let path = root.join("state.json");
            save_prepared_evaluation(&path, &value(1)).unwrap();

            let child = Command::new(std::env::current_exe().unwrap())
                .arg("--exact")
                .arg(TEST_NAME)
                .arg("--nocapture")
                .env("QX_TEST_PREPARED_CRASH_ROOT", &root)
                .env("QX_TEST_PREPARED_CRASH_POINT", point)
                .env("QX_BENCH_PREPARED_CRASH_AFTER", point)
                .status()
                .unwrap();
            assert_eq!(child.code(), Some(86), "child did not crash at {point}");

            let recovered = load_prepared_evaluation(&path)
                .unwrap_or_else(|error| panic!("{point} recovery failed: {error}"))
                .unwrap_or_else(|| panic!("{point} recovery lost the journal"));
            assert_eq!(
                recovered.run_id, expected_run_id,
                "recovered wrong generation after {point}"
            );
            assert_eq!(
                recovered.payload_sha256,
                prepared_evaluation_payload_sha256(&recovered).unwrap(),
                "recovered record is not sealed after {point}"
            );
            std::fs::remove_dir_all(&root).unwrap();
        }
    }

    #[test]
    fn oversized_prepared_evaluation_is_rejected_without_fallback_or_replacement() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "qx-prepared-capacity-{}-{nonce}",
                std::process::id()
            ));
        std::fs::create_dir_all(&root).unwrap();

        let oversized_path = root.join("oversized.json");
        let oversized_backup = prepared_evaluation_backup_path(&oversized_path);
        save_prepared_evaluation(&oversized_backup, &value(7)).unwrap();
        let oversized = std::fs::File::create(&oversized_path).unwrap();
        oversized
            .set_len((MAX_PREPARED_EVALUATION_BYTES + 1) as u64)
            .unwrap();
        let load_error = load_prepared_evaluation(&oversized_path).unwrap_err();
        assert!(load_error.contains("exceeds 8388608 bytes"), "{load_error}");
        assert_eq!(
            load_prepared_evaluation(&oversized_backup)
                .unwrap()
                .unwrap()
                .run_id,
            7,
            "the valid recovery copy must remain available after fail-closed load"
        );
        assert!(oversized_backup.exists());

        let path = root.join("existing.json");
        save_prepared_evaluation(&path, &value(1)).unwrap();
        let mut oversized_update = value(2);
        oversized_update.job_id = "x".repeat(MAX_PREPARED_EVALUATION_BYTES + 1);
        assert!(save_prepared_evaluation(&path, &oversized_update).is_err());
        assert_eq!(
            load_prepared_evaluation(&path).unwrap().unwrap().run_id,
            1,
            "oversized update must preserve the previously committed journal"
        );
        assert!(!prepared_evaluation_backup_path(&path).exists());
        assert!(!path
            .with_extension(format!("prepared.tmp.{}", std::process::id()))
            .exists());

        std::fs::remove_dir_all(root).unwrap();
    }
}
