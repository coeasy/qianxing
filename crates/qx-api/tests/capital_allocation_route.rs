use qx_api::{ApiService, ApiState};
use serde_json::{json, Value};

#[test]
fn capital_allocation_http_route_uses_the_shared_app_planner() {
    let body = json!({
        "schema_version": 1,
        "portfolio_id": "portfolio-api",
        "portfolio_budget_raw": 70,
        "accounts": [{"account_id": "acct-api", "available_cash_raw": 100}],
        "requests": [
            {"strategy_id": "strategy-b", "account_id": "acct-api", "requested_cash_raw": 60, "priority": 1},
            {"strategy_id": "strategy-a", "account_id": "acct-api", "requested_cash_raw": 60, "priority": 0}
        ]
    })
    .to_string();
    let response = ApiService::new(ApiState::default()).handle(
        "POST",
        "/app/plan-capital-allocation",
        &body,
        1,
    );
    assert_eq!(response.status, 200, "{}", response.body);
    let result: Value = serde_json::from_str(&response.body).unwrap();
    assert_eq!(result["allocations"][0]["strategy_id"], "strategy-a");
    assert_eq!(result["allocations"][0]["allocated_cash_raw"], 60);
    assert_eq!(result["allocations"][1]["allocated_cash_raw"], 10);
}
