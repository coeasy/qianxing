//! 产物发布的唯一出口：临时文件 + 原子发布，崩溃不在盘上留半份事实。
//!
//! 三种发布语义收敛到本模块一个 [`TempPublish`]：
//! - [`PublishMode::Idempotent`]：目标已有且内容相同 → `Ok`；内容不同 → `Conflict`（保护
//!   已写事实，绝不静默覆盖）；不存在 → 写临时文件后原子发布。摘要、RunRecord、
//!   RunManifest、数据集清单走这一档。
//! - [`PublishMode::CreateNew`]：目标已存在（无论内容）→ `Conflict`。
//! - [`PublishMode::Overwrite`]：目标存在即静默替换。只用于「每跑一次就整份重写」的
//!   可再生展示件（HTML/SVG/XLSX 报告、权益与成交明细）与运行记录的**状态流转**
//!   （running → 终态是同一份记录的演进，不是另一件事实），不用于任何带指纹的事实件。
//!
//! 需要边写边发（zip 流）、或发布前设置权限位（可执行导出件）的场景，用
//! [`TempPublish::begin`] 拿一个临时文件句柄（+ [`TempPublish::set_unix_mode`]），
//! 写完 [`TempPublish::commit`]；半途 [`TempPublish::abort`] 或直接 drop 都会清掉临时文件。
//!
//! 临时文件名同时带进程 id 与自增序号：前者避免多进程互踩，后者避免同进程内两个
//! 发布动作撞同一个临时路径（撞上就换下一个名字）。

use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// 发布语义，见模块文档。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublishMode {
    /// 目标内容相同则幂等，不同则冲突报错（不覆盖）。
    Idempotent,
    /// 目标已存在即冲突报错。
    CreateNew,
    /// 目标已存在即静默替换（仅可再生展示件与运行记录状态流转可用）。
    Overwrite,
}

/// 发布失败。`Conflict` 与 `Io` 分开，调用方要能把「并发写者」与「盘真的坏了」
/// 报成不同的话——这两者在收敛前各写一份时最容易分叉。
#[derive(Debug)]
pub enum PublishError {
    /// 目标已存在且内容不同（或 `CreateNew` 下目标已存在）。
    Conflict(String),
    /// 临时文件 / 同步 / 发布任一环节失败，**带原始 `io::Error`**（kind 不丢：
    /// 调用方按 `AppError::from_io` 的四类落点归类；把 kind 塞进字符串再让调用方
    /// 猜，正是 `error.rs` 明令禁止的分支方式）。
    Io(io::Error),
}

impl std::fmt::Display for PublishError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PublishError::Conflict(message) => write!(f, "{message}"),
            PublishError::Io(error) => write!(f, "{error}"),
        }
    }
}

/// 一次发布动作：临时文件句柄 + 发布目标。`commit` 前目标不受影响。
pub struct TempPublish {
    pub(crate) temporary: PathBuf,
    pub(crate) target: PathBuf,
}

static TEMP_SERIAL: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct BundleJournal {
    transaction_id: String,
    state: String,
    entries: Vec<BundleJournalEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct BundleJournalEntry {
    target: PathBuf,
    staged: PathBuf,
    backup: PathBuf,
    had_original: bool,
}

impl TempPublish {
    /// 开一个临时文件准备写 `target`。写目标前先按 `mode` 做存在性判定。
    pub fn begin(target: &Path, mode: PublishMode) -> Result<(Self, File), PublishError> {
        match mode {
            // Idempotent 的「内容相同则收口」由 `publish` 在调用本函数前判完；
            // 走到这里说明目标不存在，或内容不同（后者按 Conflict 挡回）。
            PublishMode::Idempotent | PublishMode::CreateNew => {
                if target.exists() {
                    return Err(PublishError::Conflict(format!(
                        "同一产物路径已存在: {}",
                        target.display()
                    )));
                }
            }
            PublishMode::Overwrite => {}
        }
        let temporary = temporary_path(target);
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| {
                PublishError::Io(io_message_error("创建临时产物", &temporary, &error))
            })?;
        Ok((
            Self {
                temporary,
                target: target.to_path_buf(),
            },
            file,
        ))
    }

    /// 原子发布：临时文件已在调用方写入并 sync 之后调用。
    pub fn commit(self) -> Result<(), PublishError> {
        std::fs::rename(&self.temporary, &self.target).map_err(|error| {
            PublishError::Io(io_message_error("发布产物", &self.target, &error))
        })?;
        std::mem::forget(self);
        Ok(())
    }

    /// 放弃本次发布并清掉临时文件。
    pub fn abort(self) {
        // 走 Drop
    }

    /// 发布前给临时文件设 Unix 权限位（可执行位随导出件走：源可执行、导出件才可执行）。
    /// 非 Unix 平台没有这颗方法——权限位在那里不是「保留」而是「丢失」，不做假动作。
    #[cfg(unix)]
    pub fn set_unix_mode(&self, mode: u32) -> Result<(), PublishError> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&self.temporary, std::fs::Permissions::from_mode(mode)).map_err(
            |error| PublishError::Io(io_message_error("设置产物权限", &self.temporary, &error)),
        )
    }
}

