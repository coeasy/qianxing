use super::ApiResponse;
use qx_core::Fnv1a;
use qx_provider::{DataRequest, ProviderRegistry};
use serde::Deserialize;
use serde_json::Value;
use std::sync::OnceLock;

const PRODUCT_CAPABILITY_CATALOG: &str = include_str!("../capability_catalog.generated.json");
static PRODUCT_CAPABILITY_CATALOG_VALUE: OnceLock<Result<Value, String>> = OnceLock::new();

pub(super) fn product_capability_catalog() -> Result<&'static Value, String> {
    match PRODUCT_CAPABILITY_CATALOG_VALUE.get_or_init(|| {
        serde_json::from_str(PRODUCT_CAPABILITY_CATALOG).map_err(|error| error.to_string())
    }) {
        Ok(catalog) => Ok(catalog),
        Err(error) => Err(error.clone()),
    }
}

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
        {"id":"capability.product_catalog","routes":["/app/capabilities","/app/capabilities/query"]},
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
        {"id":"research.provider_report_v1","routes":["/app/read-provider-report","/app/read-provider-depth-report"]},
        {"id":"research.multi_account_joint_report_v1","routes":["/app/read-multi-account-joint-report"]},
        {"id":"research.multi_leg_report_v1","routes":["/app/read-multi-leg-report"]},
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
    let catalog = match product_capability_catalog() {
        Ok(catalog) => catalog,
        Err(error) => return ApiResponse::json(500, crate::error_json(&error)),
    };
    document["coverage_catalog"] = serde_json::json!({
        "schema_version": catalog["schema_version"],
        "source_updated_at": catalog["source_updated_at"],
        "execution_scope_facets": catalog["execution_scope_facets"],
        "capabilities": catalog["index"],
    });
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CapabilityQuery {
    capability_id: Option<String>,
    venue: Option<String>,
    product: Option<String>,
    account_mode: Option<String>,
    account_model: Option<String>,
    margin_mode: Option<String>,
    position_mode: Option<String>,
    order_type: Option<String>,
    quantity_unit: Option<String>,
    operation: Option<String>,
}

fn query_matches(value: Option<&String>, candidate: Option<&Value>) -> bool {
    value.is_none_or(|value| {
        candidate
            .and_then(Value::as_str)
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(value.trim()))
    })
}

fn query_matches_list(value: Option<&String>, candidate: Option<&Value>) -> bool {
    value.is_none_or(|value| {
        candidate
            .and_then(Value::as_array)
            .is_some_and(|candidate| {
                candidate.iter().any(|item| {
                    item.as_str()
                        .is_some_and(|item| item.eq_ignore_ascii_case(value.trim()))
                })
            })
    })
}

fn scope_matches(scope: &Value, query: &CapabilityQuery) -> bool {
    query_matches(query.venue.as_ref(), scope.get("venue"))
        && query_matches(query.product.as_ref(), scope.get("product"))
        && query_matches(query.account_mode.as_ref(), scope.get("account_mode"))
        && query_matches_list(query.account_model.as_ref(), scope.get("account_models"))
        && query_matches_list(query.margin_mode.as_ref(), scope.get("margin_modes"))
        && query_matches_list(query.position_mode.as_ref(), scope.get("position_modes"))
        && query.operation.as_ref().is_none_or(|operation| {
            scope
                .get("operations")
                .and_then(Value::as_array)
                .is_some_and(|operations| {
                    operations.iter().any(|value| {
                        value
                            .as_str()
                            .is_some_and(|value| value.eq_ignore_ascii_case(operation.trim()))
                    })
                })
        })
        && query.order_type.as_ref().is_none_or(|order_type| {
            scope
                .get("order_shapes")
                .and_then(Value::as_array)
                .is_some_and(|shapes| {
                    shapes.iter().any(|shape| {
                        shape
                            .get("order_type")
                            .and_then(Value::as_str)
                            .is_some_and(|candidate| {
                                candidate.eq_ignore_ascii_case(order_type.trim())
                            })
                            && query.quantity_unit.as_ref().is_none_or(|unit| {
                                shape
                                    .get("quantity_unit")
                                    .and_then(Value::as_str)
                                    .is_some_and(|candidate| {
                                        candidate.eq_ignore_ascii_case(unit.trim())
                                    })
                            })
                    })
                })
        })
        && (query.order_type.is_some()
            || query.quantity_unit.as_ref().is_none_or(|unit| {
                scope
                    .get("order_shapes")
                    .and_then(Value::as_array)
                    .is_some_and(|shapes| {
                        shapes.iter().any(|shape| {
                            shape
                                .get("quantity_unit")
                                .and_then(Value::as_str)
                                .is_some_and(|candidate| {
                                    candidate.eq_ignore_ascii_case(unit.trim())
                                })
                        })
                    })
            }))
}

fn account_query_scope_matches(scope: &Value, query: &CapabilityQuery) -> bool {
    query_matches(query.venue.as_ref(), scope.get("venue"))
        && query_matches(query.product.as_ref(), scope.get("product"))
        && query_matches_list(query.account_model.as_ref(), scope.get("account_models"))
        && query.operation.as_ref().is_none_or(|operation| {
            scope
                .get("operations")
                .and_then(Value::as_array)
                .is_some_and(|operations| {
                    operations.iter().any(|value| {
                        value
                            .as_str()
                            .is_some_and(|value| value.eq_ignore_ascii_case(operation.trim()))
                    })
                })
        })
}

