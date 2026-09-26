//! 文件后端的追加式哈希审计链（V11 R5-2）。
//!
//! 同目录另外四本记录存储都走「整段 JSON + 临时文件原子替换」。这一本刻意不走：审计链
//! 的写入频率与控制面命令同量级（每条命令至少两笔），每次追加重写整段等于把 R4-7 那颗
//! 「只追加、没有保留策略」的病换成「每次写全量」的病。落盘形状因此是一行一条
//! [`AuditEntry`] 的 JSONL：追加只发生在末尾，读取只认以换行收尾的完整行，崩溃留下的
//! 半行按定义从未被任何状态引用过，下一次追加把它截掉。

use super::*;
use std::io::{Read, Seek, SeekFrom};

/// 追加与截断只回看这段字节：链尾自洽在这里保证，整条链的校验留在冷读侧。
const AUDIT_TAIL_WINDOW_BYTES: u64 = 65_536;

#[derive(Clone, Debug)]
pub struct AuditFileStore {
    root: PathBuf,
}

/// 尾部窗口一次读出的三格：最后一条已提交记录、已提交前缀长度、窗口内每行的落点。
struct AuditTail {
    last: Option<AuditEntry>,
    committed_bytes: u64,
    lines: Vec<(u64, u64)>,
}

impl AuditFileStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// 一次追加本轮事务的全部流水：整批只 fsync 一次，但仍然是"链先 durable"。
    fn append_entries_unlocked(&self, entries: &[AuditEntry]) -> Result<(), StorageError> {
        if entries.is_empty() {
            return Ok(());
        }
        let mut file = open_audit_write(&self.path())?;
        let tail = read_audit_tail(&file)?;
        // 半行不在 `drop_from` 的管辖内（它没有序号，链尾比对看不见它），所以这里按
        // 已提交前缀收一次口：落盘的字节数就是链的长度，不留崩溃前的残料。
        if file_size(&file)? != tail.committed_bytes {
            file.set_len(tail.committed_bytes).map_err(audit_io)?;
        }
        write_audit_entries(&mut file, entries, tail.committed_bytes)
    }

    pub(crate) fn read(&self) -> Result<Vec<AuditEntry>, StorageError> {
        let content = match std::fs::read_to_string(self.path()) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(StorageError::Io(error.to_string())),
        };
        let mut entries = Vec::new();
        for line in complete_lines(&content) {
            entries.push(
                serde_json::from_str(line)
                    .map_err(|error| StorageError::Io(format!("审计 JSON 非法: {error}")))?,
            );
        }
        validate_audit_chain(&entries)?;
        Ok(entries)
    }

    pub(crate) fn tail(&self) -> Result<Option<AuditEntry>, StorageError> {
        let Ok(file) = std::fs::File::open(self.path()) else {
            return Ok(None);
        };
        read_audit_tail(&file).map(|tail| tail.last)
    }

    /// 丢掉 `sequence` 及其之后的行：控制面状态没落盘，上一笔事务留在链上的就不算数。
    ///
    /// 只有拿着 `audit.append.lock` 的写入器能调它（`FileChainWriter::drop_from`）：再导出
    /// 一个"自己回来抢一次锁"的公开入口，就是给同一条链第二个截断者（V11 R5-2）。
    fn truncate_from_unlocked(&self, sequence: u64) -> Result<(), StorageError> {
        let file = open_audit_write(&self.path())?;
        let tail = read_audit_tail(&file)?;
        if tail
            .last
            .as_ref()
            .is_none_or(|last| last.sequence < sequence)
        {
            return Ok(());
        }
        let cut = if sequence == 0 {
            0
        } else {
            tail.lines
                .iter()
                .find(|(_, line_sequence)| *line_sequence >= sequence)
                .map(|(offset, _)| *offset)
                .ok_or_else(|| {
                    StorageError::Conflict(format!(
                        "审计链残尾起点不在 {AUDIT_TAIL_WINDOW_BYTES} 字节尾部窗口内，拒绝截断"
                    ))
                })?
        };
        file.set_len(cut).map_err(audit_io)?;
        file.sync_all().map_err(audit_io)?;
        Ok(())
    }

    fn path(&self) -> PathBuf {
        self.root.join("audit.jsonl")
    }
}

impl AuditStore for AuditFileStore {
    fn read_entries(&self) -> Result<Vec<AuditEntry>, StorageError> {
        self.read()
    }
}

fn audit_io(error: std::io::Error) -> StorageError {
    StorageError::Io(error.to_string())
}

/// 把已经算好序号与摘要的条目写到已提交前缀之后。
///
/// 落盘顺序是审计链先 durable、控制面状态后 durable：反过来才会造出"状态引用了一条
/// 没落盘的记录"，那种洞截不掉，只能靠人。
fn write_audit_entries(
    file: &mut std::fs::File,
    entries: &[AuditEntry],
    committed_bytes: u64,
) -> Result<(), StorageError> {
    let mut payload = String::new();
    for entry in entries {
        let line = serde_json::to_string(entry)
            .map_err(|error| StorageError::Io(format!("审计序列化失败: {error}")))?;
        payload.push_str(&line);
        payload.push('\n');
    }
    file.seek(SeekFrom::Start(committed_bytes))
        .map_err(audit_io)?;
    file.write_all(payload.as_bytes()).map_err(audit_io)?;
    file.sync_all().map_err(audit_io)
}

