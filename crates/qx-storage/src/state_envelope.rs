//! P1c 统一 JSON 文件状态信封（V10 §4.9）。
//!
//! 审计口径修正：四个文件状态存储（`JsonStateStore`、`FileConsumerStateStore`、
//! `FileOutboxStore`、`FileJobQueue`）在迁移前**已经**共享同一套原子替换
//! [`write_atomic_path`] 与跨进程锁 [`acquire_storage_lock`]（含 Phase 4t 修掉的
//! Windows `create_new` 遇 `PermissionDenied` 的有界重试，逻辑逐字保留）；
//! 真正写重复的是它们各自的“序列化 + 信封校验 + 读改写事务”层。本模块把该层收敛
//! 为唯一实现：
//!
//! - 类型化 JSON 序列化 / 反序列化与统一错误文案前缀（[`JsonStateEnvelope::LABEL`]）；
//! - schema/版本字段拒绝：内嵌版本号的负载在读写两侧都拒绝未来版本（[`JsonStateEnvelope::embedded_schema_version`]）；
//! - 损坏文件拒绝：反序列化成功后必须通过 [`JsonStateEnvelope::validate_loaded`]；
//! - 原子替换与锁：沿用迁移前逐字节相同的文件名、目录布局与锁文件命名。
//!
//! 磁盘兼容是硬约束：信封不新增、不改名任何 JSON key；无版本 key 的首代格式
//! （consumer 状态、任务队列信封等）保持原样，`embedded_schema_version` 返回
//! `None`，未来如需引入版本 key 必须另做迁移而不是就地改写。

use crate::{StorageError, TEMP_FILE_SEQUENCE};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

/// 参与统一信封的文件状态负载类型。实现方只提供“标签 + 版本 + 结构校验”，
/// 序列化、损坏拒绝与原子写路径一律复用本模块的同一份实现。
pub trait JsonStateEnvelope: Serialize + DeserializeOwned {
    /// 错误文案主体，保持各存储迁移前的历史措辞
    /// （例如 `"consumer 状态"` 拼出“consumer 状态解析失败: …”）。
    const LABEL: &'static str;
    /// 本类型可读写的最高 schema 版本；内嵌版本大于它的文件按损坏拒绝。
    const SUPPORTED_SCHEMA_VERSION: u32 = 1;
    /// 内嵌 `schema_version` 字段的当前值；`None` 表示首代格式没有版本 key
    /// （保持磁盘兼容，不就地补 key）。
    fn embedded_schema_version(&self) -> Option<u32> {
        None
    }
    /// 反序列化成功后的结构语义校验（身份自洽、链式不变量等）。
    /// 信封在读取与写入两侧都会执行一次。
    fn validate_loaded(&self) -> Result<(), StorageError> {
        Ok(())
    }
}

fn reject_unsupported_schema<T: JsonStateEnvelope>(value: &T) -> Result<(), StorageError> {
    if let Some(version) = value.embedded_schema_version() {
        if version > T::SUPPORTED_SCHEMA_VERSION {
            return Err(StorageError::Conflict(format!(
                "{} schema 版本过新: {version} > {}",
                T::LABEL,
                T::SUPPORTED_SCHEMA_VERSION
            )));
        }
    }
    Ok(())
}

/// 信封序列化（唯一实现）：版本拒绝 + 结构校验 + 类型化 JSON 文本。
pub(crate) fn encode_state_json<T: JsonStateEnvelope>(value: &T) -> Result<String, StorageError> {
    reject_unsupported_schema(value)?;
    value.validate_loaded()?;
    serde_json::to_string(value)
        .map_err(|error| StorageError::Io(format!("{}序列化失败: {error}", T::LABEL)))
}

/// 信封反序列化（唯一实现）：类型化 JSON 解析 + 版本拒绝 + 结构校验。
pub(crate) fn decode_state_json<T: JsonStateEnvelope>(text: &str) -> Result<T, StorageError> {
    let value: T = serde_json::from_str(text)
        .map_err(|error| StorageError::Io(format!("{}解析失败: {error}", T::LABEL)))?;
    reject_unsupported_schema(&value)?;
    value.validate_loaded()?;
    Ok(value)
}

pub(crate) fn read_state_json<T: JsonStateEnvelope>(path: &Path) -> Result<T, StorageError> {
    decode_state_json(&read_state_text_required(path)?)
}

pub(crate) fn read_state_json_or_default<T: JsonStateEnvelope + Default>(
    path: &Path,
) -> Result<T, StorageError> {
    match std::fs::read_to_string(path) {
        Ok(text) => decode_state_json(&text),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(T::default()),
        Err(error) => Err(StorageError::Io(error.to_string())),
    }
}

/// 信封写入（唯一实现）：校验后的类型化 JSON 原子替换。
pub(crate) fn write_state_json<T: JsonStateEnvelope>(
    root: &Path,
    path: &Path,
    value: &T,
) -> Result<(), StorageError> {
    write_atomic_path(path, root, &encode_state_json(value)?)
}

