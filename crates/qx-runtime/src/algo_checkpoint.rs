//! Durable, single-flight checkpoints for the shared TWAP/VWAP execution state.
//!
//! This store deliberately owns only checkpoint admission and serialized state
//! transitions. A host still supplies the venue, risk, and event-log ports and
//! schedules calls to `step`; persisting a checkpoint is not itself a Paper/Live
//! worker or a claim of external venue certification.

use qx_core::Fnv1a;
use qx_execution::{
    AlgoExecutionState, AlgoExecutionStep, ExecutionEventPort, ExecutionGateway, RiskPort,
};
use qx_storage::{JsonStateStore, StorageError};
use std::path::PathBuf;

#[cfg(test)]
mod tests;

#[derive(Clone, Debug)]
pub struct AlgoExecutionCheckpointStore {
    store: JsonStateStore,
}

impl AlgoExecutionCheckpointStore {
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
