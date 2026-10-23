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
