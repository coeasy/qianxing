//! 文件状态存储的共同形状（V10 §4.9）。
//!
//! 四个文件状态存储（`JsonStateStore`、`FileConsumerStateStore`、`FileOutboxStore`、
//! `FileJobQueue`）此前各自抄写同一圈“列目录 / 删除记录”样板；真正互不相同的只有
//! 目录布局与记录类型。序列化、原子替换与读改写事务一律收敛到 crate 根唯一的
//! `state_envelope`（`read_state_json` / `write_state_json` / `write_atomic_path` 等），
//! 本 trait 只保留各存储共享、且不重复信封职责的两件事：目录枚举与记录删除。
//!
//! 语义纪律：删除口径沿用原 `std::fs::remove_file(...).map_err(Io)`，文件不存在时
//! 报 Io（与原站点逐字一致）；列目录按路径排序以获得确定性遍历顺序（结果侧本就有
//! 各自排序，故最终可观察顺序不变）。

use super::*;

pub(crate) trait JsonRecordStore {
    /// 存放记录文件的目录。
    fn records_dir(&self) -> PathBuf;

    /// 删除一条记录文件；文件不存在时报错，与原 `remove_file` 口径一致。
    fn delete_record(&self, path: &Path) -> Result<(), StorageError> {
        std::fs::remove_file(path).map_err(|error| StorageError::Io(error.to_string()))
    }

    /// 按确定性顺序列出 records_dir 下的 `.json` 记录文件；目录缺失视为空集合。
    fn list_records(&self) -> Result<Vec<PathBuf>, StorageError> {
        list_json_files(&self.records_dir())
    }
}

/// 共享的目录扫描：只保留 `.json` 文件并按路径排序，目录不存在视为空。
///
/// 记录目录之外的集合（如 job lease 目录）直接复用这个自由函数。
pub(crate) fn list_json_files(dir: &Path) -> Result<Vec<PathBuf>, StorageError> {
    let mut paths = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(paths),
        Err(error) => return Err(StorageError::Io(error.to_string())),
    };
    for entry in entries {
        let path = entry
            .map_err(|error| StorageError::Io(error.to_string()))?
            .path();
        if path.extension().and_then(|value| value.to_str()) == Some("json") {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}
