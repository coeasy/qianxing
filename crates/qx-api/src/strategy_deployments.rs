//! Observed strategy process state. This read model never grants trading authority.
use crate::*;
use qx_spec::{StrategyDeploymentOperationV1, StrategyDeploymentRevisionV1};
use serde::Serialize;

pub(crate) type DeploymentRevisionCreator = Arc<
    dyn Fn(StrategyDeploymentRevisionV1) -> Result<StrategyDeploymentRevisionV1, String>
        + Send
        + Sync,
>;
pub(crate) type DeploymentOperationAcceptor = Arc<
    dyn Fn(StrategyDeploymentOperationV1) -> Result<StrategyDeploymentOperationV1, String>
        + Send
        + Sync,
>;
pub(crate) type DeploymentOperationReader =
    Arc<dyn Fn(&str) -> Result<Option<StrategyDeploymentOperationV1>, String> + Send + Sync>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateDeploymentRevisionRequest {
    worker_id: String,
    revision: u64,
    bundle_sha256: String,
    plan_sha256: String,
    runtime_config_json: String,
    desired_running: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CreateDeploymentOperationRequest {
    operation_id: String,
    idempotency_key: String,
    worker_id: String,
    action: qx_spec::StrategyDeploymentActionV1,
    target_revision: u64,
}

#[derive(Serialize)]
struct DeploymentOperationRequestIdentity<'a> {
    operation_id: &'a str,
    idempotency_key: &'a str,
    worker_id: &'a str,
    action: qx_spec::StrategyDeploymentActionV1,
    target_revision: u64,
}

impl ApiService {
    pub fn with_strategy_deployment_management(
        mut self,
        create_revision: impl Fn(StrategyDeploymentRevisionV1) -> Result<StrategyDeploymentRevisionV1, String>
            + Send
            + Sync
            + 'static,
        accept_operation: impl Fn(StrategyDeploymentOperationV1) -> Result<StrategyDeploymentOperationV1, String>
            + Send
            + Sync
            + 'static,
        read_operation: impl Fn(&str) -> Result<Option<StrategyDeploymentOperationV1>, String>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        self.deployment_revision_creator = Some(Arc::new(create_revision));
        self.deployment_operation_acceptor = Some(Arc::new(accept_operation));
        self.deployment_operation_reader = Some(Arc::new(read_operation));
        self
    }

    pub(crate) fn create_deployment_revision_response(
        &self,
        body: &str,
        timestamp: u64,
        operator: Option<&str>,
    ) -> ApiResponse {
        let Some(create) = &self.deployment_revision_creator else {
            return ApiResponse::json(
                503,
                error_json("strategy_deployment_management_unavailable"),
            );
        };
        let request: CreateDeploymentRevisionRequest = match serde_json::from_str(body) {
            Ok(request) => request,
            Err(error) => return ApiResponse::json(400, error_json(&error.to_string())),
        };
        let Some(created_by) = operator.filter(|value| !value.trim().is_empty()) else {
            return ApiResponse::json(403, error_json("authenticated_operator_required"));
        };
        let revision = StrategyDeploymentRevisionV1 {
            schema_version: qx_spec::STRATEGY_DEPLOYMENT_REVISION_SCHEMA_VERSION,
            kind: "qianxing_strategy_deployment_revision".into(),
            worker_id: request.worker_id,
            revision: request.revision,
            bundle_sha256: request.bundle_sha256,
            plan_sha256: request.plan_sha256,
            bundle_file_name: None,
            runtime_config_sha256: Some(qx_data::sha256_hex_bytes(
                request.runtime_config_json.as_bytes(),
            )),
            runtime_config_json: Some(request.runtime_config_json),
            dependency_tree: None,
            desired_running: request.desired_running,
            created_at_ms: timestamp,
            created_by: created_by.into(),
        };
        if let Err(error) = revision.validate() {
            return ApiResponse::json(400, error_json(&error));
        }
        match create(revision) {
            Ok(revision) => serialized_json_response(201, &revision),
            Err(error) => deployment_write_error(&error),
        }
    }