/// `transact_state_json` 闭包的写回决策：`Write` 原子落盘，`Keep` 幂等跳过。
pub(crate) enum Commit<R> {
    Write(R),
    Keep(R),
}

/// 统一的读改写事务（唯一实现）：确保目录 → 获取信封锁 → 读取（缺省为默认态）
/// → 变更 → 仅在标记写回时原子替换。锁内早退（幂等重复提交）沿用迁移前语义。
pub(crate) fn transact_state_json<T, R, F>(
    root: &Path,
    dir: &Path,
    path: &Path,
    lock_path: PathBuf,
    update: F,
) -> Result<R, StorageError>
where
    T: JsonStateEnvelope + Default,
    F: FnOnce(&mut T) -> Result<Commit<R>, StorageError>,
{
    std::fs::create_dir_all(dir).map_err(|error| StorageError::Io(error.to_string()))?;
    let _lock = acquire_storage_lock(lock_path)?;
    let mut state = read_state_json_or_default::<T>(path)?;
    let (write_back, result) = match update(&mut state)? {
        Commit::Write(value) => (true, value),
        Commit::Keep(value) => (false, value),
    };
    if write_back {
        write_state_json(root, path, &state)?;
    }
    Ok(result)
}

/// 无信封约束的通用 JSON 读写（供 `JsonStateStore` 的泛型公共 API 复用同一份
/// 序列化 / 原子替换实现，错误文案主体由调用方传入）。
pub(crate) fn encode_json<T: Serialize>(
    value: &T,
    pretty: bool,
    subject: &str,
) -> Result<String, StorageError> {
    let text = if pretty {
        serde_json::to_string_pretty(value)
    } else {
        serde_json::to_string(value)
    }
    .map_err(|error| StorageError::Io(format!("{subject}序列化失败: {error}")))?;
    Ok(text)
}

pub(crate) fn write_json_file<T: Serialize>(
    root: &Path,
    path: &Path,
    value: &T,
    pretty: bool,
    subject: &str,
) -> Result<(), StorageError> {
    write_atomic_path(path, root, &encode_json(value, pretty, subject)?)
}

pub(crate) fn read_json_file<T: DeserializeOwned>(
    path: &Path,
    subject: &str,
) -> Result<T, StorageError> {
    let text =
        std::fs::read_to_string(path).map_err(|error| StorageError::Io(error.to_string()))?;
    serde_json::from_str(&text)
        .map_err(|error| StorageError::Io(format!("{subject}解析失败: {error}")))
}

/// 状态文件文本读取（控制面 / 调度器等自带校验序列化的类型复用）。
pub(crate) fn read_state_text(path: &Path) -> Result<Option<String>, StorageError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(StorageError::Io(error.to_string())),
    }
}

/// 必需的文本读取：文件缺失按 IO 错误返回（保持迁移前逐后端一致的错误形态）。
pub(crate) fn read_state_text_required(path: &Path) -> Result<String, StorageError> {
    std::fs::read_to_string(path).map_err(|error| StorageError::Io(error.to_string()))
}

/// 状态文件文本原子替换。
pub(crate) fn write_state_text(
    root: &Path,
    path: &Path,
    content: &str,
) -> Result<(), StorageError> {
    write_atomic_path(path, root, content)
}

#[derive(Debug)]
pub(crate) struct StorageLock {
    path: PathBuf,
}

impl Drop for StorageLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// 跨进程锁的等待预算：墙钟时间，不是尝试次数。
///
/// 按次数计预算时，"能等多久"取决于一次 `create_new` 失败要付出多久：整树并发下
/// 释放窗口（Windows 上删除待处理 + 杀软重扫同名文件）可远超原先 100 次 × 1 毫秒的
/// 预算，于是审计追加会返回 `Conflict`（实测：`audit_file_store_serializes_concurrent_appends_without_losing_tail`
/// 在负载下把这条链跑红）。墙钟预算让"能熬过多忙的机器"变成显式承诺。
const STORAGE_LOCK_WAIT: std::time::Duration = std::time::Duration::from_millis(4_000);
/// 轮询退避上限：小步快速让路给释放方，稳态下不把文件系统调用排队拉长。
const STORAGE_LOCK_BACKOFF_STEP: std::time::Duration = std::time::Duration::from_millis(2);
const STORAGE_LOCK_BACKOFF_MAX: std::time::Duration = std::time::Duration::from_millis(16);

pub(crate) fn acquire_storage_lock(path: PathBuf) -> Result<StorageLock, StorageError> {
    acquire_storage_lock_within(path, STORAGE_LOCK_WAIT)
}

