use crate::{app_surface, ApiResponse, ApiService};
use qx_protocol::{
    AccountSnapshot, ProjectionEnvelope, ProjectionLineage, PROJECTION_ENVELOPE_SCHEMA_VERSION,
};

pub(crate) fn account_snapshot_envelope(
    snapshot: AccountSnapshot,
    source_digest: u64,
) -> Result<ProjectionEnvelope<serde_json::Value>, String> {
    let snapshot_json = snapshot
        .to_json()
        .map_err(|error| format!("账户快照线格式序列化失败: {error:?}"))?;
    let data = serde_json::from_str(&snapshot_json)
        .map_err(|error| format!("账户快照线格式无法解析: {error}"))?;
    let event_seq = snapshot.header.event_seq;
    let state_hash = snapshot.state_hash();
    Ok(ProjectionEnvelope {
        schema_version: PROJECTION_ENVELOPE_SCHEMA_VERSION,
        kind: "account_snapshot".into(),
        tenant_id: snapshot.header.account_id.clone(),
        run_id: format!(
            "account:{}:{}",
            snapshot.header.account_id, snapshot.header.venue_id
        ),
        account_id: snapshot.header.account_id.clone(),
        portfolio_id: snapshot.header.portfolio_id.clone(),
        venue_id: snapshot.header.venue_id.clone(),
        as_of: snapshot.header.as_of,
        event_seq,
        cursor: format!("{event_seq}:{state_hash:016x}"),
        state_hash,
        source: "eventlog".into(),
        lineage: ProjectionLineage {
            source_digest: format!("{source_digest:016x}"),
            ..ProjectionLineage::default()
        },
        data,
    })
}

pub(crate) fn account_snapshot_response(snapshot: AccountSnapshot) -> ApiResponse {
    match snapshot.to_json() {
        Ok(body) => ApiResponse::json(200, body),
        Err(error) => ApiResponse::json(
            500,
            serde_json::json!({"error": "account_snapshot_serialization_failed", "message": format!("{error:?}")}).to_string(),
        ),
    }
}

pub(crate) fn contract_matrix_response() -> ApiResponse {
    match qx_core::contract::contract_matrix_json() {
        Ok(payload) => ApiResponse::json(200, payload),
        Err(error) => ApiResponse::json(500, serde_json::json!({"error": "contract_matrix_serialization_failed", "message": error.to_string()}).to_string()),
    }
}

impl ApiService {
    pub(crate) fn route_app_surface(
        &self,
        method: &str,
        route: &str,
        query: &str,
        body: &str,
    ) -> Option<ApiResponse> {
        app_surface::route_app_run(
            method,
            route,
            query,
            body,
            &self.app_paths,
            &self.app_runs,
            self.provider_registry.as_deref(),
        )
    }
}