impl Drop for TempPublish {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.temporary);
    }
}

/// 原子发布一段文本。`Overwrite` 之外的语义都**不会**覆盖已有内容。
pub fn publish(path: &Path, payload: &str, mode: PublishMode) -> Result<(), PublishError> {
    if mode == PublishMode::Idempotent {
        if let Ok(existing) = read_existing(path) {
            return if existing == payload {
                Ok(())
            } else {
                Err(PublishError::Conflict(format!(
                    "同一产物路径已存在不同内容: {}",
                    path.display()
                )))
            };
        }
    }
    let (session, mut file) = TempPublish::begin(path, mode)?;
    let written = (|| {
        file.write_all(payload.as_bytes()).map_err(|error| {
            PublishError::Io(io_message_error("写入临时产物", &session.temporary, &error))
        })?;
        file.sync_all().map_err(|error| {
            PublishError::Io(io_message_error("同步临时产物", &session.temporary, &error))
        })?;
        Ok::<(), PublishError>(())
    })();
    drop(file);
    finish(session, written)
}

/// 原子发布一段字节（冻结数据集等非 UTF-8 字节流）。语义与 [`publish`] 相同。
pub fn publish_bytes(path: &Path, payload: &[u8], mode: PublishMode) -> Result<(), PublishError> {
    if mode == PublishMode::Idempotent {
        if let Ok(existing) = std::fs::read(path) {
            return if existing == payload {
                Ok(())
            } else {
                Err(PublishError::Conflict(format!(
                    "同一产物路径已存在不同内容: {}",
                    path.display()
                )))
            };
        }
    }
    let (session, mut file) = TempPublish::begin(path, mode)?;
    let written = (|| {
        file.write_all(payload).map_err(|error| {
            PublishError::Io(io_message_error("写入临时产物", &session.temporary, &error))
        })?;
        file.sync_all().map_err(|error| {
            PublishError::Io(io_message_error("同步临时产物", &session.temporary, &error))
        })?;
        Ok::<(), PublishError>(())
    })();
    drop(file);
    finish(session, written)
}

/// Stage a related set of generated files and publish them together. If any
/// install step fails, remove files already installed by this call and restore
/// every previous target before returning the original I/O error.
pub fn publish_bundle(entries: &[(PathBuf, Vec<u8>)]) -> Result<(), PublishError> {
    publish_bundle_with(entries, |_| Ok(()))
}