/// 有界等待的锁获取：只在预算耗尽时返回 `Conflict`，因此不存在自旋不退出的路径。
/// 预算作为参数留给用例在同一毫秒尺度上验证两端（等到 → 成功、超预算 → `Conflict`）。
fn acquire_storage_lock_within(
    path: PathBuf,
    wait: std::time::Duration,
) -> Result<StorageLock, StorageError> {
    // `AlreadyExists` 与 Windows 删除窗口内 `create_new` 抛出的 `PermissionDenied`
    // （os error 5）都算锁正在占用或正在释放。
    let deadline = std::time::Instant::now() + wait;
    let mut backoff = STORAGE_LOCK_BACKOFF_STEP;
    let mut attempts = 0_u32;
    loop {
        attempts += 1;
        let error = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => return Ok(StorageLock { path }),
            Err(error) => error,
        };
        if !matches!(
            error.kind(),
            ErrorKind::AlreadyExists | ErrorKind::PermissionDenied
        ) {
            return Err(StorageError::Io(error.to_string()));
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(StorageError::Conflict(format!(
                "存储追加锁被占用（{attempts} 次尝试、{} 毫秒内未取得），调用方应在恢复后重试",
                wait.as_millis()
            )));
        }
        std::thread::sleep(backoff.min(remaining));
        backoff = (backoff * 2).min(STORAGE_LOCK_BACKOFF_MAX);
    }
}

pub(crate) fn sync_file(path: &Path) -> Result<(), StorageError> {
    #[cfg(not(windows))]
    {
        std::fs::File::open(path)
            .and_then(|file| file.sync_all())
            .map_err(|error| StorageError::Io(error.to_string()))?;
    }
    #[cfg(windows)]
    {
        // Windows antivirus/indexer hooks can reject fsync on a freshly created
        // temp file; rename still provides the crash-safe visibility boundary.
        let _ = path;
    }
    Ok(())
}

pub(crate) fn write_atomic_path(
    path: &Path,
    root: &Path,
    content: &str,
) -> Result<(), StorageError> {
    let parent = path
        .parent()
        .ok_or_else(|| StorageError::Io("存储路径没有父目录".into()))?;
    if !parent.starts_with(root) {
        return Err(StorageError::InvalidName("存储路径越出根目录".into()));
    }
    std::fs::create_dir_all(parent).map_err(|error| StorageError::Io(error.to_string()))?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| StorageError::Io("存储文件名非法".into()))?;
    let temp = parent.join(format!(
        ".{name}.tmp.{}",
        TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&temp, content).map_err(|error| StorageError::Io(error.to_string()))?;
    sync_file(&temp)?;
    std::fs::rename(&temp, path).map_err(|error| StorageError::Io(error.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
    struct Probe {
        schema_version: u32,
        note: String,
    }

    impl JsonStateEnvelope for Probe {
        const LABEL: &'static str = "探针";
        fn embedded_schema_version(&self) -> Option<u32> {
            Some(self.schema_version)
        }
        fn validate_loaded(&self) -> Result<(), StorageError> {
            if self.note.is_empty() {
                return Err(StorageError::Conflict("探针 note 不能为空".into()));
            }
            Ok(())
        }
    }

    #[test]
    fn envelope_round_trips_and_rejects_corrupt_or_too_new_state() {
        let ok = Probe {
            schema_version: 1,
            note: "hello".into(),
        };
        let text = encode_state_json(&ok).unwrap();
        assert_eq!(decode_state_json::<Probe>(&text).unwrap(), ok);

        let too_new = decode_state_json::<Probe>(r#"{"schema_version":2,"note":"hello"}"#);
        assert!(matches!(too_new, Err(StorageError::Conflict(_))));

        let corrupt = decode_state_json::<Probe>("{truncated");
        assert!(corrupt.is_err());
        let validated = decode_state_json::<Probe>(r#"{"schema_version":1,"note":""}"#);
        assert!(matches!(validated, Err(StorageError::Conflict(_))));
    }

    fn lock_fixture(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "qianxing-lock-{}-{}-{}.lock",
            name,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn storage_lock_waits_out_a_brief_holder_instead_of_failing_the_write() {
        let path = lock_fixture("wait");
        std::fs::write(&path, "held").unwrap();
        let releaser = {
            let path = path.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(20));
                let _ = std::fs::remove_file(path);
            })
        };
        let lock = acquire_storage_lock_within(path.clone(), std::time::Duration::from_millis(500));
        releaser.join().unwrap();
        assert!(
            lock.is_ok(),
            "占用方在预算内释放时，等待必须拿到锁而不是把写失败上抛: {lock:?}"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn storage_lock_gives_up_at_its_deadline_rather_than_spinning_forever() {
        let path = lock_fixture("deadline");
        std::fs::write(&path, "held").unwrap();
        let started = std::time::Instant::now();
        let result =
            acquire_storage_lock_within(path.clone(), std::time::Duration::from_millis(30));
        let elapsed = started.elapsed();
        let _ = std::fs::remove_file(path);
        let detail = match result {
            Err(StorageError::Conflict(detail)) => detail,
            other => panic!("超预算的错误形态必须是 Conflict，实际为 {other:?}"),
        };
        assert!(
            elapsed >= std::time::Duration::from_millis(30)
                && elapsed < std::time::Duration::from_secs(2),
            "等待必须落在墙钟预算内结束，实际 {elapsed:?}: {detail}"
        );
    }
}