    pub(crate) fn create_deployment_operation_response(
        &self,
        body: &str,
        timestamp: u64,
        operator: Option<&str>,
    ) -> ApiResponse {
        let Some(accept) = &self.deployment_operation_acceptor else {
            return ApiResponse::json(
                503,
                error_json("strategy_deployment_management_unavailable"),
            );
        };
        let request: CreateDeploymentOperationRequest = match serde_json::from_str(body) {
            Ok(request) => request,
            Err(error) => return ApiResponse::json(400, error_json(&error.to_string())),
        };
        let Some(requested_by) = operator.filter(|value| !value.trim().is_empty()) else {
            return ApiResponse::json(403, error_json("authenticated_operator_required"));
        };
        let identity = DeploymentOperationRequestIdentity {
            operation_id: &request.operation_id,
            idempotency_key: &request.idempotency_key,
            worker_id: &request.worker_id,
            action: request.action,
            target_revision: request.target_revision,
        };
        let payload = match serde_json::to_vec(&identity) {
            Ok(payload) => payload,
            Err(error) => return ApiResponse::json(500, error_json(&error.to_string())),
        };
        let request_sha256 = qx_data::sha256_hex_bytes(&payload);
        let operation = StrategyDeploymentOperationV1 {
            schema_version: qx_spec::STRATEGY_DEPLOYMENT_OPERATION_SCHEMA_VERSION,
            kind: "qianxing_strategy_deployment_operation".into(),
            operation_id: request.operation_id,
            idempotency_key: request.idempotency_key,
            request_sha256,
            worker_id: request.worker_id,
            action: request.action,
            target_revision: request.target_revision,
            requested_at_ms: timestamp,
            requested_by: requested_by.into(),
            state: qx_spec::StrategyDeploymentOperationStateV1::Accepted,
            accepted_at_ms: timestamp,
            completed_at_ms: None,
            error_code: None,
            error_message: None,
        };
        if let Err(error) = operation.validate() {
            return ApiResponse::json(400, error_json(&error));
        }
        match accept(operation) {
            Ok(operation) => serialized_json_response(
                if operation.state.is_terminal() {
                    200
                } else {
                    202
                },
                &operation,
            ),
            Err(error) => deployment_write_error(&error),
        }
    }

    pub(crate) fn deployment_operation_response(&self, query: &str) -> ApiResponse {
        let operation_id = match query_param(query, "operation_id") {
            Ok(Some(id)) if id.trim().is_empty() => {
                return ApiResponse::json(400, error_json("operation_id must be non-empty"));
            }
            Ok(Some(id)) => id,
            Ok(None) => return ApiResponse::json(400, error_json("operation_id is required")),
            Err(error) => return ApiResponse::json(400, error_json(error)),
        };
        let Some(read) = &self.deployment_operation_reader else {
            return ApiResponse::json(
                503,
                error_json("strategy_deployment_management_unavailable"),
            );
        };
        match read(&operation_id) {
            Ok(Some(operation)) => serialized_json_response(200, &operation),
            Ok(None) => {
                ApiResponse::json(404, error_json("strategy_deployment_operation_not_found"))
            }
            Err(error) => ApiResponse::json(
                503,
                error_json(&format!(
                    "strategy_deployment_operation_unavailable: {error}"
                )),
            ),
        }
    }
}

