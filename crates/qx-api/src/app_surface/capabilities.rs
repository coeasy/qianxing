use super::ApiResponse;
use qx_provider::ProviderRegistry;

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
        {"id":"backtest.multi_account","routes":["/app/multi-account-backtest","/app/multi-account-backtest/start"]},
        {"id":"experiments.parameter_grid","routes":["/app/run-experiment","/app/run-depth-experiment","/app/run-multi-leg-experiment","/app/run-multi-market-experiment"]},
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