fn publish_bundle_with<F>(
    entries: &[(PathBuf, Vec<u8>)],
    mut before_install: F,
) -> Result<(), PublishError>
where
    F: FnMut(usize) -> io::Result<()>,
{
    if entries.is_empty() {
        return Err(PublishError::Conflict(
            "报告 bundle 至少要包含一个文件".to_string(),
        ));
    }
    for (target, _) in entries {
        if target.exists() && !target.is_file() {
            return Err(PublishError::Conflict(format!(
                "报告输出目标不是普通文件: {}",
                target.display()
            )));
        }
    }

    for (target, _) in entries {
        if let Some(parent) = target
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).map_err(|error| {
                PublishError::Io(io_message_error("创建报告目录", parent, &error))
            })?;
        }
    }

    let mut normalized = Vec::with_capacity(entries.len());
    let mut targets = std::collections::HashSet::new();
    for (target, payload) in entries {
        let identity = canonical_target(target).map_err(|error| {
            PublishError::Io(io_message_error("解析报告输出路径", target, &error))
        })?;
        if !targets.insert(identity.clone()) {
            return Err(PublishError::Conflict(format!(
                "报告 bundle 包含重复输出路径: {}",
                target.display()
            )));
        }
        let target = absolute_target(target).map_err(|error| {
            PublishError::Io(io_message_error("规范报告输出路径", target, &error))
        })?;
        normalized.push((target, payload.clone()));
    }

    let directories = normalized
        .iter()
        .filter_map(|(target, _)| target.parent().map(Path::to_path_buf))
        .collect::<std::collections::BTreeSet<_>>();
    for directory in &directories {
        recover_pending_bundles(directory).map_err(PublishError::Io)?;
    }

    let transaction_id = format!(
        "{}-{}-{}",
        std::process::id(),
        TEMP_SERIAL.fetch_add(1, Ordering::Relaxed),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let mut journal = BundleJournal {
        transaction_id: transaction_id.clone(),
        state: "prepared".into(),
        entries: Vec::with_capacity(normalized.len()),
    };
    for (index, (target, _)) in normalized.iter().enumerate() {
        journal.entries.push(BundleJournalEntry {
            target: target.clone(),
            staged: bundle_path(target, &transaction_id, index, "stage"),
            backup: bundle_path(target, &transaction_id, index, "backup"),
            had_original: target.exists(),
        });
    }

    let mut stage_cleanup = StageCleanup::default();
    for (entry, (_, payload)) in journal.entries.iter().zip(&normalized) {
        write_new_file(&entry.staged, payload, &mut stage_cleanup.paths).map_err(|error| {
            PublishError::Io(io_message_error("暂存报告文件", &entry.staged, &error))
        })?;
    }

    let journal_paths = directories
        .iter()
        .map(|directory| journal_path(directory, &transaction_id))
        .collect::<Vec<_>>();
    let payload = serde_json::to_vec(&journal)
        .map_err(|error| PublishError::Io(io::Error::new(io::ErrorKind::InvalidData, error)))?;
    let mut created_journals = Vec::new();
    for path in &journal_paths {
        if let Err(error) = publish_bytes(path, &payload, PublishMode::CreateNew) {
            for created in &created_journals {
                let _ = std::fs::remove_file(created);
            }
            return Err(error);
        }
        created_journals.push(path.clone());
    }

    for entry in &journal.entries {
        if !entry.had_original {
            continue;
        }
        if let Err(error) = std::fs::rename(&entry.target, &entry.backup) {
            let rollback = recover_journal(&journal, &journal_paths);
            return Err(PublishError::Io(append_rollback_error(
                io_message_error("暂存原报告", &entry.target, &error),
                rollback,
            )));
        }
    }

    for (index, entry) in journal.entries.iter().enumerate() {
        let install = before_install(index).and_then(|()| {
            std::fs::rename(&entry.staged, &entry.target)
                .map_err(|error| io_message_error("发布报告文件", &entry.target, &error))
        });
        if let Err(error) = install {
            let rollback = recover_journal(&journal, &journal_paths);
            return Err(PublishError::Io(append_rollback_error(error, rollback)));
        }
    }

    journal.state = "committed".into();
    let committed_payload = serde_json::to_vec(&journal)
        .map_err(|error| PublishError::Io(io::Error::new(io::ErrorKind::InvalidData, error)))?;
    let mut committed_count = 0;
    for path in &journal_paths {
        if publish_bytes(path, &committed_payload, PublishMode::Overwrite).is_ok() {
            committed_count += 1;
        }
    }
    if committed_count == 0 {
        journal.state = "prepared".into();
        let rollback = recover_journal(&journal, &journal_paths);
        return Err(PublishError::Io(append_rollback_error(
            io::Error::other("报告已切换，但无法持久化 bundle 提交标记"),
            rollback,
        )));
    }
    if committed_count == journal_paths.len() {
        cleanup_journal(&journal, &journal_paths, true).map_err(PublishError::Io)?;
    }
    Ok(())
}

#[derive(Default)]
struct StageCleanup {
    paths: Vec<PathBuf>,
}

