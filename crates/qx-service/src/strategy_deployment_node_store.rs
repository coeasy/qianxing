//! Durable, bounded receipt for authenticated remote strategy deployment snapshots.
use qx_api::{
    aggregate_strategy_deployment_nodes, ReceivedStrategyDeploymentNodeSnapshotV1,
    RegisteredStrategyDeploymentNodeV1, StrategyDeploymentNodeSnapshotV1,
    StrategyDeploymentNodeStatusV1,
};
use qx_storage::{JsonStateStore, StorageError};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const SNAPSHOT_STATE_PATH: &str = "strategy-deployment-nodes/snapshots-v1.json";
const MAX_SNAPSHOT_STATE_BYTES: usize = 64 * 1024 * 1024;
const MAX_REGISTERED_NODES: usize = 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StrategyDeploymentNodeSnapshotStateV1 {
    schema_version: u32,
    snapshots: Vec<ReceivedStrategyDeploymentNodeSnapshotV1>,
}

impl Default for StrategyDeploymentNodeSnapshotStateV1 {
    fn default() -> Self {
        Self {
            schema_version: 1,
            snapshots: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct StrategyDeploymentNodeSnapshotStore {
    data_root: PathBuf,
    json: JsonStateStore,
}

impl StrategyDeploymentNodeSnapshotStore {
    pub fn new(data_root: impl Into<PathBuf>) -> Self {
        let data_root = data_root.into();
        Self {
            json: JsonStateStore::new(data_root.clone()),
            data_root,
        }
    }

    pub fn read(
        &self,
        registered_nodes: &[RegisteredStrategyDeploymentNodeV1],
        now_ms: u64,
    ) -> Result<Vec<StrategyDeploymentNodeStatusV1>, String> {
        validate_registered_node_count(registered_nodes)?;
        let path = self.data_root.join(SNAPSHOT_STATE_PATH);
        let state = if path.exists() {
            self.json
                .load_json_at::<StrategyDeploymentNodeSnapshotStateV1>(SNAPSHOT_STATE_PATH)
                .map_err(|error| format!("读取节点部署快照失败: {error:?}"))?
        } else {
            StrategyDeploymentNodeSnapshotStateV1::default()
        };
        validate_state(&state)?;
        let registered = registered_nodes
            .iter()
            .map(|node| node.node_id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        let snapshots = state
            .snapshots
            .into_iter()
            .filter(|snapshot| registered.contains(snapshot.node_id.as_str()))
            .collect::<Vec<_>>();
        aggregate_strategy_deployment_nodes(registered_nodes, &snapshots, now_ms)
    }

    pub fn accept(
        &self,
        registered_nodes: &[RegisteredStrategyDeploymentNodeV1],
        authenticated_operator_id: &str,
        snapshot: StrategyDeploymentNodeSnapshotV1,
        received_at_ms: u64,
    ) -> Result<(), String> {
        validate_registered_node_count(registered_nodes)?;
        let Some(registration) = registered_nodes
            .iter()
            .find(|node| node.authenticated_operator_id == authenticated_operator_id)
        else {
            return Err(format!("unregistered_node:{authenticated_operator_id}"));
        };
        if received_at_ms == 0
            || snapshot.schema_version != 1
            || snapshot.generated_at_ms == 0
            || snapshot.generated_at_ms > received_at_ms
            || snapshot.deployments.len() > 1024
        {
            return Err(
                "invalid_snapshot: node snapshot identity, schema, time, or size is invalid".into(),
            );
        }
        let incoming = ReceivedStrategyDeploymentNodeSnapshotV1 {
            node_id: registration.node_id.clone(),
            authenticated_operator_id: authenticated_operator_id.to_owned(),
            received_at_ms,
            snapshot,
        };
        self.ensure_state_exists()?;
        let (_, result) = self
            .json
            .transact_json_at::<StrategyDeploymentNodeSnapshotStateV1, (), String, _>(
                SNAPSHOT_STATE_PATH,
                |state| {
                    if state.schema_version != 1 || state.snapshots.len() > MAX_REGISTERED_NODES {
                        return Err("invalid_snapshot: persisted node snapshot state is invalid or over limit".into());
                    }
                    let active_nodes = registered_nodes.iter().map(|node| node.node_id.as_str()).collect::<std::collections::BTreeSet<_>>();
                    state.snapshots.retain(|saved| active_nodes.contains(saved.node_id.as_str()));
                    if let Some(existing) = state.snapshots.iter_mut().find(|saved| saved.node_id == incoming.node_id) {
                        if existing.snapshot.generated_at_ms > incoming.snapshot.generated_at_ms {
                            return Err(format!("stale_snapshot:{}", incoming.node_id));
                        }
                        if existing.snapshot.generated_at_ms == incoming.snapshot.generated_at_ms {
                            let previous = serde_json::to_value(&existing.snapshot).map_err(|error| format!("invalid_snapshot: cannot compare saved node snapshot: {error}"))?;
                            let next = serde_json::to_value(&incoming.snapshot).map_err(|error| format!("invalid_snapshot: cannot encode node snapshot: {error}"))?;
                            if previous != next {
                                return Err(format!("invalid_snapshot: node {} reused a timestamp for different contents", incoming.node_id));
                            }
                            existing.received_at_ms = incoming.received_at_ms;
                        } else {
                            *existing = incoming.clone();
                        }
                    } else {
                        state.snapshots.push(incoming.clone());
                    }
                    aggregate_strategy_deployment_nodes(registered_nodes, &state.snapshots, received_at_ms)
                        .map_err(|error| format!("invalid_snapshot: {error}"))?;
                    let bytes = serde_json::to_vec(state).map_err(|error| format!("invalid_snapshot: cannot encode node snapshot state: {error}"))?;
                    if bytes.len() > MAX_SNAPSHOT_STATE_BYTES {
                        return Err("invalid_snapshot: persisted node snapshots exceed 64 MiB".into());
                    }
                    Ok(())
                },
            )
            .map_err(|error| format!("持久化节点部署快照失败: {error:?}"))?;
        result
    }

    fn ensure_state_exists(&self) -> Result<(), String> {
        let path = self.data_root.join(SNAPSHOT_STATE_PATH);
        if path.exists() {
            return Ok(());
        }
        match self.json.create_json_at(
            SNAPSHOT_STATE_PATH,
            &StrategyDeploymentNodeSnapshotStateV1::default(),
        ) {
            Ok(_) => Ok(()),
            Err(StorageError::Conflict(_)) if path.exists() => Ok(()),
            Err(error) => Err(format!("初始化节点部署快照状态失败: {error:?}")),
        }
    }
}

fn validate_registered_node_count(
    registered_nodes: &[RegisteredStrategyDeploymentNodeV1],
) -> Result<(), String> {
    if registered_nodes.len() > MAX_REGISTERED_NODES {
        return Err("registered strategy deployment node count exceeds 1024".into());
    }
    Ok(())
}

fn validate_state(state: &StrategyDeploymentNodeSnapshotStateV1) -> Result<(), String> {
    if state.schema_version != 1 || state.snapshots.len() > MAX_REGISTERED_NODES {
        return Err(
            "persisted strategy deployment node state is invalid or exceeds 1024 nodes".into(),
        );
    }
    let bytes = serde_json::to_vec(state)
        .map_err(|error| format!("cannot size node snapshot state: {error}"))?;
    if bytes.len() > MAX_SNAPSHOT_STATE_BYTES {
        return Err("persisted strategy deployment node state exceeds 64 MiB".into());
    }
    Ok(())
}
