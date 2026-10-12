use crate::ApiResponse;
use qx_control::Permission;
use std::sync::Arc;

pub(crate) type Provider = Arc<dyn Fn(&str, &str) -> Result<String, (u16, String)> + Send + Sync>;

pub(crate) fn respond(
    operation: &str,
    body: &str,
    provider: Option<&Provider>,
    trading_authorized: bool,
) -> ApiResponse {
    if matches!(operation, "start" | "cancel") && !trading_authorized {
        return ApiResponse::json(403, r#"{"error":"trading_permission_required"}"#);
    }
    let Some(provider) = provider else {
        return ApiResponse::json(503, r#"{"error":"algo_execution_runtime_unavailable"}"#);
    };
    match provider(operation, body) {
        Ok(payload) => ApiResponse::json(200, payload),
        Err((status, message)) => {
            ApiResponse::json(status, serde_json::json!({"error": message}).to_string())
        }
    }
}

pub(crate) fn can_trade(permission: Option<Permission>) -> bool {
    permission
        .is_some_and(|permission| matches!(permission, Permission::Trading | Permission::Admin))
}