fn is_account_query_operation(operation: &str) -> bool {
    [
        "balance.read",
        "orders.read",
        "positions.read",
        "cashflows.read",
        "funding.read",
        "account_settings.read",
    ]
    .iter()
    .any(|candidate| candidate.eq_ignore_ascii_case(operation.trim()))
}

fn is_execution_operation(operation: &str) -> bool {
    ["submit", "cancel"]
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(operation.trim()))
}

fn product_capability_query_result(body: &str) -> Result<Value, (u16, String)> {
    let query = match serde_json::from_str::<CapabilityQuery>(body) {
        Ok(query) => query,
        Err(error) => return Err((400, error.to_string())),
    };
    let filters_provided = query.venue.is_some()
        || query.product.is_some()
        || query.account_mode.is_some()
        || query.account_model.is_some()
        || query.margin_mode.is_some()
        || query.position_mode.is_some()
        || query.order_type.is_some()
        || query.quantity_unit.is_some()
        || query.operation.is_some();
    if !filters_provided
        && query
            .capability_id
            .as_ref()
            .is_none_or(|capability_id| capability_id.trim().is_empty())
    {
        return Err((400, "capability_query_filter_required".into()));
    }
    for (field, value) in [
        ("capability_id", query.capability_id.as_ref()),
        ("venue", query.venue.as_ref()),
        ("product", query.product.as_ref()),
        ("account_mode", query.account_mode.as_ref()),
        ("account_model", query.account_model.as_ref()),
        ("margin_mode", query.margin_mode.as_ref()),
        ("position_mode", query.position_mode.as_ref()),
        ("order_type", query.order_type.as_ref()),
        ("quantity_unit", query.quantity_unit.as_ref()),
        ("operation", query.operation.as_ref()),
    ] {
        if value.is_some_and(|value| value.trim().is_empty()) {
            return Err((400, format!("{field}_must_be_non_empty")));
        }
    }
    if let Some(operation) = query.operation.as_deref() {
        if !is_account_query_operation(operation) && !is_execution_operation(operation) {
            return Err((400, "unsupported_capability_operation".into()));
        }
        if is_account_query_operation(operation)
            && (query.account_mode.is_some()
                || query.margin_mode.is_some()
                || query.position_mode.is_some()
                || query.order_type.is_some()
                || query.quantity_unit.is_some())
        {
            return Err((
                400,
                "account_query_operation_conflicts_with_order_filters".into(),
            ));
        }
    }
    let catalog = match product_capability_catalog() {
        Ok(catalog) => catalog,
        Err(error) => return Err((500, error)),
    };
    let Some(capabilities) = catalog.get("capabilities").and_then(Value::as_array) else {
        return Err((500, "capability_catalog_invalid".into()));
    };
    if let Some(capability_id) = query.capability_id.as_ref().filter(|_| !filters_provided) {
        let Some(capability) = capabilities
            .iter()
            .find(|entry| entry["id"] == *capability_id)
        else {
            return Err((404, "capability_not_found".into()));
        };
        return Ok(serde_json::json!({
            "schema_version": 1,
            "capability": capability,
        }));
    }

    let mut matches = Vec::new();
    for capability in capabilities {
        if query
            .capability_id
            .as_ref()
            .is_some_and(|id| capability["id"] != *id)
        {
            continue;
        }
        let account_query = query
            .operation
            .as_deref()
            .is_some_and(is_account_query_operation);
        let scope_key = if account_query {
            "account_query_scopes"
        } else {
            "execution_scopes"
        };
        let Some(scopes) = capability.get(scope_key).and_then(Value::as_array) else {
            continue;
        };
        for scope in scopes.iter().filter(|scope| {
            if account_query {
                account_query_scope_matches(scope, &query)
            } else {
                scope_matches(scope, &query)
            }
        }) {
            matches.push(serde_json::json!({
                "capability_id": capability["id"],
                "scope": scope,
                "capability": capability,
            }));
        }
    }
    let filters = serde_json::json!({
        "capability_id": query.capability_id,
        "venue": query.venue,
        "product": query.product,
        "account_mode": query.account_mode,
        "account_model": query.account_model,
        "margin_mode": query.margin_mode,
        "operation": query.operation,
    });
    Ok(serde_json::json!({
        "schema_version": 2,
        "filters": filters,
        "match_count": matches.len(),
        "matches": matches,
    }))
}

/// Query the generated product directory without starting an API server.
pub(super) fn product_capability_query_document(body: &str) -> Result<Value, String> {
    product_capability_query_result(body).map_err(|(_, message)| message)
}

/// Return a record by stable ID or exact declared venue/product/account scope.
pub(super) fn product_capability_query(body: &str) -> ApiResponse {
    match product_capability_query_result(body) {
        Ok(document) => ApiResponse::json(200, document.to_string()),
        Err((status, error)) => ApiResponse::json(status, crate::error_json(&error)),
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