fn deployment_write_error(error: &str) -> ApiResponse {
    if error.contains("invalid deployment revision payload")
        || error.contains("invalid deployment revision bundle")
    {
        return ApiResponse::json(400, error_json(error));
    }
    if error.contains("is not configured") {
        return ApiResponse::json(404, error_json(error));
    }
    if error.contains("conflict")
        || error.contains("already exists")
        || error.contains("idempotency key")
        || error.contains("sequence")
        || error.contains("does not exist")
    {
        ApiResponse::json(409, error_json(error))
    } else {
        ApiResponse::json(
            503,
            error_json(&format!("strategy_deployment_unavailable: {error}")),
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StrategyDeploymentStatusV1 {
    pub schema_version: u32,
    pub worker_id: String,
    pub strategy_id: Option<String>,
    pub strategy_version: String,
    pub account_id: Option<String>,
    pub venue_id: Option<String>,
    pub enabled: bool,
    pub state: String,
    pub observed_at_ms: Option<u64>,
    pub observation: Option<StrategyWorkerObservationV3>,
    pub active_owner_ids: Vec<String>,
    /// None means actual trading authorization has not been evaluated.
    pub trading_enabled: Option<bool>,
    pub blocking_reasons: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desired_revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desired_running: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_operation: Option<StrategyDeploymentOperationV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reconciliation: Option<StrategyDeploymentReconciliationV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operation_history: Vec<StrategyDeploymentOperationV1>,
}

/// Read-only account/venue evidence used by live admission. A clean report does not
/// grant trading authorization; `trading_enabled` remains a separate decision.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StrategyDeploymentReconciliationV1 {
    pub schema_version: u32,
    pub state: String,
    pub matching_report_count: usize,
    pub fresh_report_count: usize,
    pub observed_at_ms: Option<u64>,
    pub freshest_age_ms: Option<u64>,
    pub blocking_reasons: Vec<String>,
}

/// A node identity that the aggregator explicitly trusts for deployment observations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegisteredStrategyDeploymentNodeV1 {
    pub node_id: String,
    pub authenticated_operator_id: String,
    pub freshness_window_ms: u64,
}

/// A node's complete local projection. Authentication identity is supplied by the transport
/// layer and is deliberately not accepted from this payload.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyDeploymentNodeSnapshotV1 {
    pub schema_version: u32,
    pub generated_at_ms: u64,
    pub deployments: Vec<StrategyDeploymentStatusV1>,
}