impl Drop for StageCleanup {
    fn drop(&mut self) {
        cleanup_stages(&self.paths);
    }
}

fn write_new_file(path: &Path, payload: &[u8], created_paths: &mut Vec<PathBuf>) -> io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    created_paths.push(path.to_path_buf());
    file.write_all(payload)?;
    file.sync_all()
}

fn cleanup_stages(paths: &[PathBuf]) {
    for path in paths {
        let _ = std::fs::remove_file(path);
    }
}

fn bundle_path(target: &Path, transaction_id: &str, index: usize, suffix: &str) -> PathBuf {
    let file_name = target.file_name().unwrap_or_default().to_string_lossy();
    target.with_file_name(format!(".{file_name}.qx-{transaction_id}-{index}.{suffix}"))
}

fn journal_path(directory: &Path, transaction_id: &str) -> PathBuf {
    directory.join(format!(".qx-report-txn-{transaction_id}.json"))
}

fn absolute_target(path: &Path) -> io::Result<PathBuf> {
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "报告输出路径缺少文件名"))?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    Ok(parent.canonicalize()?.join(name))
}

fn recover_pending_bundles(directory: &Path) -> io::Result<()> {
    let directory = match directory.canonicalize() {
        Ok(directory) => directory,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let mut seen = std::collections::HashSet::new();
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(transaction_id) = name
            .strip_prefix(".qx-report-txn-")
            .and_then(|name| name.strip_suffix(".json"))
        else {
            continue;
        };
        if !seen.insert(transaction_id.to_string()) {
            continue;
        }
        let journal_file = entry.path();
        let bytes = std::fs::read(&journal_file)?;
        let journal: BundleJournal = serde_json::from_slice(&bytes).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("损坏的报告事务日志 {}: {error}", journal_file.display()),
            )
        })?;
        validate_journal(&journal, &journal_file)?;
        let paths = journal_directories(&journal)
            .iter()
            .map(|directory| journal_path(directory, transaction_id))
            .collect::<Vec<_>>();
        recover_journal(&journal, &paths)?;
    }
    Ok(())
}

fn validate_journal(journal: &BundleJournal, path: &Path) -> io::Result<()> {
    if !matches!(journal.state.as_str(), "prepared" | "committed")
        || journal.transaction_id.is_empty()
        || journal.transaction_id.contains(['/', '\\'])
        || journal.entries.is_empty()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "报告事务日志字段无效",
        ));
    }
    let directories = journal_directories(journal);
    let journal_directory = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    if !directories.contains(&journal_directory)
        || path
            != journal_path(
                path.parent().unwrap_or(Path::new(".")),
                &journal.transaction_id,
            )
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "报告事务日志路径无效: path={} directory={} expected={} journal_directories={directories:?}",
                path.display(),
                journal_directory.display(),
                journal_path(&journal_directory, &journal.transaction_id).display(),
            ),
        ));
    }
    for (index, entry) in journal.entries.iter().enumerate() {
        if !entry.target.is_absolute()
            || entry.staged != bundle_path(&entry.target, &journal.transaction_id, index, "stage")
            || entry.backup != bundle_path(&entry.target, &journal.transaction_id, index, "backup")
            || entry.target.parent() != entry.staged.parent()
            || entry.target.parent() != entry.backup.parent()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "报告事务日志包含不安全路径",
            ));
        }
    }
    Ok(())
}

fn journal_directories(journal: &BundleJournal) -> std::collections::BTreeSet<PathBuf> {
    journal
        .entries
        .iter()
        .filter_map(|entry| entry.target.parent().map(Path::to_path_buf))
        .collect()
}

fn recover_journal(journal: &BundleJournal, paths: &[PathBuf]) -> io::Result<()> {
    let mut committed = false;
    for path in paths {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let copy: BundleJournal = serde_json::from_slice(&bytes).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("损坏的报告事务日志 {}: {error}", path.display()),
            )
        })?;
        validate_journal(&copy, path)?;
        if copy.transaction_id != journal.transaction_id || copy.entries != journal.entries {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "同一报告事务的日志副本不一致",
            ));
        }
        committed |= copy.state == "committed";
    }
    if committed {
        for entry in &journal.entries {
            remove_if_exists(&entry.backup)?;
            remove_if_exists(&entry.staged)?;
        }
    } else {
        for entry in journal.entries.iter().rev() {
            if entry.backup.exists() {
                remove_if_exists(&entry.target)?;
                std::fs::rename(&entry.backup, &entry.target)?;
            } else if !entry.had_original && !entry.staged.exists() {
                remove_if_exists(&entry.target)?;
            }
            remove_if_exists(&entry.staged)?;
        }
    }
    for path in paths {
        remove_if_exists(path)?;
    }
    Ok(())
}