/// 控制面事务的审计链写入器。
///
/// 它在构造时就拿走 `audit.append.lock` 并一直握到本轮事务结束：截残尾、比对链尾、
/// 追加必须是一段不被插队的动作，分三次加锁会让两个写入者各自只看见半条链。
pub(crate) struct FileChainWriter {
    store: AuditFileStore,
    _lock: FileLock,
}

impl FileChainWriter {
    pub(crate) fn lock(root: &Path) -> Result<Self, StorageError> {
        std::fs::create_dir_all(root).map_err(|error| StorageError::Io(error.to_string()))?;
        let _lock = acquire_storage_lock(root.join("audit.append.lock"))?;
        Ok(Self {
            store: AuditFileStore::new(root),
            _lock,
        })
    }
}

impl AuditChainWriter for FileChainWriter {
    fn tail(&mut self) -> Result<Option<AuditEntry>, StorageError> {
        self.store.tail()
    }

    fn drop_from(&mut self, sequence: u64) -> Result<(), StorageError> {
        self.store.truncate_from_unlocked(sequence)
    }

    fn append_entries(&mut self, entries: &[AuditEntry]) -> Result<(), StorageError> {
        self.store.append_entries_unlocked(entries)
    }
}

fn open_audit_write(path: &Path) -> Result<std::fs::File, StorageError> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(audit_io)
}

fn file_size(file: &std::fs::File) -> Result<u64, StorageError> {
    file.metadata().map_err(audit_io).map(|meta| meta.len())
}

/// 只交出以换行收尾的行；末尾不带换行的一段是崩溃留下的半行，按未提交处理。
fn complete_lines(content: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut offset = 0;
    while let Some(relative) = content[offset..].find('\n') {
        lines.push(&content[offset..offset + relative]);
        offset += relative + 1;
    }
    lines
}

/// 有界地读出链尾：只回看尾部窗口，窗口内逐条复算摘要并核对序号与前置摘要。
fn read_audit_tail(file: &std::fs::File) -> Result<AuditTail, StorageError> {
    let size = file_size(file)?;
    if size == 0 {
        return Ok(AuditTail {
            last: None,
            committed_bytes: 0,
            lines: Vec::new(),
        });
    }
    let window = size.min(AUDIT_TAIL_WINDOW_BYTES);
    let start = size - window;
    let mut bytes = vec![0_u8; window as usize];
    let mut probe = file.try_clone().map_err(audit_io)?;
    probe.seek(SeekFrom::Start(start)).map_err(audit_io)?;
    probe.read_exact(&mut bytes).map_err(audit_io)?;
    let text = String::from_utf8(bytes)
        .map_err(|_| StorageError::Conflict("审计链尾部不是合法 UTF-8".to_string()))?;
    let Some(last_newline) = text.rfind('\n') else {
        if start > 0 {
            return Err(StorageError::Conflict(format!(
                "审计链尾部单行超过 {AUDIT_TAIL_WINDOW_BYTES} 字节窗口，无法确定已提交前缀"
            )));
        }
        return Ok(AuditTail {
            last: None,
            committed_bytes: 0,
            lines: Vec::new(),
        });
    };
    let committed_bytes = start + last_newline as u64 + 1;
    let mut chunks = Vec::new();
    let mut cursor = start;
    for (index, chunk) in text[..last_newline].split('\n').enumerate() {
        let absolute = cursor;
        cursor += chunk.len() as u64 + 1;
        if start > 0 && index == 0 {
            // 窗口起点落在某一行中间，这一段不是完整行，也不算残尾。
            continue;
        }
        chunks.push((absolute, chunk));
    }
    let mut last: Option<AuditEntry> = None;
    let mut lines = Vec::new();
    for (absolute, chunk) in chunks {
        let entry: AuditEntry = serde_json::from_str(chunk)
            .map_err(|error| StorageError::Io(format!("审计 JSON 非法: {error}")))?;
        if let Some(previous) = &last {
            if entry.sequence != previous.sequence + 1 || entry.previous_hash != previous.entry_hash
            {
                return Err(StorageError::Conflict(format!(
                    "审计链序号或前置摘要非法: expected {}",
                    previous.sequence + 1
                )));
            }
        }
        let expected = audit_entry_hash(entry.sequence, entry.previous_hash, &entry.record);
        if entry.entry_hash != expected {
            return Err(StorageError::Conflict(format!(
                "审计记录 {} 摘要不一致",
                entry.sequence
            )));
        }
        lines.push((absolute, entry.sequence));
        last = Some(entry);
    }
    Ok(AuditTail {
        last,
        committed_bytes,
        lines,
    })
}
