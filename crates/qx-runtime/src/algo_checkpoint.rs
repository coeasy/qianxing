//! Durable, single-flight checkpoints for the shared TWAP/VWAP execution state.
//!
//! This store deliberately owns only checkpoint admission and serialized state
//! transitions. A host still supplies the venue, risk, and event-log ports and
//! schedules calls to `step`; persisting a checkpoint is not itself a Paper/Live
//! worker or a claim of external venue certification.

use qx_core::Fnv1a;
use qx_execution::{
    AlgoExecutionState, AlgoExecutionStatus, AlgoExecutionStep, ExecutionEventPort,
    ExecutionGateway, RiskPort,
};
use qx_storage::{JsonStateStore, StorageError};
use std::path::{Path, PathBuf};

#[cfg(test)]
mod tests;

#[derive(Clone, Debug)]
pub struct AlgoExecutionCheckpointStore {
    store: JsonStateStore,
}

impl AlgoExecutionCheckpointStore {
    /// Hard ceiling for one checkpoint directory. Larger installations must
    /// shard stores rather than turn worker discovery into an unbounded scan.
    pub const MAX_DISCOVERY_ENTRIES: usize = 100_000;

    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            store: JsonStateStore::new(root),
        }
    }

    /// Persist a new parent checkpoint without allowing an existing parent ID to
    /// be silently replaced.
    pub fn create(&self, state: &AlgoExecutionState) -> Result<(), String> {
        state.to_json()?;
        let path = checkpoint_path(&state.parent_id)?;
        self.store
            .create_json_at(path, state)
            .map(|_| ())
            .map_err(storage_error)
    }

    /// Load and validate a checkpoint, including its full parent ID. The file
    /// name is a stable hash for bounded path length; the stored ID detects a
    /// theoretical hash collision and fails closed.
    pub fn load(&self, parent_id: &str) -> Result<AlgoExecutionState, String> {
        let path = checkpoint_path(parent_id)?;
        let state: AlgoExecutionState = self.store.load_json_at(path).map_err(storage_error)?;
        if state.parent_id != parent_id {
            return Err("算法父单 checkpoint 身份与请求不匹配".into());
        }
        state.to_json()?;
        Ok(state)
    }

    /// Read a checkpoint without conflating absence with corrupt or inaccessible storage.
    pub fn load_optional(&self, parent_id: &str) -> Result<Option<AlgoExecutionState>, String> {
        let relative = checkpoint_path(parent_id)?;
        let path = self.store.root().join(relative);
        match std::fs::metadata(&path) {
            Ok(_) => {
                let state = read_checkpoint(&path)?;
                if state.parent_id != parent_id {
                    return Err("算法父单 checkpoint 身份与请求不匹配".into());
                }
                Ok(Some(state))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!("检查算法 checkpoint 是否存在失败: {error}")),
        }
    }

    /// List active checkpoints in parent-id order, after an exclusive cursor.
    /// This lets supervised workers page durable work across restarts without
    /// retaining an in-memory-only registry or starving later parents.
    pub fn list_active(
        &self,
        after_parent_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<AlgoExecutionState>, String> {
        if limit == 0 || limit > Self::MAX_DISCOVERY_ENTRIES {
            return Err(format!(
                "算法 checkpoint 扫描上限必须在 1..={} 之间",
                Self::MAX_DISCOVERY_ENTRIES
            ));
        }
        let directory = self.store.root().join("algo-execution");
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(format!("读取算法 checkpoint 目录失败: {error}")),
        };
        let mut paths = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| format!("枚举算法 checkpoint 目录失败: {error}"))?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|error| format!("读取算法 checkpoint 文件类型失败: {error}"))?;
            if !file_type.is_file() || path.extension().map_or(true, |ext| ext != "json") {
                continue;
            }
            if paths.len() == Self::MAX_DISCOVERY_ENTRIES {
                return Err(format!(
                    "算法 checkpoint 数量超过安全扫描上限 {}",
                    Self::MAX_DISCOVERY_ENTRIES
                ));
            }
            paths.push(path);
        }
        paths.sort();
        let mut active = Vec::new();
        for path in paths {
            let state = read_checkpoint(&path)?;
            if !is_terminal(state.status)
                && after_parent_id.map_or(true, |cursor| state.parent_id.as_str() > cursor)
            {
                active.push(state);
            }
        }
        active.sort_by(|left, right| left.parent_id.cmp(&right.parent_id));
        active.truncate(limit);
        Ok(active)
    }

    /// Serialize a bounded execution step with all other checkpoint mutations.
    /// The checkpoint is atomically saved only when the shared kernel returns a
    /// step result; stable child IDs and OMS lookup make a retry after an unknown
    /// process outcome reconcile instead of blindly resubmit.
    pub fn step<V, P, R>(
        &self,
        parent_id: &str,
        now: u64,
        gateway: &mut ExecutionGateway<'_, V, P>,
        risk: &R,
    ) -> Result<AlgoExecutionStep, String>
    where
        V: qx_execution::VenuePort,
        P: ExecutionEventPort,
        R: RiskPort,
    {
        let path = checkpoint_path(parent_id)?;
        let (_, result) = self
            .store
            .transact_json_at(path, |state: &mut AlgoExecutionState| {
                if state.parent_id != parent_id {
                    return Err("算法父单 checkpoint 身份与请求不匹配".to_string());
                }
                state.step(now, gateway, risk)
            })
            .map_err(storage_error)?;
        result
    }

    /// Persist cancellation intent through the same single-writer transaction.
    pub fn request_cancel(&self, parent_id: &str) -> Result<AlgoExecutionState, String> {
        let path = checkpoint_path(parent_id)?;
        let (state, result) = self
            .store
            .transact_json_at(path, |state: &mut AlgoExecutionState| {
                if state.parent_id != parent_id {
                    return Err("算法父单 checkpoint 身份与请求不匹配".to_string());
                }
                state.request_cancel()
            })
            .map_err(storage_error)?;
        result?;
        Ok(state)
    }
}

fn read_checkpoint(path: &Path) -> Result<AlgoExecutionState, String> {
    let payload = std::fs::read_to_string(path)
        .map_err(|error| format!("读取算法 checkpoint 失败: {error}"))?;
    let state = AlgoExecutionState::from_json(&payload)?;
    let expected = checkpoint_path(&state.parent_id)?;
    if path.file_name().and_then(|name| name.to_str())
        != Path::new(&expected)
            .file_name()
            .and_then(|name| name.to_str())
    {
        return Err("算法 checkpoint 文件名与父单身份不匹配".into());
    }
    Ok(state)
}

fn is_terminal(status: AlgoExecutionStatus) -> bool {
    matches!(
        status,
        AlgoExecutionStatus::Completed
            | AlgoExecutionStatus::Cancelled
            | AlgoExecutionStatus::Failed
            | AlgoExecutionStatus::ReconcileRequired
    )
}

fn checkpoint_path(parent_id: &str) -> Result<String, String> {
    if parent_id.trim().is_empty() || parent_id.len() > 4096 {
        return Err("算法父单 ID 为空或超过 4096 字节".into());
    }
    let mut hash = Fnv1a::new();
    hash.write_text(parent_id);
    Ok(format!("algo-execution/{:016x}.json", hash.finish()))
}

fn storage_error(error: StorageError) -> String {
    format!("算法父单 checkpoint 存储失败: {error:?}")
}