fn cleanup_journal(journal: &BundleJournal, paths: &[PathBuf], keep_new: bool) -> io::Result<()> {
    for entry in &journal.entries {
        if keep_new {
            remove_if_exists(&entry.backup)?;
        }
        remove_if_exists(&entry.staged)?;
    }
    for path in paths {
        remove_if_exists(path)?;
    }
    Ok(())
}

fn remove_if_exists(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn canonical_target(path: &Path) -> io::Result<PathBuf> {
    if path.exists() {
        return path.canonicalize();
    }
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "报告输出路径缺少文件名"))?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    Ok(parent.canonicalize()?.join(name))
}

fn append_rollback_error(error: io::Error, rollback: io::Result<()>) -> io::Error {
    let Err(rollback_error) = rollback else {
        return error;
    };
    let kind = error.kind();
    io::Error::new(kind, format!("{error}; 报告回滚失败: {rollback_error}"))
}

/// 原子发布一段由调用方逐块写出的内容（zip 包这类流式件；要带权限位发布用
/// `begin` + [`TempPublish::set_unix_mode`]）。
pub fn publish_stream<F>(path: &Path, mode: PublishMode, mut fill: F) -> Result<(), PublishError>
where
    F: FnMut(&mut File) -> Result<(), PublishError>,
{
    if mode == PublishMode::Idempotent {
        return Err(PublishError::Conflict(format!(
            "流式发布不做内容比较，请改用 CreateNew/Overwrite: {}",
            path.display()
        )));
    }
    let (session, mut file) = TempPublish::begin(path, mode)?;
    let written = (|| {
        fill(&mut file)?;
        file.sync_all().map_err(|error| {
            PublishError::Io(io_message_error("同步临时产物", &session.temporary, &error))
        })?;
        Ok::<(), PublishError>(())
    })();
    drop(file);
    finish(session, written)
}

/// `commit`/放弃的公共收尾：写成功才发布；任一环节失败都显式放弃（Drop 兜底同类清理，
/// 但在失败路径上写出来，读代码的人才不用靠「记得 Drop 会删文件」推断发布没有发生）。
fn finish(session: TempPublish, written: Result<(), PublishError>) -> Result<(), PublishError> {
    match written {
        Ok(()) => session.commit(),
        Err(error) => {
            session.abort();
            Err(error)
        }
    }
}

fn read_existing(path: &Path) -> Result<String, PublishError> {
    std::fs::read_to_string(path)
        .map_err(|error| PublishError::Io(io_message_error("读取已有产物", path, &error)))
}

fn temporary_path(target: &Path) -> PathBuf {
    let serial = TEMP_SERIAL.fetch_add(1, Ordering::Relaxed);
    target.with_extension(format!("tmp-{}-{serial}", std::process::id()))
}