/// One authenticated node's received snapshot. The node scope remains a nested boundary; the
/// aggregator never flattens workers into a synthetic shared ownership view.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceivedStrategyDeploymentNodeSnapshotV1 {
    pub node_id: String,
    pub authenticated_operator_id: String,
    pub received_at_ms: u64,
    pub snapshot: StrategyDeploymentNodeSnapshotV1,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyDeploymentNodeFreshnessV1 {
    Unobserved,
    Fresh,
    Stale,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyDeploymentNodeStatusV1 {
    pub schema_version: u32,
    pub node_id: String,
    pub authenticated_operator_id: String,
    pub freshness: StrategyDeploymentNodeFreshnessV1,
    pub received_at_ms: Option<u64>,
    pub source_observed_at_ms: Option<u64>,
    pub age_ms: Option<u64>,
    pub deployments: Vec<StrategyDeploymentStatusV1>,
}

/// Project trusted node snapshots without merging worker ownership across nodes.
pub fn aggregate_strategy_deployment_nodes(
    registered_nodes: &[RegisteredStrategyDeploymentNodeV1],
    received_snapshots: &[ReceivedStrategyDeploymentNodeSnapshotV1],
    now_ms: u64,
) -> Result<Vec<StrategyDeploymentNodeStatusV1>, String> {
    use std::collections::{BTreeMap, BTreeSet};

    if registered_nodes.len() > 1024 || received_snapshots.len() > 1024 {
        return Err("strategy deployment node count exceeds 1024".into());
    }
    let mut registrations = BTreeMap::new();
    let mut authenticated_operators = BTreeSet::new();
    for node in registered_nodes {
        if !valid_deployment_node_identity(&node.node_id)
            || !valid_deployment_node_identity(&node.authenticated_operator_id)
            || node.freshness_window_ms == 0
            || node.freshness_window_ms > 30 * 24 * 60 * 60 * 1000
            || !authenticated_operators.insert(node.authenticated_operator_id.as_str())
            || registrations.insert(node.node_id.as_str(), node).is_some()
        {
            return Err("strategy deployment node registration is invalid or duplicated".into());
        }
    }
    let mut received = BTreeMap::new();
    for snapshot in received_snapshots {
        if !valid_deployment_node_identity(&snapshot.node_id)
            || !valid_deployment_node_identity(&snapshot.authenticated_operator_id)
        {
            return Err("strategy deployment node snapshot identity is invalid".into());
        }
        let Some(registration) = registrations.get(snapshot.node_id.as_str()) else {
            return Err(format!(
                "strategy deployment snapshot came from unregistered node {}",
                snapshot.node_id
            ));
        };
        if registration.authenticated_operator_id != snapshot.authenticated_operator_id {
            return Err(format!(
                "strategy deployment node {} authenticated as the wrong operator",
                snapshot.node_id
            ));
        }
        if snapshot.snapshot.schema_version != 1
            || snapshot.received_at_ms == 0
            || snapshot.received_at_ms > now_ms
            || snapshot.snapshot.generated_at_ms == 0
            || snapshot.snapshot.generated_at_ms > snapshot.received_at_ms
            || snapshot.snapshot.deployments.len() > 1024
            || received
                .insert(snapshot.node_id.as_str(), snapshot)
                .is_some()
        {
            return Err(format!(
                "strategy deployment snapshot for node {} is invalid or duplicated",
                snapshot.node_id
            ));
        }
        let mut worker_ids = BTreeSet::new();
        for deployment in &snapshot.snapshot.deployments {
            if deployment.schema_version != 1
                || deployment.worker_id.trim().is_empty()
                || deployment
                    .observed_at_ms
                    .is_some_and(|observed| observed > snapshot.snapshot.generated_at_ms)
                || !worker_ids.insert(deployment.worker_id.as_str())
            {
                return Err(format!(
                    "strategy deployment snapshot for node {} has invalid worker identity or time",
                    snapshot.node_id
                ));
            }
        }
    }

    Ok(registered_nodes
        .iter()
        .map(|registration| {
            let snapshot = received.get(registration.node_id.as_str()).copied();
            let (freshness, received_at_ms, source_observed_at_ms, age_ms, deployments) =
                match snapshot {
                    None => (
                        StrategyDeploymentNodeFreshnessV1::Unobserved,
                        None,
                        None,
                        None,
                        Vec::new(),
                    ),
                    Some(snapshot) => {
                        let age = now_ms.saturating_sub(snapshot.snapshot.generated_at_ms);
                        (
                            if age <= registration.freshness_window_ms {
                                StrategyDeploymentNodeFreshnessV1::Fresh
                            } else {
                                StrategyDeploymentNodeFreshnessV1::Stale
                            },
                            Some(snapshot.received_at_ms),
                            Some(snapshot.snapshot.generated_at_ms),
                            Some(age),
                            snapshot.snapshot.deployments.clone(),
                        )
                    }
                };
            StrategyDeploymentNodeStatusV1 {
                schema_version: 1,
                node_id: registration.node_id.clone(),
                authenticated_operator_id: registration.authenticated_operator_id.clone(),
                freshness,
                received_at_ms,
                source_observed_at_ms,
                age_ms,
                deployments,
            }
        })
        .collect())
}

fn valid_deployment_node_identity(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= 128
        && value == value.trim()
        && !value.chars().any(char::is_control)
}

pub(crate) type StrategyDeploymentsProvider =
    Arc<dyn Fn() -> Result<Vec<StrategyDeploymentStatusV1>, String> + Send + Sync>;
pub(crate) type StrategyDeploymentNodesProvider =
    Arc<dyn Fn() -> Result<Vec<StrategyDeploymentNodeStatusV1>, String> + Send + Sync>;
pub(crate) type StrategyDeploymentNodeSnapshotAcceptor =
    Arc<dyn Fn(&str, StrategyDeploymentNodeSnapshotV1, u64) -> Result<(), String> + Send + Sync>;

impl ApiService {
    pub fn with_strategy_deployment_nodes_provider(
        mut self,
        provider: impl Fn() -> Result<Vec<StrategyDeploymentNodeStatusV1>, String>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        self.strategy_deployment_nodes_provider = Some(Arc::new(provider));
        self
    }

    /// Install the durable receiver for node snapshots. `authenticated_operator_id` must come
    /// from the mTLS transport boundary; request JSON cannot supply or override it.
    pub fn with_strategy_deployment_node_snapshot_acceptor(
        mut self,
        acceptor: impl Fn(&str, StrategyDeploymentNodeSnapshotV1, u64) -> Result<(), String>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        self.strategy_deployment_node_snapshot_acceptor = Some(Arc::new(acceptor));
        self
    }

    pub(crate) fn strategy_deployment_nodes_response(&self) -> ApiResponse {
        let Some(provider) = &self.strategy_deployment_nodes_provider else {
            return ApiResponse::json(503, error_json("strategy_deployment_nodes_unavailable"));
        };
        match provider() {
            Ok(nodes) => serialized_json_response(200, &nodes),
            Err(error) => ApiResponse::json(
                503,
                error_json(&format!("strategy_deployment_nodes_unavailable: {error}")),
            ),
        }
    }

    pub(crate) fn receive_strategy_deployment_node_snapshot_response(
        &self,
        body: &str,
        timestamp: u64,
        authenticated_operator_id: Option<&str>,
    ) -> ApiResponse {
        let Some(accept) = &self.strategy_deployment_node_snapshot_acceptor else {
            return ApiResponse::json(503, error_json("strategy_deployment_nodes_unavailable"));
        };
        let Some(operator_id) = authenticated_operator_id.filter(|value| !value.trim().is_empty())
        else {
            return ApiResponse::json(403, error_json("authenticated_operator_required"));
        };
        let snapshot: StrategyDeploymentNodeSnapshotV1 = match serde_json::from_str(body) {
            Ok(snapshot) => snapshot,
            Err(error) => return ApiResponse::json(400, error_json(&error.to_string())),
        };
        match accept(operator_id, snapshot, timestamp) {
            Ok(()) => ApiResponse::json(202, "{\"schema_version\":1,\"accepted\":true}"),
            Err(error) if error.starts_with("invalid_snapshot:") => {
                ApiResponse::json(400, error_json(&error))
            }
            Err(error) if error.starts_with("unregistered_node:") => {
                ApiResponse::json(403, error_json(&error))
            }
            Err(error) if error.starts_with("stale_snapshot:") => {
                ApiResponse::json(409, error_json(&error))
            }
            Err(error) => ApiResponse::json(
                503,
                error_json(&format!("strategy_deployment_nodes_unavailable: {error}")),
            ),
        }
    }

    pub fn with_strategy_deployments_provider(
        mut self,
        provider: impl Fn() -> Result<Vec<StrategyDeploymentStatusV1>, String> + Send + Sync + 'static,
    ) -> Self {
        self.strategy_deployments_provider = Some(Arc::new(provider));
        self
    }

    pub(crate) fn strategy_deployments_response(&self, query: &str) -> ApiResponse {
        let worker_id = match query_param(query, "worker_id") {
            Ok(Some(id)) if id.trim().is_empty() => {
                return ApiResponse::json(400, error_json("worker_id must be non-empty"));
            }
            Ok(id) => id,
            Err(error) => return ApiResponse::json(400, error_json(error)),
        };
        let Some(provider) = &self.strategy_deployments_provider else {
            return ApiResponse::json(503, error_json("strategy_deployments_unavailable"));
        };
        match provider() {
            Ok(mut deployments) => {
                if let Some(id) = worker_id {
                    deployments.retain(|deployment| deployment.worker_id == id);
                    if deployments.is_empty() {
                        return ApiResponse::json(404, error_json("strategy_worker_not_found"));
                    }
                }
                serialized_json_response(200, &deployments)
            }
            Err(error) => ApiResponse::json(
                503,
                error_json(&format!("strategy_deployments_unavailable: {error}")),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deployment_reads_require_provider_and_existing_api_authentication() {
        let service = ApiService::new(ApiState::default());
        assert_eq!(
            service
                .handle("GET", "/strategies/deployments", "", 1)
                .status,
            503
        );
        let service = service.with_strategy_deployments_provider(|| Ok(vec![]));
        assert_eq!(
            service
                .handle("GET", "/strategies/deployments", "", 1)
                .status,
            200
        );
        assert_eq!(
            service
                .handle("GET", "/strategies/deployments?worker_id=missing", "", 1)
                .status,
            404
        );
        assert_eq!(
            service
                .handle("GET", "/strategies/deployments?worker_id=", "", 1)
                .status,
            400
        );
        assert_eq!(
            service
                .handle("GET", "/strategies/deployments?unknown=x", "", 1)
                .status,
            400
        );
        let service = ApiService::with_policy(ApiState::default(), ApiPolicy::new());
        assert_eq!(
            service
                .handle("GET", "/strategies/deployments", "", 1)
                .status,
            403
        );
    }
}
