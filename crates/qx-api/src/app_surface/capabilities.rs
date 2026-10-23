use super::ApiResponse;
use qx_core::Fnv1a;
use qx_provider::{DataRequest, ProviderRegistry};

/// Runtime discovery of shared Rust use cases. This describes server routes,
/// not venue/provider deployment readiness; those are request-time checks.
pub(super) fn application_capabilities(
    provider_registry: Option<&ProviderRegistry>,
) -> ApiResponse {
    const BODY: &str = r#"{
      "schema_version": 1,
      "execution_backend": "shared-rust",
      "runtime_preflight_required": true,
      "capabilities": [
        {"id":"backtest.bar","routes":["/app/backtest","/app/backtest/start"]},
        {"id":"backtest.tick_orderbook","routes":["/app/depth-backtest","/app/depth-backtest/start"]},
        {"id":"backtest.multi_leg","routes":["/app/multi-leg-backtest","/app/multi-leg-backtest/start"]},
        {"id":"backtest.multi_market","routes":["/app/multi-market-backtest","/app/multi-market-backtest/start"]},
        {"id":"backtest.multi_market_tick_orderbook","routes":["/app/multi-market-depth-backtest","/app/multi-market-depth-backtest/start"]},
        {"id":"backtest.multi_account","routes":["/app/multi-account-backtest","/app/multi-account-backtest/start"]},
        {"id":"backtest.multi_account_joint","routes":["/app/multi-account-joint-backtest","/app/multi-account-joint-backtest/start"]},
        {"id":"backtest.multi_account_tick_orderbook","routes":["/app/multi-account-depth-backtest","/app/multi-account-depth-backtest/start"]},
        {"id":"data.provider_preflight","routes":["/app/capabilities/providers/preflight"]},
        {"id":"research.report_drilldown","routes":["/app/research-artifacts"]},
        {"id":"experiments.parameter_grid","routes":["/app/run-experiment","/app/run-depth-experiment","/app/run-multi-leg-experiment","/app/run-multi-market-experiment","/app/run-multi-market-depth-experiment","/app/run-multi-market-depth-experiment/start"]},
        {"id":"paper.commands","routes":["/app/paper/commands","/app/paper/algo-executions/start","/app/paper/algo-executions/get","/app/paper/algo-executions/cancel","/app/paper/algo-executions/list"]},
        {"id":"live.commands","routes":["/app/live/commands"]},
        {"id":"run.lifecycle_events","routes":["/app/runs/{run_id}","/app/runs/{run_id}/events","/app/runs/{run_id}/cancel"]}
      ],
      "limitations": [
        "Venue/provider support and credentials are checked at runtime.",
        "Live readiness requires a configured and separately verified venue adapter.",
        "Multi-account backtests keep each account isolated; they do not net cross-account risk."
      ]
    }"#;
    let mut document: serde_json::Value = match serde_json::from_str(BODY) {
        Ok(document) => document,
        Err(error) => {
            return ApiResponse::json(500, crate::error_json(&error.to_string()));
        }
    };
    let providers = provider_registry
        .map(ProviderRegistry::capabilities)
        .unwrap_or_default();
    let providers = match serde_json::to_value(providers) {
        Ok(providers) => providers,
        Err(error) => return ApiResponse::json(500, crate::error_json(&error.to_string())),
    };
    document["provider_capabilities"] = providers;
    match serde_json::to_string(&document) {
        Ok(body) => ApiResponse::json(200, body),
        Err(error) => ApiResponse::json(500, crate::error_json(&error.to_string())),
    }
}

/// Match a concrete data request against configured Provider declarations without I/O.
pub(super) fn provider_preflight(
    body: &str,
    provider_registry: Option<&ProviderRegistry>,
) -> ApiResponse {
    let Some(provider_registry) = provider_registry else {
        return ApiResponse::json(503, crate::error_json("provider_registry_unavailable"));
    };
    let request = match DataRequest::from_json(body) {
        Ok(request) => request,
        Err(error) => return ApiResponse::json(400, crate::error_json(&error.message)),
    };
    let canonical = match request.to_json() {
        Ok(canonical) => canonical,
        Err(error) => return ApiResponse::json(400, crate::error_json(&error.message)),
    };
    let mut hasher = Fnv1a::new();
    hasher.write_text(&canonical);
    let request_fingerprint = format!("{:016x}", hasher.finish());
    let provider_ids = match provider_registry.candidates_for_request(&request) {
        Ok(provider_ids) => provider_ids,
        Err(error) => return ApiResponse::json(400, crate::error_json(&format!("{error:?}"))),
    };
    let declarations = provider_registry.capabilities();
    let mut candidates = Vec::with_capacity(provider_ids.len());
    for provider_id in provider_ids {
        let Some(declaration) = declarations
            .iter()
            .find(|declaration| declaration.provider_id == provider_id)
        else {
            return ApiResponse::json(500, crate::error_json("provider_registry_inconsistent"));
        };
        candidates.push(declaration.clone());
    }
    ApiResponse::json(
        200,
        serde_json::json!({
            "schema_version": 1,
            "request_fingerprint": request_fingerprint,
            "matched": !candidates.is_empty(),
            "candidates": candidates,
        })
        .to_string(),
    )
}