/// 把 io 失败连同类别一起收进 `PublishError::Io`：文案带上路径，`kind` 原样留给
/// 调用方归类（见 `PublishError::Io` 的注释）。
fn io_message_error(label: &str, path: &Path, error: &io::Error) -> io::Error {
    io::Error::new(
        error.kind(),
        format!("{label}失败 {}: {error}", path.display()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(label: &str) -> PathBuf {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("target")
            .join("qx-app-test-tmp")
            .join(format!("qx-sink-{label}-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(&root).expect("建测试目录");
        root
    }

    /// 盘上残留的 `.tmp-` 半成品名——发布失败却不清理，会在这里现形。
    fn leftovers(directory: &Path) -> Vec<String> {
        std::fs::read_dir(directory)
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .filter(|name| name.contains(".tmp-"))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn overwrite_creates_then_replaces_and_never_leaves_half_a_file() {
        let root = scratch("overwrite");
        let path = root.join("report.html");
        publish(&path, "<html>1</html>", PublishMode::Overwrite).expect("首写");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "<html>1</html>");
        publish(&path, "<html>2</html>", PublishMode::Overwrite).expect("整份重写");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "<html>2</html>");
        assert!(leftovers(&root).is_empty(), "发布后不留临时文件");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn idempotent_takes_same_facts_and_refuses_different_ones() {
        let root = scratch("idempotent");
        let path = root.join("summary.json");
        publish(&path, "{\"v\":1}", PublishMode::Idempotent).expect("首写");
        publish(&path, "{\"v\":1}", PublishMode::Idempotent).expect("同内容重跑必须幂等");
        let conflict = publish(&path, "{\"v\":2}", PublishMode::Idempotent).unwrap_err();
        assert!(
            matches!(conflict, PublishError::Conflict(_)),
            "内容不同即冲突，实测 {conflict:?}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"v\":1}");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn create_new_never_replaces_an_existing_artifact() {
        let root = scratch("create-new");
        let path = root.join("record.json");
        publish(&path, "first", PublishMode::CreateNew).expect("首写");
        let conflict = publish(&path, "second", PublishMode::CreateNew).unwrap_err();
        assert!(matches!(conflict, PublishError::Conflict(_)));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn aborted_publish_leaves_target_untouched_and_no_temp() {
        let root = scratch("abort");
        let path = root.join("bundle.zip");
        publish_bytes(&path, b"kept", PublishMode::Overwrite).expect("先落一份");
        let before = std::fs::read(&path).unwrap();
        {
            let (session, mut file) =
                TempPublish::begin(&path, PublishMode::Overwrite).expect("开临时文件");
            file.write_all(b"half").expect("只写半个");
            session.abort();
        }
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "放弃不影响已发布内容"
        );
        assert!(leftovers(&root).is_empty(), "放弃即清临时文件");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn bundle_publication_failure_restores_every_previous_target() {
        let root = scratch("bundle-rollback");
        let first = root.join("report.equity.svg");
        let second = root.join("report.fills.svg");
        std::fs::write(&first, b"old equity").unwrap();
        std::fs::write(&second, b"old fills").unwrap();
        let entries = vec![
            (first.clone(), b"new equity".to_vec()),
            (second.clone(), b"new fills".to_vec()),
        ];
        let error = publish_bundle_with(&entries, |index| {
            if index == 1 {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "injected failure",
                ))
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        assert!(
            matches!(error, PublishError::Io(ref error) if error.kind() == io::ErrorKind::PermissionDenied)
        );
        assert_eq!(std::fs::read(&first).unwrap(), b"old equity");
        assert_eq!(std::fs::read(&second).unwrap(), b"old fills");
        assert!(
            leftovers(&root).is_empty(),
            "failed bundle leaves no staged or backup files"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    fn seed_interrupted_bundle(root: &Path, state: &str, installed_count: usize) -> BundleJournal {
        std::fs::create_dir_all(root).unwrap();
        let root = root.canonicalize().unwrap();
        let transaction_id = format!("test-{}", std::process::id());
        let first = root.join("first.html");
        let second = root.join("second.svg");
        std::fs::write(&first, b"old first").unwrap();
        let targets = [first, second];
        let mut journal = BundleJournal {
            transaction_id: transaction_id.clone(),
            state: state.to_string(),
            entries: Vec::new(),
        };
        for (index, target) in targets.into_iter().enumerate() {
            let had_original = index == 0;
            let staged = bundle_path(&target, &transaction_id, index, "stage");
            let backup = bundle_path(&target, &transaction_id, index, "backup");
            std::fs::write(&staged, format!("new {index}")).unwrap();
            if had_original {
                std::fs::rename(&target, &backup).unwrap();
            }
            if index < installed_count {
                std::fs::rename(&staged, &target).unwrap();
            }
            journal.entries.push(BundleJournalEntry {
                target,
                staged,
                backup,
                had_original,
            });
        }
        let bytes = serde_json::to_vec(&journal).unwrap();
        publish_bytes(
            &journal_path(&root, &transaction_id),
            &bytes,
            PublishMode::CreateNew,
        )
        .unwrap();
        journal
    }

    #[test]
    fn interrupted_prepared_bundle_recovers_old_complete_set() {
        let root = scratch("bundle-crash-rollback");
        for installed_count in 0..=2 {
            let attempt = root.join(format!("prepared-{installed_count}"));
            let journal = seed_interrupted_bundle(&attempt, "prepared", installed_count);
            publish_bundle(&[(attempt.join("recovery-trigger.txt"), b"recovered".to_vec())])
                .unwrap();
            assert_eq!(
                std::fs::read(&journal.entries[0].target).unwrap(),
                b"old first",
                "restore original after {installed_count} installed outputs"
            );
            assert!(!journal.entries[1].target.exists());
            for entry in &journal.entries {
                assert!(!entry.staged.exists());
                assert!(!entry.backup.exists());
            }
            assert!(std::fs::read_dir(&attempt).unwrap().all(|entry| {
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".qx-report-txn-")
            }));
            std::fs::remove_dir_all(attempt).unwrap();
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn interrupted_committed_bundle_keeps_new_complete_set() {
        let root = scratch("bundle-crash-commit");
        let journal = seed_interrupted_bundle(&root, "committed", 2);
        publish_bundle(&[(root.join("recovery-trigger.txt"), b"recovered".to_vec())]).unwrap();
        assert_eq!(std::fs::read(&journal.entries[0].target).unwrap(), b"new 0");
        assert_eq!(std::fs::read(&journal.entries[1].target).unwrap(), b"new 1");
        for entry in &journal.entries {
            assert!(!entry.staged.exists());
            assert!(!entry.backup.exists());
        }
        assert!(std::fs::read_dir(&root).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".qx-report-txn-")
        }));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn failed_fill_publishes_nothing() {
        let root = scratch("failed-fill");
        let path = root.join("out.bin");
        let error = publish_stream(&path, PublishMode::Overwrite, |file| {
            file.write_all(b"half").map_err(|error| {
                PublishError::Io(io::Error::other(format!("写一半失败: {error}")))
            })?;
            Err(PublishError::Io(io::Error::other("故意失败")))
        })
        .unwrap_err();
        assert!(matches!(error, PublishError::Io(_)), "实测 {error:?}");
        assert!(!path.exists(), "半途失败不得发布");
        assert!(leftovers(&root).is_empty(), "失败即清临时文件");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn publish_stream_publishes_streamed_payload_but_refuses_idempotent() {
        let root = scratch("stream");
        let path = root.join("book.xlsx");
        publish_stream(&path, PublishMode::Overwrite, |file| {
            file.write_all(b"PK")
                .and_then(|()| file.sync_all())
                .map_err(|error| PublishError::Io(io::Error::other(format!("流式写失败: {error}"))))
        })
        .expect("流式发布");
        assert_eq!(std::fs::read(&path).unwrap(), b"PK");
        let conflict = publish_stream(&path, PublishMode::Idempotent, |_| Ok(())).unwrap_err();
        assert!(
            matches!(conflict, PublishError::Conflict(_)),
            "流式档不做内容比较，实测 {conflict:?}"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn two_publishes_in_one_process_get_distinct_temporaries() {
        let root = scratch("serial");
        let (first, _) =
            TempPublish::begin(&root.join("a"), PublishMode::CreateNew).expect("第一个临时文件");
        let (second, _) =
            TempPublish::begin(&root.join("b"), PublishMode::CreateNew).expect("第二个临时文件");
        assert_ne!(
            first.temporary, second.temporary,
            "同进程内两次发布不撞同一个临时路径"
        );
        assert_eq!(leftovers(&root).len(), 2);
        drop((first, second));
        assert!(leftovers(&root).is_empty(), "Drop 清理两颗临时文件");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn unix_mode_is_carried_onto_the_published_file() {
        use std::os::unix::fs::PermissionsExt;
        let root = scratch("mode");
        let path = root.join("tool.sh");
        let (session, _file) =
            TempPublish::begin(&path, PublishMode::CreateNew).expect("开临时文件");
        session.set_unix_mode(0o755).expect("设权限");
        assert_eq!(
            std::fs::metadata(&session.temporary)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        session.commit().expect("发布");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o755,
            "发布后的文件权限保留可执行位"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}
